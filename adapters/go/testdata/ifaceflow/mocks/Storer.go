package mocks

// Storer is a test double: a file under mocks/.
type Storer struct{}

func (m *Storer) Get(ctx string, id string) error { return nil }
