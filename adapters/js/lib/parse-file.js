'use strict';
// Parse one JavaScript file and collect what the resolver needs:
//   - the scope tree (which name means which declaration),
//   - every call expression with the scope it sits in.
// This file knows nothing about modules. It only reads one file's syntax.

const acorn = require('../vendor/acorn.js');

// Positions follow the contract: 1-based line, 1-based column in UTF-8 bytes.
// Lines split at "\n" only, the same way aft and tree-sitter count rows.
function lineStartsOf(src) {
  const starts = [0];
  for (let i = src.indexOf('\n'); i !== -1; i = src.indexOf('\n', i + 1)) starts.push(i + 1);
  return starts;
}

function positionOf(file, index) {
  const starts = file.lineStarts;
  let low = 0;
  let high = starts.length - 1;
  while (low < high) {
    const middle = (low + high + 1) >> 1;
    if (starts[middle] <= index) low = middle;
    else high = middle - 1;
  }
  const column = Buffer.byteLength(file.src.slice(starts[low], index)) + 1;
  return { line: low + 1, col: column };
}

// A call such as `a.b?.(x)` has its open parenthesis after the callee, maybe after `)`, `?.`,
// spaces and comments. Scan forward from the end of the callee to find it.
function openParenIndex(src, calleeEnd) {
  let i = calleeEnd;
  while (i < src.length) {
    const c = src[i];
    if (c === '(') return i;
    if (c === '/' && src[i + 1] === '/') {
      const newline = src.indexOf('\n', i);
      i = newline === -1 ? src.length : newline + 1;
    } else if (c === '/' && src[i + 1] === '*') {
      const close = src.indexOf('*/', i + 2);
      i = close === -1 ? src.length : close + 2;
    } else {
      i += 1; // space, `)`, `?` or `.`
    }
  }
  return -1;
}

function parseSource(src) {
  const base = {
    ecmaVersion: 'latest',
    allowHashBang: true,
    allowReturnOutsideFunction: true,
    allowAwaitOutsideFunction: true,
  };
  try {
    return acorn.parse(src, { ...base, sourceType: 'script' });
  } catch (scriptError) {
    try {
      return acorn.parse(src, { ...base, sourceType: 'module' });
    } catch (moduleError) {
      throw scriptError;
    }
  }
}

class Scope {
  constructor(parent) {
    this.parent = parent;
    this.bindings = new Map();
  }

  lookup(name) {
    for (let scope = this; scope !== null; scope = scope.parent) {
      const binding = scope.bindings.get(name);
      if (binding !== undefined) return binding;
    }
    return null;
  }
}

// A binding is one declared name. `init` and `path` say where its value comes from:
//   const m = EXPR            init = EXPR, path = []
//   const { a: { b } } = EXPR init = EXPR, path = ['a', 'b']
// init === null means "the value is not known" (parameters, loop variables, arrays, rest).
// `assigned` means some later code writes the name again, so the value is not trusted.
function addBinding(scope, file, fields) {
  const existing = scope.bindings.get(fields.name);
  if (existing !== undefined) {
    existing.assigned = true; // declared twice: do not trust either value
    return;
  }
  scope.bindings.set(fields.name, {
    name: fields.name,
    kind: fields.kind, // 'function' | 'class' | 'var' | 'param'
    idNode: fields.idNode,
    declNode: fields.declNode || null,
    init: fields.init === undefined ? null : fields.init,
    path: fields.path === undefined ? null : fields.path,
    scope,
    file,
    assigned: false,
    lazy: null,
  });
}

function propertyKeyName(property) {
  const key = property.key;
  if (!property.computed && key.type === 'Identifier') return key.name;
  if (key.type === 'Literal' && typeof key.value === 'string') return key.value;
  return null;
}

function declarePattern(pattern, scope, file, kind, init, path) {
  switch (pattern.type) {
    case 'Identifier':
      addBinding(scope, file, { name: pattern.name, kind, idNode: pattern, init, path });
      return;
    case 'ObjectPattern':
      for (const property of pattern.properties) {
        if (property.type === 'RestElement') {
          declarePattern(property.argument, scope, file, kind, null, null);
          continue;
        }
        const key = propertyKeyName(property);
        const known = init !== null && path !== null && key !== null;
        declarePattern(property.value, scope, file, kind, known ? init : null, known ? [...path, key] : null);
      }
      return;
    case 'AssignmentPattern':
      declarePattern(pattern.left, scope, file, kind, init, path);
      return;
    case 'ArrayPattern':
      for (const element of pattern.elements) {
        if (element !== null) declarePattern(element, scope, file, kind, null, null);
      }
      return;
    case 'RestElement':
      declarePattern(pattern.argument, scope, file, kind, null, null);
      return;
    default:
      return; // a member expression in a for-in head, for example
  }
}

function declareVariables(declaration, scope, file) {
  for (const declarator of declaration.declarations) {
    const hasInit = declarator.init !== null;
    declarePattern(declarator.id, scope, file, 'var', hasInit ? declarator.init : null, hasInit ? [] : null);
  }
}

// `var` declarations belong to the whole function, wherever they sit in blocks.
function hoistVars(node, scope, file) {
  if (node === null || typeof node !== 'object') return;
  if (Array.isArray(node)) {
    for (const child of node) hoistVars(child, scope, file);
    return;
  }
  if (typeof node.type !== 'string') return;
  if (/^(Function|Arrow|Class)/.test(node.type)) return;
  if (node.type === 'VariableDeclaration' && node.kind === 'var') declareVariables(node, scope, file);
  for (const key of Object.keys(node)) {
    if (key !== 'type') hoistVars(node[key], scope, file);
  }
}

// let, const, function and class declarations belong to the block that holds them.
function declareLexical(statements, scope, file) {
  for (const statement of statements) {
    if (statement.type === 'VariableDeclaration' && statement.kind !== 'var') {
      declareVariables(statement, scope, file);
    } else if (statement.type === 'FunctionDeclaration' && statement.id) {
      addBinding(scope, file, { name: statement.id.name, kind: 'function', idNode: statement.id, declNode: statement });
    } else if (statement.type === 'ClassDeclaration' && statement.id) {
      addBinding(scope, file, { name: statement.id.name, kind: 'class', idNode: statement.id, declNode: statement });
    }
  }
}

function markAssigned(target, scope) {
  if (target === null) return;
  switch (target.type) {
    case 'Identifier': {
      const binding = scope.lookup(target.name);
      if (binding !== null) binding.assigned = true;
      return;
    }
    case 'ObjectPattern':
      for (const property of target.properties) markAssigned(property.type === 'RestElement' ? property.argument : property.value, scope);
      return;
    case 'ArrayPattern':
      for (const element of target.elements) markAssigned(element, scope);
      return;
    case 'AssignmentPattern':
      markAssigned(target.left, scope);
      return;
    case 'RestElement':
      markAssigned(target.argument, scope);
      return;
    default:
      return;
  }
}

function walkChildren(node, scope, file) {
  for (const key of Object.keys(node)) {
    if (key === 'type') continue;
    const child = node[key];
    if (Array.isArray(child)) {
      for (const item of child) if (item !== null && typeof item.type === 'string') walk(item, scope, file);
    } else if (child !== null && typeof child === 'object' && typeof child.type === 'string') {
      walk(child, scope, file);
    }
  }
}

function walkFunction(node, scope, file) {
  const inner = new Scope(scope);
  if (node.type === 'FunctionExpression' && node.id) {
    declarePattern(node.id, inner, file, 'param', null, null);
  }
  for (const param of node.params) declarePattern(param, inner, file, 'param', null, null);
  if (node.body.type === 'BlockStatement') {
    hoistVars(node.body.body, inner, file);
    declareLexical(node.body.body, inner, file);
    for (const statement of node.body.body) walk(statement, inner, file);
  } else {
    walk(node.body, inner, file);
  }
  for (const param of node.params) walk(param, inner, file); // default values
}

function walk(node, scope, file) {
  switch (node.type) {
    case 'FunctionDeclaration':
    case 'FunctionExpression':
    case 'ArrowFunctionExpression':
      walkFunction(node, scope, file);
      return;
    case 'BlockStatement':
    case 'StaticBlock': {
      const inner = new Scope(scope);
      declareLexical(node.body, inner, file);
      for (const statement of node.body) walk(statement, inner, file);
      return;
    }
    case 'ForStatement':
    case 'ForInStatement':
    case 'ForOfStatement': {
      const inner = new Scope(scope);
      const head = node.type === 'ForStatement' ? node.init : node.left;
      if (head !== null && head.type === 'VariableDeclaration' && head.kind !== 'var') {
        if (node.type === 'ForStatement') declareVariables(head, inner, file);
        else for (const d of head.declarations) declarePattern(d.id, inner, file, 'param', null, null);
      }
      if (node.type !== 'ForStatement') markAssigned(head.type === 'VariableDeclaration' ? null : head, inner);
      walkChildren(node, inner, file);
      return;
    }
    case 'SwitchStatement': {
      walk(node.discriminant, scope, file);
      const inner = new Scope(scope);
      for (const switchCase of node.cases) declareLexical(switchCase.consequent, inner, file);
      for (const switchCase of node.cases) walk(switchCase, inner, file);
      return;
    }
    case 'CatchClause': {
      const inner = new Scope(scope);
      if (node.param !== null) declarePattern(node.param, inner, file, 'param', null, null);
      walk(node.body, inner, file);
      return;
    }
    case 'AssignmentExpression':
      markAssigned(node.left, scope);
      walkChildren(node, scope, file);
      return;
    case 'UpdateExpression':
      markAssigned(node.argument, scope);
      walkChildren(node, scope, file);
      return;
    case 'CallExpression':
      file.callSites.push({ node, scope });
      walkChildren(node, scope, file);
      return;
    default:
      walkChildren(node, scope, file);
  }
}

// Returns a FileInfo, or throws the parser's syntax error.
function parseFile(rel, src) {
  const program = parseSource(src);
  const file = {
    rel,
    src,
    lineStarts: lineStartsOf(src),
    program,
    programScope: new Scope(null),
    callSites: [],
  };
  hoistVars(program.body, file.programScope, file);
  declareLexical(program.body, file.programScope, file);
  for (const statement of program.body) walk(statement, file.programScope, file);
  return file;
}

module.exports = { parseFile, positionOf, openParenIndex, propertyKeyName };
