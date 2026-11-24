package main

import (
	"go/ast"
	"go/token"
	"go/types"
	"path/filepath"
)

// Producer: `writes` facts. One fact per (declared function, written variable or field).
//
//	subject = the declared name of the function (a closure counts for the function that holds it);
//	object  = the declared name of the package variable or struct field;
//	object_name = "Var" for a package variable, "Type.Field" for a struct field.
//
// What counts as a write: `x = v`, `x op= v`, `x++`, an assignment into a map or array element of x,
// and a struct literal that sets a field (`T{F: v}`).
//
// Tag `init`: the write fills a value that the function made itself: a field in a struct literal, or a
// field of a local variable that holds a struct value or a pointer from `&T{}` or `new(T)`.
// Most struct writes are of this kind (92 % in the spike), so registry.example.json hides them.
// When a function does both kinds of write to the same field, the fact has no `init` tag.
//
// Fields and variables outside the repository are listed only for writes that are not `init`; the
// object path is then absolute.

type writeKey struct {
	funcPos position
	object  position
	name    string
}

type writesProducer struct {
	proj *project
	info *types.Info
	// fresh holds the local pointer variables that point at a value this function created.
	fresh   map[*types.Var]bool
	pending map[writeKey]*factRecord
	order   []writeKey
	owner   *ast.FuncDecl
	ownerAt position
}

func produceWrites(proj *project, out *result) {
	for _, rp := range proj.roots {
		if !rp.typeChecked {
			continue
		}
		for _, file := range rp.files {
			for _, decl := range file.Decls {
				fd, ok := decl.(*ast.FuncDecl)
				if !ok || fd.Body == nil {
					continue
				}
				at, ok := proj.position(fd.Name.Pos())
				if !ok {
					continue
				}
				w := &writesProducer{proj: proj, info: rp.pkg.TypesInfo, pending: map[writeKey]*factRecord{}, owner: fd, ownerAt: at}
				w.fresh = w.freshPointers(fd.Body)
				ast.Inspect(fd.Body, w.visit)
				for _, key := range w.order {
					out.facts = append(out.facts, *w.pending[key])
				}
			}
		}
	}
}

func (w *writesProducer) visit(n ast.Node) bool {
	switch n := n.(type) {
	case *ast.AssignStmt:
		if n.Tok == token.DEFINE {
			return true // declares new local variables; nothing outside the function is written
		}
		for _, lhs := range n.Lhs {
			w.assigned(lhs)
		}
	case *ast.IncDecStmt:
		w.assigned(n.X)
	case *ast.CompositeLit:
		w.literal(n)
	}
	return true
}

// assigned records the write to one assignment target.
func (w *writesProducer) assigned(lhs ast.Expr) {
	switch e := ast.Unparen(lhs).(type) {
	case *ast.Ident:
		if v, ok := w.info.Uses[e].(*types.Var); ok && isPackageVar(v) {
			w.add(v, v.Name(), false)
		}
	case *ast.SelectorExpr:
		selection := w.info.Selections[e]
		if selection == nil {
			// pkg.Var
			if v, ok := w.info.Uses[e.Sel].(*types.Var); ok && isPackageVar(v) {
				w.add(v, v.Name(), false)
			}
			return
		}
		field, ok := selection.Obj().(*types.Var)
		if selection.Kind() != types.FieldVal || !ok {
			return
		}
		w.add(field, fieldDisplayName(selection), w.isLocalValue(e.X))
	case *ast.IndexExpr:
		// m[k] = v and a[i] = v change the container that holds the element.
		w.assigned(e.X)
	}
}

// literal records the fields that a struct literal sets.
func (w *writesProducer) literal(lit *ast.CompositeLit) {
	t := w.info.TypeOf(lit)
	if t == nil {
		return
	}
	structType, ok := t.Underlying().(*types.Struct)
	if !ok {
		return
	}
	owner := ownerName(t)
	for i, elt := range lit.Elts {
		var field *types.Var
		if kv, ok := elt.(*ast.KeyValueExpr); ok {
			key, ok := kv.Key.(*ast.Ident)
			if !ok {
				continue
			}
			field, _ = w.info.Uses[key].(*types.Var)
		} else if i < structType.NumFields() {
			field = structType.Field(i)
		}
		if field == nil || !field.IsField() {
			continue
		}
		name := field.Name()
		if owner != "" {
			name = owner + "." + name
		}
		w.add(field.Origin(), name, true)
	}
}

// add records one write. isInit marks a write that fills a value the function made.
func (w *writesProducer) add(obj *types.Var, name string, isInit bool) {
	at, inRepo := w.proj.position(obj.Pos())
	if !inRepo {
		if isInit || !obj.Pos().IsValid() {
			return // an outside field set in a literal: noise
		}
		raw := w.proj.fset.PositionFor(obj.Pos(), false)
		at = position{File: filepath.ToSlash(raw.Filename), Line: raw.Line, Col: raw.Column}
	} else if rp := w.proj.rootByPath[objPkgPath(obj)]; rp != nil && !rp.typeChecked {
		return
	}
	key := writeKey{funcPos: w.ownerAt, object: at, name: name}
	if fact, seen := w.pending[key]; seen {
		if !isInit {
			fact.Tags = nil // one non-init write makes the whole fact a real write
		}
		return
	}
	fact := &factRecord{
		Type: "fact", Kind: "writes",
		Subject: w.ownerAt, SubjectName: w.proj.funcDisplayName(w.owner),
		Object: at, ObjectName: name,
	}
	if isInit {
		fact.Tags = []string{"init"}
	}
	w.pending[key] = fact
	w.order = append(w.order, key)
}

func objPkgPath(v *types.Var) string {
	if v.Pkg() == nil {
		return ""
	}
	return v.Pkg().Path()
}

func isPackageVar(v *types.Var) bool {
	return !v.IsField() && v.Pkg() != nil && v.Parent() == v.Pkg().Scope()
}

// fieldDisplayName renders the field of a selection as "Type.Field". For a field reached through
// embedded structs it names the struct that really holds the field.
func fieldDisplayName(selection *types.Selection) string {
	field := selection.Obj().(*types.Var)
	t := selection.Recv()
	index := selection.Index()
	for _, i := range index[:len(index)-1] {
		st, ok := deref(t).Underlying().(*types.Struct)
		if !ok {
			return field.Name()
		}
		t = st.Field(i).Type()
	}
	if owner := ownerName(t); owner != "" {
		return owner + "." + field.Name()
	}
	return field.Name()
}

func deref(t types.Type) types.Type {
	if p, ok := types.Unalias(t).(*types.Pointer); ok {
		return p.Elem()
	}
	return t
}

// ownerName is the name of a named struct type (through a pointer), or "".
func ownerName(t types.Type) string {
	if named, ok := types.Unalias(deref(t)).(*types.Named); ok {
		return named.Obj().Name()
	}
	return ""
}

// isLocalValue reports whether writing into e changes a value that lives in this function only.
func (w *writesProducer) isLocalValue(e ast.Expr) bool {
	switch e := ast.Unparen(e).(type) {
	case *ast.Ident:
		v, ok := w.info.Uses[e].(*types.Var)
		if !ok || v.IsField() || isPackageVar(v) {
			return false
		}
		switch types.Unalias(v.Type()).Underlying().(type) {
		case *types.Struct, *types.Array:
			return true // a copy that belongs to this function
		case *types.Pointer:
			return w.fresh[v]
		}
		return false
	case *ast.SelectorExpr:
		selection := w.info.Selections[e]
		if selection == nil || selection.Kind() != types.FieldVal {
			return false
		}
		if _, viaPointer := types.Unalias(w.info.TypeOf(e.X)).(*types.Pointer); viaPointer {
			return w.isLocalValue(e.X) // only when e.X is a fresh local pointer
		}
		return w.isLocalValue(e.X)
	case *ast.IndexExpr:
		if _, isArray := types.Unalias(w.info.TypeOf(e.X)).Underlying().(*types.Array); isArray {
			return w.isLocalValue(e.X)
		}
	}
	return false
}

// freshPointers finds the local variables set from `&T{...}` or `new(T)`: they point at a value the
// function made.
func (w *writesProducer) freshPointers(body *ast.BlockStmt) map[*types.Var]bool {
	fresh := map[*types.Var]bool{}
	mark := func(name *ast.Ident, value ast.Expr) {
		if !makesNewValue(w.info, value) {
			return
		}
		if v, ok := w.info.Defs[name].(*types.Var); ok {
			fresh[v] = true
		} else if v, ok := w.info.Uses[name].(*types.Var); ok {
			fresh[v] = true
		}
	}
	ast.Inspect(body, func(n ast.Node) bool {
		switch n := n.(type) {
		case *ast.AssignStmt:
			if len(n.Lhs) == len(n.Rhs) {
				for i, lhs := range n.Lhs {
					if id, ok := lhs.(*ast.Ident); ok {
						mark(id, n.Rhs[i])
					}
				}
			}
		case *ast.ValueSpec:
			if len(n.Names) == len(n.Values) {
				for i, id := range n.Names {
					mark(id, n.Values[i])
				}
			}
		}
		return true
	})
	return fresh
}

func makesNewValue(info *types.Info, e ast.Expr) bool {
	switch e := ast.Unparen(e).(type) {
	case *ast.UnaryExpr:
		_, isLiteral := ast.Unparen(e.X).(*ast.CompositeLit)
		return e.Op == token.AND && isLiteral
	case *ast.CallExpr:
		id, ok := ast.Unparen(e.Fun).(*ast.Ident)
		if !ok {
			return false
		}
		b, ok := info.Uses[id].(*types.Builtin)
		return ok && b.Name() == "new"
	}
	return false
}
