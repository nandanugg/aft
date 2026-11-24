package svc

import "example.com/ifaceflow/store"

type Service struct {
	st store.Storer
}

func (s *Service) Find(id string) error {
	return s.st.Get("ctx", id)
}

// MyErr is an error type of the repository: err.Error() on an `error` value must not flow into it.
type MyErr struct{}

func (e *MyErr) Error() string { return "" }

func Describe(err error) string {
	return err.Error()
}
