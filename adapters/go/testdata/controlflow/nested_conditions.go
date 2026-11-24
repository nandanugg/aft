package controlflow

import "errors"

func check() bool { return true }

func doWork() error { return nil }

func handleErr() {}

// DeepNested has 3-level-deep conditions with an error branch at the innermost.
func DeepNested() {
	if check() {
		if check() {
			if err := doWork(); err != nil {
				handleErr()
			}
		}
	}
}

func errFn() error { return errors.New("e") }

// NestedReturn returns an error from a 3-level-deep branch.
func NestedReturn() error {
	if check() {
		if check() {
			if err := errFn(); err != nil {
				return err
			}
		}
	}
	return nil
}
