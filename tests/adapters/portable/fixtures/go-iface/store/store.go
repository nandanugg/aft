package store

// Saver is implemented by MemStore. Callers reach Save through this interface.
type Saver interface {
	Save(id string) error
}

type MemStore struct{ saved []string }

func (m *MemStore) Save(id string) error {
	m.saved = append(m.saved, id)
	return nil
}
