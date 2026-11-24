// Package impls defines interfaces for the implements-edges test fixture.
package impls

// Storer is an interface with two methods — cross-package implementations
// should produce implements edges.
type Storer interface {
	Create(name string) error
	Delete(id int) error
}

// Embedded is embedded in CompositeIface below.
type Embedded interface {
	Ping() bool
}

// CompositeIface embeds Embedded and adds its own method.
// Any type implementing CompositeIface must implement Embedded too.
type CompositeIface interface {
	Embedded
	Fetch(id int) (string, error)
}

// localImpl is in the same file as the interface — tree-sitter sees it,
// so the helper must NOT emit implements edges for it.
type localImpl struct{}

func (l *localImpl) Create(name string) error { return nil }
func (l *localImpl) Delete(id int) error      { return nil }
