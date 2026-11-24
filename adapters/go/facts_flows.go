package main

import (
	"go/ast"
	"go/token"
	"go/types"
)

// Producer: `flows` facts. One fact says that the value of one variable flows into another variable.
//
//	subject = the declared name of the source variable, subject_name = its name;
//	object  = the declared name of the variable that receives the value, object_name = its name.
//
// The position of a variable is always where it is declared, so the object of one fact is the subject of
// the next fact and `--depth` can follow the chain through functions.
//
// label tells how the value moves:
//
//	parameter    f(x), f(&x), f(x.Field), x.M(): into a parameter or the receiver of a function of the repository;
//	             for a call through an interface, into each non-mock method of the repository that can run
//	             (the same targets as the call's site record), with tag interface
//	assignment   y = x, y := x, y := T{F: x}, a, b := x.(T), a, b := m[k]
//	field_write  m.F = x: into a field; object_name is "m.F", the position is that of m
//	writer       json.Unmarshal(raw, &out): the Unmarshal family writes `raw` into `out`
//
// tag approximate: the source is a part of a variable (x.Field, x[i], x.(T)) or a literal wraps it.
// tag interface: the call goes through an interface; the value flows into this method only when it is
// the implementation that runs.
//
// Not covered: values that pass through function results or channels. The old trace_data stopped there too.

type flowKey struct {
	subject, object position
	objectName      string
	label           string
}

type flowsProducer struct {
	proj *project
	impl *implIndex
	info *types.Info
	seen map[flowKey]bool
	out  *result
}

func produceFlows(proj *project, impl *implIndex, out *result) {
	for _, rp := range proj.roots {
		if !rp.typeChecked {
			continue
		}
		p := &flowsProducer{proj: proj, impl: impl, info: rp.pkg.TypesInfo, seen: map[flowKey]bool{}, out: out}
		for _, file := range rp.files {
			for _, decl := range file.Decls {
				if fd, ok := decl.(*ast.FuncDecl); ok && fd.Body != nil {
					ast.Inspect(fd.Body, p.visit)
				}
			}
		}
	}
}

func (p *flowsProducer) visit(n ast.Node) bool {
	switch n := n.(type) {
	case *ast.CallExpr:
		p.call(n)
	case *ast.AssignStmt:
		if n.Tok == token.ASSIGN || n.Tok == token.DEFINE {
			p.assignment(n.Lhs, n.Rhs)
		}
	case *ast.ValueSpec:
		names := make([]ast.Expr, len(n.Names))
		for i, id := range n.Names {
			names[i] = id
		}
		p.assignment(names, n.Values)
	}
	return true
}

// source is a variable that a value comes from.
type source struct {
	v           *types.Var
	approximate bool
}

// rootVar finds the variable that an expression reads from: x, &x, *x, x.F, x[i], x.(T), T(x).
func (p *flowsProducer) rootVar(e ast.Expr) (source, bool) {
	switch e := ast.Unparen(e).(type) {
	case *ast.Ident:
		if v, ok := p.info.Uses[e].(*types.Var); ok && !v.IsField() {
			return source{v: v}, true
		}
	case *ast.UnaryExpr:
		if e.Op == token.AND {
			return p.rootVar(e.X)
		}
	case *ast.StarExpr:
		return p.approximate(e.X)
	case *ast.SelectorExpr:
		if selection := p.info.Selections[e]; selection != nil {
			if selection.Kind() == types.FieldVal {
				return p.approximate(e.X)
			}
			return source{}, false
		}
		if v, ok := p.info.Uses[e.Sel].(*types.Var); ok && !v.IsField() {
			return source{v: v}, true // pkg.Var
		}
	case *ast.IndexExpr:
		return p.approximate(e.X)
	case *ast.SliceExpr:
		return p.approximate(e.X)
	case *ast.TypeAssertExpr:
		return p.approximate(e.X)
	case *ast.CallExpr:
		if tv, ok := p.info.Types[e.Fun]; ok && tv.IsType() && len(e.Args) == 1 {
			return p.approximate(e.Args[0]) // a conversion T(x)
		}
	}
	return source{}, false
}

func (p *flowsProducer) approximate(e ast.Expr) (source, bool) {
	s, ok := p.rootVar(e)
	s.approximate = true
	return s, ok
}

// sourcesOf lists the variables whose values an expression is made of. A literal wraps the values
// of its elements.
func (p *flowsProducer) sourcesOf(e ast.Expr) []source {
	e = ast.Unparen(e)
	if unary, ok := e.(*ast.UnaryExpr); ok && unary.Op == token.AND {
		if lit, ok := ast.Unparen(unary.X).(*ast.CompositeLit); ok {
			return p.literalSources(lit)
		}
	}
	if lit, ok := e.(*ast.CompositeLit); ok {
		return p.literalSources(lit)
	}
	if s, ok := p.rootVar(e); ok {
		return []source{s}
	}
	return nil
}

func (p *flowsProducer) literalSources(lit *ast.CompositeLit) []source {
	var sources []source
	for _, elt := range lit.Elts {
		if kv, ok := elt.(*ast.KeyValueExpr); ok {
			elt = kv.Value
		}
		for _, s := range p.sourcesOf(elt) {
			s.approximate = true
			sources = append(sources, s)
		}
	}
	return sources
}

// assignment handles `lhs... = rhs...` and `lhs... := rhs...`.
func (p *flowsProducer) assignment(lhs, rhs []ast.Expr) {
	switch {
	case len(lhs) == len(rhs):
		for i := range lhs {
			for _, src := range p.sourcesOf(rhs[i]) {
				p.assigned(src, lhs[i])
			}
		}
	case len(rhs) == 1:
		// Destructuring: a, b := x.(T) or a, b := m[k]. A call result is not followed.
		if _, isCall := ast.Unparen(rhs[0]).(*ast.CallExpr); isCall {
			return
		}
		for _, src := range p.sourcesOf(rhs[0]) {
			src.approximate = true
			for _, target := range lhs {
				p.assigned(src, target)
			}
		}
	}
}

// assigned adds the flow of src into the assignment target.
func (p *flowsProducer) assigned(src source, target ast.Expr) {
	switch t := ast.Unparen(target).(type) {
	case *ast.Ident:
		if t.Name == "_" {
			return
		}
		v, ok := p.info.Defs[t].(*types.Var)
		if !ok {
			v, ok = p.info.Uses[t].(*types.Var)
		}
		if !ok || v.IsField() {
			return
		}
		p.add(src, v, v.Name(), "assignment")
	case *ast.SelectorExpr:
		selection := p.info.Selections[t]
		if selection == nil || selection.Kind() != types.FieldVal {
			return
		}
		if root, ok := p.rootVar(t.X); ok {
			p.add(src, root.v, types.ExprString(t), "field_write")
		}
	}
}

// call handles the flows into the parameters of a called function of the repository, and the
// Unmarshal family. A call through an interface flows into every non-mock method of the repository
// that can run for it.
func (p *flowsProducer) call(call *ast.CallExpr) {
	c, ok := resolveCallee(p.info, call)
	if !ok || c.fn.Pkg() == nil {
		return // no declared callee, or the universe error.Error (as for site records)
	}
	if c.kind == kindInterface {
		methods, _ := p.impl.implementersOf(c.iface, c.fn)
		for _, fn := range methods {
			if at, ok := p.proj.position(fn.Pos()); ok && !isMockFile(at.File) {
				p.into(call, fn, "interface")
			}
		}
		return
	}
	if c.kind == kindStatic && c.fn.Name() == "Unmarshal" && len(call.Args) >= 2 {
		from, okFrom := p.rootVar(call.Args[0])
		to, okTo := p.rootVar(call.Args[1])
		if okFrom && okTo {
			from.approximate = from.approximate || to.approximate
			p.add(from, to.v, to.v.Name(), "writer")
		}
	}
	p.into(call, c.fn, "")
}

// into records the flows of the call's receiver and arguments into the receiver and parameters of fn,
// a function of the repository. via is an extra tag ("interface") or "".
func (p *flowsProducer) into(call *ast.CallExpr, fn *types.Func, via string) {
	rp := p.proj.rootByPath[pkgPathOf(fn)]
	if rp == nil || !rp.typeChecked {
		return
	}
	sig := fn.Type().(*types.Signature)

	if sel, isSelector := ast.Unparen(call.Fun).(*ast.SelectorExpr); isSelector && sig.Recv() != nil {
		if selection := p.info.Selections[sel]; selection != nil && selection.Kind() == types.MethodVal {
			if src, ok := p.rootVar(sel.X); ok {
				p.addVia(src, sig.Recv(), sig.Recv().Name(), "parameter", via)
			}
		}
	}

	params := sig.Params()
	for i, arg := range call.Args {
		index := i
		if sig.Variadic() && i >= params.Len()-1 {
			index = params.Len() - 1
		}
		if index >= params.Len() {
			break
		}
		if src, ok := p.rootVar(arg); ok {
			p.addVia(src, params.At(index), params.At(index).Name(), "parameter", via)
		}
	}
}

func pkgPathOf(fn *types.Func) string {
	if fn.Pkg() == nil {
		return ""
	}
	return fn.Pkg().Path()
}

// add records that src flows into the variable dst. name is how dst is shown.
func (p *flowsProducer) add(src source, dst *types.Var, name, label string) {
	p.addVia(src, dst, name, label, "")
}

// addVia is add with an extra tag via ("interface") when it is not "".
func (p *flowsProducer) addVia(src source, dst *types.Var, name, label, via string) {
	if src.v == nil || dst == nil || src.v == dst || dst.Name() == "" || dst.Name() == "_" || src.v.Name() == "_" {
		return
	}
	subject, ok := p.proj.position(src.v.Pos())
	if !ok {
		return
	}
	object, ok := p.proj.position(dst.Pos())
	if !ok {
		return
	}
	key := flowKey{subject: subject, object: object, objectName: name, label: label}
	if p.seen[key] {
		return
	}
	p.seen[key] = true
	fact := factRecord{
		Type: "fact", Kind: "flows",
		Subject: subject, SubjectName: src.v.Name(),
		Object: object, ObjectName: name,
		Label: label,
	}
	if src.approximate {
		fact.Tags = append(fact.Tags, "approximate")
	}
	if via != "" {
		fact.Tags = append(fact.Tags, via)
	}
	p.out.facts = append(p.out.facts, fact)
}
