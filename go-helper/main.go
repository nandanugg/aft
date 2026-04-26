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
	"slices"
	"strings"

	"golang.org/x/tools/go/callgraph"
	"golang.org/x/tools/go/packages"
	"golang.org/x/tools/go/ssa"
	"golang.org/x/tools/go/ssa/ssautil"
	"golang.org/x/tools/go/types/typeutil"
)

const helperSchemaVersion = 1
const sidecarProviderID = "aft-go-sidecar"
const sidecarProviderVersion = "0.1.0"

// implKey is used for deduplication of implements edges.
type implKey struct {
	ifaceFile   string
	ifaceLine   int
	ifaceSymbol string
	concPkg     string
	concRecv    string
	concSymbol  string
}

// Position describes a file location in the caller.
type Position struct {
	File   string `json:"file"`             // path relative to root
	Line   int    `json:"line"`             // 1-based
	Symbol string `json:"symbol,omitempty"` // containing func/method
}

// Target describes a resolved callee.
type Target struct {
	File     string `json:"file"`               // path relative to root
	Line     int    `json:"line,omitempty"`     // 1-based line number (0 = unknown)
	Symbol   string `json:"symbol"`             // function or method name (no receiver)
	Receiver string `json:"receiver,omitempty"` // e.g. "*pkg.concreteSvc"
	Pkg      string `json:"pkg,omitempty"`      // full package path
}

// CallContext holds control-flow context about a call site: whether it's
// inside a defer, goroutine, loop body, or error-handling branch. Emitted
// on every edge except "implements" (which has no call-site context).
type CallContext struct {
	InDefer       bool `json:"in_defer,omitempty"`
	InGoroutine   bool `json:"in_goroutine,omitempty"`
	InLoop        bool `json:"in_loop,omitempty"`
	InErrorBranch bool `json:"in_error_branch,omitempty"`
	BranchDepth   int  `json:"branch_depth,omitempty"`
}

// ReturnSite describes one return statement in a function and the path
// condition that must hold for execution to reach it.
type ReturnSite struct {
	Line                int    `json:"line"`
	Value               string `json:"value"`
	PathCondition       string `json:"path_condition"`
	PathConditionSimple bool   `json:"path_condition_simple"`
}

// ReturnInfo holds the per-return-site analysis for one function.
type ReturnInfo struct {
	File    string       `json:"file"`
	Symbol  string       `json:"symbol"`
	Returns []ReturnSite `json:"returns"`
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
	// DispatchedVia is the FQN of the function whose call received the
	// function-value argument. Set only on "dispatches" edges where the
	// callee can be resolved. Format follows Go's ssa.Function.String():
	//   - Free function: "pkg/path.FuncName"
	//   - Pointer receiver: "pkg/path.(*TypeName).Method"
	//   - Interface invoke: "(pkg/path.InterfaceName).Method"
	DispatchedVia string `json:"dispatched_via,omitempty"`
	// Context holds control-flow context annotations for the call site.
	// Absent for "implements" edges. Omitted when all booleans are false
	// and branch_depth is 0.
	Context *CallContext `json:"context,omitempty"`
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
	// Returns holds per-function return-site analysis, one entry per
	// in-project function that has at least one interesting return.
	Returns []ReturnInfo `json:"returns,omitempty"`
}

// edgeKey is used for deduplication of edges.
type edgeKey struct {
	callerFile   string
	callerLine   int
	callerSymbol string
	calleeFile   string
	calleeRecv   string
	calleeSymbol string
	kind         string
	nearbyString string
}

func main() {
	var (
		rootFlag             = flag.String("root", ".", "project root (absolute path preferred)")
		noDispatchesFlag     = flag.Bool("no-dispatches", false, "disable emission of dispatches/goroutine/defer edge kinds")
		noImplementsFlag     = flag.Bool("no-implements", false, "disable emission of implements edges")
		noWritesFlag         = flag.Bool("no-writes", false, "disable emission of writes edges for package-level variables")
		noCallContextFlag    = flag.Bool("no-call-context", false, "disable caller-context annotations on edges")
		noReturnAnalysisFlag = flag.Bool("no-return-analysis", false, "disable per-return path-condition analysis")
		sidecarMode          = flag.Bool("sidecar", false, "run as a long-lived JSON protocol sidecar")
		sidecarInfoFile      = flag.String("sidecar-info-file", "", "write sidecar discovery metadata to JSON file once listening")
	)
	flag.Parse()

	root, err := filepath.Abs(*rootFlag)
	if err != nil {
		fmt.Fprintf(os.Stderr, "aft-go-helper: resolve root: %v\n", err)
		os.Exit(1)
	}

	if *sidecarMode {
		if err := runSidecar(root, *sidecarInfoFile); err != nil {
			fmt.Fprintf(os.Stderr, "aft-go-helper: %v\n", err)
			os.Exit(1)
		}
		return
	}

	out, err := analyze(root, !*noDispatchesFlag, !*noImplementsFlag, !*noWritesFlag, !*noCallContextFlag, !*noReturnAnalysisFlag)
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
func analyze(root string, emitDispatches bool, emitImplements bool, emitWrites bool, emitCallContext bool, emitReturnAnalysis bool) (*Output, error) {
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

	// Edges must be a non-nil slice so JSON marshals as `[]` instead of
	// `null`. The Rust side declares `edges: Vec<HelperEdge>`, which serde
	// will not coerce from a JSON null even with #[serde(default)].
	out := &Output{
		Version: helperSchemaVersion,
		Root:    root,
		Edges:   []Edge{},
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
			emitDispatchGoroutineDefer(out, prog.Fset, root, f, fns, calleesOf, seen)
		}
	}

	// Tier 1.4: emit implements edges — one per (interface method, concrete method)
	// pair for all in-project interfaces and their in-project implementations.
	if emitImplements {
		emitImplementsEdges(out, prog, root)
	}

	// Tier 1.5: emit writes edges for cross-package stores to *ssa.Global.
	// Same-package writes are filtered at source (tree-sitter already sees them).
	if emitWrites {
		emitWritesEdges(out, prog, fns, root, seen)
	}

	// Feature 1: annotate edges with caller-context booleans.
	if emitCallContext {
		annotateCallContext(out, prog, fns)
	}

	// Feature 2: per-return path-condition analysis.
	if emitReturnAnalysis {
		out.Returns = analyzeReturnPaths(prog, fns, root)
	}

	return out, nil
}

// emitImplementsEdges enumerates all in-project interface types and emits
// "implements" edges for every in-project concrete type that satisfies them.
//
// Filter rules:
//   - Skip empty interfaces (any/interface{}) — every type implements them.
//   - Skip when either side is outside the project root (stdlib, vendored).
//
// Note: same-file implementations are NOT filtered. Tree-sitter cannot resolve
// Go interface satisfaction (it's structural, no declaration site says "X satisfies Y"),
// so only the helper's CHA pass has this information — even for implementations
// co-located with their interface in the same file, which is idiomatic Go
// (e.g. `type FooStorer interface { ... }` + `type fooStore struct { ... }`
// in the same `foo_store.go`).
//
// Deduplication uses implKey keyed on (ifaceFile, ifaceLine, ifaceSymbol,
// concPkg, concRecv, concSymbol).
func emitImplementsEdges(out *Output, prog *ssa.Program, root string) {
	implSeen := make(map[implKey]bool)

	// Collect all in-project named types so we can check implementations.
	// We need both named types (for value receivers) and pointer-to-named
	// (for pointer receivers) — Go's implements check is receiver-type specific.
	type concreteMethod struct {
		fn      *ssa.Function
		recv    string // textual receiver type, e.g. "*pkg.T"
		pkg     string // full package path
		file    string // relative to root
		symName string // method name (bare)
		line    int    // 1-based line number of the method declaration
	}

	// methodsByName maps method name → list of concrete methods that could implement an interface method with that name.
	// Keyed by the method's Id() (package-qualified name) to avoid cross-package collisions.
	type methodID struct {
		id   string // types.Func.Id()
		name string // bare method name
	}
	methodsByID := make(map[string][]concreteMethod) // key = types.Func.Id()

	for _, pkg := range prog.AllPackages() {
		if pkg == nil || pkg.Pkg == nil {
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
			// Skip generic types (instantiations handled by SSA canonicalization).
			if named.TypeParams() != nil {
				continue
			}
			// Enumerate both T and *T — Go methods can have value or pointer receivers.
			for _, recvType := range []types.Type{named, types.NewPointer(named)} {
				mset := prog.MethodSets.MethodSet(recvType)
				for i := 0; i < mset.Len(); i++ {
					sel := mset.At(i)
					fn := prog.MethodValue(sel)
					if fn == nil || fn.Synthetic != "" {
						continue
					}
					if fn.Pos() == token.NoPos {
						continue
					}
					pos := prog.Fset.Position(fn.Pos())
					if pos.Filename == "" {
						continue
					}
					relFile, ok := relPath(root, pos.Filename)
					if !ok {
						// Outside project root.
						continue
					}
					obj := sel.Obj()
					tf, ok := obj.(*types.Func)
					if !ok {
						continue
					}
					recv := recvType.String()
					pkgPath := ""
					if pkg.Pkg != nil {
						pkgPath = pkg.Pkg.Path()
					}
					methodsByID[tf.Id()] = append(methodsByID[tf.Id()], concreteMethod{
						fn:      fn,
						recv:    recv,
						pkg:     pkgPath,
						file:    relFile,
						symName: fn.Name(),
						line:    pos.Line,
					})
				}
			}
		}
	}

	// Now enumerate all in-project interface types and find implementations.
	for _, pkg := range prog.AllPackages() {
		if pkg == nil || pkg.Pkg == nil {
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
			iface, ok := named.Underlying().(*types.Interface)
			if !ok {
				continue
			}
			// Skip empty interfaces — every type implements them.
			if iface.NumMethods() == 0 {
				continue
			}
			// Interface must be in-project.
			ifacePos := prog.Fset.Position(named.Obj().Pos())
			if ifacePos.Filename == "" {
				continue
			}
			ifaceFile, ok := relPath(root, ifacePos.Filename)
			if !ok {
				continue
			}
			ifaceName := named.Obj().Name()

			// For each method of the interface, find all concrete implementations.
			for mi := 0; mi < iface.NumMethods(); mi++ {
				ifaceMethod := iface.Method(mi)
				ifaceMethodPos := prog.Fset.Position(ifaceMethod.Pos())
				ifaceMethodLine := ifaceMethodPos.Line
				if ifaceMethodLine == 0 {
					// Use the interface type's position as fallback.
					ifaceMethodLine = ifacePos.Line
				}

				// Look up all concrete methods with this method's Id.
				concretes := methodsByID[ifaceMethod.Id()]
				for _, cm := range concretes {
					// Verify the concrete type's receiver actually implements the interface.
					// We do this by checking types.Implements on the receiver type.
					if cm.fn.Signature == nil || cm.fn.Signature.Recv() == nil {
						continue
					}
					recvT := cm.fn.Signature.Recv().Type()
					if !types.Implements(recvT, iface) {
						// Also check pointer receiver.
						ptrT := types.NewPointer(recvT)
						if !types.Implements(ptrT, iface) {
							continue
						}
					}

					// Filter note: tree-sitter cannot compute interface
					// satisfaction (it's structural in Go), so we do NOT
					// filter same-file pairs — the helper is the only way
					// to derive these edges even when the concrete type
					// and interface live in the same file.

					key := implKey{
						ifaceFile:   ifaceFile,
						ifaceLine:   ifaceMethodLine,
						ifaceSymbol: ifaceName,
						concPkg:     cm.pkg,
						concRecv:    cm.recv,
						concSymbol:  cm.symName,
					}
					if implSeen[key] {
						continue
					}
					implSeen[key] = true

					out.Edges = append(out.Edges, Edge{
						Caller: Position{
							File:   ifaceFile,
							Line:   ifaceMethodLine,
							Symbol: ifaceName,
						},
						Callee: Target{
							File:     cm.file,
							Line:     cm.line,
							Symbol:   cm.symName,
							Receiver: cm.recv,
							Pkg:      cm.pkg,
						},
						Kind: "implements",
					})
				}
			}
		}
	}
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
	calleesOf func(ssa.CallInstruction) []*ssa.Function,
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
				emitDispatchesFromCall(out, fset, root, f, callerSym, v, fns, calleesOf, seen)
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
		recv := funcReceiverText(callee)

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
			calleeRecv:   recv,
			calleeSymbol: calleeSym,
			kind:         kind,
			nearbyString: nearby,
		}
		if seen[key] {
			continue
		}
		seen[key] = true

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
	calleesOf func(ssa.CallInstruction) []*ssa.Function,
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
		// Closure handling: an anonymous closure (fn.Parent() != nil) has no
		// source-level identifier to point at directly. But the idiomatic Go
		// registration pattern
		//
		//     mux.HandleFunc("Type", func(ctx, task) error { return HandleX(ctx, task) })
		//
		// wraps a single named handler in a thin lambda. If the closure body
		// contains exactly one call to a named in-project function, treat
		// that function as the dispatched target (amendment to
		// ADR-0001-dispatch-edges.md §1.1). Zero or ≥2 in-project calls is
		// genuinely ambiguous — drop.
		if fn.Parent() != nil {
			resolved := resolveClosureTargets(fn, fns, callerFn.Prog, root, calleesOf)
			if len(resolved) == 0 {
				continue
			}
			for _, target := range resolved {
				dispatched = append(dispatched, dispatchArg{fn: target, idx: i})
			}
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

	// Compute nearby_string: exactly one string constant ≤128 chars
	// (bare literals, typed-string constants, string(...) casts).
	nearby := extractNearbyString(args)

	// Compute dispatched_via: FQN of the function receiving the handler.
	// Same for all dispatches from this call site.
	dispatchedVia, _ := calleeFQN(common)

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
		recv := funcReceiverText(callee)
		calleeFile := funcFile(fset, root, callee)
		if calleeFile == "" {
			continue
		}

		key := edgeKey{
			callerFile:   callerFile,
			callerLine:   callerLine,
			callerSymbol: callerSym,
			calleeFile:   calleeFile,
			calleeRecv:   recv,
			calleeSymbol: calleeSym,
			kind:         "dispatches",
			nearbyString: nearby,
		}
		if seen[key] {
			continue
		}
		seen[key] = true

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
			Kind:          "dispatches",
			NearbyString:  nearby,
			DispatchedVia: dispatchedVia,
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

// resolveClosureTargets returns the named in-project functions that a closure
// body dispatches to, if unambiguous. This supports the idiomatic
// registration pattern:
//
//	mux.HandleFunc("Type", func(ctx, task) error { return HandleXTask(ctx, task) })
//
// where the second argument is an anonymous lambda whose only purpose is to
// forward to a real handler. Counting rules:
//   - Only calls to named in-project functions are counted. `fns` alone is
//     insufficient because it also includes functions from module-cache
//     packages that happen to be loaded; we additionally verify that the
//     callee's source file lives under `root`.
//   - Exactly one top-level call site with one or more named in-project
//     callees → return the distinct callee set for that call site.
//   - Zero or ≥2 distinct top-level call sites with in-project callees →
//     return nil (drop the dispatch edge; ambiguous).
//   - Calls in nested closures inside the lambda are NOT recursed; only the
//     top-level body is inspected. That keeps the heuristic conservative.
func resolveClosureTargets(
	closure *ssa.Function,
	fns map[*ssa.Function]bool,
	prog *ssa.Program,
	root string,
	calleesOf func(ssa.CallInstruction) []*ssa.Function,
) []*ssa.Function {
	if closure == nil || closure.Parent() == nil {
		return nil
	}
	if calleesOf == nil {
		return nil
	}
	found := make(map[*ssa.Function]bool)
	resolvedSites := 0
	for _, b := range closure.Blocks {
		for _, instr := range b.Instrs {
			call, ok := instr.(ssa.CallInstruction)
			if !ok {
				continue
			}
			common := call.Common()
			if common == nil {
				continue
			}
			var siteTargets []*ssa.Function
			for _, fn := range calleesOf(call) {
				if fn == nil || fn.Parent() != nil || fn.Synthetic != "" {
					continue
				}
				if fn.Pos() == token.NoPos {
					continue
				}
				pos := prog.Fset.Position(fn.Pos())
				if pos.Filename == "" {
					continue
				}
				if _, ok := relPath(root, pos.Filename); !ok {
					continue
				}
				if !fns[fn] {
					continue
				}
				siteTargets = append(siteTargets, fn)
			}
			if len(siteTargets) == 0 {
				continue
			}
			resolvedSites++
			if resolvedSites > 1 {
				return nil
			}
			for _, fn := range siteTargets {
				found[fn] = true
			}
		}
	}
	if resolvedSites != 1 || len(found) == 0 {
		return nil
	}
	out := make([]*ssa.Function, 0, len(found))
	for fn := range found {
		out = append(out, fn)
	}
	slices.SortFunc(out, func(a, b *ssa.Function) int {
		return strings.Compare(a.String(), b.String())
	})
	return out
}

// resolveStringConst resolves val to a string constant, unwrapping type
// conversions and named-type aliases. Returns ("", false) if the value is
// not a string constant or if depth exceeds 3.
//
// Resolution rules (first match wins):
//  1. *ssa.Const with Kind == constant.String → return StringVal.
//  2. *ssa.Convert or *ssa.ChangeType → recurse into X.
//  3. Named-type string alias (*ssa.Const whose underlying type is string).
func resolveStringConst(val ssa.Value, depth int) (string, bool) {
	if depth > 3 {
		return "", false
	}
	switch v := val.(type) {
	case *ssa.Const:
		if v.Value == nil || v.Value.Kind() != constant.String {
			return "", false
		}
		// Accept both plain string and named-type string aliases.
		u := v.Type().Underlying()
		basic, ok := u.(*types.Basic)
		if !ok || basic.Kind() != types.String {
			return "", false
		}
		return constant.StringVal(v.Value), true
	case *ssa.Convert:
		return resolveStringConst(v.X, depth+1)
	case *ssa.ChangeType:
		return resolveStringConst(v.X, depth+1)
	}
	return "", false
}

// extractNearbyString returns the single string constant ≤128 chars from
// args, or "" if there are 0 or ≥2 such constants. Accepts typed-string
// constants and string(...) type-casts in addition to bare string literals.
func extractNearbyString(args []ssa.Value) string {
	var found string
	count := 0
	for _, arg := range args {
		s, ok := resolveStringConst(arg, 0)
		if !ok {
			continue
		}
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

// calleeFQN returns the fully-qualified name of the function being called at
// common, following Go's ssa.Function.String() rendering convention. Returns
// ("", false) when the callee cannot be resolved (e.g. dynamic function value
// from a map or reflection).
func calleeFQN(common *ssa.CallCommon) (string, bool) {
	if common == nil {
		return "", false
	}
	// Direct function call: value is *ssa.Function.
	if fn, ok := common.Value.(*ssa.Function); ok {
		return fn.String(), true
	}
	// Interface method call (invoke mode): IsInvoke() && Method != nil.
	if common.IsInvoke() && common.Method != nil {
		recv := common.Value.Type().String()
		return fmt.Sprintf("(%s).%s", recv, common.Method.Name()), true
	}
	// Method call via bound method closure (MakeClosure).
	if mc, ok := common.Value.(*ssa.MakeClosure); ok {
		if fn, ok := mc.Fn.(*ssa.Function); ok {
			return fn.String(), true
		}
	}
	return "", false
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
		calleeRecv:   calleeRecv,
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

// emitWritesEdges walks all SSA Store instructions and emits "writes" edges
// for cross-package stores to *ssa.Global.
//
// Filter contract (helper-contract.md):
//   - Same-package writes are dropped: tree-sitter sees them.
//   - Out-of-project globals are dropped.
//   - Only direct *ssa.Store → *ssa.Global is considered (no pointer indirection).
//
// Initialization writes from synthetic init functions are emitted with
// caller.symbol = "init" — SSA synthesizes a package-init function that
// runs var initializers.
func emitWritesEdges(
	out *Output,
	prog *ssa.Program,
	fns map[*ssa.Function]bool,
	root string,
	seen map[edgeKey]bool,
) {
	for f := range fns {
		if f.Pkg == nil {
			continue
		}

		for _, block := range f.Blocks {
			for _, instr := range block.Instrs {
				store, ok := instr.(*ssa.Store)
				if !ok {
					continue
				}
				glob, ok := store.Addr.(*ssa.Global)
				if !ok {
					continue
				}

				// Must be an in-project global.
				globPos := prog.Fset.Position(glob.Pos())
				if globPos.Filename == "" {
					continue
				}
				globRel, ok := relPath(root, globPos.Filename)
				if !ok {
					continue
				}

				// Filter: drop same-package writes — tree-sitter handles them.
				globPkg := ""
				if glob.Pkg != nil && glob.Pkg.Pkg != nil {
					globPkg = glob.Pkg.Pkg.Path()
				}
				callerPkg := ""
				if f.Pkg != nil && f.Pkg.Pkg != nil {
					callerPkg = f.Pkg.Pkg.Path()
				}
				if globPkg != "" && callerPkg != "" && globPkg == callerPkg {
					continue
				}

				// Caller position: prefer the store instruction's position,
				// fall back to the enclosing function's position.
				var callerFile string
				var callerLine int
				storePos := prog.Fset.Position(store.Pos())
				if storePos.Filename != "" {
					rel, ok2 := relPath(root, storePos.Filename)
					if !ok2 {
						continue
					}
					callerFile = rel
					callerLine = storePos.Line
				} else {
					fnPos := prog.Fset.Position(f.Pos())
					if fnPos.Filename == "" {
						continue
					}
					rel, ok2 := relPath(root, fnPos.Filename)
					if !ok2 {
						continue
					}
					callerFile = rel
					callerLine = fnPos.Line
				}

				// Caller symbol: use the SSA function's name directly.
				// For synthetic package-init functions the name is "init".
				callerSym := f.Name()
				if callerSym == "" {
					continue
				}
				// Collapse closures to their outer named function.
				callerSym = containingName(f)
				if callerSym == "" {
					continue
				}

				globName := glob.Name()
				if globName == "" {
					continue
				}

				key := edgeKey{
					callerFile:   callerFile,
					callerLine:   callerLine,
					callerSymbol: callerSym,
					calleeFile:   globRel,
					calleeSymbol: globName,
					kind:         "writes",
					nearbyString: "",
				}
				if seen[key] {
					continue
				}
				seen[key] = true

				pkg := globPkg

				out.Edges = append(out.Edges, Edge{
					Caller: Position{
						File:   callerFile,
						Line:   callerLine,
						Symbol: callerSym,
					},
					Callee: Target{
						File:   globRel,
						Symbol: globName,
						Pkg:    pkg,
					},
					Kind: "writes",
				})
			}
		}
	}
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

// =============================================================================
// Feature 1 — Caller-context annotations
// =============================================================================

// blockCtxData holds pre-computed control-flow classification for a basic block.
type blockCtxData struct {
	inLoop        bool
	inErrorBranch bool
	branchDepth   int
}

// annotateCallContext walks all edges in out and, for each, computes the
// CallContext for the call site using SSA dominator analysis. Edges with
// kind=="implements" are skipped (no call-site context).
//
// The implementation uses a per-function context map keyed by basic-block
// index, computed once per function and cached in a local map for the
// duration of this function.
func annotateCallContext(out *Output, prog *ssa.Program, fns map[*ssa.Function]bool) {
	// Build a lookup: (callerFile, callerLine) → *ssa.Function
	// so we can find the SSA function for each edge.
	type fileLineKey struct {
		file string
		line int
	}
	funcByFileLine := make(map[fileLineKey]*ssa.Function)
	for f := range fns {
		if f.Synthetic != "" || f.Pos() == token.NoPos {
			continue
		}
		pos := prog.Fset.Position(f.Pos())
		if pos.Filename == "" {
			continue
		}
		funcByFileLine[fileLineKey{pos.Filename, pos.Line}] = f
	}

	// Cache: function → per-block context data (built lazily).
	fnBlockCtxCache := make(map[*ssa.Function]map[int]blockCtxData)

	getBlockCtx := func(fn *ssa.Function) map[int]blockCtxData {
		if cached, ok := fnBlockCtxCache[fn]; ok {
			return cached
		}
		result := computeBlockContexts(fn)
		fnBlockCtxCache[fn] = result
		return result
	}

	// For each edge, compute context.
	for i := range out.Edges {
		edge := &out.Edges[i]
		if edge.Kind == "implements" {
			continue
		}

		// Annotate in_defer and in_goroutine from the edge kind directly.
		ctx := &CallContext{}
		if edge.Kind == "defer" {
			ctx.InDefer = true
		}
		if edge.Kind == "goroutine" {
			ctx.InGoroutine = true
		}

		// Find the SSA function containing the call site.
		// We need to find the function by walking the SSA fns and matching
		// the caller file+line to a function's block instructions.
		fn := findFunctionContainingCall(prog, fns, edge.Caller.File, edge.Caller.Line)
		if fn != nil {
			blockCtxs := getBlockCtx(fn)
			// Find the basic block that contains this line.
			blk := findBlockContainingLine(fn, prog.Fset, edge.Caller.Line)
			if blk != nil {
				if bc, ok := blockCtxs[blk.Index]; ok {
					ctx.InLoop = bc.inLoop
					if !ctx.InErrorBranch {
						ctx.InErrorBranch = bc.inErrorBranch
					}
					ctx.BranchDepth = bc.branchDepth
				}
			}
		}

		// Only emit context if it carries information (any bool true or depth > 0).
		if ctx.InDefer || ctx.InGoroutine || ctx.InLoop || ctx.InErrorBranch || ctx.BranchDepth > 0 {
			edge.Context = ctx
		}
	}
}

// findFunctionContainingCall returns the *ssa.Function that contains the
// given (file, line) call site, searching by checking if the call site
// line falls within any block's instruction lines.
func findFunctionContainingCall(prog *ssa.Program, fns map[*ssa.Function]bool, callerFile string, callerLine int) *ssa.Function {
	for f := range fns {
		if f.Synthetic != "" {
			continue
		}
		// Quick filter: check if this function even has the right file.
		if f.Pos() == token.NoPos {
			continue
		}
		fnPos := prog.Fset.Position(f.Pos())
		if fnPos.Filename == "" {
			continue
		}
		// Check if the absolute path suffix matches callerFile.
		// callerFile is relative to root; fnPos.Filename is absolute.
		// We check via suffix match (callerFile is a relative path).
		if !hasSuffix(fnPos.Filename, callerFile) {
			continue
		}
		// Check if any instruction in this function is at callerLine.
		for _, blk := range f.Blocks {
			for _, instr := range blk.Instrs {
				pos := prog.Fset.Position(instr.Pos())
				if pos.Line == callerLine && pos.Filename != "" && hasSuffix(pos.Filename, callerFile) {
					return f
				}
			}
		}
	}
	return nil
}

// hasSuffix checks if abs ends with the relative path rel (path-separator aware).
func hasSuffix(abs, rel string) bool {
	if rel == "" {
		return false
	}
	if abs == rel {
		return true
	}
	sep := string(filepath.Separator)
	return len(abs) > len(rel) && abs[len(abs)-len(rel)-1] == sep[0] && abs[len(abs)-len(rel):] == rel
}

// findBlockContainingLine returns the basic block that contains an
// instruction at the given line number, or nil if none found.
func findBlockContainingLine(fn *ssa.Function, fset *token.FileSet, line int) *ssa.BasicBlock {
	for _, blk := range fn.Blocks {
		for _, instr := range blk.Instrs {
			pos := fset.Position(instr.Pos())
			if pos.Line == line {
				return blk
			}
		}
	}
	return nil
}

// computeBlockContexts performs dominator-tree analysis for fn and returns
// a map from block index → blockCtxData containing loop/error/depth info.
func computeBlockContexts(fn *ssa.Function) map[int]blockCtxData {
	result := make(map[int]blockCtxData)
	if len(fn.Blocks) == 0 {
		return result
	}

	// Step 1: find loop headers via back-edges in the CFG.
	// A back-edge is a→b where b dominates a (b is a loop header).
	loopHeaders := make(map[*ssa.BasicBlock]bool)
	for _, blk := range fn.Blocks {
		for _, succ := range blk.Succs {
			// succ dominates blk iff succ is an ancestor in the dominator tree.
			if dominates(succ, blk) {
				loopHeaders[succ] = true
			}
		}
	}

	// Step 2: a block is "in a loop" if it is dominated by a loop header.
	loopBlocks := make(map[*ssa.BasicBlock]bool)
	for h := range loopHeaders {
		for _, blk := range fn.Blocks {
			if dominates(h, blk) {
				loopBlocks[blk] = true
			}
		}
	}

	// Step 3: for each block, walk the dominator chain toward entry and
	// classify each dominating If terminator.
	for _, blk := range fn.Blocks {
		depth := 0
		inError := false

		// Walk dominator chain (excluding blk itself, upward toward entry).
		cur := blk.Idom()
		for cur != nil {
			ifInstr := blockIfTerminator(cur)
			if ifInstr != nil {
				depth++
				if !inError {
					// Determine which side of this If the current block is on.
					onTrue := dominates(cur.Succs[0], blk)
					if onTrue && isErrorCondition(ifInstr.Cond) {
						inError = true
					}
				}
			}
			cur = cur.Idom()
		}

		result[blk.Index] = blockCtxData{
			inLoop:        loopBlocks[blk],
			inErrorBranch: inError,
			branchDepth:   depth,
		}
	}

	return result
}

// dominates returns true iff block a dominates block b in the dominator tree.
// Uses BasicBlock.Idom() to walk up the tree from b.
func dominates(a, b *ssa.BasicBlock) bool {
	if a == nil || b == nil {
		return false
	}
	if a == b {
		return true
	}
	// Walk dominator chain of b upward.
	cur := b.Idom()
	for cur != nil {
		if cur == a {
			return true
		}
		cur = cur.Idom()
	}
	return false
}

// blockIfTerminator returns the *ssa.If terminator of blk, or nil if the
// block doesn't end with an If.
func blockIfTerminator(blk *ssa.BasicBlock) *ssa.If {
	if len(blk.Instrs) == 0 {
		return nil
	}
	last := blk.Instrs[len(blk.Instrs)-1]
	ifInstr, ok := last.(*ssa.If)
	if !ok {
		return nil
	}
	return ifInstr
}

// isErrorCondition returns true if val looks like an error check:
//   - BinOp with NEQ/EQL where one operand implements the error interface, or
//   - BinOp where one operand is named "err" (name-based fallback).
func isErrorCondition(val ssa.Value) bool {
	binop, ok := val.(*ssa.BinOp)
	if !ok {
		return false
	}
	if binop.Op != token.NEQ && binop.Op != token.EQL {
		return false
	}
	errIface := types.Universe.Lookup("error").Type().Underlying().(*types.Interface)
	for _, operand := range []ssa.Value{binop.X, binop.Y} {
		if operand == nil {
			continue
		}
		t := operand.Type()
		if types.Implements(t, errIface) {
			return true
		}
		if pt, ok2 := t.(*types.Pointer); ok2 {
			if types.Implements(pt.Elem(), errIface) {
				return true
			}
		}
		// Name-based fallback: if the SSA name contains "err".
		if name := operand.Name(); name == "err" || name == "error" {
			return true
		}
	}
	return false
}

// =============================================================================
// Feature 2 — Per-return path-condition analysis
// =============================================================================

// analyzeReturnPaths walks every in-project function and collects path
// conditions for each *ssa.Return instruction. Returns one ReturnInfo per
// function that has at least one interesting return.
func analyzeReturnPaths(prog *ssa.Program, fns map[*ssa.Function]bool, root string) []ReturnInfo {
	var results []ReturnInfo

	for f := range fns {
		if f.Synthetic != "" {
			continue
		}
		if len(f.Blocks) == 0 {
			continue
		}
		// Must be in-project.
		if f.Pos() == token.NoPos {
			continue
		}
		fnPos := prog.Fset.Position(f.Pos())
		if fnPos.Filename == "" {
			continue
		}
		fnFile, ok := relPath(root, fnPos.Filename)
		if !ok {
			continue
		}

		var sites []ReturnSite
		for _, blk := range f.Blocks {
			for _, instr := range blk.Instrs {
				ret, ok := instr.(*ssa.Return)
				if !ok {
					continue
				}

				retPos := prog.Fset.Position(ret.Pos())
				line := 0
				if retPos.IsValid() {
					line = retPos.Line
				}

				// Collect (cond, side) pairs by walking dominator chain.
				type condSide struct {
					cond ssa.Value
					neg  bool // true = on False branch (negate cond)
				}
				var chain []condSide
				cur := blk.Idom()
				depth := 0
				for cur != nil && depth < 4 {
					ifInstr := blockIfTerminator(cur)
					if ifInstr != nil {
						onTrue := len(cur.Succs) > 0 && dominates(cur.Succs[0], blk)
						chain = append(chain, condSide{cond: ifInstr.Cond, neg: !onTrue})
						depth++
					}
					cur = cur.Idom()
				}
				// Remaining conditions beyond depth 4.
				extraDepth := 0
				{
					tmp := blk.Idom()
					for tmp != nil {
						if blockIfTerminator(tmp) != nil {
							if extraDepth >= depth {
								// count beyond what we captured
								extraDepth++
							} else {
								extraDepth++
							}
						}
						tmp = tmp.Idom()
					}
					extraDepth = extraDepth - depth // how many we didn't capture
					if extraDepth < 0 {
						extraDepth = 0
					}
				}

				// Render the path condition.
				var terms []string
				simple := true
				for _, cs := range chain {
					rendered, isSimple := renderSSAValue(cs.cond, prog.Fset, 0)
					if !isSimple {
						simple = false
					}
					if cs.neg {
						// negate: flip NEQ→EQL or wrap with !
						negated := negateCondition(rendered, cs.cond)
						terms = append(terms, negated)
					} else {
						terms = append(terms, rendered)
					}
				}
				// Reverse so outermost condition is first (dominator chain walks
				// from innermost outward).
				for li, ri := 0, len(terms)-1; li < ri; li, ri = li+1, ri-1 {
					terms[li], terms[ri] = terms[ri], terms[li]
				}

				// Simplify: remove duplicates and trivially-true terms.
				terms = simplifyTerms(terms)
				if len(terms) == 0 && extraDepth > 0 {
					simple = false
				}

				pathCond := joinTerms(terms, extraDepth)
				if pathCond == "" {
					pathCond = "true"
				}

				// Handle Phi values in the return.
				var retValues []string
				if len(ret.Results) == 0 {
					retValues = []string{""}
				} else {
					for _, rv := range ret.Results {
						phi, isPhi := rv.(*ssa.Phi)
						if isPhi {
							// Split: emit one entry per Phi incoming edge.
							for edgeIdx, incoming := range phi.Edges {
								if edgeIdx < len(blk.Preds) {
									// Use the predecessor block's condition context for split.
									rendered, isS := renderSSAValue(incoming, prog.Fset, 0)
									if !isS {
										simple = false
									}
									retValues = append(retValues, rendered)
								}
							}
						} else {
							rendered, isS := renderSSAValue(rv, prog.Fset, 0)
							if !isS {
								simple = false
							}
							retValues = append(retValues, rendered)
						}
					}
				}

				if len(retValues) == 0 {
					retValues = []string{""}
				}

				// For Phi splits: emit one ReturnSite per value.
				for _, rv := range retValues {
					sites = append(sites, ReturnSite{
						Line:                line,
						Value:               rv,
						PathCondition:       pathCond,
						PathConditionSimple: simple,
					})
				}
			}
		}

		if len(sites) > 0 {
			fnName := containingName(f)
			if fnName == "" {
				fnName = f.Name()
			}
			results = append(results, ReturnInfo{
				File:    fnFile,
				Symbol:  fnName,
				Returns: sites,
			})
		}
	}

	return results
}

// renderSSAValue renders an SSA value as a Go-source-like string.
// Returns (rendered, isSimple). isSimple is false when structural rendering
// was used or recursion depth was hit.
func renderSSAValue(val ssa.Value, fset *token.FileSet, depth int) (string, bool) {
	if val == nil {
		return "?", false
	}
	if depth > 5 {
		return "...", false
	}

	switch v := val.(type) {
	case *ssa.Const:
		if v.Value == nil {
			return "nil", true
		}
		switch v.Value.Kind() {
		case constant.Bool:
			return v.Value.String(), true
		case constant.String:
			return fmt.Sprintf("%q", constant.StringVal(v.Value)), true
		default:
			return v.Value.String(), true
		}
	case *ssa.BinOp:
		x, xSimple := renderSSAValue(v.X, fset, depth+1)
		y, ySimple := renderSSAValue(v.Y, fset, depth+1)
		return fmt.Sprintf("%s %s %s", x, v.Op.String(), y), xSimple && ySimple
	case *ssa.UnOp:
		x, xSimple := renderSSAValue(v.X, fset, depth+1)
		return fmt.Sprintf("%s%s", v.Op.String(), x), xSimple
	case *ssa.Phi:
		return "<merged value>", false
	case *ssa.Call:
		return renderSSACallValue(v, fset, depth), false
	default:
		// Try position-based source recovery.
		if val.Pos() != token.NoPos {
			p := fset.Position(val.Pos())
			if p.IsValid() {
				// Use the SSA Name() as a proxy for the source identifier.
				name := val.Name()
				if name != "" && !isSSASyntheticName(name) {
					return name, true
				}
			}
		}
		// Structural fallback: use the SSA name.
		name := val.Name()
		if name == "" {
			return "?", false
		}
		return name, !isSSASyntheticName(name)
	}
}

// renderSSACallValue renders a Call value as a function-call string.
func renderSSACallValue(v *ssa.Call, fset *token.FileSet, depth int) string {
	if depth > 5 {
		return "..."
	}
	common := v.Common()
	if common == nil {
		return "?"
	}
	var fnName string
	if sf := common.StaticCallee(); sf != nil {
		fnName = sf.Name()
	} else {
		fnName, _ = renderSSAValue(common.Value, fset, depth+1)
	}
	var argStrs []string
	for _, arg := range common.Args {
		s, _ := renderSSAValue(arg, fset, depth+1)
		argStrs = append(argStrs, s)
	}
	if len(argStrs) > 3 {
		argStrs = argStrs[:3]
		argStrs = append(argStrs, "...")
	}
	return fmt.Sprintf("%s(%s)", fnName, joinStrings(argStrs, ", "))
}

// isSSASyntheticName returns true for SSA-generated temporaries like "t0", "t1" etc.
func isSSASyntheticName(name string) bool {
	if len(name) < 2 {
		return false
	}
	return name[0] == 't' && isDigit(name[1:])
}

func isDigit(s string) bool {
	for _, c := range s {
		if c < '0' || c > '9' {
			return false
		}
	}
	return len(s) > 0
}

// negateCondition returns the logical negation of the rendered condition string.
// If the original SSA value is a BinOp with NEQ, flips to EQL and vice versa.
func negateCondition(rendered string, val ssa.Value) string {
	if binop, ok := val.(*ssa.BinOp); ok {
		switch binop.Op {
		case token.NEQ:
			// Flip to ==
			x := rendered
			// Replace " != " with " == "
			if idx := findOpInRendered(x, "!="); idx >= 0 {
				return x[:idx] + "==" + x[idx+2:]
			}
		case token.EQL:
			if idx := findOpInRendered(rendered, "=="); idx >= 0 {
				return rendered[:idx] + "!=" + rendered[idx+2:]
			}
		}
	}
	return "!(" + rendered + ")"
}

// findOpInRendered finds the first occurrence of op surrounded by spaces.
func findOpInRendered(s, op string) int {
	search := " " + op + " "
	idx := 0
	for idx <= len(s)-len(search) {
		if s[idx:idx+len(search)] == search {
			return idx + 1 // position of op itself
		}
		idx++
	}
	return -1
}

// simplifyTerms removes duplicates and "true"/"false" identity terms.
func simplifyTerms(terms []string) []string {
	seen := make(map[string]bool)
	var out []string
	for _, t := range terms {
		if t == "true" {
			continue // x && true = x
		}
		if t == "false" {
			return []string{"false"} // x && false = false (unreachable)
		}
		if seen[t] {
			continue // x && x = x
		}
		seen[t] = true
		out = append(out, t)
	}
	return out
}

// joinTerms joins condition terms with " && ", appending "...and N more" if extra > 0.
func joinTerms(terms []string, extra int) string {
	if len(terms) == 0 && extra == 0 {
		return ""
	}
	result := joinStrings(terms, " && ")
	if extra > 0 {
		if result != "" {
			result += fmt.Sprintf(" && ...and %d more", extra)
		} else {
			result = fmt.Sprintf("...and %d more", extra)
		}
	}
	return result
}

func joinStrings(ss []string, sep string) string {
	if len(ss) == 0 {
		return ""
	}
	result := ss[0]
	for _, s := range ss[1:] {
		result += sep + s
	}
	return result
}
