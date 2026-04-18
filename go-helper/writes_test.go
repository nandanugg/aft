package main

import (
	"path/filepath"
	"testing"
)

// writesKind is the kind string for writes edges.
const writesKind = "writes"

// TestWritesEdgesCrossPackage verifies that cross-package writes to package-level
// variables are emitted as "writes" edges.
func TestWritesEdgesCrossPackage(t *testing.T) {
	root := filepath.Join("testdata", "writes")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true /* emitWrites */)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	// Collect writes edges.
	writesEdges := make(map[string]bool) // "callerSym->calleeSym" set
	for _, e := range out.Edges {
		if e.Kind == writesKind {
			key := e.Caller.Symbol + "->" + e.Callee.Symbol
			writesEdges[key] = true
		}
	}

	// StartServer writes to HandlerRegistry and DefaultConfig.
	wantEdges := []string{
		"StartServer->HandlerRegistry",
		"StartServer->DefaultConfig",
	}
	for _, want := range wantEdges {
		if !writesEdges[want] {
			t.Errorf("expected writes edge not found: %s", want)
		}
	}

	// initRegistry (called from init) writes to GroupedVarA and GroupedVarB.
	// SSA may collapse the init chain; accept either initRegistry or init as caller.
	foundGroupedA := writesEdges["initRegistry->GroupedVarA"] || writesEdges["init->GroupedVarA"]
	foundGroupedB := writesEdges["initRegistry->GroupedVarB"] || writesEdges["init->GroupedVarB"]
	if !foundGroupedA {
		t.Errorf("expected writes edge for GroupedVarA not found; edges: %v", writesEdges)
	}
	if !foundGroupedB {
		t.Errorf("expected writes edge for GroupedVarB not found; edges: %v", writesEdges)
	}
}

// TestWritesEdgesSamePackageDropped verifies that same-package writes are
// not emitted — tree-sitter handles those.
func TestWritesEdgesSamePackageDropped(t *testing.T) {
	root := filepath.Join("testdata", "writes")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	// samePackageWriter is in the same package as HandlerRegistry; its write
	// must NOT appear in the output.
	for _, e := range out.Edges {
		if e.Kind == writesKind && e.Caller.Symbol == "samePackageWriter" {
			t.Errorf("same-package write should be filtered at source, got: %+v", e)
		}
	}
}

// TestNoWritesFlag verifies that -no-writes suppresses all writes edges.
func TestNoWritesFlag(t *testing.T) {
	root := filepath.Join("testdata", "writes")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, false /* emitWrites = false */)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	for _, e := range out.Edges {
		if e.Kind == writesKind {
			t.Errorf("unexpected writes edge with -no-writes: %+v", e)
		}
	}
}
