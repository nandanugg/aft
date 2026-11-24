package dispatch

// closure_method_dispatch.go exercises registration closures that forward to a
// named receiver method. The helper should resolve the concrete method target
// instead of dropping the dispatch edge.

type ClosureMethodMux struct{}

func (m *ClosureMethodMux) HandleFunc(pattern string, handler func() error) {}

type ClosureMethodHandler struct{}

func (ClosureMethodHandler) Serve() error { return nil }

func RegisterClosureMethod(mux *ClosureMethodMux, handler ClosureMethodHandler) {
	mux.HandleFunc("/closure-method", func() error {
		return handler.Serve()
	})
}
