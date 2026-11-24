package dispatch

// no_string_const.go exercises the case where a dispatch site has no nearby
// string constant at all. dispatched_via should still be populated; nearby_string
// should be absent.

// NoStringMux receives a handler with no string key argument.
type NoStringMux struct{}

func (m *NoStringMux) AddHandler(handler func() error) {}

func HandleNoKey() error { return nil }

// RegisterNoKey dispatches HandleNoKey with no string argument at the call site.
// Expected: nearby_string absent, dispatched_via = "example.com/dispatch.(*NoStringMux).AddHandler".
func RegisterNoKey(mux *NoStringMux) {
	mux.AddHandler(HandleNoKey)
}
