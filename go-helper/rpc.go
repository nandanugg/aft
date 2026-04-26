package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"
)

const (
	sidecarMethodHello       = "hello"
	sidecarMethodStatus      = "status"
	sidecarMethodRefresh     = "refresh"
	sidecarMethodJobStatus   = "job_status"
	sidecarMethodGetSnapshot = "get_snapshot"
	sidecarMethodInvalidate  = "invalidate"
	sidecarMethodShutdown    = "shutdown"

	sidecarResponseProviderID      = sidecarProviderID
	sidecarResponseSchemaVersion   = helperSchemaVersion
	sidecarResponseProviderVersion = sidecarProviderVersion

	sidecarJobQueued  = "queued"
	sidecarJobRunning = "running"
	sidecarJobDone    = "done"
	sidecarJobFailed  = "failed"

	sidecarSnapshotMissing = "missing"
	sidecarSnapshotStale   = "stale"
	sidecarSnapshotFresh   = "fresh"
)

var sidecarCapabilities = []string{
	sidecarMethodHello,
	sidecarMethodStatus,
	sidecarMethodRefresh,
	sidecarMethodJobStatus,
	sidecarMethodGetSnapshot,
	sidecarMethodInvalidate,
	sidecarMethodShutdown,
}

var sidecarRelevantEnvKeys = []string{
	"GO111MODULE",
	"GOENV",
	"GOBIN",
	"GOCACHE",
	"GOMODCACHE",
	"GOPATH",
	"GOPROXY",
	"GOSUMDB",
	"GOFLAGS",
}

type sidecarInfo struct {
	ProviderID    string `json:"provider_id"`
	ProviderVer   string `json:"provider_version"`
	SchemaVersion int    `json:"schema_version"`
	Addr          string `json:"addr"`
	PID           int    `json:"pid"`
	StartedAt     string `json:"started_at"`
}

type rpcFeatureFlags struct {
	Dispatches     *bool `json:"dispatches,omitempty"`
	NoDispatches   *bool `json:"no_dispatches,omitempty"`
	Implements     *bool `json:"implements,omitempty"`
	NoImplements   *bool `json:"no_implements,omitempty"`
	Writes         *bool `json:"writes,omitempty"`
	NoWrites       *bool `json:"no_writes,omitempty"`
	CallContext    *bool `json:"call_context,omitempty"`
	NoCallContext  *bool `json:"no_call_context,omitempty"`
	ReturnAnalysis *bool `json:"return_analysis,omitempty"`
	NoReturn       *bool `json:"no_return_analysis,omitempty"`
}

func (f *rpcFeatureFlags) featureSet() featureSet {
	if f == nil {
		return defaultFeatureSet()
	}

	fs := defaultFeatureSet()
	if f.Dispatches != nil {
		fs.Dispatches = *f.Dispatches
	}
	if f.NoDispatches != nil {
		fs.Dispatches = !*f.NoDispatches
	}
	if f.Implements != nil {
		fs.Implements = *f.Implements
	}
	if f.NoImplements != nil {
		fs.Implements = !*f.NoImplements
	}
	if f.Writes != nil {
		fs.Writes = *f.Writes
	}
	if f.NoWrites != nil {
		fs.Writes = !*f.NoWrites
	}
	if f.CallContext != nil {
		fs.CallContext = *f.CallContext
	}
	if f.NoCallContext != nil {
		fs.CallContext = !*f.NoCallContext
	}
	if f.ReturnAnalysis != nil {
		fs.ReturnAnalysis = *f.ReturnAnalysis
	}
	if f.NoReturn != nil {
		fs.ReturnAnalysis = !*f.NoReturn
	}
	return fs
}

func (f *rpcFeatureFlags) hasExplicitFeatureRequest() bool {
	return f != nil &&
		(f.Dispatches != nil ||
			f.NoDispatches != nil ||
			f.Implements != nil ||
			f.NoImplements != nil ||
			f.Writes != nil ||
			f.NoWrites != nil ||
			f.CallContext != nil ||
			f.NoCallContext != nil ||
			f.ReturnAnalysis != nil ||
			f.NoReturn != nil)
}

type featureSet struct {
	Dispatches     bool `json:"dispatches"`
	Implements     bool `json:"implements"`
	Writes         bool `json:"writes"`
	CallContext    bool `json:"call_context"`
	ReturnAnalysis bool `json:"return_analysis"`
}

func defaultFeatureSet() featureSet {
	return featureSet{
		Dispatches:     true,
		Implements:     true,
		Writes:         true,
		CallContext:    true,
		ReturnAnalysis: true,
	}
}

var sidecarAnalyze = analyze

type sidecarRequest struct {
	ID            string            `json:"id,omitempty"`
	Method        string            `json:"method"`
	JobID         string            `json:"job_id,omitempty"`
	Root          string            `json:"root,omitempty"`
	Fingerprint   string            `json:"fingerprint,omitempty"`
	EnvHash       string            `json:"env_hash,omitempty"`
	Env           map[string]string `json:"env,omitempty"`
	Features      *rpcFeatureFlags  `json:"features,omitempty"`
	TimeoutMs     int64             `json:"timeout_ms,omitempty"`
	DirtyFiles    []string          `json:"dirty_files,omitempty"`
	DirtyPackages []string          `json:"dirty_packages,omitempty"`
	ModuleDirty   bool              `json:"module_dirty,omitempty"`
}

type sidecarResponse struct {
	ID     string      `json:"id,omitempty"`
	Method string      `json:"method"`
	Ok     bool        `json:"ok"`
	Error  string      `json:"error,omitempty"`
	Result interface{} `json:"result,omitempty"`
}

type sidecarHelloResult struct {
	ProviderID      string   `json:"provider_id"`
	ProviderVersion string   `json:"provider_version"`
	SchemaVersion   int      `json:"schema_version"`
	Capabilities    []string `json:"capabilities"`
	DefaultRoot     string   `json:"default_root"`
}

type sidecarStatusResult struct {
	Root                 string `json:"root"`
	FeatureHash          string `json:"feature_hash"`
	EnvHash              string `json:"env_hash"`
	Fingerprint          string `json:"fingerprint,omitempty"`
	RequestedFingerprint string `json:"requested_fingerprint,omitempty"`
	HasSnapshot          bool   `json:"has_snapshot"`
	Stale                bool   `json:"stale"`
	LastRefreshedAt      string `json:"last_refreshed_at,omitempty"`
	CurrentJobID         string `json:"current_job_id,omitempty"`
	CurrentJobState      string `json:"current_job_state,omitempty"`
	PendingJobID         string `json:"pending_job_id,omitempty"`
	PendingJobState      string `json:"pending_job_state,omitempty"`
}

type sidecarRefreshResult struct {
	JobID           string `json:"job_id,omitempty"`
	State           string `json:"state"`
	SnapshotState   string `json:"snapshot_state"`
	Root            string `json:"root"`
	FeatureHash     string `json:"feature_hash"`
	EnvHash         string `json:"env_hash"`
	Fingerprint     string `json:"fingerprint"`
	LastRefreshedAt string `json:"last_refreshed_at,omitempty"`
	ModuleDirty     bool   `json:"module_dirty"`
}

type sidecarJobStatusResult struct {
	JobID           string `json:"job_id"`
	State           string `json:"state"`
	Root            string `json:"root"`
	FeatureHash     string `json:"feature_hash"`
	EnvHash         string `json:"env_hash"`
	Fingerprint     string `json:"fingerprint"`
	SnapshotState   string `json:"snapshot_state"`
	Error           string `json:"error,omitempty"`
	StartedAt       string `json:"started_at,omitempty"`
	UpdatedAt       string `json:"updated_at,omitempty"`
	CompletedAt     string `json:"completed_at,omitempty"`
	LastRefreshedAt string `json:"last_refreshed_at,omitempty"`
}

type sidecarGetSnapshotResult struct {
	Snapshot        Output `json:"snapshot"`
	Stale           bool   `json:"stale"`
	Root            string `json:"root"`
	FeatureHash     string `json:"feature_hash"`
	EnvHash         string `json:"env_hash"`
	Fingerprint     string `json:"fingerprint"`
	LastRefreshedAt string `json:"last_refreshed_at,omitempty"`
	ModuleDirty     bool   `json:"module_dirty"`
}

type sidecarInvalidateResult struct {
	Marked            int    `json:"marked"`
	ModuleDirty       bool   `json:"module_dirty"`
	SourceFingerprint string `json:"source_fingerprint"`
}

type sidecarShutdownResult struct {
	Status string `json:"status"`
}

type sidecarSnapshot struct {
	Output             *Output
	FeatureHash        string
	EnvHash            string
	Root               string
	SourceFingerprint  string
	DesiredFingerprint string
	ModuleDirty        bool
	DesiredModuleDirty bool
	Stale              bool
	RefreshedAt        time.Time
	CurrentJobID       string
	PendingJobID       string
	LastJobID          string
}

type sidecarRefreshJob struct {
	ID          string
	Key         sidecarStateKey
	Features    featureSet
	Fingerprint string
	ModuleDirty bool
	State       string
	Error       string
	StartedAt   time.Time
	UpdatedAt   time.Time
	CompletedAt time.Time
}

type sidecarStateKey struct {
	Root        string
	FeatureHash string
	EnvHash     string
}

type sidecarServer struct {
	defaultRoot string
	mu          sync.Mutex
	states      map[sidecarStateKey]*sidecarSnapshot
	jobs        map[string]*sidecarRefreshJob
	nextJobID   uint64
	startedAt   time.Time
}

func runSidecar(defaultRoot, infoFile string) error {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return fmt.Errorf("listen sidecar: %w", err)
	}
	server := newSidecarServer(defaultRoot)
	return runSidecarWithListener(server, ln, infoFile)
}

func runSidecarWithListener(server *sidecarServer, ln net.Listener, infoFile string) error {
	defer ln.Close()

	if err := writeSidecarInfo(infoFile, ln.Addr().String(), os.Getpid(), server.startedAt, sidecarResponseProviderID, sidecarResponseProviderVersion, sidecarResponseSchemaVersion); err != nil {
		return err
	}

	for {
		conn, err := ln.Accept()
		if err != nil {
			if errors.Is(err, net.ErrClosed) {
				return nil
			}
			return err
		}
		done, err := handleSidecarConn(server, conn)
		conn.Close()
		if err != nil {
			fmt.Fprintf(os.Stderr, "aft-go-helper: sidecar conn: %v\n", err)
			continue
		}
		if done {
			return nil
		}
	}
}

func handleSidecarConn(server *sidecarServer, conn net.Conn) (bool, error) {
	var req sidecarRequest
	decoder := json.NewDecoder(conn)
	if err := decoder.Decode(&req); err != nil {
		resp := sidecarResponse{
			Method: sidecarMethodHello,
			Ok:     false,
			Error:  fmt.Sprintf("invalid request: %v", err),
		}
		if err := json.NewEncoder(conn).Encode(resp); err != nil {
			return false, err
		}
		return false, nil
	}

	method := strings.ToLower(strings.TrimSpace(req.Method))
	req.Method = method

	resp := server.handleRequest(req)
	if err := json.NewEncoder(conn).Encode(resp); err != nil {
		return false, err
	}

	return method == sidecarMethodShutdown, nil
}

func (s *sidecarServer) handleRequest(req sidecarRequest) sidecarResponse {
	switch req.Method {
	case sidecarMethodHello:
		return s.handleHello(req)
	case sidecarMethodStatus:
		return s.handleStatus(req)
	case sidecarMethodRefresh:
		return s.handleRefresh(req)
	case sidecarMethodJobStatus:
		return s.handleJobStatus(req)
	case sidecarMethodGetSnapshot:
		return s.handleGetSnapshot(req)
	case sidecarMethodInvalidate:
		return s.handleInvalidate(req)
	case sidecarMethodShutdown:
		return s.handleShutdown(req)
	default:
		return sidecarResponse{
			ID:     req.ID,
			Method: req.Method,
			Ok:     false,
			Error:  fmt.Sprintf("unknown method: %s", req.Method),
		}
	}
}

func (s *sidecarServer) handleHello(req sidecarRequest) sidecarResponse {
	return sidecarResponse{
		ID:     req.ID,
		Method: sidecarMethodHello,
		Ok:     true,
		Result: sidecarHelloResult{
			ProviderID:      sidecarResponseProviderID,
			ProviderVersion: sidecarResponseProviderVersion,
			SchemaVersion:   sidecarResponseSchemaVersion,
			Capabilities:    sidecarCapabilities,
			DefaultRoot:     s.defaultRoot,
		},
	}
}

func (s *sidecarServer) handleStatus(req sidecarRequest) sidecarResponse {
	key, err := s.makeStateKey(req.Root, req.Features, req.EnvHash, req.Env)
	if err != nil {
		return statusErrorResponse(req, sidecarMethodStatus, err)
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	state, ok := s.states[key]
	if !ok || state.Output == nil {
		currentJobID := stateJobID(state, true)
		pendingJobID := stateJobID(state, false)
		return sidecarResponse{
			ID:     req.ID,
			Method: sidecarMethodStatus,
			Ok:     true,
			Result: sidecarStatusResult{
				Root:                 key.Root,
				FeatureHash:          key.FeatureHash,
				EnvHash:              key.EnvHash,
				RequestedFingerprint: req.Fingerprint,
				HasSnapshot:          false,
				Stale:                true,
				CurrentJobID:         currentJobID,
				CurrentJobState:      s.jobStateLocked(currentJobID),
				PendingJobID:         pendingJobID,
				PendingJobState:      s.jobStateLocked(pendingJobID),
			},
		}
	}

	return sidecarResponse{
		ID:     req.ID,
		Method: sidecarMethodStatus,
		Ok:     true,
		Result: sidecarStatusResult{
			Root:                 key.Root,
			FeatureHash:          key.FeatureHash,
			EnvHash:              key.EnvHash,
			Fingerprint:          state.SourceFingerprint,
			RequestedFingerprint: req.Fingerprint,
			HasSnapshot:          true,
			Stale:                snapshotStateFor(state, req.Fingerprint) != sidecarSnapshotFresh,
			LastRefreshedAt:      formatTime(state.RefreshedAt),
			CurrentJobID:         stateJobID(state, true),
			CurrentJobState:      s.jobStateLocked(stateJobID(state, true)),
			PendingJobID:         stateJobID(state, false),
			PendingJobState:      s.jobStateLocked(stateJobID(state, false)),
		},
	}
}

func (s *sidecarServer) handleRefresh(req sidecarRequest) sidecarResponse {
	key, err := s.makeStateKey(req.Root, req.Features, req.EnvHash, req.Env)
	if err != nil {
		return statusErrorResponse(req, sidecarMethodRefresh, err)
	}

	features := req.Features.featureSet()
	s.mu.Lock()
	state := s.ensureStateLocked(key)
	if req.Fingerprint != "" {
		state.DesiredFingerprint = req.Fingerprint
	}
	state.DesiredModuleDirty = req.ModuleDirty
	if state.Output == nil || state.SourceFingerprint != req.Fingerprint || state.ModuleDirty != req.ModuleDirty {
		state.Stale = true
	}

	if snapshotStateFor(state, req.Fingerprint) == sidecarSnapshotFresh {
		job := s.completedJobForStateLocked(key, features, state, req.Fingerprint, req.ModuleDirty)
		result := s.refreshResultLocked(job, state)
		s.mu.Unlock()
		return sidecarResponse{
			ID:     req.ID,
			Method: sidecarMethodRefresh,
			Ok:     true,
			Result: result,
		}
	}

	if state.CurrentJobID != "" {
		if current := s.jobs[state.CurrentJobID]; current != nil && current.Fingerprint == req.Fingerprint {
			result := s.refreshResultLocked(current, state)
			s.mu.Unlock()
			return sidecarResponse{
				ID:     req.ID,
				Method: sidecarMethodRefresh,
				Ok:     true,
				Result: result,
			}
		}
		if state.PendingJobID != "" {
			if pending := s.jobs[state.PendingJobID]; pending != nil && pending.Fingerprint == req.Fingerprint {
				result := s.refreshResultLocked(pending, state)
				s.mu.Unlock()
				return sidecarResponse{
					ID:     req.ID,
					Method: sidecarMethodRefresh,
					Ok:     true,
					Result: result,
				}
			}
			s.dropJobLocked(state.PendingJobID)
			state.PendingJobID = ""
		}
		job := s.newJobLocked(key, features, req.Fingerprint, req.ModuleDirty, sidecarJobQueued)
		state.PendingJobID = job.ID
		result := s.refreshResultLocked(job, state)
		s.mu.Unlock()
		return sidecarResponse{
			ID:     req.ID,
			Method: sidecarMethodRefresh,
			Ok:     true,
			Result: result,
		}
	}

	job := s.newJobLocked(key, features, req.Fingerprint, req.ModuleDirty, sidecarJobRunning)
	state.CurrentJobID = job.ID
	s.startJobLocked(job)
	result := s.refreshResultLocked(job, state)
	s.mu.Unlock()
	return sidecarResponse{
		ID:     req.ID,
		Method: sidecarMethodRefresh,
		Ok:     true,
		Result: result,
	}
}

func (s *sidecarServer) handleJobStatus(req sidecarRequest) sidecarResponse {
	if strings.TrimSpace(req.JobID) == "" {
		return sidecarResponse{
			ID:     req.ID,
			Method: sidecarMethodJobStatus,
			Ok:     false,
			Error:  "missing job_id",
		}
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	job, ok := s.jobs[req.JobID]
	if !ok {
		return sidecarResponse{
			ID:     req.ID,
			Method: sidecarMethodJobStatus,
			Ok:     false,
			Error:  "job not found",
		}
	}
	state := s.states[job.Key]
	lastRefreshedAt := ""
	snapshotState := sidecarSnapshotMissing
	if state != nil {
		lastRefreshedAt = formatTime(state.RefreshedAt)
		snapshotState = snapshotStateFor(state, job.Fingerprint)
	}
	return sidecarResponse{
		ID:     req.ID,
		Method: sidecarMethodJobStatus,
		Ok:     true,
		Result: sidecarJobStatusResult{
			JobID:           job.ID,
			State:           job.State,
			Root:            job.Key.Root,
			FeatureHash:     job.Key.FeatureHash,
			EnvHash:         job.Key.EnvHash,
			Fingerprint:     job.Fingerprint,
			SnapshotState:   snapshotState,
			Error:           job.Error,
			StartedAt:       formatTime(job.StartedAt),
			UpdatedAt:       formatTime(job.UpdatedAt),
			CompletedAt:     formatTime(job.CompletedAt),
			LastRefreshedAt: lastRefreshedAt,
		},
	}
}

func (s *sidecarServer) handleGetSnapshot(req sidecarRequest) sidecarResponse {
	key, err := s.makeStateKey(req.Root, req.Features, req.EnvHash, req.Env)
	if err != nil {
		return statusErrorResponse(req, sidecarMethodGetSnapshot, err)
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	state, ok := s.states[key]
	if !ok || state.Output == nil {
		return sidecarResponse{
			ID:     req.ID,
			Method: sidecarMethodGetSnapshot,
			Ok:     false,
			Error:  "snapshot not found",
		}
	}

	return sidecarResponse{
		ID:     req.ID,
		Method: sidecarMethodGetSnapshot,
		Ok:     true,
		Result: sidecarGetSnapshotResult{
			Snapshot:        *state.Output,
			Stale:           snapshotStateFor(state, req.Fingerprint) != sidecarSnapshotFresh,
			Root:            key.Root,
			FeatureHash:     key.FeatureHash,
			EnvHash:         key.EnvHash,
			Fingerprint:     state.SourceFingerprint,
			LastRefreshedAt: formatTime(state.RefreshedAt),
			ModuleDirty:     state.ModuleDirty,
		},
	}
}

func (s *sidecarServer) handleInvalidate(req sidecarRequest) sidecarResponse {
	root, err := s.resolveRequestRoot(req.Root)
	if err != nil {
		return statusErrorResponse(req, sidecarMethodInvalidate, err)
	}

	features := req.Features.featureSet()
	envHash := snapshotEnvHash(req.EnvHash, req.Env)
	target := sidecarStateKey{
		Root:        root,
		FeatureHash: featureSetHash(features),
		EnvHash:     envHash,
	}
	markAll := !req.Features.hasExplicitFeatureRequest() && req.EnvHash == "" && len(req.Env) == 0

	s.mu.Lock()
	defer s.mu.Unlock()

	marked := 0
	if markAll {
		for key, state := range s.states {
			if key.Root != root {
				continue
			}
			state.Stale = true
			if req.Fingerprint != "" {
				state.DesiredFingerprint = req.Fingerprint
			}
			state.DesiredModuleDirty = req.ModuleDirty
			marked++
		}
	} else if state, ok := s.states[target]; ok {
		state.Stale = true
		if req.Fingerprint != "" {
			state.DesiredFingerprint = req.Fingerprint
		}
		state.DesiredModuleDirty = req.ModuleDirty
		marked = 1
	}

	return sidecarResponse{
		ID:     req.ID,
		Method: sidecarMethodInvalidate,
		Ok:     true,
		Result: sidecarInvalidateResult{
			Marked:            marked,
			ModuleDirty:       req.ModuleDirty,
			SourceFingerprint: req.Fingerprint,
		},
	}
}

func (s *sidecarServer) handleShutdown(req sidecarRequest) sidecarResponse {
	return sidecarResponse{
		ID:     req.ID,
		Method: sidecarMethodShutdown,
		Ok:     true,
		Result: sidecarShutdownResult{
			Status: "shutting_down",
		},
	}
}

func (s *sidecarServer) makeStateKey(root string, features *rpcFeatureFlags, envHash string, env map[string]string) (sidecarStateKey, error) {
	resolvedRoot, err := s.resolveRequestRoot(root)
	if err != nil {
		return sidecarStateKey{}, err
	}
	fs := features.featureSet()
	return sidecarStateKey{
		Root:        resolvedRoot,
		FeatureHash: featureSetHash(fs),
		EnvHash:     snapshotEnvHash(envHash, env),
	}, nil
}

func (s *sidecarServer) resolveRequestRoot(rawRoot string) (string, error) {
	target := rawRoot
	if target == "" {
		target = s.defaultRoot
	}
	if target == "" {
		return "", fmt.Errorf("missing root")
	}
	return filepath.Abs(target)
}

func newSidecarServer(defaultRoot string) *sidecarServer {
	return &sidecarServer{
		defaultRoot: defaultRoot,
		states:      make(map[sidecarStateKey]*sidecarSnapshot),
		jobs:        make(map[string]*sidecarRefreshJob),
		startedAt:   time.Now(),
	}
}

func (s *sidecarServer) ensureStateLocked(key sidecarStateKey) *sidecarSnapshot {
	if state, ok := s.states[key]; ok {
		return state
	}
	state := &sidecarSnapshot{
		FeatureHash: key.FeatureHash,
		EnvHash:     key.EnvHash,
		Root:        key.Root,
		Stale:       true,
	}
	s.states[key] = state
	return state
}

func (s *sidecarServer) newJobLocked(
	key sidecarStateKey,
	features featureSet,
	fingerprint string,
	moduleDirty bool,
	state string,
) *sidecarRefreshJob {
	s.nextJobID++
	now := time.Now()
	job := &sidecarRefreshJob{
		ID:          fmt.Sprintf("job-%d", s.nextJobID),
		Key:         key,
		Features:    features,
		Fingerprint: fingerprint,
		ModuleDirty: moduleDirty,
		State:       state,
		UpdatedAt:   now,
	}
	if state == sidecarJobRunning || state == sidecarJobDone || state == sidecarJobFailed {
		job.StartedAt = now
	}
	if state == sidecarJobDone || state == sidecarJobFailed {
		job.CompletedAt = now
	}
	s.jobs[job.ID] = job
	return job
}

func (s *sidecarServer) completedJobForStateLocked(
	key sidecarStateKey,
	features featureSet,
	state *sidecarSnapshot,
	fingerprint string,
	moduleDirty bool,
) *sidecarRefreshJob {
	if state.LastJobID != "" {
		if job := s.jobs[state.LastJobID]; job != nil && job.State == sidecarJobDone {
			return job
		}
	}
	job := s.newJobLocked(key, features, fingerprint, moduleDirty, sidecarJobDone)
	state.LastJobID = job.ID
	return job
}

func (s *sidecarServer) refreshResultLocked(job *sidecarRefreshJob, state *sidecarSnapshot) sidecarRefreshResult {
	lastRefreshedAt := ""
	if state != nil {
		lastRefreshedAt = formatTime(state.RefreshedAt)
	}
	return sidecarRefreshResult{
		JobID:           job.ID,
		State:           job.State,
		SnapshotState:   snapshotStateFor(state, job.Fingerprint),
		Root:            job.Key.Root,
		FeatureHash:     job.Key.FeatureHash,
		EnvHash:         job.Key.EnvHash,
		Fingerprint:     job.Fingerprint,
		LastRefreshedAt: lastRefreshedAt,
		ModuleDirty:     job.ModuleDirty,
	}
}

func (s *sidecarServer) jobStateLocked(jobID string) string {
	if jobID == "" {
		return ""
	}
	if job := s.jobs[jobID]; job != nil {
		return job.State
	}
	return ""
}

func (s *sidecarServer) startJobLocked(job *sidecarRefreshJob) {
	go s.runRefreshJob(job.ID)
}

func (s *sidecarServer) runRefreshJob(jobID string) {
	s.mu.Lock()
	job, ok := s.jobs[jobID]
	if !ok {
		s.mu.Unlock()
		return
	}
	job.State = sidecarJobRunning
	if job.StartedAt.IsZero() {
		job.StartedAt = time.Now()
	}
	job.UpdatedAt = time.Now()
	features := job.Features
	key := job.Key
	fingerprint := job.Fingerprint
	moduleDirty := job.ModuleDirty
	s.mu.Unlock()

	out, err := sidecarAnalyze(
		key.Root,
		features.Dispatches,
		features.Implements,
		features.Writes,
		features.CallContext,
		features.ReturnAnalysis,
	)
	if err != nil {
		fmt.Fprintf(os.Stderr, "aft-go-helper: refresh root=%s: %v\n", key.Root, err)
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	job, ok = s.jobs[jobID]
	if !ok {
		return
	}
	now := time.Now()
	job.UpdatedAt = now
	job.CompletedAt = now

	state := s.ensureStateLocked(key)
	if err != nil {
		job.State = sidecarJobFailed
		job.Error = err.Error()
	} else {
		job.State = sidecarJobDone
		job.Error = ""
		state.Output = out
		state.SourceFingerprint = fingerprint
		state.ModuleDirty = moduleDirty
		state.RefreshedAt = now
		state.LastJobID = job.ID
		if state.DesiredFingerprint == "" {
			state.DesiredFingerprint = fingerprint
		}
		state.Stale = state.DesiredFingerprint != fingerprint || state.DesiredModuleDirty != moduleDirty
	}

	if state.CurrentJobID == job.ID {
		state.CurrentJobID = ""
	}
	if state.PendingJobID != "" {
		nextID := state.PendingJobID
		state.PendingJobID = ""
		state.CurrentJobID = nextID
		if next := s.jobs[nextID]; next != nil {
			next.State = sidecarJobRunning
			next.StartedAt = time.Now()
			next.UpdatedAt = next.StartedAt
			s.startJobLocked(next)
		}
	}
}

func (s *sidecarServer) dropJobLocked(jobID string) {
	if jobID == "" {
		return
	}
	delete(s.jobs, jobID)
}

func snapshotStateFor(state *sidecarSnapshot, requestedFingerprint string) string {
	if state == nil || state.Output == nil {
		return sidecarSnapshotMissing
	}
	stale := state.Stale
	if requestedFingerprint != "" && state.SourceFingerprint != "" && state.SourceFingerprint != requestedFingerprint {
		stale = true
	}
	if stale {
		return sidecarSnapshotStale
	}
	return sidecarSnapshotFresh
}

func stateJobID(state *sidecarSnapshot, current bool) string {
	if state == nil {
		return ""
	}
	if current {
		return state.CurrentJobID
	}
	return state.PendingJobID
}

func formatTime(value time.Time) string {
	if value.IsZero() {
		return ""
	}
	return value.Format(time.RFC3339)
}

func writeSidecarInfo(path, addr string, pid int, startedAt time.Time, providerID, providerVersion string, schemaVersion int) error {
	if strings.TrimSpace(path) == "" {
		return nil
	}
	if parent := filepath.Dir(path); parent != "." {
		if err := os.MkdirAll(parent, 0o700); err != nil {
			return fmt.Errorf("make sidecar info dir: %w", err)
		}
	}

	raw, err := json.Marshal(sidecarInfo{
		ProviderID:    providerID,
		ProviderVer:   providerVersion,
		SchemaVersion: schemaVersion,
		Addr:          addr,
		PID:           pid,
		StartedAt:     startedAt.Format(time.RFC3339Nano),
	})
	if err != nil {
		return fmt.Errorf("marshal sidecar info: %w", err)
	}

	if err := os.WriteFile(path, append(raw, '\n'), 0o600); err != nil {
		return fmt.Errorf("write sidecar info: %w", err)
	}
	return nil
}

func featureSetHash(fs featureSet) string {
	raw, err := json.Marshal(fs)
	if err != nil {
		return fmt.Sprintf("feature-hash-error:%s", err)
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:])
}

func snapshotEnvHash(explicitHash string, explicit map[string]string) string {
	if explicitHash != "" {
		return explicitHash
	}

	env := map[string]string{}
	for _, key := range sidecarRelevantEnvKeys {
		if value, ok := os.LookupEnv(key); ok {
			env[key] = value
		}
	}
	for key, value := range explicit {
		env[key] = value
	}
	if len(env) == 0 {
		return ""
	}

	keys := make([]string, 0, len(env))
	for key := range env {
		keys = append(keys, key)
	}
	sort.Strings(keys)

	var builder strings.Builder
	for _, key := range keys {
		builder.WriteString(key)
		builder.WriteByte('=')
		builder.WriteString(env[key])
		builder.WriteByte(';')
	}
	sum := sha256.Sum256([]byte(builder.String()))
	return hex.EncodeToString(sum[:])
}

func statusErrorResponse(req sidecarRequest, method string, err error) sidecarResponse {
	return sidecarResponse{
		ID:     req.ID,
		Method: method,
		Ok:     false,
		Error:  err.Error(),
	}
}
