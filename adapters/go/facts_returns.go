package main

import (
	"go/ast"
	"go/token"
	"go/types"
	"strconv"
	"strings"
	"unicode/utf8"
)

// Producer: `returns` facts. One fact per return statement of a declared function.
//
//	subject = the declared name of the function;
//	object  = the `return` keyword; object_name = the returned values as written;
//	label   = the path condition: the conditions of the enclosing if, loop and case clauses, and the
//	          negated condition of every earlier `if` whose body ends the block (a guard).
//
// Example, for `if input == "" { return ... }` followed by `if err := fetch(); err != nil { return err }`
// and a last `return nil`: the last return has the label `input != "" && err == nil`.
//
// The old helper built this from the SSA dominator tree. Here it comes from the syntax tree, which
// gives the same answer for structured code and needs no SSA. At most 4 conditions are listed,
// the innermost ones; the rest is counted ("...and 2 more").
//
// A function whose whole body is one return statement has nothing to ask and gets no fact.
// Returns inside function literals belong to the literal and are not listed.

const maxPathTerms = 4

type returnsProducer struct {
	proj *project
	info *types.Info
	fd   *ast.FuncDecl
	at   position
	out  *result
}

func produceReturns(proj *project, out *result) {
	for _, rp := range proj.roots {
		if !rp.typeChecked {
			continue
		}
		for _, file := range rp.files {
			for _, decl := range file.Decls {
				fd, ok := decl.(*ast.FuncDecl)
				if !ok || fd.Body == nil || isSingleReturn(fd.Body) {
					continue
				}
				at, ok := proj.position(fd.Name.Pos())
				if !ok {
					continue
				}
				r := &returnsProducer{proj: proj, info: rp.pkg.TypesInfo, fd: fd, at: at, out: out}
				r.block(fd.Body.List, nil)
			}
		}
	}
}

func isSingleReturn(body *ast.BlockStmt) bool {
	if len(body.List) != 1 {
		return false
	}
	_, ok := body.List[0].(*ast.ReturnStmt)
	return ok
}

// block walks a statement list. conds are the conditions that hold on entry; an early-exit `if`
// adds its negated condition for the statements after it.
func (r *returnsProducer) block(stmts []ast.Stmt, conds []string) {
	for _, stmt := range stmts {
		conds = r.statement(stmt, conds)
	}
}

// statement handles one statement and returns the conditions for the statements that follow it.
func (r *returnsProducer) statement(stmt ast.Stmt, conds []string) []string {
	switch s := stmt.(type) {
	case *ast.ReturnStmt:
		r.emit(s, conds)

	case *ast.BlockStmt:
		r.block(s.List, conds)

	case *ast.LabeledStmt:
		return r.statement(s.Stmt, conds)

	case *ast.IfStmt:
		cond := types.ExprString(s.Cond)
		r.block(s.Body.List, with(conds, cond))
		if s.Else != nil {
			r.statement(s.Else, with(conds, negate(s.Cond)))
		} else if endsBlock(s.Body) {
			return with(conds, negate(s.Cond))
		}

	case *ast.ForStmt:
		if s.Cond != nil {
			r.block(s.Body.List, with(conds, types.ExprString(s.Cond)))
		} else {
			r.block(s.Body.List, conds)
		}

	case *ast.RangeStmt:
		r.block(s.Body.List, with(conds, "range "+types.ExprString(s.X)))

	case *ast.SwitchStmt:
		tag := ""
		if s.Tag != nil {
			tag = types.ExprString(s.Tag)
		}
		for _, clause := range s.Body.List {
			cc := clause.(*ast.CaseClause)
			r.block(cc.Body, with(conds, caseCondition(tag, cc.List, "==")))
		}

	case *ast.TypeSwitchStmt:
		subject := typeSwitchSubject(s.Assign)
		for _, clause := range s.Body.List {
			cc := clause.(*ast.CaseClause)
			r.block(cc.Body, with(conds, caseCondition(subject, cc.List, "is")))
		}

	case *ast.SelectStmt:
		for _, clause := range s.Body.List {
			r.block(clause.(*ast.CommClause).Body, with(conds, "select case"))
		}
	}
	return conds
}

func with(conds []string, extra string) []string {
	out := make([]string, 0, len(conds)+1)
	out = append(out, conds...)
	return append(out, extra)
}

// caseCondition renders a case clause: `tag == a || tag == b`, or `default`.
func caseCondition(tag string, exprs []ast.Expr, op string) string {
	if len(exprs) == 0 {
		return "default"
	}
	var parts []string
	for _, e := range exprs {
		text := types.ExprString(e)
		if tag != "" {
			text = tag + " " + op + " " + text
		}
		parts = append(parts, text)
	}
	return strings.Join(parts, " || ")
}

func typeSwitchSubject(assign ast.Stmt) string {
	switch a := assign.(type) {
	case *ast.ExprStmt:
		return types.ExprString(a.X)
	case *ast.AssignStmt:
		if len(a.Rhs) == 1 {
			return types.ExprString(a.Rhs[0])
		}
	}
	return ""
}

// endsBlock reports whether a block always leaves the enclosing block: its last statement is a
// return, a panic, or a jump.
func endsBlock(body *ast.BlockStmt) bool {
	if len(body.List) == 0 {
		return false
	}
	switch last := body.List[len(body.List)-1].(type) {
	case *ast.ReturnStmt:
		return true
	case *ast.BranchStmt:
		return last.Tok == token.BREAK || last.Tok == token.CONTINUE || last.Tok == token.GOTO
	case *ast.ExprStmt:
		call, ok := last.X.(*ast.CallExpr)
		if !ok {
			return false
		}
		id, ok := call.Fun.(*ast.Ident)
		return ok && id.Name == "panic"
	}
	return false
}

// negate renders the negation of a condition: `a != b` becomes `a == b`, `!x` becomes `x`.
func negate(cond ast.Expr) string {
	cond = ast.Unparen(cond)
	switch c := cond.(type) {
	case *ast.UnaryExpr:
		if c.Op == token.NOT {
			return types.ExprString(c.X)
		}
	case *ast.BinaryExpr:
		flipped := map[token.Token]string{
			token.EQL: "!=", token.NEQ: "==", token.LSS: ">=", token.LEQ: ">", token.GTR: "<=", token.GEQ: "<",
		}
		if op, ok := flipped[c.Op]; ok {
			return types.ExprString(c.X) + " " + op + " " + types.ExprString(c.Y)
		}
	}
	return "!(" + types.ExprString(cond) + ")"
}

func (r *returnsProducer) emit(ret *ast.ReturnStmt, conds []string) {
	object, ok := r.proj.position(ret.Return)
	if !ok {
		return
	}
	values := make([]string, 0, len(ret.Results))
	for _, v := range ret.Results {
		values = append(values, types.ExprString(v))
	}
	r.out.facts = append(r.out.facts, factRecord{
		Type: "fact", Kind: "returns",
		Subject: r.at, SubjectName: r.proj.funcDisplayName(r.fd),
		Object: object, ObjectName: limit(strings.Join(values, ", "), 100),
		Label: pathLabel(conds),
	})
}

// pathLabel joins the conditions, outermost first. Only the innermost maxPathTerms are listed.
func pathLabel(conds []string) string {
	seen := map[string]bool{}
	var terms []string
	for _, c := range conds {
		if seen[c] {
			continue
		}
		seen[c] = true
		terms = append(terms, limit(c, 100))
	}
	if len(terms) == 0 {
		return "true"
	}
	hidden := 0
	if len(terms) > maxPathTerms {
		hidden = len(terms) - maxPathTerms
		terms = terms[hidden:]
	}
	label := strings.Join(terms, " && ")
	if hidden > 0 {
		label += " && ...and " + strconv.Itoa(hidden) + " more"
	}
	return label
}

func limit(s string, n int) string {
	s = strings.Join(strings.Fields(s), " ")
	if len(s) <= n {
		return s
	}
	for n > 0 && !utf8.RuneStart(s[n]) {
		n-- // do not cut a character in half
	}
	return s[:n] + "..."
}
