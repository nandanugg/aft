package controlflow

import "errors"

func validate() bool { return true }
func fetch() error   { return nil }

// MultiReturn has 4 distinct return paths with different conditions.
func MultiReturn(input string) error {
	if input == "" {
		return errors.New("empty input")
	}
	if !validate() {
		return errors.New("invalid")
	}
	if err := fetch(); err != nil {
		return err
	}
	return nil
}
