// aft-go-helper resolves Go call edges using the standard toolchain's
// type checker + SSA + VTA (Variable Type Analysis). It takes a project
// root on the command line, loads all packages under that root, and
// emits a JSON document with resolved call edges.
//
// AFT's tree-sitter parser handles syntax but not types. This helper
// fills the gap for Go-specific things that need type info:
//   - Interface dispatch: `d.Do(x)` where `d Doer` resolves to every
//     concrete implementation of `Doer.Do`.
//   - Concrete-method disambiguation: `s.Method()` where `s *Foo`
//     resolves to `(*Foo).Method`, not just any method named `Method`.
//   - Cross-package calls: resolved via go/packages, not heuristic import
//     maps.
//
// The helper is invoked opportunistically by AFT at configure time. It
// writes JSON to stdout. Any error writes to stderr and exits non-zero;
// AFT treats this as "no VTA data available" and falls back to
// tree-sitter behavior.
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"go/token"
	"go/types"
	"os"
	"path/filepath"

	"golang.org/x/tools/go/callgraph"
	"golang.org/x/tools/go/packages"
	"golang.org/x/tools/go/ssa"
	"golang.org/x/tools/go/ssa/ssautil"
	"golang.org/x/tools/go/types/typeutil"
)

// Position describes a file location in the caller.
type Position struct {
	File   string `json:"file"`             // path relative to root
	Line   int    `json:"line"`             // 1-based
	Symbol string `json:"symbol,omitempty"` // containing func/method
}

// Target describes a resolved callee.
type Target struct {
	File     string `json:"file"`               // path relative to root
	Symbol   string `json:"symbol"`             // function or method name (no receiver)
	Receiver string `json:"receiver,omitempty"` // e.g. "*pkg.concreteSvc"
	Pkg      string `json:"pkg,omitempty"`      // full package path
}

// Edge is one resolved call edge.
type Edge struct {
	Caller Position `json:"caller"`
	Callee Target   `json:"callee"`
	// Kind is "static" for package-level functions, "concrete" for
	// methods bound to a concrete type, "interface" for VTA-resolved
	// dynamic dispatch sites (one edge per concrete target).
	Kind string `json:"kind"`
}

// Output is the top-level JSON document.
type Output struct {
	Version int    `json:"version"`
	Root    string `json:"root"`
	Edges   []Edge `json:"edges"`
	// Skipped packages (e.g. those with build errors). AFT treats
	// these as "no VTA data for this package" and falls back to
	// tree-sitter.
	Skipped []string `json:"skipped,omitempty"`
}

func main() {
	var (
		rootFlag = flag.String("root", ".", "project root (absolute path preferred)")
	)
	flag.Parse()

	root, err := filepath.Abs(*rootFlag)
	if err != nil {
		fmt.Fprintf(os.Stderr, "aft-go-helper: resolve root: %v\n", err)
		os.Exit(1)
	}

	out, err := analyze(root)
	if err != nil {
		fmt.Fprintf(os.Stderr, "aft-go-helper: %v\n", err)
		os.Exit(1)
	}

	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(out); err != nil {
		fmt.Fprintf(os.Stderr, "aft-go-helper: encode: %v\n", err)
		os.Exit(1)
	}
}

// analyze loads all packages under root and returns resolved call edges.
func analyze(root string) (*Output, error) {
	cfg := &packages.Config{
		Mode: packages.NeedName |
			packages.NeedFiles |
			packages.NeedCompiledGoFiles |
			packages.NeedImports |
			packages.NeedTypes |
			packages.NeedTypesInfo |
			packages.NeedSyntax |
			packages.NeedDeps |
			packages.NeedModule,
		Dir:   root,
		Tests: false,
	}

	pkgs, err := packages.Load(cfg, "./...")
	if err != nil {
		return nil, fmt.Errorf("packages.Load: %w", err)
	}

	// Collect packages with errors so we can report them as "skipped"
	// without failing the whole run. VTA still works over whichever
	// packages loaded cleanly.
	var skipped []string
	for _, p := range pkgs {
		if len(p.Errors) > 0 {
			skipped = append(skipped, p.PkgPath)
		}
	}

	prog, ssaPkgs := ssautil.AllPackages(pkgs, ssa.BuilderMode(0))
	prog.Build()

	// Collect the set of functions we want CHA to consider. We can't
	// use golang.org/x/tools/go/callgraph/cha directly: its underlying
	// ssautil.AllFunctions only enumerates methods of *exported* named
	// types, so unexported types (lowercase `type doer struct{}` with
	// a method) silently disappear from interface dispatch, and any
	// `d.Do(x)` call goes unresolved. Build our own set that includes
	// every method of every named type, regardless of export, then
	// walk the resulting function set with class-hierarchy-analysis
	// logic inlined below.
	fns := collectAllFunctions(prog)

	// CHA logic inlined from golang.org/x/tools/go/callgraph/cha so we
	// can feed it our own function set. For a call site, the callees
	// are:
	//   - interface invoke `d.M(...)` → every method `M` on a named
	//     type whose receiver implements the interface.
	//   - static call `f(...)` → the one concrete callee.
	//   - dynamic non-interface call (function value) → every function
	//     with a matching signature.
	calleesOf := buildCalleeIndex(fns)

	cg := callgraph.New(nil)
	for f := range fns {
		fnode := cg.CreateNode(f)
		for _, b := range f.Blocks {
			for _, instr := range b.Instrs {
				site, ok := instr.(ssa.CallInstruction)
				if !ok {
					continue
				}
				for _, g := range calleesOf(site) {
					callgraph.AddEdge(fnode, site, cg.CreateNode(g))
				}
			}
		}
	}
	cg.DeleteSyntheticNodes()

	out := &Output{
		Version: 1,
		Root:    root,
		Edges:   nil,
		Skipped: skipped,
	}

	_ = ssaPkgs // retained for debugging; not used directly

	// Iterate every node's outgoing edges. GraphVisitEdges walks only
	// from the root, which drops calls inside functions that aren't
	// reachable from main (common in library packages and tests).
	for _, node := range cg.Nodes {
		if node == nil {
			continue
		}
		for _, e := range node.Out {
			if err := emitEdge(out, prog.Fset, root, e); err != nil {
				return nil, err
			}
		}
	}

	return out, nil
}

// emitEdge adds one edge to out.Edges if the edge carries information
// the consumer's generic (tree-sitter) resolver cannot derive on its own.
//
// Filter contract (see helper-contract.md):
//   - static calls (bare package-level function) are dropped; tree-sitter
//     handles these with same-file and same-directory same-package
//     fallbacks.
//   - concrete same-package method calls are dropped for the same reason.
//   - concrete cross-package and interface dispatch are always kept —
//     tree-sitter cannot infer receiver types across package boundaries
//     or resolve dynamic dispatch without a full type checker.
//
// Stdlib / out-of-project callers and callees are always skipped.
func emitEdge(out *Output, fset *token.FileSet, root string, e *callgraph.Edge) error {
	if e == nil || e.Site == nil || e.Caller == nil || e.Callee == nil {
		return nil
	}
	callerFn := e.Caller.Func
	calleeFn := e.Callee.Func
	if callerFn == nil || calleeFn == nil {
		return nil
	}
	// Skip calls emanating from synthetic functions (e.g. package
	// init wrappers) — they don't map to source.
	if callerFn.Synthetic != "" {
		return nil
	}

	sitePos := e.Site.Pos()
	if sitePos == token.NoPos {
		return nil
	}
	p := fset.Position(sitePos)
	if p.Filename == "" {
		return nil
	}
	callerRel, ok := relPath(root, p.Filename)
	if !ok {
		// Caller lives outside the project (e.g. stdlib / deps).
		return nil
	}

	// Caller symbol: the top-level enclosing function. Closures
	// collapse to their outer function so the edge references a
	// symbol tree-sitter can find.
	callerSym := containingName(callerFn)

	var calleeFile string
	if calleeFn.Pos() != token.NoPos {
		if cp := fset.Position(calleeFn.Pos()); cp.Filename != "" {
			if rel, ok2 := relPath(root, cp.Filename); ok2 {
				calleeFile = rel
			} else {
				// Callee is outside the project (stdlib, vendored,
				// etc.). Skip — AFT only cares about in-project edges.
				return nil
			}
		}
	}
	if calleeFile == "" {
		return nil
	}

	calleeSym := calleeName(calleeFn)
	if calleeSym == "" {
		// Callee is a closure — no source-level name to report.
		return nil
	}
	calleeRecv := funcReceiverText(calleeFn)
	calleePkg := ""
	if calleeFn.Pkg != nil && calleeFn.Pkg.Pkg != nil {
		calleePkg = calleeFn.Pkg.Pkg.Path()
	}
	callerPkg := ""
	if callerFn.Pkg != nil && callerFn.Pkg.Pkg != nil {
		callerPkg = callerFn.Pkg.Pkg.Path()
	}

	kind := edgeKind(calleeFn, e.Site)

	// Filter per contract: drop edges the generic resolver already handles.
	if !shouldEmit(kind, callerPkg, calleePkg) {
		return nil
	}

	out.Edges = append(out.Edges, Edge{
		Caller: Position{
			File:   callerRel,
			Line:   p.Line,
			Symbol: callerSym,
		},
		Callee: Target{
			File:     calleeFile,
			Symbol:   calleeSym,
			Receiver: calleeRecv,
			Pkg:      calleePkg,
		},
		Kind: kind,
	})
	return nil
}

// shouldEmit is the filter contract: emit an edge only when the generic
// (tree-sitter) layer cannot resolve it on its own.
//
//   - "interface": always emit. Dynamic dispatch requires type flow; no
//     syntactic layer can figure this out.
//   - "concrete":  emit only for cross-package calls. Same-package method
//     calls are resolved by the Rust consumer's sibling-directory fallback.
//   - "static":    never emit. Bare package-level calls are resolved
//     entirely by tree-sitter (same-file + same-package heuristics).
func shouldEmit(kind, callerPkg, calleePkg string) bool {
	switch kind {
	case "interface":
		return true
	case "concrete":
		// Missing package metadata on either side: be generous, emit.
		if callerPkg == "" || calleePkg == "" {
			return true
		}
		return callerPkg != calleePkg
	case "static":
		return false
	default:
		// Unknown kinds are new enough that we can't confidently filter;
		// emit so the consumer has them.
		return true
	}
}

// collectAllFunctions returns the union of ssautil.AllFunctions and
// every method of every named type in every package — exported or not.
// This is what CHA should see but doesn't: ssautil.AllFunctions only
// walks methods of exported named types, so interface dispatch to
// unexported implementers is otherwise invisible.
func collectAllFunctions(prog *ssa.Program) map[*ssa.Function]bool {
	fns := ssautil.AllFunctions(prog)
	for _, pkg := range prog.AllPackages() {
		if pkg == nil {
			continue
		}
		for _, mem := range pkg.Members {
			t, ok := mem.(*ssa.Type)
			if !ok {
				continue
			}
			named, ok := t.Type().(*types.Named)
			if !ok {
				continue
			}
			if named.TypeParams() != nil {
				// Generic types can't be enumerated without instantiations.
				continue
			}
			for _, T := range []types.Type{named, types.NewPointer(named)} {
				mset := prog.MethodSets.MethodSet(T)
				for i := 0; i < mset.Len(); i++ {
					fn := prog.MethodValue(mset.At(i))
					if fn != nil {
						fns[fn] = true
					}
				}
			}
		}
	}
	return fns
}

// buildCalleeIndex returns a resolver that maps a call site to its
// possible concrete callees. This mirrors the logic in
// golang.org/x/tools/go/callgraph/internal/chautil (which is not
// importable) but operates on the expanded function set produced by
// collectAllFunctions.
func buildCalleeIndex(fns map[*ssa.Function]bool) func(ssa.CallInstruction) []*ssa.Function {
	// funcsBySig: functions keyed by Signature, used for dynamic
	// non-interface calls through a function variable.
	var funcsBySig typeutil.Map
	// methodsByID: method set by types.Func.Id(), used for interface
	// invoke resolution. Keying by ID rather than name disambiguates
	// unexported methods of the same name in different packages.
	methodsByID := make(map[string][]*ssa.Function)

	for f := range fns {
		if f.Signature.Recv() == nil {
			if f.Name() == "init" && f.Synthetic == "package initializer" {
				continue
			}
			existing, _ := funcsBySig.At(f.Signature).([]*ssa.Function)
			funcsBySig.Set(f.Signature, append(existing, f))
			continue
		}
		obj := f.Object()
		if obj == nil {
			continue
		}
		tf, ok := obj.(*types.Func)
		if !ok {
			continue
		}
		id := tf.Id()
		methodsByID[id] = append(methodsByID[id], f)
	}

	type imethod struct {
		I  *types.Interface
		id string
	}
	memo := make(map[imethod][]*ssa.Function)
	lookupMethods := func(I *types.Interface, m *types.Func) []*ssa.Function {
		id := m.Id()
		key := imethod{I, id}
		if v, ok := memo[key]; ok {
			return v
		}
		var result []*ssa.Function
		for _, f := range methodsByID[id] {
			recvT := f.Signature.Recv().Type()
			if types.Implements(recvT, I) {
				result = append(result, f)
			}
		}
		memo[key] = result
		return result
	}

	return func(site ssa.CallInstruction) []*ssa.Function {
		call := site.Common()
		if call.IsInvoke() {
			iface, ok := call.Value.Type().Underlying().(*types.Interface)
			if !ok {
				return nil
			}
			return lookupMethods(iface, call.Method)
		}
		if g := call.StaticCallee(); g != nil {
			return []*ssa.Function{g}
		}
		if _, isBuiltin := call.Value.(*ssa.Builtin); isBuiltin {
			return nil
		}
		matches, _ := funcsBySig.At(call.Signature()).([]*ssa.Function)
		return matches
	}
}

// containingName walks up the SSA parent chain to find the top-level
// named function that encloses f. A caller inside a closure maps to
// its containing function so the edge is useful to tree-sitter.
func containingName(f *ssa.Function) string {
	if f == nil {
		return ""
	}
	cur := f
	for cur.Parent() != nil {
		cur = cur.Parent()
	}
	return cur.Name()
}

// calleeName returns the callee's short name if it's a named function
// or method. Returns "" for closures — a closure has no source-level
// identifier that tree-sitter could resolve.
func calleeName(f *ssa.Function) string {
	if f == nil || f.Parent() != nil {
		return ""
	}
	return f.Name()
}

// funcReceiverText returns a textual form of a method receiver's type
// (e.g. "*pkg.T") or "" for non-methods.
func funcReceiverText(f *ssa.Function) string {
	if f == nil || f.Signature == nil {
		return ""
	}
	recv := f.Signature.Recv()
	if recv == nil {
		return ""
	}
	return recv.Type().String()
}

// edgeKind classifies a call site as static (package-level function),
// concrete (method on a concrete type), or interface (dynamic dispatch).
func edgeKind(callee *ssa.Function, site ssa.CallInstruction) string {
	if site == nil {
		return "static"
	}
	common := site.Common()
	if common != nil && common.IsInvoke() {
		return "interface"
	}
	if callee != nil && callee.Signature != nil && callee.Signature.Recv() != nil {
		return "concrete"
	}
	return "static"
}

// relPath returns filename relative to root, or (_, false) if filename
// is outside root.
func relPath(root, filename string) (string, bool) {
	abs, err := filepath.Abs(filename)
	if err != nil {
		return "", false
	}
	rel, err := filepath.Rel(root, abs)
	if err != nil {
		return "", false
	}
	if len(rel) > 0 && rel[0] == '.' && (len(rel) == 1 || rel[1] == '.') {
		// rel starts with ".." — outside root
		return "", false
	}
	return rel, true
}
