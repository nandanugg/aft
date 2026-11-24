package main

import (
	"go/ast"
	"go/token"
	"go/types"
	"sort"
	"strings"
)

// Producer: call sites. One site record for every call whose callee type information names.
//
// What is left out, on purpose (contract v1, decision 6):
//   - a call through a function value (a variable, a field, a call result): there is no declared callee;
//   - conversions and builtins: they are not calls to a declared function;
//   - the universe method error.Error(): every type has one, so the list of targets is noise;
//   - an interface call when a generic type of the repository declares all methods of the interface:
//     the adapter cannot test generic types, so it cannot promise the list of targets is complete.
// aft keeps its own answer for these sites.

const (
	kindStatic    = "static"
	kindConcrete  = "concrete"
	kindInterface = "interface"
	// kindInterfaceMethod marks the interface method that an interface call names (not an implementation).
	kindInterfaceMethod = "interface method"
)

// callee is what a call expression names.
type callee struct {
	fn   *types.Func // origin of the called function or method
	kind string
	// iface is the interface type whose implementers can run; only set for kindInterface.
	iface types.Type
	// name is the called name as written at the site.
	name string
}

// resolveCallee finds the declared function a call expression names, if any.
func resolveCallee(info *types.Info, call *ast.CallExpr) (callee, bool) {
	fun := ast.Unparen(call.Fun)
	// An explicit instantiation f[T](x) has the function inside an index expression.
	switch f := fun.(type) {
	case *ast.IndexExpr:
		fun = ast.Unparen(f.X)
	case *ast.IndexListExpr:
		fun = ast.Unparen(f.X)
	}

	switch f := fun.(type) {
	case *ast.Ident:
		fn, ok := info.Uses[f].(*types.Func)
		if !ok {
			return callee{}, false // variable, builtin or type conversion
		}
		return callee{fn: fn.Origin(), kind: kindStatic, name: f.Name}, true

	case *ast.SelectorExpr:
		selection := info.Selections[f]
		if selection == nil {
			// pkg.F: a qualified identifier.
			fn, ok := info.Uses[f.Sel].(*types.Func)
			if !ok {
				return callee{}, false
			}
			return callee{fn: fn.Origin(), kind: kindStatic, name: f.Sel.Name}, true
		}
		if selection.Kind() == types.FieldVal {
			return callee{}, false // a function-valued field
		}
		fn, ok := selection.Obj().(*types.Func)
		if !ok {
			return callee{}, false
		}
		fn = fn.Origin()
		// The static type of the receiver value decides: an interface (or a type parameter) dispatches.
		// A struct that embeds an interface calls the embedded interface's method.
		if isInterfaceType(selection.Recv()) {
			return callee{fn: fn, kind: kindInterface, iface: selection.Recv(), name: f.Sel.Name}, true
		}
		if recv := fn.Type().(*types.Signature).Recv(); recv != nil && isInterfaceType(recv.Type()) {
			return callee{fn: fn, kind: kindInterface, iface: recv.Type(), name: f.Sel.Name}, true
		}
		return callee{fn: fn, kind: kindConcrete, name: f.Sel.Name}, true
	}
	return callee{}, false
}

func isInterfaceType(t types.Type) bool {
	if t == nil {
		return false
	}
	_, ok := types.Unalias(t).Underlying().(*types.Interface)
	return ok
}

// flowTags is the control-flow context of a call: where in the function it runs.
type flowTags struct {
	inLoop, inErrorBranch, inDefer, inGoroutine bool
}

func (t flowTags) list() []string {
	var tags []string
	if t.inLoop {
		tags = append(tags, "in_loop")
	}
	if t.inErrorBranch {
		tags = append(tags, "in_error_branch")
	}
	if t.inDefer {
		tags = append(tags, "in_defer")
	}
	if t.inGoroutine {
		tags = append(tags, "in_goroutine")
	}
	return tags
}

// siteProducer walks the syntax trees of one package and appends site records.
type siteProducer struct {
	proj *project
	impl *implIndex
	info *types.Info
	out  *result
}

func produceSites(proj *project, impl *implIndex, out *result) {
	for _, rp := range proj.roots {
		if !rp.typeChecked {
			continue
		}
		p := &siteProducer{proj: proj, impl: impl, info: rp.pkg.TypesInfo, out: out}
		for _, file := range rp.files {
			p.walk(file, flowTags{})
		}
	}
}

// walk visits n and everything below it. Statements that change the context are handled here: the
// context is lexical, so a call inside a function literal that sits in a loop is in that loop.
func (p *siteProducer) walk(n ast.Node, tags flowTags) {
	if n == nil {
		return
	}
	ast.Inspect(n, func(n ast.Node) bool {
		switch n := n.(type) {
		case *ast.ForStmt:
			// The init statement runs once. The condition, the post statement and the body run per iteration.
			inLoop := tags
			inLoop.inLoop = true
			p.walkStmt(n.Init, tags)
			p.walkExpr(n.Cond, inLoop)
			p.walkStmt(n.Post, inLoop)
			p.walk(n.Body, inLoop)
			return false

		case *ast.RangeStmt:
			inLoop := tags
			inLoop.inLoop = true
			p.walkExpr(n.X, tags) // evaluated once
			p.walkExpr(n.Key, inLoop)
			p.walkExpr(n.Value, inLoop)
			p.walk(n.Body, inLoop)
			return false

		case *ast.IfStmt:
			p.walkStmt(n.Init, tags)
			p.walkExpr(n.Cond, tags)
			thenTags, elseTags := tags, tags
			switch errorCheck(p.info, n.Cond) {
			case errorNotNil:
				thenTags.inErrorBranch = true
			case errorIsNil:
				elseTags.inErrorBranch = true
			}
			p.walk(n.Body, thenTags)
			if n.Else != nil {
				p.walk(n.Else, elseTags)
			}
			return false

		case *ast.GoStmt:
			inGo := tags
			inGo.inGoroutine = true
			p.walkDeferredCall(n.Call, tags, inGo)
			return false

		case *ast.DeferStmt:
			inDefer := tags
			inDefer.inDefer = true
			p.walkDeferredCall(n.Call, tags, inDefer)
			return false

		case *ast.CallExpr:
			p.site(n, tags)
		}
		return true
	})
}

func (p *siteProducer) walkStmt(n ast.Stmt, tags flowTags) {
	if n != nil {
		p.walk(n, tags)
	}
}

func (p *siteProducer) walkExpr(n ast.Expr, tags flowTags) {
	if n != nil {
		p.walk(n, tags)
	}
}

// walkDeferredCall handles `go f(x)` and `defer f(x)`. The call itself, and the body of a function
// literal that is called, run in the new context. The arguments, and the parts of the callee
// expression that name the function, are evaluated now.
func (p *siteProducer) walkDeferredCall(call *ast.CallExpr, now, later flowTags) {
	p.site(call, later)
	if lit, ok := ast.Unparen(call.Fun).(*ast.FuncLit); ok {
		p.walk(lit, later)
	} else {
		p.walk(call.Fun, now)
	}
	for _, arg := range call.Args {
		p.walk(arg, now)
	}
}

// site appends the record of one call, if the call has a declared callee.
func (p *siteProducer) site(call *ast.CallExpr, tags flowTags) {
	c, ok := resolveCallee(p.info, call)
	if !ok || c.fn.Pkg() == nil {
		return
	}
	at, ok := p.proj.position(call.Lparen)
	if !ok {
		return
	}

	rec := siteRecord{Type: "site", File: at.File, Line: at.Line, Col: at.Col, Name: c.name, Tags: tags.list(), Targets: []target{}}
	externals := map[externalLabel]bool{}

	switch c.kind {
	case kindInterface:
		if p.proj.rootByPath[c.fn.Pkg().Path()] == nil {
			externals[p.externalLabelOf(c.fn)] = true
		}
		implementers, complete := p.impl.implementersOf(c.iface, c.fn)
		if !complete {
			p.out.leftOutGenericSites++ // a generic type may implement it too: do not claim a complete list
			return
		}
		for _, fn := range implementers {
			p.addTarget(&rec, externals, fn, c.kind)
		}
		// The interface method itself is also a target: the call names it, so `callers` on the
		// interface method (for example port.Downstream.Fetch) lists this site.
		if p.proj.rootByPath[c.fn.Pkg().Path()] != nil {
			p.addTarget(&rec, externals, c.fn, kindInterfaceMethod)
		}
	default:
		p.addTarget(&rec, externals, c.fn, c.kind)
	}

	sort.Slice(rec.Targets, func(i, j int) bool { return lessTarget(rec.Targets[i], rec.Targets[j]) })
	for label := range externals {
		rec.External = append(rec.External, label)
	}
	sort.Slice(rec.External, func(i, j int) bool {
		a, b := rec.External[i], rec.External[j]
		if a.Package != b.Package {
			return a.Package < b.Package
		}
		return a.Name < b.Name
	})
	p.out.sites = append(p.out.sites, rec)
}

// addTarget adds fn to the site: as a target when it is declared in an analysed file of the
// repository, as an external label when it is declared outside the repository. A function in a
// repository package that did not type-check is neither: that package is listed as unanalysed.
func (p *siteProducer) addTarget(rec *siteRecord, externals map[externalLabel]bool, fn *types.Func, kind string) {
	if fn.Pkg() == nil {
		return
	}
	rp := p.proj.rootByPath[fn.Pkg().Path()]
	if rp == nil {
		externals[p.externalLabelOf(fn)] = true
		return
	}
	if !rp.typeChecked {
		return
	}
	at, ok := p.proj.position(fn.Pos())
	if !ok {
		return
	}
	for _, t := range rec.Targets {
		if t.File == at.File && t.Line == at.Line && t.Col == at.Col {
			return
		}
	}
	t := target{File: at.File, Line: at.Line, Col: at.Col, Name: fn.Name(), Kind: kind}
	if isMockFile(at.File) {
		t.Tags = []string{"mock"}
	}
	rec.Targets = append(rec.Targets, t)
}

func (p *siteProducer) externalLabelOf(fn *types.Func) externalLabel {
	path := fn.Pkg().Path()
	return externalLabel{Name: fn.Name(), Package: path, Module: p.proj.moduleLabel(path)}
}

// isMockFile is true for a file in a directory named mocks.
func isMockFile(rel string) bool {
	return strings.HasPrefix(rel, "mocks/") || strings.Contains(rel, "/mocks/")
}

func lessTarget(a, b target) bool {
	if a.File != b.File {
		return a.File < b.File
	}
	if a.Line != b.Line {
		return a.Line < b.Line
	}
	return a.Col < b.Col
}

// errorCheckResult tells what an if-condition says about an error value.
type errorCheckResult int

const (
	notAnErrorCheck errorCheckResult = iota
	errorNotNil                      // err != nil: the then-branch handles an error
	errorIsNil                       // err == nil: the else-branch handles an error
)

var errorInterface = types.Universe.Lookup("error").Type().Underlying().(*types.Interface)

// errorCheck recognises `err != nil` and `err == nil`, also as one side of &&.
func errorCheck(info *types.Info, cond ast.Expr) errorCheckResult {
	bin, ok := ast.Unparen(cond).(*ast.BinaryExpr)
	if !ok {
		return notAnErrorCheck
	}
	switch bin.Op {
	case token.LAND:
		if errorCheck(info, bin.X) == errorNotNil || errorCheck(info, bin.Y) == errorNotNil {
			return errorNotNil
		}
	case token.NEQ, token.EQL:
		other := bin.X
		if isNilIdent(info, bin.X) {
			other = bin.Y
		} else if !isNilIdent(info, bin.Y) {
			return notAnErrorCheck
		}
		if t := info.TypeOf(other); t != nil && types.Implements(t, errorInterface) {
			if bin.Op == token.NEQ {
				return errorNotNil
			}
			return errorIsNil
		}
	}
	return notAnErrorCheck
}

func isNilIdent(info *types.Info, e ast.Expr) bool {
	id, ok := ast.Unparen(e).(*ast.Ident)
	if !ok {
		return false
	}
	_, isNil := info.Uses[id].(*types.Nil)
	return isNil
}
