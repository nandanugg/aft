package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"sort"
	"testing"
)

// edgeKinds is the set of dispatch-family kinds this test cares about.
var dispatchKinds = map[string]bool{
	"dispatches": true,
	"goroutine":  true,
	"defer":      true,
}

// dispatchEdgeKey uniquely identifies a dispatch edge for equality comparison.
type dispatchEdgeKey struct {
	callerSymbol string
	calleeSymbol string
	kind         string
	nearbyString string
}

func edgeToKey(e Edge) dispatchEdgeKey {
	return dispatchEdgeKey{
		callerSymbol: e.Caller.Symbol,
		calleeSymbol: e.Callee.Symbol,
		kind:         e.Kind,
		nearbyString: e.NearbyString,
	}
}

func TestDispatchEdgesAsynq(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true /* emitDispatches */, true /* emitImplements */, true /* emitWrites */, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	// Collect all dispatch-family edges.
	var dispatches []Edge
	for _, e := range out.Edges {
		if dispatchKinds[e.Kind] {
			dispatches = append(dispatches, e)
		}
	}

	// Expected: RegisterHandlers dispatches HandleTaskA with key "TypeTaskA" and
	// HandleTaskB with key "TypeTaskB".
	wantKeys := map[dispatchEdgeKey]bool{
		{callerSymbol: "RegisterHandlers", calleeSymbol: "HandleTaskA", kind: "dispatches", nearbyString: "TypeTaskA"}: false,
		{callerSymbol: "RegisterHandlers", calleeSymbol: "HandleTaskB", kind: "dispatches", nearbyString: "TypeTaskB"}: false,
	}
	for _, e := range dispatches {
		k := edgeToKey(e)
		if _, want := wantKeys[k]; want {
			wantKeys[k] = true
		}
	}
	for k, found := range wantKeys {
		if !found {
			t.Errorf("expected dispatch edge not found: caller=%s callee=%s kind=%s key=%q",
				k.callerSymbol, k.calleeSymbol, k.kind, k.nearbyString)
		}
	}
}

func TestDispatchEdgesHTTP(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true, true, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	wantKeys := map[dispatchEdgeKey]bool{
		{callerSymbol: "RegisterHTTPHandlers", calleeSymbol: "handleHome", kind: "dispatches", nearbyString: "/"}: false,
		{callerSymbol: "RegisterHTTPHandlers", calleeSymbol: "handleAPI", kind: "dispatches", nearbyString: "/api"}: false,
	}
	for _, e := range out.Edges {
		if !dispatchKinds[e.Kind] {
			continue
		}
		k := edgeToKey(e)
		if _, want := wantKeys[k]; want {
			wantKeys[k] = true
		}
	}
	for k, found := range wantKeys {
		if !found {
			t.Errorf("expected dispatch edge not found: caller=%s callee=%s kind=%s key=%q",
				k.callerSymbol, k.calleeSymbol, k.kind, k.nearbyString)
		}
	}
}

func TestDispatchEdgesGoroutineDefer(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true, true, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	wantKeys := map[dispatchEdgeKey]bool{
		{callerSymbol: "StartWorkers", calleeSymbol: "workerLoop", kind: "goroutine"}: false,
		{callerSymbol: "StartWorkers", calleeSymbol: "process", kind: "goroutine"}:   false,
		{callerSymbol: "WithDefer", calleeSymbol: "cleanup", kind: "defer"}:           false,
	}
	for _, e := range out.Edges {
		if !dispatchKinds[e.Kind] {
			continue
		}
		k := edgeToKey(e)
		if _, want := wantKeys[k]; want {
			wantKeys[k] = true
		}
	}
	for k, found := range wantKeys {
		if !found {
			t.Errorf("expected dispatch edge not found: caller=%s callee=%s kind=%s",
				k.callerSymbol, k.calleeSymbol, k.kind)
		}
	}
}

func TestNoDispatchesFlag(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	// With emitDispatches=false, no dispatch/goroutine/defer edges should appear.
	out, err := analyze(absRoot, false, true, true, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	for _, e := range out.Edges {
		if dispatchKinds[e.Kind] {
			t.Errorf("unexpected dispatch-family edge with -no-dispatches: %+v", e)
		}
	}
}

func TestBadAnonymousClosure(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true, true, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	// Anonymous closures should not appear as dispatch callees.
	for _, e := range out.Edges {
		if e.Kind == "dispatches" && e.Callee.Symbol == "" {
			t.Errorf("anonymous closure appeared as dispatch callee: %+v", e)
		}
	}
}

// TestDispatchGoldenJSON tests that the sorted dispatch edges match a golden file
// if it exists. To regenerate: delete testdata/dispatch/expected.json and run the test.
func TestDispatchGoldenJSON(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true, true, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	// Filter to dispatch-family edges only.
	var edges []Edge
	for _, e := range out.Edges {
		if dispatchKinds[e.Kind] {
			edges = append(edges, e)
		}
	}
	// Sort for deterministic comparison.
	sort.Slice(edges, func(i, j int) bool {
		a, b := edges[i], edges[j]
		if a.Kind != b.Kind {
			return a.Kind < b.Kind
		}
		if a.Caller.Symbol != b.Caller.Symbol {
			return a.Caller.Symbol < b.Caller.Symbol
		}
		if a.Callee.Symbol != b.Callee.Symbol {
			return a.Callee.Symbol < b.Callee.Symbol
		}
		return a.NearbyString < b.NearbyString
	})

	goldenPath := filepath.Join("testdata", "dispatch", "expected.json")
	data, err := json.MarshalIndent(edges, "", "  ")
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	if _, serr := os.Stat(goldenPath); os.IsNotExist(serr) {
		// Golden file doesn't exist — write it and pass.
		if werr := os.WriteFile(goldenPath, data, 0644); werr != nil {
			t.Fatalf("write golden: %v", werr)
		}
		t.Logf("wrote golden file %s", goldenPath)
		return
	}

	golden, rerr := os.ReadFile(goldenPath)
	if rerr != nil {
		t.Fatalf("read golden: %v", rerr)
	}

	// Compare JSON-normalized.
	var gotSlice, wantSlice []any
	if uerr := json.Unmarshal(data, &gotSlice); uerr != nil {
		t.Fatalf("unmarshal got: %v", uerr)
	}
	if uerr := json.Unmarshal(golden, &wantSlice); uerr != nil {
		t.Fatalf("unmarshal want: %v", uerr)
	}
	gotNorm, _ := json.Marshal(gotSlice)
	wantNorm, _ := json.Marshal(wantSlice)
	if string(gotNorm) != string(wantNorm) {
		t.Errorf("dispatch edges differ from golden.\ngot:\n%s\nwant:\n%s", data, golden)
	}
}
