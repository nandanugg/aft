package main

import (
	"path/filepath"
	"testing"
)

// implEdgeKey uniquely identifies an implements edge for test comparison.
type implEdgeKey struct {
	ifaceSymbol string
	concRecv    string
	concSymbol  string
}

func edgeToImplKey(e Edge) implEdgeKey {
	return implEdgeKey{
		ifaceSymbol: e.Caller.Symbol,
		concRecv:    e.Callee.Receiver,
		concSymbol:  e.Callee.Symbol,
	}
}

// filterImplements returns only "implements" edges from a slice.
func filterImplements(edges []Edge) []Edge {
	var out []Edge
	for _, e := range edges {
		if e.Kind == "implements" {
			out = append(out, e)
		}
	}
	return out
}

// TestImplementsCrossPackage verifies that cross-package implementations are emitted.
func TestImplementsCrossPackage(t *testing.T) {
	root := filepath.Join("testdata", "impls")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, false /* emitDispatches */, true /* emitImplements */)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	impls := filterImplements(out.Edges)

	// StoreImpl (pointer receiver) must implement Create and Delete.
	// Receiver format uses the full package path as types.Type.String() renders it.
	wantKeys := map[implEdgeKey]bool{
		{ifaceSymbol: "Storer", concRecv: "*example.com/impls/impl.StoreImpl", concSymbol: "Create"}: false,
		{ifaceSymbol: "Storer", concRecv: "*example.com/impls/impl.StoreImpl", concSymbol: "Delete"}: false,
		{ifaceSymbol: "Storer", concRecv: "example.com/impls/impl.ValueImpl", concSymbol: "Create"}:  false,
		{ifaceSymbol: "Storer", concRecv: "example.com/impls/impl.ValueImpl", concSymbol: "Delete"}:  false,
	}

	for _, e := range impls {
		k := edgeToImplKey(e)
		if _, want := wantKeys[k]; want {
			wantKeys[k] = true
		}
	}

	for k, found := range wantKeys {
		if !found {
			t.Errorf("expected implements edge not found: iface=%s recv=%s method=%s",
				k.ifaceSymbol, k.concRecv, k.concSymbol)
		}
	}
}

// TestImplementsSameFileExcluded verifies that same-file implementations are NOT emitted.
func TestImplementsSameFileExcluded(t *testing.T) {
	root := filepath.Join("testdata", "impls")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, false, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	impls := filterImplements(out.Edges)

	// localImpl is in the same file as Storer interface — must not appear.
	// Receiver format uses full package path as types.Type.String() renders it.
	for _, e := range impls {
		recv := e.Callee.Receiver
		if recv == "*example.com/impls.localImpl" || recv == "example.com/impls.localImpl" {
			t.Errorf("same-file implementation should not be emitted: %+v", e)
		}
	}
}

// TestImplementsEmbeddedInterface verifies that embedded interface methods are covered.
func TestImplementsEmbeddedInterface(t *testing.T) {
	root := filepath.Join("testdata", "impls")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, false, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	impls := filterImplements(out.Edges)

	// CompositeImpl implements CompositeIface (which embeds Embedded).
	// We expect edges for both Ping (from Embedded) and Fetch.
	wantKeys := map[implEdgeKey]bool{
		{ifaceSymbol: "CompositeIface", concRecv: "*example.com/impls/impl.CompositeImpl", concSymbol: "Ping"}:  false,
		{ifaceSymbol: "CompositeIface", concRecv: "*example.com/impls/impl.CompositeImpl", concSymbol: "Fetch"}: false,
	}

	for _, e := range impls {
		k := edgeToImplKey(e)
		if _, want := wantKeys[k]; want {
			wantKeys[k] = true
		}
	}

	for k, found := range wantKeys {
		if !found {
			t.Errorf("expected embedded-interface implements edge not found: iface=%s recv=%s method=%s",
				k.ifaceSymbol, k.concRecv, k.concSymbol)
		}
	}
}

// TestNoImplementsFlag verifies the -no-implements flag suppresses all implements edges.
func TestNoImplementsFlag(t *testing.T) {
	root := filepath.Join("testdata", "impls")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, false /* emitDispatches */, false /* emitImplements */)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	for _, e := range out.Edges {
		if e.Kind == "implements" {
			t.Errorf("unexpected implements edge with -no-implements: %+v", e)
		}
	}
}

// TestImplementsReceiverType verifies that implements edges carry the correct
// receiver type string (pointer vs value).
func TestImplementsReceiverType(t *testing.T) {
	root := filepath.Join("testdata", "impls")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, false, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	impls := filterImplements(out.Edges)

	hasPointerRecv := false
	hasValueRecv := false
	for _, e := range impls {
		if e.Caller.Symbol == "Storer" {
			if e.Callee.Receiver == "*example.com/impls/impl.StoreImpl" {
				hasPointerRecv = true
			}
			if e.Callee.Receiver == "example.com/impls/impl.ValueImpl" {
				hasValueRecv = true
			}
		}
	}
	if !hasPointerRecv {
		t.Error("expected pointer receiver (*impl.StoreImpl) in implements edges")
	}
	if !hasValueRecv {
		t.Error("expected value receiver (impl.ValueImpl) in implements edges")
	}
}
