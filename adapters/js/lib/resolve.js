'use strict';
// Find what a call reaches, across files, for CommonJS code.
//
// The resolver evaluates small expressions to "values". A value is one of:
//   { t: 'fn',   file, at, name, method }   a function. `at` is the index of its declared name.
//   { t: 'obj',  file, members, spreads }   an object literal or a module's exports object
//   { t: 'cls',  cls }                      a class
//   { t: 'inst', cls }                      an instance made with `new`
//   { t: 'ext',  pkg, member }              something from a package or Node built-in
//   UNKNOWN                                 anything the resolver cannot follow
//
// The resolver answers only when it is sure. A name it cannot follow gives UNKNOWN, and the call
// gets no record, so aft keeps its own answer for that call.

const path = require('node:path');
const { propertyKeyName } = require('./parse-file.js');

const UNKNOWN = { t: 'unknown' };

// A value that is computed on first use. Reading it again while it is being computed (a cycle
// between files, or `const a = b; const b = a`) gives UNKNOWN instead of looping.
class Lazy {
  constructor(compute) {
    this.compute = compute;
    this.state = 'new';
    this.value = UNKNOWN;
  }

  get() {
    if (this.state === 'done') return this.value;
    if (this.state === 'running') return UNKNOWN;
    this.state = 'running';
    this.value = force(this.compute());
    this.state = 'done';
    return this.value;
  }
}

function force(value) {
  return value instanceof Lazy ? value.get() : value;
}

// 'node:fs' -> 'fs', 'lodash/fp' -> 'lodash', '@scope/pkg/sub' -> '@scope/pkg'
function packageName(spec) {
  const bare = spec.startsWith('node:') ? spec.slice('node:'.length) : spec;
  const parts = bare.split('/');
  return bare.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0];
}

function isRelativeSpec(spec) {
  return spec === '.' || spec === '..' || spec.startsWith('./') || spec.startsWith('../');
}

// The string argument of a require call, or null for anything else.
function stringArgument(call) {
  if (call.arguments.length !== 1) return null;
  const arg = call.arguments[0];
  if (arg.type === 'Literal' && typeof arg.value === 'string') return arg.value;
  if (arg.type === 'TemplateLiteral' && arg.expressions.length === 0) return arg.quasis[0].value.cooked;
  return null;
}

// `obj.name` or `obj['name']`: the member name, or null when it is computed.
function memberName(member) {
  const property = member.property;
  if (!member.computed && property.type === 'Identifier') return property.name;
  if (member.computed && property.type === 'Literal' && typeof property.value === 'string') return property.value;
  return null;
}

class Resolver {
  // files: Map of root-relative path -> FileInfo (from parse-file.js)
  constructor(files) {
    this.files = files;
    this.modules = new Map();
  }

  // ---- modules ---------------------------------------------------------------------------

  requireValue(spec, fromFile) {
    if (isRelativeSpec(spec)) {
      const target = this.resolveRelative(fromFile, spec);
      return target === null ? UNKNOWN : this.moduleOf(target);
    }
    if (spec.startsWith('/') || spec.endsWith('.json')) return UNKNOWN;
    return { t: 'ext', pkg: packageName(spec), member: null };
  }

  resolveRelative(fromFile, spec) {
    const base = path.posix.join(path.posix.dirname(fromFile.rel), spec);
    if (base.startsWith('..')) return null; // outside the root
    const candidates = [base, `${base}.js`, `${base}.cjs`, `${base}/index.js`, `${base}/index.cjs`];
    for (const candidate of candidates) {
      const file = this.files.get(candidate);
      if (file !== undefined) return file;
    }
    return null;
  }

  moduleOf(file) {
    let lazy = this.modules.get(file.rel);
    if (lazy === undefined) {
      lazy = new Lazy(() => this.computeModuleExports(file));
      this.modules.set(file.rel, lazy);
    }
    return lazy.get();
  }

  // Reads the top-level `module.exports = …`, `module.exports.x = …` and `exports.x = …`
  // statements, in order. Nested statements (inside `if` or functions) are not followed.
  computeModuleExports(file) {
    const scope = file.programScope;
    const moduleIsOurs = !scope.bindings.has('module');
    const exportsIsOurs = !scope.bindings.has('exports');
    const original = this.newObject(file);
    let current = original;
    let replaced = false;

    for (const statement of file.program.body) {
      if (statement.type !== 'ExpressionStatement') continue;
      const assignment = statement.expression;
      if (assignment.type !== 'AssignmentExpression' || assignment.operator !== '=') continue;
      const left = assignment.left;
      if (left.type !== 'MemberExpression') continue;

      if (moduleIsOurs && this.isModuleExports(left)) {
        let right = assignment.right;
        while (right.type === 'AssignmentExpression' && right.operator === '=') right = right.right; // module.exports = exports = X
        replaced = true;
        current = right.type === 'ObjectExpression'
          ? this.objectValue(right, scope, file)
          : new Lazy(() => this.evalExpr(right, scope, file, null));
        continue;
      }

      const name = memberName(left);
      if (name === null) continue;
      const viaModule = moduleIsOurs && this.isModuleExports(left.object);
      const viaExports = exportsIsOurs && left.object.type === 'Identifier' && left.object.name === 'exports';
      let target = null;
      if (viaExports && !replaced) target = original;
      if (viaModule && current.t === 'obj') target = current;
      if (target === null) continue;
      const hint = { at: left.property.start, name };
      target.members.set(name, new Lazy(() => this.evalExpr(assignment.right, scope, file, hint)));
    }
    return current;
  }

  isModuleExports(node) {
    return node.type === 'MemberExpression'
      && node.object.type === 'Identifier' && node.object.name === 'module'
      && memberName(node) === 'exports';
  }

  newObject(file) {
    return { t: 'obj', file, members: new Map(), spreads: [] };
  }

  // ---- expressions -----------------------------------------------------------------------

  // hint = { at, name }: the declared name for a function written without its own name,
  // for example the variable in `const f = () => {}` or the key in `{ f: () => {} }`.
  evalExpr(node, scope, file, hint) {
    switch (node.type) {
      case 'Identifier': {
        const binding = scope.lookup(node.name);
        return binding === null ? UNKNOWN : force(this.bindingValue(binding));
      }
      case 'ChainExpression':
        return this.evalExpr(node.expression, scope, file, null);
      case 'CallExpression': {
        const isRequire = node.callee.type === 'Identifier' && node.callee.name === 'require' && scope.lookup('require') === null;
        const spec = isRequire ? stringArgument(node) : null;
        return spec === null ? UNKNOWN : force(this.requireValue(spec, file));
      }
      case 'MemberExpression': {
        const name = memberName(node);
        if (name === null) return UNKNOWN;
        return this.getMember(this.evalExpr(node.object, scope, file, null), name);
      }
      case 'NewExpression': {
        const constructor = this.evalExpr(node.callee, scope, file, null);
        return constructor.t === 'cls' ? { t: 'inst', cls: constructor.cls } : UNKNOWN;
      }
      case 'FunctionExpression':
      case 'ArrowFunctionExpression': {
        if (hint) return { t: 'fn', file, at: hint.at, name: hint.name, method: false };
        if (node.id) return { t: 'fn', file, at: node.id.start, name: node.id.name, method: false };
        return UNKNOWN;
      }
      case 'ObjectExpression':
        return this.objectValue(node, scope, file);
      case 'ClassExpression':
        return { t: 'cls', cls: this.classInfo(node, scope, file) };
      default:
        return UNKNOWN;
    }
  }

  bindingValue(binding) {
    if (binding.lazy === null) binding.lazy = new Lazy(() => this.computeBinding(binding));
    return binding.lazy;
  }

  computeBinding(binding) {
    if (binding.assigned) return UNKNOWN;
    const { file, scope, idNode } = binding;
    if (binding.kind === 'function') {
      return { t: 'fn', file, at: idNode.start, name: binding.name, method: false };
    }
    if (binding.kind === 'class') {
      return { t: 'cls', cls: this.classInfo(binding.declNode, scope, file) };
    }
    if (binding.kind !== 'var' || binding.init === null || binding.path === null) return UNKNOWN;
    const hint = binding.path.length === 0 ? { at: idNode.start, name: binding.name } : null;
    let value = this.evalExpr(binding.init, scope, file, hint);
    for (const key of binding.path) value = this.getMember(value, key);
    return value;
  }

  objectValue(node, scope, file) {
    const object = this.newObject(file);
    for (const property of node.properties) {
      if (property.type === 'SpreadElement') {
        object.spreads.push(new Lazy(() => this.evalExpr(property.argument, scope, file, null)));
        continue;
      }
      if (property.kind !== 'init') continue; // getters and setters
      const key = propertyKeyName(property);
      if (key === null) continue;
      const hint = { at: property.key.start, name: key };
      object.members.set(key, new Lazy(() => this.evalExpr(property.value, scope, file, hint)));
    }
    return object;
  }

  classInfo(node, scope, file) {
    if (node.aftClassInfo) return node.aftClassInfo;
    const info = { file, methods: new Map(), statics: new Map(), superclass: null };
    node.aftClassInfo = info;
    for (const element of node.body.body) {
      if (element.type !== 'MethodDefinition' && element.type !== 'PropertyDefinition') continue;
      if (element.type === 'MethodDefinition' && element.kind !== 'method') continue;
      if (element.type === 'PropertyDefinition' && !/^(Arrow)?Function/.test(element.value ? element.value.type : '')) continue;
      const key = propertyKeyName(element);
      if (key === null) continue;
      (element.static ? info.statics : info.methods).set(key, element.key.start);
    }
    if (node.superClass) info.superclass = new Lazy(() => this.evalExpr(node.superClass, scope, file, null));
    return info;
  }

  getMember(value, name) {
    switch (value.t) {
      case 'obj': {
        const own = value.members.get(name);
        if (own !== undefined) return force(own);
        for (const spread of value.spreads) {
          const found = this.getMember(force(spread), name);
          if (found !== UNKNOWN) return found;
        }
        return UNKNOWN;
      }
      case 'inst':
        return this.findMethod(value.cls, name, false, 0);
      case 'cls':
        return this.findMethod(value.cls, name, true, 0);
      case 'ext':
        return { t: 'ext', pkg: value.pkg, member: name };
      default:
        return UNKNOWN;
    }
  }

  findMethod(cls, name, wantStatic, depth) {
    const at = (wantStatic ? cls.statics : cls.methods).get(name);
    if (at !== undefined) return { t: 'fn', file: cls.file, at, name, method: true };
    if (cls.superclass === null || depth > 20) return UNKNOWN;
    const parent = force(cls.superclass);
    return parent.t === 'cls' ? this.findMethod(parent.cls, name, wantStatic, depth + 1) : UNKNOWN;
  }

  // ---- calls -----------------------------------------------------------------------------

  // Returns { name, targets: [{ file, at, name, kind }], external: [{ name, package }] }
  // or null when the call is not resolved.
  resolveCall(site, file) {
    const callee = site.node.callee;
    let written = null;
    if (callee.type === 'Identifier') written = callee.name;
    else if (callee.type === 'MemberExpression') written = memberName(callee);
    if (written === null) return null;

    const value = this.evalExpr(callee, site.scope, file, null);
    if (value.t === 'fn') {
      const kind = value.method ? 'method' : value.file === file ? 'local' : 'require';
      return { name: written, targets: [{ file: value.file, at: value.at, name: value.name, kind }], external: [] };
    }
    if (value.t === 'ext') {
      return { name: written, targets: [], external: [{ name: value.member || value.pkg, package: value.pkg }] };
    }
    return null;
  }
}

module.exports = { Resolver, packageName };
