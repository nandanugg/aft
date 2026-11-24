package main

import (
	"go/ast"
	"go/constant"
	"go/types"
	"regexp"
	"sort"
	"strings"

	"golang.org/x/tools/go/packages"
)

// Producer: `registers` facts. A registration call hands a function to someone who will call it later:
//
//	mux.HandleFunc("/api", handleAPI)
//	mux.HandleFunc("job:refund", func(ctx context.Context) error { return HandleRefund(ctx) })
//	pb.RegisterOrderServiceServer(srv, handler)
//
// subject = the open parenthesis of the registration call; object = the declared name of the handler;
// via = the registration API that was called, written as the reader sees it in the source: package
// name, receiver type and method, for example `(*asynq.ServeMux).HandleFunc`, `kafka.InitConsumer`
// or `pb.RegisterOrderServiceServer` (see registrationAPI);
// label = the one string constant among the arguments (typed constants and string(...) casts are
// folded by the type checker), or nothing when there is not exactly one.
//
// A function literal counts when its body makes exactly one call that reaches functions of the
// repository: the literal only forwards, and its target is the object. When the call has a label and
// the literal does more than forward, the literal itself is the object (the position of its `func`
// keyword, object_name "func literal"): it is the handler, for example an asynq job. Without a label
// such a literal is skipped (sort.Slice, errgroup.Go and similar are not registrations).
//
// Lifecycle hooks are not handlers: when the called function has a function parameter that receives
// arguments (the slot of the event handler), an argument passed in a slot of type func() with no
// parameters (for example a close hook: kafka.InitConsumer(..., handler, app.Close)) is skipped.

const maxRegisterKeyLen = 128

var grpcRegisterName = regexp.MustCompile(`^Register(.+)Server$`)

type registersProducer struct {
	proj *project
	impl *implIndex
	rp   *rootPackage
	info *types.Info
	out  *result
	// serviceNames caches the gRPC service names read from generated code: package path + name.
	serviceNames map[string]string
}

func produceRegisters(proj *project, impl *implIndex, out *result) {
	serviceNames := map[string]string{}
	for _, rp := range proj.roots {
		if !rp.typeChecked {
			continue
		}
		p := &registersProducer{proj: proj, impl: impl, rp: rp, info: rp.pkg.TypesInfo, out: out, serviceNames: serviceNames}
		for _, file := range rp.files {
			p.file(file)
		}
	}
}

func (p *registersProducer) file(file *ast.File) {
	for _, decl := range file.Decls {
		switch d := decl.(type) {
		case *ast.FuncDecl:
			if d.Body != nil {
				p.calls(d.Body, p.proj.funcDisplayName(d), p.info.Defs[d.Name])
			}
		case *ast.GenDecl:
			p.calls(d, "init", nil)
		}
	}
}

// calls looks at every call below n. enclosing is the declared function the calls belong to (nil for
// package-level initialisers); a handler that is the enclosing function itself is not a registration.
func (p *registersProducer) calls(n ast.Node, ownerName string, enclosing types.Object) {
	ast.Inspect(n, func(n ast.Node) bool {
		call, ok := n.(*ast.CallExpr)
		if !ok {
			return true
		}
		if tv, ok := p.info.Types[call.Fun]; ok && (tv.IsType() || tv.IsBuiltin()) {
			return true // conversion or builtin
		}
		p.call(call, ownerName, enclosing)
		return true
	})
}

func (p *registersProducer) call(call *ast.CallExpr, ownerName string, enclosing types.Object) {
	subject, ok := p.proj.position(call.Lparen)
	if !ok {
		return
	}
	label := p.stringKey(call.Args)
	via := ""
	if c, ok := resolveCallee(p.info, call); ok {
		via = registrationAPI(c.fn)
	}
	sig, _ := p.info.TypeOf(call.Fun).(*types.Signature)

	for i, arg := range call.Args {
		if isLifecycleHookSlot(sig, i) {
			continue
		}
		handlers := p.handlersOf(arg)
		for _, handler := range handlers {
			if types.Object(handler) == enclosing {
				continue
			}
			p.addFact(subject, ownerName, handler, label, via)
		}
		if lit, isLiteral := ast.Unparen(arg).(*ast.FuncLit); isLiteral && len(handlers) == 0 && label != "" {
			p.addLiteralFact(subject, ownerName, lit, label, via)
		}
	}
	p.grpcRegistration(call, subject, ownerName, via)
}

// registrationAPI names the called registration function for a reader: `pkg.Func` for a function and
// `(*pkg.Type).Method` or `(pkg.Type).Method` for a method. pkg is the package name (what the source
// writes before the dot), not the import path. A method of an interface is written with the interface.
func registrationAPI(fn *types.Func) string {
	qualified := func(pkg *types.Package, name string) string {
		if pkg == nil {
			return name
		}
		return pkg.Name() + "." + name
	}
	recv := fn.Type().(*types.Signature).Recv()
	if recv == nil {
		return qualified(fn.Pkg(), fn.Name())
	}
	recvType, star := recv.Type(), ""
	if pointer, ok := recvType.(*types.Pointer); ok {
		recvType, star = pointer.Elem(), "*"
	}
	named, ok := types.Unalias(recvType).(*types.Named)
	if !ok {
		return fn.Name() // an unnamed interface or a type parameter: no type to write
	}
	return "(" + star + qualified(named.Obj().Pkg(), named.Obj().Name()) + ")." + fn.Name()
}

// isLifecycleHookSlot is true when parameter i of sig has type func() with no parameters while
// another function parameter of sig receives arguments (see the producer comment).
func isLifecycleHookSlot(sig *types.Signature, i int) bool {
	if sig == nil || sig.Params().Len() == 0 {
		return false
	}
	paramType := func(index int) types.Type {
		params := sig.Params()
		if sig.Variadic() && index >= params.Len()-1 {
			if slice, ok := params.At(params.Len() - 1).Type().(*types.Slice); ok {
				return slice.Elem()
			}
		}
		if index >= params.Len() {
			return nil
		}
		return params.At(index).Type()
	}
	funcParams := func(t types.Type) (int, bool) {
		if t == nil {
			return 0, false
		}
		fn, ok := t.Underlying().(*types.Signature)
		if !ok {
			return 0, false
		}
		return fn.Params().Len(), true
	}
	if n, isFunc := funcParams(paramType(i)); !isFunc || n > 0 {
		return false
	}
	for j := 0; j < sig.Params().Len(); j++ {
		if n, isFunc := funcParams(sig.Params().At(j).Type()); isFunc && n > 0 {
			return true
		}
	}
	return false
}

// stringKey returns the only string constant (at most 128 bytes) among args, or "".
func (p *registersProducer) stringKey(args []ast.Expr) string {
	found, count := "", 0
	for _, arg := range args {
		tv, ok := p.info.Types[arg]
		if !ok || tv.Value == nil || tv.Value.Kind() != constant.String {
			continue
		}
		s := constant.StringVal(tv.Value)
		if len(s) > maxRegisterKeyLen {
			continue
		}
		found = s
		count++
	}
	if count == 1 {
		return found
	}
	return ""
}

// handlersOf returns the declared functions that an argument hands over.
func (p *registersProducer) handlersOf(arg ast.Expr) []*types.Func {
	switch a := ast.Unparen(arg).(type) {
	case *ast.Ident:
		if fn, ok := p.info.Uses[a].(*types.Func); ok {
			return []*types.Func{fn.Origin()}
		}
	case *ast.SelectorExpr:
		if selection := p.info.Selections[a]; selection != nil {
			fn, ok := selection.Obj().(*types.Func)
			if ok && !isInterfaceType(selection.Recv()) {
				return []*types.Func{fn.Origin()}
			}
			return nil
		}
		if fn, ok := p.info.Uses[a.Sel].(*types.Func); ok {
			return []*types.Func{fn.Origin()}
		}
	case *ast.FuncLit:
		return p.forwardedTargets(a)
	}
	return nil
}

// forwardedTargets returns what a function literal forwards to: the functions of the repository that
// its one call reaches. Calls in nested literals are not looked at.
func (p *registersProducer) forwardedTargets(lit *ast.FuncLit) []*types.Func {
	var found []*types.Func
	callsReachingRepo := 0
	ast.Inspect(lit.Body, func(n ast.Node) bool {
		if _, nested := n.(*ast.FuncLit); nested {
			return false
		}
		call, ok := n.(*ast.CallExpr)
		if !ok {
			return true
		}
		c, ok := resolveCallee(p.info, call)
		if !ok || c.fn.Pkg() == nil {
			return true // no declared callee, or the universe error.Error (as for site records)
		}
		var inRepo []*types.Func
		if c.kind == kindInterface {
			inRepo, _ = p.impl.implementersOf(c.iface, c.fn)
		} else {
			inRepo = []*types.Func{c.fn}
		}
		inRepo = p.keepAnalysed(inRepo)
		if len(inRepo) > 0 {
			callsReachingRepo++
			found = inRepo
		}
		return true
	})
	if callsReachingRepo != 1 {
		return nil
	}
	return found
}

// keepAnalysed keeps the functions declared in analysed files of the repository.
func (p *registersProducer) keepAnalysed(fns []*types.Func) []*types.Func {
	var kept []*types.Func
	for _, fn := range fns {
		if fn.Pkg() == nil {
			continue
		}
		if rp := p.proj.rootByPath[fn.Pkg().Path()]; rp != nil && rp.typeChecked {
			kept = append(kept, fn)
		}
	}
	return kept
}

func (p *registersProducer) addFact(subject position, ownerName string, handler *types.Func, label, via string) {
	if kept := p.keepAnalysed([]*types.Func{handler}); len(kept) == 0 {
		return
	}
	object, ok := p.proj.position(handler.Pos())
	if !ok {
		return
	}
	fact := factRecord{
		Type: "fact", Kind: "registers",
		Subject: subject, SubjectName: ownerName,
		Object: object, ObjectName: handler.Name(),
		Label: label, Via: via,
	}
	if isMockFile(object.File) {
		fact.Tags = []string{"mock"}
	}
	p.out.facts = append(p.out.facts, fact)
}

// addLiteralFact records a function literal as the handler: object = its `func` keyword.
func (p *registersProducer) addLiteralFact(subject position, ownerName string, lit *ast.FuncLit, label, via string) {
	object, ok := p.proj.position(lit.Type.Func)
	if !ok {
		return
	}
	fact := factRecord{
		Type: "fact", Kind: "registers",
		Subject: subject, SubjectName: ownerName,
		Object: object, ObjectName: "func literal",
		Label: label, Via: via,
	}
	if isMockFile(object.File) {
		fact.Tags = []string{"mock"}
	}
	p.out.facts = append(p.out.facts, fact)
}

// grpcRegistration adds one fact per method of the registered server, for the generated gRPC call
//
//	pb.RegisterXServer(server, impl)
//
// where impl implements the interface that is the second parameter. When impl has the interface type
// itself, every implementer in the repository counts. The label is the full gRPC method name, "/package.Service/Method", read from the generated service descriptor when found.
func (p *registersProducer) grpcRegistration(call *ast.CallExpr, subject position, ownerName, via string) {
	c, ok := resolveCallee(p.info, call)
	if !ok || c.kind != kindStatic || len(call.Args) != 2 {
		return
	}
	match := grpcRegisterName.FindStringSubmatch(c.fn.Name())
	if match == nil {
		return
	}
	sig := c.fn.Type().(*types.Signature)
	if sig.Params().Len() != 2 {
		return
	}
	iface, ok := types.Unalias(sig.Params().At(1).Type()).Underlying().(*types.Interface)
	if !ok {
		return
	}
	impl := p.info.TypeOf(call.Args[1])
	if impl == nil {
		return
	}
	service := p.serviceName(c.fn, match[1])
	for i := 0; i < iface.NumMethods(); i++ {
		method := iface.Method(i)
		if !method.Exported() {
			continue // mustEmbedUnimplementedXServer and similar
		}
		label := "/" + service + "/" + method.Name()
		if isInterfaceType(impl) {
			// The constructor returned the interface: every type of the repository that implements it may be the one.
			implementers, _ := p.impl.implementersOf(impl, method)
			for _, fn := range implementers {
				p.addFact(subject, ownerName, fn, label, via)
			}
			continue
		}
		obj, _, _ := types.LookupFieldOrMethod(impl, true, method.Pkg(), method.Name())
		if fn, ok := obj.(*types.Func); ok {
			p.addFact(subject, ownerName, fn.Origin(), label, via)
		}
	}
}

// serviceName finds "package.Service" for the registration function of generated code. It looks for
// the descriptor variable Service_ServiceDesc (or the older _Service_serviceDesc) in the package of
// the registration function and reads its ServiceName field. Without it, the bare service name
// is used.
func (p *registersProducer) serviceName(register *types.Func, bare string) string {
	pkgPath := register.Pkg().Path()
	cacheKey := pkgPath + "\x00" + bare
	if cached, ok := p.serviceNames[cacheKey]; ok {
		return cached
	}
	name := bare
	if pkg := p.rp.pkg.Imports[pkgPath]; pkg != nil {
		if found := serviceNameFromDescriptor(pkg, bare); found != "" {
			name = found
		}
	} else if pkgPath == p.rp.pkg.PkgPath {
		if found := serviceNameFromDescriptor(p.rp.pkg, bare); found != "" {
			name = found
		}
	}
	p.serviceNames[cacheKey] = name
	return name
}

func serviceNameFromDescriptor(pkg *packages.Package, bare string) string {
	wanted := map[string]bool{bare + "_ServiceDesc": true, "_" + bare + "_serviceDesc": true}
	for _, file := range pkg.Syntax {
		for _, decl := range file.Decls {
			gen, ok := decl.(*ast.GenDecl)
			if !ok {
				continue
			}
			for _, spec := range gen.Specs {
				value, ok := spec.(*ast.ValueSpec)
				if !ok || len(value.Names) != 1 || len(value.Values) != 1 || !wanted[value.Names[0].Name] {
					continue
				}
				if name := serviceNameField(value.Values[0]); name != "" {
					return name
				}
			}
		}
	}
	return ""
}

// serviceNameField reads ServiceName: "..." from a composite literal of grpc.ServiceDesc.
func serviceNameField(expr ast.Expr) string {
	if unary, ok := expr.(*ast.UnaryExpr); ok {
		expr = unary.X
	}
	lit, ok := expr.(*ast.CompositeLit)
	if !ok {
		return ""
	}
	for _, elt := range lit.Elts {
		kv, ok := elt.(*ast.KeyValueExpr)
		if !ok {
			continue
		}
		if key, ok := kv.Key.(*ast.Ident); ok && key.Name == "ServiceName" {
			if basic, ok := kv.Value.(*ast.BasicLit); ok {
				return strings.Trim(basic.Value, "\"`")
			}
		}
	}
	return ""
}

// sortFacts orders facts so that the output does not depend on map order.
func sortFacts(facts []factRecord) {
	sort.SliceStable(facts, func(i, j int) bool {
		a, b := facts[i], facts[j]
		if a.Kind != b.Kind {
			return a.Kind < b.Kind
		}
		if c := comparePositions(a.Subject, b.Subject); c != 0 {
			return c < 0
		}
		if c := comparePositions(a.Object, b.Object); c != 0 {
			return c < 0
		}
		if a.Label != b.Label {
			return a.Label < b.Label
		}
		return a.ObjectName < b.ObjectName
	})
}

func comparePositions(a, b position) int {
	switch {
	case a.File != b.File:
		return strings.Compare(a.File, b.File)
	case a.Line != b.Line:
		return a.Line - b.Line
	default:
		return a.Col - b.Col
	}
}
