package dispatch

// interface_dispatch_site.go exercises Feature 1: dispatched_via for
// interface-method call sites.
//
// When the function that receives the handler is an interface method,
// dispatched_via should render as "(pkg.Iface).Method".

// Dispatcher is a minimal interface for registering handlers.
type Dispatcher interface {
	Register(name string, handler func() error)
}

// HandleViaInterface is a handler registered through a Dispatcher interface.
func HandleViaInterface() error { return nil }

// UseDispatcher calls Dispatcher.Register, passing a function value.
// dispatched_via should be "(example.com/dispatch.Dispatcher).Register".
func UseDispatcher(d Dispatcher) {
	d.Register("interface-dispatch-key", HandleViaInterface)
}
