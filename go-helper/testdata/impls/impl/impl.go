// Package impl provides cross-package implementations of the interfaces
// defined in the parent package. The helper must emit implements edges here
// because these are in a different file (and package) from the interfaces.
package impl

import "example.com/impls"

// StoreImpl implements impls.Storer with a pointer receiver.
type StoreImpl struct{ name string }

func (s *StoreImpl) Create(name string) error { return nil }
func (s *StoreImpl) Delete(id int) error      { return nil }

// ValueImpl implements impls.Storer with a value receiver.
// Go's type system accepts value receivers for interface satisfaction when
// the interface variable holds a non-pointer value.
type ValueImpl struct{}

func (v ValueImpl) Create(name string) error { return nil }
func (v ValueImpl) Delete(id int) error      { return nil }

// PingImpl implements impls.Embedded (and thus can satisfy CompositeIface's
// Embedded part, but doesn't implement Fetch so it's not a CompositeIface impl).
type PingImpl struct{}

func (p *PingImpl) Ping() bool { return true }

// CompositeImpl implements impls.CompositeIface (Embedded + Fetch).
type CompositeImpl struct{}

func (c *CompositeImpl) Ping() bool                    { return true }
func (c *CompositeImpl) Fetch(id int) (string, error)  { return "", nil }

// Ensure the implementations satisfy the interfaces at compile time.
var _ impls.Storer = (*StoreImpl)(nil)
var _ impls.Storer = ValueImpl{}
var _ impls.CompositeIface = (*CompositeImpl)(nil)
