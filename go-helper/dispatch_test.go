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

	out, err := analyze(absRoot, true /* emitDispatches */, true /* emitImplements */, true /* emitWrites */)
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

	out, err := analyze(absRoot, true, true, true)
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

	out, err := analyze(absRoot, true, true, true)
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
	out, err := analyze(absRoot, false, true, true)
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

	out, err := analyze(absRoot, true, true, true)
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

// TestTypedConstNearbyString tests Feature 2: resolveStringConst resolves
// typed-string constants and string(...) casts to the underlying string value.
func TestTypedConstNearbyString(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	// RegisterTypedHandlers uses string(TypeMerchantSettlement) and
	// string(TypeRefundCallback). Both should resolve to their string values.
	wantKeys := map[dispatchEdgeKey]bool{
		{callerSymbol: "RegisterTypedHandlers", calleeSymbol: "HandleMerchantSettlementTask", kind: "dispatches", nearbyString: "merchant_settlement:merchant_id"}: false,
		{callerSymbol: "RegisterTypedHandlers", calleeSymbol: "HandleRefundCallbackTask", kind: "dispatches", nearbyString: "refund_callback:payment_id"}:         false,
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
			t.Errorf("expected typed-const dispatch edge not found: caller=%s callee=%s key=%q",
				k.callerSymbol, k.calleeSymbol, k.nearbyString)
		}
	}
}

// TestDispatchedViaPresent tests Feature 1: dispatched_via is populated on
// dispatches edges and reflects the callee FQN.
func TestDispatchedViaPresent(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	// All dispatches edges should have dispatched_via populated.
	for _, e := range out.Edges {
		if e.Kind != "dispatches" {
			continue
		}
		if e.DispatchedVia == "" {
			t.Errorf("dispatches edge missing dispatched_via: caller=%s callee=%s key=%q",
				e.Caller.Symbol, e.Callee.Symbol, e.NearbyString)
		}
	}
}

// TestDispatchedViaNoStringConst tests that dispatched_via is populated even
// when there is no nearby_string (no string arg at the call site).
func TestDispatchedViaNoStringConst(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	found := false
	for _, e := range out.Edges {
		if e.Kind == "dispatches" && e.Callee.Symbol == "HandleNoKey" {
			found = true
			if e.NearbyString != "" {
				t.Errorf("HandleNoKey edge should have no nearby_string, got %q", e.NearbyString)
			}
			if e.DispatchedVia == "" {
				t.Errorf("HandleNoKey edge missing dispatched_via")
			}
		}
	}
	if !found {
		t.Error("expected dispatch edge for HandleNoKey not found")
	}
}

// TestDispatchedViaInterfaceSite tests that dispatched_via uses the
// parenthesized interface format for interface method call sites.
func TestDispatchedViaInterfaceSite(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true, true)
	if err != nil {
		t.Fatalf("analyze: %v", err)
	}

	found := false
	for _, e := range out.Edges {
		if e.Kind == "dispatches" && e.Callee.Symbol == "HandleViaInterface" {
			found = true
			if e.DispatchedVia == "" {
				t.Errorf("HandleViaInterface edge missing dispatched_via")
			}
			// Should contain the interface method name.
			if !containsStr(e.DispatchedVia, "Register") {
				t.Errorf("dispatched_via %q should contain 'Register'", e.DispatchedVia)
			}
		}
	}
	if !found {
		t.Error("expected dispatch edge for HandleViaInterface not found")
	}
}

func containsStr(s, sub string) bool {
	return len(s) >= len(sub) && (s == sub || len(sub) == 0 ||
		func() bool {
			for i := 0; i <= len(s)-len(sub); i++ {
				if s[i:i+len(sub)] == sub {
					return true
				}
			}
			return false
		}())
}

// TestDispatchGoldenJSON tests that the sorted dispatch edges match a golden file
// if it exists. To regenerate: delete testdata/dispatch/expected.json and run the test.
func TestDispatchGoldenJSON(t *testing.T) {
	root := filepath.Join("testdata", "dispatch")
	absRoot, err := filepath.Abs(root)
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}

	out, err := analyze(absRoot, true, true, true)
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
