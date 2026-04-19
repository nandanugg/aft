package controlflow

import "errors"

func do() error {
	return errors.New("oops")
}

func cleanup() {
	// cleanup logic
}

// Handle calls cleanup only on the error branch.
func Handle() {
	if err := do(); err != nil {
		cleanup()
	}
}
