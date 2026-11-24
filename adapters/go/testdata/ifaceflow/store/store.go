package store

// Storer is called through an interface by svc.
type Storer interface {
	Get(ctx string, id string) error
}

type storer struct{}

// NewStorer returns the real implementation.
func NewStorer() Storer { return &storer{} }

func (s *storer) Get(ctx string, id string) error { return nil }
