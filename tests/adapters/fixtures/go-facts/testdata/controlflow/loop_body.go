package controlflow

func process(x int) {
	// process logic
	_ = x
}

// RangeLoop calls process for each element.
func RangeLoop(xs []int) {
	for _, x := range xs {
		process(x)
	}
}
