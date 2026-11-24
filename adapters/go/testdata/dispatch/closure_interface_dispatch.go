package dispatch

// closure_interface_dispatch.go exercises registration closures that forward
// through an interface-typed handler. The helper should resolve each concrete
// in-project target rather than dropping the dispatch edge.

type ClosureIfaceMux struct{}

func (m *ClosureIfaceMux) HandleFunc(pattern string, handler func() error) {}

type ClosureInterfaceHandler interface {
	Serve() error
}

type ClosureIfaceAlpha struct{}

func (ClosureIfaceAlpha) Serve() error { return nil }

type ClosureIfaceBeta struct{}

func (ClosureIfaceBeta) Serve() error { return nil }

func RegisterClosureInterfaceMethod(mux *ClosureIfaceMux, handler ClosureInterfaceHandler) {
	mux.HandleFunc("/closure-interface", func() error {
		return handler.Serve()
	})
}
