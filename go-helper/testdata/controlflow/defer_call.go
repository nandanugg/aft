package controlflow

func closer() {
	// close logic
}

// WithDefer defers closer.
func WithDefer() {
	defer closer()
	// main logic
}
