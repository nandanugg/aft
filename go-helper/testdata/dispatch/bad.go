package dispatch

// "Bad" cases that should NOT emit dispatch edges:
//   - Anonymous closure (no source-level name)
//   - Multiple string arguments (ambiguous key)

type registrar struct{}

func (r *registrar) Register(name, category string, handler func() error) {}

func badAnonymousClosure(r *registrar) {
	// Anonymous inline closure: should be skipped (no source-level identifier)
	r.Register("key", "cat", func() error { return nil })
}

func badMultipleStrings(r *registrar) {
	var handler func() error
	// Two string args: ambiguous which is the dispatch key — should emit no nearby_string
	// but still emit a dispatches edge if handler resolves to an in-project function
	_ = handler
}
