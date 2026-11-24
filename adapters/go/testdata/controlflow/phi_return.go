package controlflow

// PhiReturn uses a phi merge: v is assigned conditionally then returned.
func PhiReturn(cond bool) int {
	v := 0
	if cond {
		v = 42
	}
	return v
}
