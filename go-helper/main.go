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
//   - Dispatch edges: `Register("task", handler)` where handler is a
//     function value — the callee receives the function for later call.
//   - Goroutine launches: `go fn(args)` — in-project goroutine callees.
//   - Defer calls: `defer fn(args)` — in-project defer callees.
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
	"go/constant"
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
	// "dispatches" for function-value arguments passed to a call.
	// "goroutine" for `go fn(args)` launches.
	// "defer" for `defer fn(args)` calls.
	Kind string `json:"kind"`
	// NearbyString is set on "dispatches" edges when the same call site
	// has exactly one string literal argument of ≤128 chars. Used as a
	// human-readable label (e.g. the task name in a job scheduler).
	NearbyString string `json:"nearby_string,omitempty"`
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

// edgeKey is used for deduplication of edges.
type edgeKey struct {
	callerFile   string
	callerLine   int
	callerSymbol string
	calleeFile   string
	calleeSymbol string
	kind         string
	nearbyString string
}

func main() {
	var (
		rootFlag        = flag.String("root", ".", "project root (absolute path preferred)")
		noDispatchesFlag = flag.Bool("no-dispatches", false, "disable emission of dispatches/goroutine/defer edge kinds")
	)
	flag.Parse()

	root, err := filepath.Abs(*rootFlag)
	if err != nil {
		fmt.Fprintf(os.Stderr, "aft-go-helper: resolve root: %v\n", err)
		os.Exit(1)
	}

	out, err := analyze(root, !*noDispatchesFlag)
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
func analyze(root string, emitDispatches bool) (*Output, error) {
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

	// Deduplication set for all emitted edges.
	seen := make(map[edgeKey]bool)

	_ = ssaPkgs // retained for debugging; not used directly

	// Iterate every node's outgoing edges. GraphVisitEdges walks only
	// from the root, which drops calls inside functions that aren't
	// reachable from main (common in library packages and tests).
	for _, node := range cg.Nodes {
		if node == nil {
			continue
		}
		for _, e := range node.Out {
			if err := emitEdge(out, prog.Fset, root, e, seen); err != nil {
				return nil, err
			}
		}
	}

	// Tier 1.1/1.2/1.3: emit dispatch/goroutine/defer edges by walking
	// all SSA instructions directly — these are not captured in the
	// callgraph edges above.
	if emitDispatches {
		for f := range fns {
			if f.Synthetic != "" {
				continue
			}
			emitDispatchGoroutineDefer(out, prog.Fset, root, f, fns, seen)
		}
	}

	return out, nil
}

// emitDispatchGoroutineDefer walks all instructions in f and emits:
//
//   - "dispatches" edges: a call site passes a named in-project function
//     as an argument. Exactly one string literal arg ≤128 chars → nearby_string.
//   - "goroutine" edges: `go callee(args)` where callee is a named in-project func.
//   - "defer" edges: `defer callee(args)` where callee is a named in-project func.
func emitDispatchGoroutineDefer(
	out *Output,
	fset *token.FileSet,
	root string,
	f *ssa.Function,
	fns map[*ssa.Function]bool,
	seen map[edgeKey]bool,
) {
	callerSym := containingName(f)
	if callerSym == "" {
		return
	}

	for _, b := range f.Blocks {
		for _, instr := range b.Instrs {
			switch v := instr.(type) {
			case *ssa.Go:
				// `go fn(args)` — goroutine launch.
				emitGoOrDeferEdge(out, fset, root, f, callerSym, v.Common(), "goroutine", fns, seen, nil)

			case *ssa.Defer:
				// `defer fn(args)` — deferred call.
				emitGoOrDeferEdge(out, fset, root, f, callerSym, v.Common(), "defer", fns, seen, nil)

			case ssa.CallInstruction:
				// Regular call — scan args for function values being passed.
				emitDispatchesFromCall(out, fset, root, f, callerSym, v, fns, seen)
			}
		}
	}
}

// emitGoOrDeferEdge handles one `*ssa.Go` or `*ssa.Defer` instruction.
// Only in-project, named callees are emitted. Closures are skipped (no
// source-level name). If nearbyOverride is non-nil its value is used as
// nearby_string (reserved for future use by callers that already computed it).
func emitGoOrDeferEdge(
	out *Output,
	fset *token.FileSet,
	root string,
	callerFn *ssa.Function,
	callerSym string,
	common *ssa.CallCommon,
	kind string,
	fns map[*ssa.Function]bool,
	seen map[edgeKey]bool,
	nearbyOverride *string,
) {
	if common == nil {
		return
	}

	var targets []*ssa.Function
	if g := common.StaticCallee(); g != nil {
		targets = []*ssa.Function{g}
	} else if !common.IsInvoke() {
		// Dynamic non-interface call: function value. Collect all
		// matching-signature functions from our known set.
		//
		// Note: for goroutine/defer with a plain function value we
		// still emit edges to in-project callees — the same logic
		// that handles dispatches applies here.
		if _, isBuiltin := common.Value.(*ssa.Builtin); !isBuiltin {
			// We don't have direct access to funcsBySig here; use
			// a simpler heuristic: if the Value is an *ssa.Function
			// itself (i.e. `go myFunc`), use it directly.
			if fn, ok := common.Value.(*ssa.Function); ok {
				targets = []*ssa.Function{fn}
			}
		}
	}

	for _, callee := range targets {
		if callee == nil || callee.Parent() != nil {
			// Skip closures.
			continue
		}
		if callee.Synthetic != "" {
			continue
		}
		// Must be in our function set (i.e. in-project).
		if !fns[callee] {
			continue
		}

		calleeSym := calleeName(callee)
		if calleeSym == "" {
			continue
		}

		calleeFile := funcFile(fset, root, callee)
		if calleeFile == "" {
			continue
		}

		// Caller position.
		var callerFile string
		var callerLine int
		if pos := common.Pos(); pos != token.NoPos {
			p := fset.Position(pos)
			rel, ok := relPath(root, p.Filename)
			if !ok {
				continue
			}
			callerFile = rel
			callerLine = p.Line
		} else {
			// No position info; try caller function's position.
			cp := fset.Position(callerFn.Pos())
			if cp.Filename == "" {
				continue
			}
			rel, ok := relPath(root, cp.Filename)
			if !ok {
				continue
			}
			callerFile = rel
			callerLine = cp.Line
		}

		nearby := ""
		if nearbyOverride != nil {
			nearby = *nearbyOverride
		}

		key := edgeKey{
			callerFile:   callerFile,
			callerLine:   callerLine,
			callerSymbol: callerSym,
			calleeFile:   calleeFile,
			calleeSymbol: calleeSym,
			kind:         kind,
			nearbyString: nearby,
		}
		if seen[key] {
			continue
		}
		seen[key] = true

		recv := funcReceiverText(callee)
		pkg := ""
		if callee.Pkg != nil && callee.Pkg.Pkg != nil {
			pkg = callee.Pkg.Pkg.Path()
		}

		out.Edges = append(out.Edges, Edge{
			Caller: Position{
				File:   callerFile,
				Line:   callerLine,
				Symbol: callerSym,
			},
			Callee: Target{
				File:     calleeFile,
				Symbol:   calleeSym,
				Receiver: recv,
				Pkg:      pkg,
			},
			Kind:         kind,
			NearbyString: nearby,
		})
	}
}

// emitDispatchesFromCall scans the arguments of a call instruction for
// function values that refer to named in-project functions and emits
// "dispatches" edges for each. Applies filter rules:
//   - Self-reference (caller == callee) is skipped.
//   - Anonymous closures (no source-level name) are skipped.
//   - Out-of-project callees are skipped.
//
// nearby_string: if the call has exactly one string literal arg ≤128
// chars among all its arguments, it is attached to every dispatches edge
// emitted from that call site.
func emitDispatchesFromCall(
	out *Output,
	fset *token.FileSet,
	root string,
	callerFn *ssa.Function,
	callerSym string,
	site ssa.CallInstruction,
	fns map[*ssa.Function]bool,
	seen map[edgeKey]bool,
) {
	common := site.Common()
	if common == nil {
		return
	}

	// All arguments (excludes the callee value itself).
	args := common.Args

	// Scan for function-value arguments.
	type dispatchArg struct {
		fn  *ssa.Function
		idx int
	}
	var dispatched []dispatchArg

	for i, arg := range args {
		fn, ok := extractFuncValue(arg)
		if !ok || fn == nil {
			continue
		}
		// Skip closures (no source-level identifier).
		if fn.Parent() != nil {
			continue
		}
		// Skip synthetic functions.
		if fn.Synthetic != "" {
			continue
		}
		// Must be in our in-project function set.
		if !fns[fn] {
			continue
		}
		dispatched = append(dispatched, dispatchArg{fn: fn, idx: i})
	}

	if len(dispatched) == 0 {
		return
	}

	// Compute nearby_string: exactly one string literal ≤128 chars.
	nearby := extractNearbyString(args)

	// Caller position.
	sitePos := site.Pos()
	if sitePos == token.NoPos {
		return
	}
	p := fset.Position(sitePos)
	if p.Filename == "" {
		return
	}
	callerFile, ok := relPath(root, p.Filename)
	if !ok {
		return
	}
	callerLine := p.Line

	for _, d := range dispatched {
		callee := d.fn

		// Self-reference filter: skip if caller and callee resolve to
		// the same top-level symbol in the same file.
		if containingName(callee) == callerSym {
			calleeF := funcFile(fset, root, callee)
			if calleeF == callerFile {
				continue
			}
		}

		calleeSym := calleeName(callee)
		if calleeSym == "" {
			continue
		}
		calleeFile := funcFile(fset, root, callee)
		if calleeFile == "" {
			continue
		}

		key := edgeKey{
			callerFile:   callerFile,
			callerLine:   callerLine,
			callerSymbol: callerSym,
			calleeFile:   calleeFile,
			calleeSymbol: calleeSym,
			kind:         "dispatches",
			nearbyString: nearby,
		}
		if seen[key] {
			continue
		}
		seen[key] = true

		recv := funcReceiverText(callee)
		pkg := ""
		if callee.Pkg != nil && callee.Pkg.Pkg != nil {
			pkg = callee.Pkg.Pkg.Path()
		}

		out.Edges = append(out.Edges, Edge{
			Caller: Position{
				File:   callerFile,
				Line:   callerLine,
				Symbol: callerSym,
			},
			Callee: Target{
				File:     calleeFile,
				Symbol:   calleeSym,
				Receiver: recv,
				Pkg:      pkg,
			},
			Kind:         "dispatches",
			NearbyString: nearby,
		})
	}
}

// extractFuncValue returns the *ssa.Function referred to by val, if val
// is an SSA value that directly references a named function. Returns
// (nil, false) for non-function values.
func extractFuncValue(val ssa.Value) (*ssa.Function, bool) {
	switch v := val.(type) {
	case *ssa.Function:
		return v, true
	case *ssa.MakeClosure:
		// Closure wrapping a named function — use the underlying func.
		if fn, ok := v.Fn.(*ssa.Function); ok {
			return fn, true
		}
		return nil, false
	default:
		return nil, false
	}
}

// extractNearbyString returns the single string literal ≤128 chars from
// args, or "" if there are 0 or ≥2 such literals.
func extractNearbyString(args []ssa.Value) string {
	var found string
	count := 0
	for _, arg := range args {
		c, ok := arg.(*ssa.Const)
		if !ok {
			continue
		}
		if c.Value == nil || c.Value.Kind() != constant.String {
			continue
		}
		s := constant.StringVal(c.Value)
		if len(s) > 128 {
			continue
		}
		count++
		found = s
	}
	if count == 1 {
		return found
	}
	return ""
}

// funcFile returns the project-relative file path where fn is defined,
// or "" if fn is outside the project root or has no position info.
func funcFile(fset *token.FileSet, root string, fn *ssa.Function) string {
	if fn == nil || fn.Pos() == token.NoPos {
		return ""
	}
	cp := fset.Position(fn.Pos())
	if cp.Filename == "" {
		return ""
	}
	rel, ok := relPath(root, cp.Filename)
	if !ok {
		return ""
	}
	return rel
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
func emitEdge(out *Output, fset *token.FileSet, root string, e *callgraph.Edge, seen map[edgeKey]bool) error {
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

	key := edgeKey{
		callerFile:   callerRel,
		callerLine:   p.Line,
		callerSymbol: callerSym,
		calleeFile:   calleeFile,
		calleeSymbol: calleeSym,
		kind:         kind,
		nearbyString: "",
	}
	if seen[key] {
		return nil
	}
	seen[key] = true

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
