package main

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"
)

type sidecarInfoForTest struct {
	ProviderID    string `json:"provider_id"`
	ProviderVer   string `json:"provider_version"`
	SchemaVersion int    `json:"schema_version"`
	Addr          string `json:"addr"`
	PID           int    `json:"pid"`
	StartedAt     string `json:"started_at"`
}

func startTestSidecar(t *testing.T, root, infoFile string) (string, func()) {
	t.Helper()

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen for sidecar test: %v", err)
	}

	server := newSidecarServer(root)
	done := make(chan error, 1)
	go func() {
		done <- runSidecarWithListener(server, ln, infoFile)
	}()

	addr := ln.Addr().String()

	waitForSidecarReady(t, addr)

	stop := func() {
		// Use explicit shutdown so we can also verify that the handler exists.
		req := sidecarRequest{
			Method: sidecarMethodShutdown,
		}
		if _, callErr := doSidecarRequest(t, addr, req); callErr != nil {
			_ = ln.Close()
		}
		err := waitForSidecarStop(done)
		if err != nil {
			t.Fatalf("sidecar stop: %v", err)
		}
	}

	t.Cleanup(stop)
	return addr, stop
}

func waitForSidecarReady(t *testing.T, addr string) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		conn, err := net.Dial("tcp", addr)
		if err == nil {
			conn.Close()
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("sidecar not ready at %s", addr)
}

func waitForSidecarStop(done <-chan error) error {
	select {
	case err := <-done:
		return err
	case <-time.After(2 * time.Second):
		return os.ErrDeadlineExceeded
	}
}

func doSidecarRequest(t *testing.T, addr string, req sidecarRequest) (sidecarResponse, error) {
	t.Helper()

	conn, err := net.Dial("tcp", addr)
	if err != nil {
		return sidecarResponse{}, err
	}
	defer conn.Close()

	if err := json.NewEncoder(conn).Encode(req); err != nil {
		return sidecarResponse{}, err
	}

	var resp sidecarResponse
	if err := json.NewDecoder(conn).Decode(&resp); err != nil {
		return sidecarResponse{}, err
	}
	return resp, nil
}

func decodeResult[T any](t *testing.T, resp sidecarResponse) T {
	t.Helper()
	var out T
	raw, err := json.Marshal(resp.Result)
	if err != nil {
		t.Fatalf("marshal result: %v", err)
	}
	if len(raw) == 0 || string(raw) == "null" {
		return out
	}
	if err := json.Unmarshal(raw, &out); err != nil {
		t.Fatalf("decode result: %v", err)
	}
	return out
}

func waitForJobState(t *testing.T, addr, jobID, want string) sidecarJobStatusResult {
	t.Helper()

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		resp, err := doSidecarRequest(t, addr, sidecarRequest{
			Method: sidecarMethodJobStatus,
			JobID:  jobID,
		})
		if err != nil {
			t.Fatalf("job_status request: %v", err)
		}
		if !resp.Ok {
			t.Fatalf("job_status failed: %v", resp.Error)
		}
		status := decodeResult[sidecarJobStatusResult](t, resp)
		if status.State == want {
			return status
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("job %s did not reach state %s", jobID, want)
	return sidecarJobStatusResult{}
}

func absRoot(t *testing.T, rel string) string {
	t.Helper()
	root, err := filepath.Abs(filepath.Join("testdata", rel))
	if err != nil {
		t.Fatalf("abs root: %v", err)
	}
	return root
}

func TestSidecarInfoFileAndHello(t *testing.T) {
	root := absRoot(t, "impls")
	infoFile := filepath.Join(t.TempDir(), "sidecar-info.json")

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen for sidecar test: %v", err)
	}
	server := newSidecarServer(root)
	done := make(chan error, 1)
	go func() {
		done <- runSidecarWithListener(server, ln, infoFile)
	}()
	addr := ln.Addr().String()
	waitForSidecarReady(t, addr)

	defer func() {
		if err := waitForSidecarStop(done); err != nil && err != os.ErrDeadlineExceeded {
			t.Fatalf("sidecar stop: %v", err)
		}
	}()

	// Ensure metadata file was written as soon as listener is live.
	raw, err := os.ReadFile(infoFile)
	if err != nil {
		t.Fatalf("read sidecar info file: %v", err)
	}
	var info sidecarInfoForTest
	if err := json.Unmarshal(raw, &info); err != nil {
		t.Fatalf("unmarshal sidecar info: %v", err)
	}
	if info.ProviderID != sidecarResponseProviderID {
		t.Fatalf("provider id: %q", info.ProviderID)
	}
	if info.ProviderVer != sidecarResponseProviderVersion {
		t.Fatalf("provider version: %q", info.ProviderVer)
	}
	if info.SchemaVersion != sidecarResponseSchemaVersion {
		t.Fatalf("schema version: %d", info.SchemaVersion)
	}
	if info.Addr != addr {
		t.Fatalf("info addr %q != %q", info.Addr, addr)
	}
	if info.PID <= 0 {
		t.Fatalf("pid invalid: %d", info.PID)
	}

	resp, err := doSidecarRequest(t, addr, sidecarRequest{Method: sidecarMethodHello})
	if err != nil {
		t.Fatalf("hello request: %v", err)
	}
	if !resp.Ok {
		t.Fatalf("hello failed: %v", resp.Error)
	}
	hello := decodeResult[sidecarHelloResult](t, resp)
	if hello.ProviderID != sidecarResponseProviderID {
		t.Fatalf("hello provider id: %q", hello.ProviderID)
	}
	if hello.SchemaVersion != sidecarResponseSchemaVersion {
		t.Fatalf("hello schema version: %d", hello.SchemaVersion)
	}
	if len(hello.Capabilities) != len(sidecarCapabilities) {
		t.Fatalf("hello capabilities len: %d", len(hello.Capabilities))
	}
	if hello.DefaultRoot != root {
		t.Fatalf("hello default root: %q", hello.DefaultRoot)
	}

	stopReq := sidecarRequest{Method: sidecarMethodShutdown}
	if _, err := doSidecarRequest(t, addr, stopReq); err != nil {
		t.Fatalf("shutdown: %v", err)
	}
	if err := waitForSidecarStop(done); err != nil {
		t.Fatalf("sidecar stop: %v", err)
	}
}

func TestSidecarRefreshStatusGetSnapshotInvalidate(t *testing.T) {
	root := absRoot(t, "impls")
	addr, _ := startTestSidecar(t, root, "")

	origAnalyze := sidecarAnalyze
	firstRelease := make(chan struct{})
	secondRelease := make(chan struct{})
	started := make(chan int, 2)
	callCount := 0
	sidecarAnalyze = func(
		root string,
		dispatches, implements, writes, callContext, returnAnalysis bool,
	) (*Output, error) {
		callCount++
		call := callCount
		started <- call
		switch call {
		case 1:
			<-firstRelease
		case 2:
			<-secondRelease
		}
		return &Output{
			Version: helperSchemaVersion,
			Root:    root,
			Edges: []Edge{{
				Caller: Position{File: "storer.go", Line: call, Symbol: fmt.Sprintf("Iface%d", call)},
				Callee: Target{File: "impl/impl.go", Line: call, Symbol: fmt.Sprintf("Impl%d", call)},
				Kind:   "implements",
			}},
			Skipped: nil,
			Returns: nil,
		}, nil
	}
	t.Cleanup(func() {
		sidecarAnalyze = origAnalyze
	})

	statusResp, err := doSidecarRequest(t, addr, sidecarRequest{
		Method: sidecarMethodStatus,
		Root:   root,
	})
	if err != nil {
		t.Fatalf("status request: %v", err)
	}
	if !statusResp.Ok {
		t.Fatalf("status failed: %v", statusResp.Error)
	}
	status := decodeResult[sidecarStatusResult](t, statusResp)
	if status.HasSnapshot || !status.Stale {
		t.Fatalf("unexpected initial status: %+v", status)
	}

	refreshResp, err := doSidecarRequest(t, addr, sidecarRequest{
		Method:      sidecarMethodRefresh,
		Root:        root,
		Fingerprint: "fp-1",
	})
	if err != nil {
		t.Fatalf("refresh request: %v", err)
	}
	if !refreshResp.Ok {
		t.Fatalf("refresh failed: %v", refreshResp.Error)
	}
	refresh := decodeResult[sidecarRefreshResult](t, refreshResp)
	if refresh.JobID == "" {
		t.Fatalf("refresh missing job id: %+v", refresh)
	}
	if refresh.State != sidecarJobRunning {
		t.Fatalf("refresh state: %+v", refresh)
	}
	if refresh.SnapshotState != sidecarSnapshotMissing {
		t.Fatalf("refresh snapshot state: %+v", refresh)
	}

	reusedRefresh, err := doSidecarRequest(t, addr, sidecarRequest{
		Method:      sidecarMethodRefresh,
		Root:        root,
		Fingerprint: "fp-1",
	})
	if err != nil {
		t.Fatalf("refresh request: %v", err)
	}
	if !reusedRefresh.Ok {
		t.Fatalf("refresh failed: %v", reusedRefresh.Error)
	}
	reused := decodeResult[sidecarRefreshResult](t, reusedRefresh)
	if reused.JobID != refresh.JobID {
		t.Fatalf("expected singleflight job reuse: %+v vs %+v", refresh, reused)
	}
	if reused.State != sidecarJobRunning {
		t.Fatalf("reused refresh state: %+v", reused)
	}

	if call := <-started; call != 1 {
		t.Fatalf("expected first analyze call, got %d", call)
	}
	running := waitForJobState(t, addr, refresh.JobID, sidecarJobRunning)
	if running.SnapshotState != sidecarSnapshotMissing {
		t.Fatalf("expected missing snapshot while first job runs: %+v", running)
	}

	invalidateResp, err := doSidecarRequest(t, addr, sidecarRequest{
		Method:        sidecarMethodInvalidate,
		Root:          root,
		Fingerprint:   "fp-2",
		ModuleDirty:   true,
		DirtyFiles:    []string{"impl/impl.go"},
		DirtyPackages: []string{"impl"},
	})
	if err != nil {
		t.Fatalf("invalidate request: %v", err)
	}
	if !invalidateResp.Ok {
		t.Fatalf("invalidate failed: %v", invalidateResp.Error)
	}
	inv := decodeResult[sidecarInvalidateResult](t, invalidateResp)
	if inv.Marked == 0 {
		t.Fatalf("invalidate marked nothing")
	}

	refreshAfterInvalidate, err := doSidecarRequest(t, addr, sidecarRequest{
		Method:      sidecarMethodRefresh,
		Root:        root,
		Fingerprint: "fp-2",
		ModuleDirty: true,
	})
	if err != nil {
		t.Fatalf("refresh request: %v", err)
	}
	if !refreshAfterInvalidate.Ok {
		t.Fatalf("refresh failed: %v", refreshAfterInvalidate.Error)
	}
	queued := decodeResult[sidecarRefreshResult](t, refreshAfterInvalidate)
	if queued.JobID == "" || queued.JobID == refresh.JobID {
		t.Fatalf("expected distinct queued job after invalidate: %+v", queued)
	}
	if queued.State != sidecarJobQueued {
		t.Fatalf("expected queued state: %+v", queued)
	}

	statusAfterResp, err := doSidecarRequest(t, addr, sidecarRequest{
		Method:      sidecarMethodStatus,
		Root:        root,
		Fingerprint: "fp-1",
	})
	if err != nil {
		t.Fatalf("status request: %v", err)
	}
	if !statusAfterResp.Ok {
		t.Fatalf("status failed: %v", statusAfterResp.Error)
	}
	statusAfter := decodeResult[sidecarStatusResult](t, statusAfterResp)
	if statusAfter.HasSnapshot || !statusAfter.Stale {
		t.Fatalf("expected no fresh snapshot before releases: %+v", statusAfter)
	}
	close(firstRelease)
	doneFirst := waitForJobState(t, addr, refresh.JobID, sidecarJobDone)
	if doneFirst.SnapshotState != sidecarSnapshotStale {
		t.Fatalf("first job should end stale after invalidate: %+v", doneFirst)
	}

	if call := <-started; call != 2 {
		t.Fatalf("expected second analyze call, got %d", call)
	}
	waitForJobState(t, addr, queued.JobID, sidecarJobRunning)

	staleResp, err := doSidecarRequest(t, addr, sidecarRequest{
		Method:      sidecarMethodStatus,
		Root:        root,
		Fingerprint: "fp-2",
	})
	if err != nil {
		t.Fatalf("status request: %v", err)
	}
	if !staleResp.Ok {
		t.Fatalf("status failed: %v", staleResp.Error)
	}
	stale := decodeResult[sidecarStatusResult](t, staleResp)
	if !stale.Stale || !stale.HasSnapshot {
		t.Fatalf("expected stale=true after invalidate: %+v", stale)
	}

	snapshotAfterInvalidate, err := doSidecarRequest(t, addr, sidecarRequest{
		Method:      sidecarMethodGetSnapshot,
		Root:        root,
		Fingerprint: "fp-2",
	})
	if err != nil {
		t.Fatalf("get_snapshot request: %v", err)
	}
	if !snapshotAfterInvalidate.Ok {
		t.Fatalf("expected stale snapshot to remain available: %+v", snapshotAfterInvalidate)
	}
	staleSnapshot := decodeResult[sidecarGetSnapshotResult](t, snapshotAfterInvalidate)
	if !staleSnapshot.Stale || staleSnapshot.Fingerprint != "fp-1" {
		t.Fatalf("expected stale fp-1 snapshot after invalidate: %+v", staleSnapshot)
	}

	close(secondRelease)
	doneSecond := waitForJobState(t, addr, queued.JobID, sidecarJobDone)
	if doneSecond.SnapshotState != sidecarSnapshotFresh {
		t.Fatalf("second job should finish fresh: %+v", doneSecond)
	}

	statusFreshResp, err := doSidecarRequest(t, addr, sidecarRequest{
		Method:      sidecarMethodStatus,
		Root:        root,
		Fingerprint: "fp-2",
	})
	if err != nil {
		t.Fatalf("status request: %v", err)
	}
	if !statusFreshResp.Ok {
		t.Fatalf("status failed: %v", statusFreshResp.Error)
	}
	statusFresh := decodeResult[sidecarStatusResult](t, statusFreshResp)
	if !statusFresh.HasSnapshot || statusFresh.Stale {
		t.Fatalf("status after second refresh: %+v", statusFresh)
	}

	freshSnapshotResp, err := doSidecarRequest(t, addr, sidecarRequest{
		Method:      sidecarMethodGetSnapshot,
		Root:        root,
		Fingerprint: "fp-2",
	})
	if err != nil {
		t.Fatalf("get_snapshot request: %v", err)
	}
	if !freshSnapshotResp.Ok {
		t.Fatalf("get_snapshot failed: %+v", freshSnapshotResp)
	}
	freshSnapshot := decodeResult[sidecarGetSnapshotResult](t, freshSnapshotResp)
	if freshSnapshot.Stale || freshSnapshot.Fingerprint != "fp-2" {
		t.Fatalf("expected fresh fp-2 snapshot: %+v", freshSnapshot)
	}
}

// Output.Edges must marshal as `[]` even when no edges were produced. The
// Rust side declares `edges: Vec<HelperEdge>` and rejects an explicit JSON
// null. A regression here causes every empty-edge project to fall back to
// local_helper with "decode sidecar result: invalid type: null, expected a
// sequence".
func TestOutputEdgesMarshalAsEmptyArrayWhenNil(t *testing.T) {
	out := &Output{Version: helperSchemaVersion, Root: "/x", Edges: []Edge{}}
	data, err := json.Marshal(out)
	if err != nil {
		t.Fatalf("marshal empty Output: %v", err)
	}
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	got, ok := raw["edges"]
	if !ok {
		t.Fatalf("edges key missing from %s", data)
	}
	if string(got) != "[]" {
		t.Fatalf("edges must marshal as []; got %s (full doc: %s)", got, data)
	}
}
