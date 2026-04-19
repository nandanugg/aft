//! Bridge to the optional `aft-go-helper` binary.
//!
//! AFT's tree-sitter parser handles syntax across all supported languages,
//! but Go programs need type information to resolve interface dispatch and
//! method calls correctly. The companion Go helper (`go-helper/`) uses the
//! standard toolchain's SSA + class-hierarchy analysis to produce a list of
//! resolved call edges, which AFT merges into its reverse index for Go
//! files only.
//!
//! This module owns the deserialization side of the contract. The schema
//! mirrors `go-helper/main.go` exactly — keep them in sync. A `version`
//! field is included so future schema changes can be detected and old
//! cached outputs ignored without crashing.
//!
//! When the helper is unavailable (no `go` on PATH, helper binary missing,
//! helper exits non-zero), the rest of AFT must continue to work — the
//! integration is strictly additive.
//
// Schema version. Bump when the on-disk JSON format changes in a way old
// readers cannot tolerate. Cached outputs with a different version are
// discarded rather than parsed.
pub const HELPER_SCHEMA_VERSION: u32 = 1;

/// Environment variable that overrides helper-binary discovery. Useful
/// for development (point at `go-helper/go-helper` from the repo) and
/// for environments where the helper isn't on PATH.
pub const HELPER_PATH_ENV: &str = "AFT_GO_HELPER_PATH";

/// Default helper binary name on PATH.
pub const HELPER_BIN_NAME: &str = "aft-go-helper";

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Top-level document returned by `aft-go-helper -root <dir>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperOutput {
    /// Schema version (see `HELPER_SCHEMA_VERSION`).
    pub version: u32,
    /// Absolute project root the helper was invoked against.
    pub root: String,
    /// Resolved call edges. Empty if the project has no in-project edges
    /// (e.g. a single file with only stdlib calls).
    #[serde(default)]
    pub edges: Vec<HelperEdge>,
    /// Packages skipped due to load errors. Reported for diagnostics; AFT
    /// falls back to tree-sitter for these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

/// A single resolved call edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperEdge {
    /// Where the call site is (file + line + enclosing symbol).
    pub caller: HelperCaller,
    /// What the call resolves to.
    pub callee: HelperCallee,
    /// Classification of the edge. See `EdgeKind`.
    pub kind: EdgeKind,
    /// For `dispatches` edges: a string literal co-located at the same
    /// call site (e.g. a task name passed alongside the handler).
    /// Present only when exactly one string literal arg ≤128 chars
    /// appears in the call. Absent for all other edge kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearby_string: Option<String>,
    /// For `dispatches` edges: the FQN of the function whose call received
    /// the function-value argument. Format follows Go's ssa.Function.String():
    ///   - Free function: `"pkg/path.FuncName"`
    ///   - Pointer receiver: `"pkg/path.(*TypeName).Method"`
    ///   - Interface invoke: `"(pkg/path.InterfaceName).Method"`
    /// Absent when the callee cannot be resolved, or for non-dispatches kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_via: Option<String>,
}

/// Caller-side position for an edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperCaller {
    /// File path relative to the helper's `root`.
    pub file: String,
    /// 1-based line number of the call expression.
    pub line: u32,
    /// Enclosing top-level function/method name. Closures collapse to
    /// their containing named function so AFT can find the symbol via
    /// tree-sitter.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub symbol: String,
}

/// Callee-side description of a resolved target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperCallee {
    /// File path relative to the helper's `root`.
    pub file: String,
    /// 1-based line number of the callee declaration.
    /// Present for `implements` edges (Tier 1.4); 0 / absent for call edges
    /// where the callee position is resolved by tree-sitter instead.
    #[serde(default, skip_serializing_if = "crate::go_helper::is_zero_u32")]
    pub line: u32,
    /// Function or method name (without receiver).
    pub symbol: String,
    /// Receiver type as Go renders it, e.g. `"*example.com/pkg.T"`.
    /// Empty for non-methods.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub receiver: String,
    /// Full package import path, e.g. `"example.com/pkg"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pkg: String,
}

/// What sort of call this edge represents. Drives AFT's display of the
/// caller (e.g. "interface" sites get a marker so users know multiple
/// concrete callees are possible).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    /// Package-level function call: `pkg.Foo()` or bare `Foo()`.
    Static,
    /// Method on a concrete type: `(&T{}).Method()`.
    Concrete,
    /// Interface dispatch resolved by class-hierarchy analysis. One
    /// `HelperEdge` is emitted per concrete implementation.
    Interface,
    /// A function value is passed as an argument to a call site.
    /// E.g. `scheduler.Register("task", myHandler)` — `myHandler` is
    /// dispatched to `Register` for later invocation. See `nearby_string`
    /// on the edge for the accompanying string label, if any.
    Dispatches,
    /// The callee is launched as a goroutine: `go callee(args)`.
    Goroutine,
    /// The callee is deferred: `defer callee(args)`.
    Defer,
    /// Concrete method M_C satisfies interface method M_I (Tier 1.4).
    /// caller = interface declaration site (file + line + interface type name).
    /// callee = concrete method (file + method name + receiver type + pkg).
    /// This is an existence edge, not a call edge — it answers
    /// "which concrete types implement interface X?"
    Implements,
    /// Cross-package write to a package-level variable (`*ssa.Store` → `*ssa.Global`).
    /// `callee.symbol` is the variable name; same-package writes are filtered at source.
    Writes,
}

impl EdgeKind {
    /// Return the wire-format name of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Concrete => "concrete",
            Self::Interface => "interface",
            Self::Dispatches => "dispatches",
            Self::Goroutine => "goroutine",
            Self::Defer => "defer",
            Self::Implements => "implements",
            Self::Writes => "writes",
        }
    }

    /// Parse from the wire-format string. Returns `None` for unknown kinds.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "static" => Some(Self::Static),
            "concrete" => Some(Self::Concrete),
            "interface" => Some(Self::Interface),
            "dispatches" => Some(Self::Dispatches),
            "goroutine" => Some(Self::Goroutine),
            "defer" => Some(Self::Defer),
            "implements" => Some(Self::Implements),
            "writes" => Some(Self::Writes),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Discovery + invocation
// ---------------------------------------------------------------------------

/// Reasons the helper could not produce edges. AFT treats every variant
/// as "no VTA data available" and falls back to tree-sitter — but the
/// distinct cases let us log usefully and skip retrying obviously
/// hopeless cases.
#[derive(Debug)]
pub enum HelperError {
    /// `go` is not on PATH. The helper might be installed but it can't
    /// load packages without the toolchain, so we don't bother running it.
    GoNotInstalled,
    /// We couldn't find the helper binary (no `AFT_GO_HELPER_PATH`,
    /// nothing named `aft-go-helper` on PATH).
    HelperNotFound,
    /// Project root has no `go.mod` — almost certainly not a Go project,
    /// so don't waste time loading packages.
    NotAGoProject,
    /// Helper exited non-zero. Stderr is captured for diagnostics.
    HelperFailed { status: Option<i32>, stderr: String },
    /// Helper produced output we can't parse (likely a schema mismatch).
    ParseFailed(String),
    /// IO failure spawning the process or reading its output.
    Io(String),
}

impl std::fmt::Display for HelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GoNotInstalled => write!(f, "go toolchain not on PATH"),
            Self::HelperNotFound => write!(
                f,
                "aft-go-helper binary not found (set {HELPER_PATH_ENV} or place on PATH)"
            ),
            Self::NotAGoProject => write!(f, "project has no go.mod at the root"),
            Self::HelperFailed { status, stderr } => {
                let trimmed = stderr.trim();
                match status {
                    Some(code) => write!(f, "helper exited {code}: {trimmed}"),
                    None => write!(f, "helper terminated by signal: {trimmed}"),
                }
            }
            Self::ParseFailed(msg) => write!(f, "helper output parse error: {msg}"),
            Self::Io(msg) => write!(f, "helper IO error: {msg}"),
        }
    }
}

impl std::error::Error for HelperError {}

/// Serde skip helper — omit zero line numbers from serialized output.
#[doc(hidden)]
pub fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}


/// Probe whether `go` is on PATH. Cheap (`go env GOROOT` is fast and
/// doesn't touch any modules).
pub fn is_go_available() -> bool {
    Command::new("go")
        .arg("env")
        .arg("GOROOT")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Baked-in path to the helper compiled by build.rs. `None` if build.rs
/// didn't run `go build` (e.g. Go not on PATH at build time, or in a
/// `cargo install` context where the build machine path doesn't apply).
const BAKED_HELPER_PATH: Option<&str> = option_env!("AFT_GO_HELPER_BAKED_PATH");

/// Locate the helper binary. Search order:
///   1. `$AFT_GO_HELPER_PATH` (explicit runtime override)
///   2. Baked-in path from build.rs (dev builds; file must still exist)
///   3. `aft-go-helper` on PATH
///
/// Returns `None` if none of the above yields an executable file.
pub fn find_helper_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(HELPER_PATH_ENV) {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
        // Bad override — fall through rather than fail outright.
    }
    // Dev builds: build.rs compiled the helper into OUT_DIR and baked the path.
    // The file may not exist after `cargo clean` — check before returning.
    if let Some(baked) = BAKED_HELPER_PATH {
        let path = PathBuf::from(baked);
        if path.is_file() {
            return Some(path);
        }
    }
    which_on_path(HELPER_BIN_NAME)
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Returns true if `root/go.mod` exists. Used as a quick filter so we
/// don't run the helper on non-Go projects.
pub fn looks_like_go_project(root: &Path) -> bool {
    root.join("go.mod").is_file()
}

/// Invoke the helper synchronously and parse its JSON output. Caller is
/// responsible for putting this on a background thread if it shouldn't
/// block the request loop.
///
/// `timeout` bounds total wall-clock — packages.Load can be slow on
/// first invocation while the module graph is fetched.
pub fn run_helper(
    helper: &Path,
    root: &Path,
    timeout: Duration,
) -> Result<HelperOutput, HelperError> {
    use std::io::Read;

    let mut child = Command::new(helper)
        .arg("-root")
        .arg(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| HelperError::Io(format!("spawn: {e}")))?;

    // Drain stdout and stderr concurrently. The helper emits several MB of
    // JSON on real projects; if we don't read the pipes while the child
    // runs, the pipe buffer (typically 64KB) fills and the child blocks
    // on write forever. Previously this deadlock looked like a timeout
    // because the wait loop never saw the child exit.
    let stdout_handle = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });

    // Poll-based timeout. Kill the child if it runs past the deadline.
    // The reader threads will see EOF and exit on their own.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(HelperError::Io(format!(
                        "helper exceeded timeout of {}s",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(HelperError::Io(format!("wait: {e}"))),
        }
    }

    let stdout = stdout_handle
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr = stderr_handle
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();

    let status = child
        .wait()
        .map_err(|e| HelperError::Io(format!("final wait: {e}")))?;

    if !status.success() {
        return Err(HelperError::HelperFailed {
            status: status.code(),
            stderr,
        });
    }

    let parsed: HelperOutput =
        serde_json::from_str(&stdout).map_err(|e| HelperError::ParseFailed(e.to_string()))?;
    if parsed.version != HELPER_SCHEMA_VERSION {
        return Err(HelperError::ParseFailed(format!(
            "schema version mismatch: helper produced {}, expected {}",
            parsed.version, HELPER_SCHEMA_VERSION
        )));
    }
    Ok(parsed)
}

/// Run all the discovery checks plus the helper itself in one call.
/// Returns `Err(HelperError)` for every failure mode so callers can log
/// once and silently continue. This is the entry point intended for
/// `configure`-time use.
pub fn resolve_for_root(root: &Path, timeout: Duration) -> Result<HelperOutput, HelperError> {
    if !looks_like_go_project(root) {
        return Err(HelperError::NotAGoProject);
    }
    if !is_go_available() {
        return Err(HelperError::GoNotInstalled);
    }
    let helper = find_helper_binary().ok_or(HelperError::HelperNotFound)?;
    run_helper(&helper, root, timeout)
}

// ---------------------------------------------------------------------------
// Cache I/O
// ---------------------------------------------------------------------------

/// Path under which a project's helper output is cached.
pub fn cache_file_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("go-helper-edges.json")
}

/// Read a cached helper output. Returns `None` if the file is missing,
/// unreadable, has the wrong schema version, or is stale (root differs).
pub fn read_cached(cache_dir: &Path, expected_root: &Path) -> Option<HelperOutput> {
    let path = cache_file_path(cache_dir);
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: HelperOutput = serde_json::from_str(&raw).ok()?;
    if parsed.version != HELPER_SCHEMA_VERSION {
        return None;
    }
    let want = expected_root.to_string_lossy();
    if parsed.root != want {
        // Cache was written for a different project root.
        return None;
    }
    Some(parsed)
}

/// Persist a helper output to the cache. Best-effort — failures are
/// returned to the caller but typically logged-and-ignored.
pub fn write_cached(cache_dir: &Path, output: &HelperOutput) -> Result<(), HelperError> {
    std::fs::create_dir_all(cache_dir).map_err(|e| HelperError::Io(format!("mkdir cache: {e}")))?;
    let path = cache_file_path(cache_dir);
    let body = serde_json::to_string(output).map_err(|e| HelperError::Io(e.to_string()))?;
    std::fs::write(&path, body).map_err(|e| HelperError::Io(format!("write cache: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Sample matches the actual helper output for the fixture used during
    // development — keeps the deserializer locked to the wire format.
    const SAMPLE_OUTPUT: &str = r#"{
      "version": 1,
      "root": "/tmp/go-fixture",
      "edges": [
        {
          "caller": {"file": "go_resolution.go", "line": 42, "symbol": "interfaceCaller"},
          "callee": {"file": "go_resolution.go", "symbol": "Do", "receiver": "*example.com/fixture.doerA", "pkg": "example.com/fixture"},
          "kind": "interface"
        },
        {
          "caller": {"file": "go_resolution.go", "line": 24, "symbol": "concreteMethodCaller"},
          "callee": {"file": "go_resolution.go", "symbol": "concreteMethod", "receiver": "*example.com/fixture.concreteSvc", "pkg": "example.com/fixture"},
          "kind": "concrete"
        },
        {
          "caller": {"file": "go_resolution.go", "line": 10, "symbol": "barePkgCaller"},
          "callee": {"file": "go_resolution.go", "symbol": "barePkgTarget", "pkg": "example.com/fixture"},
          "kind": "static"
        }
      ]
    }"#;

    #[test]
    fn deserializes_sample_output() {
        let out: HelperOutput = serde_json::from_str(SAMPLE_OUTPUT).unwrap();
        assert_eq!(out.version, HELPER_SCHEMA_VERSION);
        assert_eq!(out.root, "/tmp/go-fixture");
        assert_eq!(out.edges.len(), 3);
        assert!(out.skipped.is_empty());

        let iface = &out.edges[0];
        assert_eq!(iface.kind, EdgeKind::Interface);
        assert_eq!(iface.caller.symbol, "interfaceCaller");
        assert_eq!(iface.callee.symbol, "Do");
        assert_eq!(iface.callee.receiver, "*example.com/fixture.doerA");

        let stat = &out.edges[2];
        assert_eq!(stat.kind, EdgeKind::Static);
        assert_eq!(stat.callee.receiver, "");
    }

    #[test]
    fn missing_optional_fields_default_to_empty() {
        let json = r#"{
            "version": 1,
            "root": "/x",
            "edges": [
                {
                    "caller": {"file": "a.go", "line": 1},
                    "callee": {"file": "b.go", "symbol": "F"},
                    "kind": "static"
                }
            ]
        }"#;
        let out: HelperOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.edges[0].caller.symbol, "");
        assert_eq!(out.edges[0].callee.pkg, "");
    }

    #[test]
    fn round_trips_through_serde() {
        let out: HelperOutput = serde_json::from_str(SAMPLE_OUTPUT).unwrap();
        let s = serde_json::to_string(&out).unwrap();
        let again: HelperOutput = serde_json::from_str(&s).unwrap();
        assert_eq!(out, again);
    }

    #[test]
    fn cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = dir.path().join("cache");
        let project_root = dir.path().join("project");
        let out = HelperOutput {
            version: HELPER_SCHEMA_VERSION,
            root: project_root.to_string_lossy().into_owned(),
            edges: vec![HelperEdge {
                caller: HelperCaller {
                    file: "a.go".into(),
                    line: 10,
                    symbol: "f".into(),
                },
                callee: HelperCallee {
                    file: "b.go".into(),
                    line: 0,
                    symbol: "g".into(),
                    receiver: String::new(),
                    pkg: String::new(),
                },
                kind: EdgeKind::Static,
                nearby_string: None,
                dispatched_via: None,
            }],
            skipped: vec![],
        };
        write_cached(&cache_root, &out).unwrap();
        let back = read_cached(&cache_root, &project_root).unwrap();
        assert_eq!(out, back);
    }

    #[test]
    fn cache_rejects_wrong_root() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = dir.path().join("cache");
        let project_root = dir.path().join("project");
        let other_root = dir.path().join("other");
        let out = HelperOutput {
            version: HELPER_SCHEMA_VERSION,
            root: project_root.to_string_lossy().into_owned(),
            edges: vec![],
            skipped: vec![],
        };
        write_cached(&cache_root, &out).unwrap();
        assert!(read_cached(&cache_root, &other_root).is_none());
    }

    #[test]
    fn cache_rejects_wrong_version() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = dir.path().join("cache");
        let project_root = dir.path().join("project");
        std::fs::create_dir_all(&cache_root).unwrap();
        std::fs::write(
            cache_file_path(&cache_root),
            format!(
                r#"{{"version": {}, "root": "{}", "edges": []}}"#,
                HELPER_SCHEMA_VERSION + 99,
                project_root.to_string_lossy()
            ),
        )
        .unwrap();
        assert!(read_cached(&cache_root, &project_root).is_none());
    }

    #[test]
    fn looks_like_go_project_requires_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!looks_like_go_project(dir.path()));
        std::fs::write(dir.path().join("go.mod"), "module x\ngo 1.22\n").unwrap();
        assert!(looks_like_go_project(dir.path()));
    }

    #[test]
    fn find_helper_binary_honors_env_override() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("aft-go-helper-stub");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();

        // Save & restore env to avoid polluting other tests in the same process.
        let prev = std::env::var_os(HELPER_PATH_ENV);
        // SAFETY: tests run single-threaded by default with #[test]; this is a
        // best-effort scoped override.
        unsafe {
            std::env::set_var(HELPER_PATH_ENV, &bin);
        }
        let found = find_helper_binary();
        unsafe {
            match prev {
                Some(v) => std::env::set_var(HELPER_PATH_ENV, v),
                None => std::env::remove_var(HELPER_PATH_ENV),
            }
        }
        assert_eq!(found.as_deref(), Some(bin.as_path()));
    }

    #[test]
    fn unknown_edge_kind_is_rejected() {
        let json = r#"{
            "version": 1,
            "root": "/x",
            "edges": [
                {
                    "caller": {"file": "a.go", "line": 1, "symbol": "f"},
                    "callee": {"file": "b.go", "symbol": "g"},
                    "kind": "telepathy"
                }
            ]
        }"#;
        let err = serde_json::from_str::<HelperOutput>(json).unwrap_err();
        assert!(err.to_string().contains("telepathy") || err.to_string().contains("variant"));
    }

    // -----------------------------------------------------------------------
    // Tier 1.1/1.2/1.3: new kinds and nearby_string
    // -----------------------------------------------------------------------

    #[test]
    fn new_kinds_parse_correctly() {
        let json = r#"{
            "version": 1,
            "root": "/x",
            "edges": [
                {
                    "caller": {"file": "a.go", "line": 10, "symbol": "Register"},
                    "callee": {"file": "b.go", "symbol": "HandleTask"},
                    "kind": "dispatches",
                    "nearby_string": "my-task"
                },
                {
                    "caller": {"file": "a.go", "line": 20, "symbol": "LaunchWorker"},
                    "callee": {"file": "b.go", "symbol": "WorkerLoop"},
                    "kind": "goroutine"
                },
                {
                    "caller": {"file": "a.go", "line": 30, "symbol": "CleanUp"},
                    "callee": {"file": "b.go", "symbol": "Close"},
                    "kind": "defer"
                }
            ]
        }"#;
        let out: HelperOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.edges.len(), 3);

        let dispatches = &out.edges[0];
        assert_eq!(dispatches.kind, EdgeKind::Dispatches);
        assert_eq!(dispatches.nearby_string, Some("my-task".to_string()));

        let goroutine = &out.edges[1];
        assert_eq!(goroutine.kind, EdgeKind::Goroutine);
        assert_eq!(goroutine.nearby_string, None);

        let defer_edge = &out.edges[2];
        assert_eq!(defer_edge.kind, EdgeKind::Defer);
        assert_eq!(defer_edge.nearby_string, None);
    }

    #[test]
    fn nearby_string_absent_means_none() {
        // An edge without nearby_string should deserialize to None
        let json = r#"{
            "version": 1,
            "root": "/x",
            "edges": [
                {
                    "caller": {"file": "a.go", "line": 1, "symbol": "f"},
                    "callee": {"file": "b.go", "symbol": "g"},
                    "kind": "dispatches"
                }
            ]
        }"#;
        let out: HelperOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.edges[0].nearby_string, None);
    }

    #[test]
    fn nearby_string_skipped_in_serialization_when_none() {
        let edge = HelperEdge {
            caller: HelperCaller {
                file: "a.go".into(),
                line: 1,
                symbol: "f".into(),
            },
            callee: HelperCallee {
                file: "b.go".into(),
                line: 0,
                symbol: "g".into(),
                receiver: String::new(),
                pkg: String::new(),
            },
            kind: EdgeKind::Dispatches,
            nearby_string: None,
            dispatched_via: None,
        };
        let s = serde_json::to_string(&edge).unwrap();
        assert!(!s.contains("nearby_string"), "nearby_string=None should be omitted: {s}");
        assert!(!s.contains("dispatched_via"), "dispatched_via=None should be omitted: {s}");
    }

    #[test]
    fn nearby_string_included_in_serialization_when_some() {
        let edge = HelperEdge {
            caller: HelperCaller {
                file: "a.go".into(),
                line: 1,
                symbol: "f".into(),
            },
            callee: HelperCallee {
                file: "b.go".into(),
                line: 0,
                symbol: "g".into(),
                receiver: String::new(),
                pkg: String::new(),
            },
            kind: EdgeKind::Dispatches,
            nearby_string: Some("send-email".to_string()),
            dispatched_via: None,
        };
        let s = serde_json::to_string(&edge).unwrap();
        assert!(s.contains("nearby_string"), "nearby_string=Some should be present: {s}");
        assert!(s.contains("send-email"));
    }

    #[test]
    fn old_v1_output_round_trips_with_new_fields() {
        // An old-format v1 output (no nearby_string, no new kinds) should
        // still parse without errors — backward compat via #[serde(default)].
        let out: HelperOutput = serde_json::from_str(SAMPLE_OUTPUT).unwrap();
        let serialized = serde_json::to_string(&out).unwrap();
        let again: HelperOutput = serde_json::from_str(&serialized).unwrap();
        assert_eq!(out, again);
        // All edges should have nearby_string=None (old format)
        for e in &out.edges {
            assert_eq!(e.nearby_string, None, "old-format edges should have no nearby_string");
        }
    }

    #[test]
    fn edge_kind_as_str_round_trips() {
        let kinds = [
            EdgeKind::Static,
            EdgeKind::Concrete,
            EdgeKind::Interface,
            EdgeKind::Dispatches,
            EdgeKind::Goroutine,
            EdgeKind::Defer,
            EdgeKind::Writes,
        ];
        for kind in kinds {
            let s = kind.as_str();
            let back = EdgeKind::from_str(s).expect("from_str should parse as_str output");
            assert_eq!(kind, back, "round-trip failed for {s}");
        }
    }

    // -----------------------------------------------------------------------
    // Tier 1.5: writes kind
    // -----------------------------------------------------------------------

    #[test]
    fn writes_kind_parses_correctly() {
        let json = r#"{
            "version": 1,
            "root": "/x",
            "edges": [
                {
                    "caller": {"file": "server/asynq.go", "line": 47, "symbol": "startServer"},
                    "callee": {"file": "server/registry.go", "symbol": "handlerRegistry", "pkg": "server"},
                    "kind": "writes"
                },
                {
                    "caller": {"file": "server/registry.go", "line": 12, "symbol": "init"},
                    "callee": {"file": "server/registry.go", "symbol": "defaultRegistry", "pkg": "server"},
                    "kind": "writes"
                }
            ]
        }"#;
        let out: HelperOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.edges.len(), 2);

        let write_edge = &out.edges[0];
        assert_eq!(write_edge.kind, EdgeKind::Writes);
        assert_eq!(write_edge.caller.symbol, "startServer");
        assert_eq!(write_edge.callee.symbol, "handlerRegistry");
        assert_eq!(write_edge.nearby_string, None);

        let init_edge = &out.edges[1];
        assert_eq!(init_edge.kind, EdgeKind::Writes);
        assert_eq!(init_edge.caller.symbol, "init");
        assert_eq!(init_edge.callee.symbol, "defaultRegistry");
    }

    #[test]
    fn writes_kind_serializes_correctly() {
        let edge = HelperEdge {
            caller: HelperCaller {
                file: "a.go".into(),
                line: 10,
                symbol: "SetRegistry".into(),
            },
            callee: HelperCallee {
                file: "b.go".into(),
                line: 0,
                symbol: "globalRegistry".into(),
                receiver: String::new(),
                pkg: "example.com/b".into(),
            },
            kind: EdgeKind::Writes,
            nearby_string: None,
            dispatched_via: None,
        };
        let s = serde_json::to_string(&edge).unwrap();
        assert!(s.contains("\"writes\""), "kind should be 'writes': {s}");
        assert!(!s.contains("nearby_string"), "nearby_string=None should be omitted: {s}");
    }

    // -----------------------------------------------------------------------
    // Feature: dispatched_via round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn dispatched_via_absent_means_none() {
        // Old-format output without dispatched_via should deserialize to None.
        let json = r#"{
            "version": 1,
            "root": "/x",
            "edges": [
                {
                    "caller": {"file": "a.go", "line": 1, "symbol": "f"},
                    "callee": {"file": "b.go", "symbol": "g"},
                    "kind": "dispatches",
                    "nearby_string": "my-task"
                }
            ]
        }"#;
        let out: HelperOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.edges[0].nearby_string, Some("my-task".to_string()));
        assert_eq!(out.edges[0].dispatched_via, None);
    }

    #[test]
    fn dispatched_via_present_deserializes_correctly() {
        let json = r#"{
            "version": 1,
            "root": "/x",
            "edges": [
                {
                    "caller": {"file": "a.go", "line": 1, "symbol": "f"},
                    "callee": {"file": "b.go", "symbol": "HandleTask"},
                    "kind": "dispatches",
                    "nearby_string": "my-task",
                    "dispatched_via": "github.com/hibiken/asynq.(*ServeMux).HandleFunc"
                }
            ]
        }"#;
        let out: HelperOutput = serde_json::from_str(json).unwrap();
        assert_eq!(
            out.edges[0].dispatched_via,
            Some("github.com/hibiken/asynq.(*ServeMux).HandleFunc".to_string())
        );
    }

    #[test]
    fn dispatched_via_skipped_in_serialization_when_none() {
        let edge = HelperEdge {
            caller: HelperCaller {
                file: "a.go".into(),
                line: 1,
                symbol: "f".into(),
            },
            callee: HelperCallee {
                file: "b.go".into(),
                line: 0,
                symbol: "HandleTask".into(),
                receiver: String::new(),
                pkg: String::new(),
            },
            kind: EdgeKind::Dispatches,
            nearby_string: Some("task-key".to_string()),
            dispatched_via: None,
        };
        let s = serde_json::to_string(&edge).unwrap();
        assert!(!s.contains("dispatched_via"), "dispatched_via=None should be omitted: {s}");
    }

    #[test]
    fn dispatched_via_included_in_serialization_when_some() {
        let edge = HelperEdge {
            caller: HelperCaller {
                file: "a.go".into(),
                line: 1,
                symbol: "f".into(),
            },
            callee: HelperCallee {
                file: "b.go".into(),
                line: 0,
                symbol: "HandleTask".into(),
                receiver: String::new(),
                pkg: String::new(),
            },
            kind: EdgeKind::Dispatches,
            nearby_string: Some("task-key".to_string()),
            dispatched_via: Some("example.com/pkg.(*Mux).Register".to_string()),
        };
        let s = serde_json::to_string(&edge).unwrap();
        assert!(s.contains("dispatched_via"), "dispatched_via=Some should be present: {s}");
        assert!(s.contains("example.com/pkg.(*Mux).Register"));
    }

    #[test]
    fn dispatched_via_round_trips() {
        let edge = HelperEdge {
            caller: HelperCaller {
                file: "server/register.go".into(),
                line: 42,
                symbol: "startServer".into(),
            },
            callee: HelperCallee {
                file: "server/handler.go".into(),
                line: 0,
                symbol: "HandleMerchantTask".into(),
                receiver: String::new(),
                pkg: "example.com/server".into(),
            },
            kind: EdgeKind::Dispatches,
            nearby_string: Some("merchant_settlement:merchant_id".to_string()),
            dispatched_via: Some("github.com/hibiken/asynq.(*ServeMux).HandleFunc".to_string()),
        };
        let s = serde_json::to_string(&edge).unwrap();
        let back: HelperEdge = serde_json::from_str(&s).unwrap();
        assert_eq!(edge, back);
        assert_eq!(
            back.dispatched_via,
            Some("github.com/hibiken/asynq.(*ServeMux).HandleFunc".to_string())
        );
    }
}
