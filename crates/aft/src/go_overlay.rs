use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::GoOverlayBackend;
use crate::go_helper::{
    self, find_helper_binary, HelperError, HelperFlags, HelperOutput, HELPER_SCHEMA_VERSION,
};
use crate::persistent_cache::{project_hash, write_helper_input_hash};

const SIDECAR_PROVIDER_ID: &str = "aft-go-sidecar";
const LOCAL_PROVIDER_ID: &str = "local_helper";
pub const DEFAULT_GO_OVERLAY_TIMEOUT: Duration = Duration::from_secs(180);
const SIDECAR_START_TIMEOUT: Duration = Duration::from_secs(5);
const SIDECAR_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const SIDECAR_RPC_TIMEOUT: Duration = Duration::from_secs(2);
const SIDECAR_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(15);
const SIDECAR_JOB_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct GoOverlayRequest {
    pub root: PathBuf,
    pub timeout: Duration,
    pub flags: HelperFlags,
    pub backend: GoOverlayBackend,
    pub helper_path: Option<PathBuf>,
    pub feature_hash: String,
    pub env_hash: String,
    pub source_fingerprint: String,
}

impl GoOverlayRequest {
    pub fn new(
        root: PathBuf,
        timeout: Duration,
        flags: HelperFlags,
        backend: GoOverlayBackend,
    ) -> Self {
        let helper_path = find_helper_binary();
        let feature_hash = compute_feature_hash(flags);
        let env_hash = compute_env_hash(helper_path.as_deref(), backend);
        let source_fingerprint = compute_source_fingerprint(&root);
        Self {
            root,
            timeout,
            flags,
            backend,
            helper_path,
            feature_hash,
            env_hash,
            source_fingerprint,
        }
    }

    pub fn provider_id(&self) -> &'static str {
        match self.backend {
            GoOverlayBackend::LocalHelper => LOCAL_PROVIDER_ID,
            GoOverlayBackend::AftGoSidecar => SIDECAR_PROVIDER_ID,
        }
    }

    pub fn cache_identity(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.provider_id().as_bytes());
        hasher.update([0]);
        hasher.update(self.feature_hash.as_bytes());
        hasher.update([0]);
        hasher.update(self.env_hash.as_bytes());
        hasher.update([0]);
        hasher.update(self.source_fingerprint.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoOverlaySnapshotMeta {
    pub provider_id: String,
    pub provider_version: String,
    pub schema_version: u32,
    pub feature_hash: String,
    pub env_hash: String,
    pub source_fingerprint: String,
    pub produced_at: String,
}

impl GoOverlaySnapshotMeta {
    pub fn matches_request(&self, request: &GoOverlayRequest) -> bool {
        self.schema_version == HELPER_SCHEMA_VERSION
            && self.provider_id == request.provider_id()
            && self.feature_hash == request.feature_hash
            && self.env_hash == request.env_hash
            && self.source_fingerprint == request.source_fingerprint
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoOverlaySnapshot {
    pub meta: GoOverlaySnapshotMeta,
    pub output: HelperOutput,
}

#[derive(Debug, Clone)]
pub struct GoOverlayRuntimeConfig {
    pub backend: GoOverlayBackend,
    pub cache_dir: PathBuf,
}

impl GoOverlayRuntimeConfig {
    pub fn new(backend: GoOverlayBackend, cache_dir: PathBuf) -> Self {
        Self { backend, cache_dir }
    }

    fn build_provider(&self) -> Box<dyn GoOverlayProvider> {
        match self.backend {
            GoOverlayBackend::LocalHelper => Box::new(LocalHelperProvider {
                cache_dir: self.cache_dir.clone(),
            }),
            GoOverlayBackend::AftGoSidecar => Box::new(AftGoSidecarProvider {
                cache_dir: self.cache_dir.clone(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoOverlayInvalidation {
    pub dirty_files: Vec<String>,
    pub dirty_packages: Vec<String>,
    pub module_dirty: bool,
    pub source_fingerprint: String,
}

pub trait GoOverlayProvider {
    fn load_snapshot(&mut self, request: &GoOverlayRequest) -> Option<GoOverlaySnapshot>;
    fn refresh(&mut self, request: &GoOverlayRequest) -> Result<GoOverlaySnapshot, HelperError>;
    fn invalidate(
        &mut self,
        request: &GoOverlayRequest,
        invalidation: &GoOverlayInvalidation,
    ) -> Result<(), HelperError>;
}

pub fn load_available_snapshot(
    runtime: &GoOverlayRuntimeConfig,
    request: &GoOverlayRequest,
) -> Option<GoOverlaySnapshot> {
    let mut provider = runtime.build_provider();
    if let Some(snapshot) = provider.load_snapshot(request) {
        return Some(snapshot);
    }
    let fallback = fallback_request(request)?;
    log::info!(
        "[aft] go-overlay: falling back to {} cache for {}",
        fallback.provider_id(),
        fallback.root.display()
    );
    LocalHelperProvider {
        cache_dir: runtime.cache_dir.clone(),
    }
    .load_snapshot(&fallback)
}

pub fn spawn_refresh(
    runtime: GoOverlayRuntimeConfig,
    request: GoOverlayRequest,
) -> Receiver<Result<GoOverlaySnapshot, HelperError>> {
    let (tx, rx) = unbounded();
    thread::spawn(move || {
        let mut provider = runtime.build_provider();
        let result = provider.refresh(&request);
        if let Ok(ref snapshot) = result {
            if let Err(err) = write_cached_snapshot(&runtime.cache_dir, snapshot) {
                log::debug!("[aft] go-overlay cache write failed: {err}");
            }
        }
        let _ = tx.send(result);
    });
    rx
}

pub fn refresh_now(
    runtime: &GoOverlayRuntimeConfig,
    request: &GoOverlayRequest,
) -> Result<GoOverlaySnapshot, HelperError> {
    refresh_with_fallback(runtime, request)
}

pub fn invalidate_provider(
    runtime: &GoOverlayRuntimeConfig,
    request: &GoOverlayRequest,
    invalidation: &GoOverlayInvalidation,
) -> Result<(), HelperError> {
    runtime.build_provider().invalidate(request, invalidation)
}

pub fn build_invalidation(root: &Path, changed: &[PathBuf]) -> GoOverlayInvalidation {
    let mut dirty_files = Vec::new();
    let mut dirty_packages = std::collections::BTreeSet::new();
    let mut module_dirty = false;

    for path in changed {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.ends_with(".go") {
            dirty_files.push(rel.clone());
            let pkg = Path::new(&rel)
                .parent()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            dirty_packages.insert(pkg);
        }
        if matches!(rel.as_str(), "go.mod" | "go.sum" | "go.work") {
            module_dirty = true;
        }
    }

    GoOverlayInvalidation {
        dirty_files,
        dirty_packages: dirty_packages.into_iter().collect(),
        module_dirty,
        source_fingerprint: compute_source_fingerprint(root),
    }
}

pub fn compute_source_fingerprint(root: &Path) -> String {
    let mut entries = Vec::new();
    collect_go_inputs(root, root, &mut entries);
    entries.sort_unstable();

    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

fn collect_go_inputs(root: &Path, current: &Path, entries: &mut Vec<String>) {
    let Ok(read_dir) = fs::read_dir(current) else {
        return;
    };

    for item in read_dir.flatten() {
        let path = item.path();
        let Ok(ft) = item.file_type() else {
            continue;
        };
        if ft.is_dir() {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if matches!(name, ".git" | ".opencode" | "node_modules" | "target") {
                continue;
            }
            collect_go_inputs(root, &path, entries);
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let include = name.ends_with(".go") || matches!(name, "go.mod" | "go.sum" | "go.work");
        if !include {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if let Ok(meta) = item.metadata() {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            entries.push(format!("{rel}:{mtime}:{}", meta.len()));
        }
    }
}

fn compute_feature_hash(flags: HelperFlags) -> String {
    #[derive(Serialize)]
    struct CanonicalFeatureSet {
        dispatches: bool,
        implements: bool,
        writes: bool,
        call_context: bool,
        return_analysis: bool,
    }

    let feature_set = CanonicalFeatureSet {
        dispatches: true,
        implements: true,
        writes: true,
        call_context: !flags.no_call_context,
        return_analysis: !flags.no_return_analysis,
    };

    let raw = serde_json::to_vec(&feature_set).unwrap_or_else(|_| {
        format!(
            "{{\"dispatches\":true,\"implements\":true,\"writes\":true,\"call_context\":{},\"return_analysis\":{}}}",
            (!flags.no_call_context),
            (!flags.no_return_analysis)
        )
        .into_bytes()
    });
    let mut hasher = Sha256::new();
    hasher.update(raw);
    format!("{:x}", hasher.finalize())
}

fn fallback_request(request: &GoOverlayRequest) -> Option<GoOverlayRequest> {
    match request.backend {
        GoOverlayBackend::LocalHelper => None,
        GoOverlayBackend::AftGoSidecar => Some(GoOverlayRequest::new(
            request.root.clone(),
            request.timeout,
            request.flags,
            GoOverlayBackend::LocalHelper,
        )),
    }
}

fn refresh_with_fallback(
    runtime: &GoOverlayRuntimeConfig,
    request: &GoOverlayRequest,
) -> Result<GoOverlaySnapshot, HelperError> {
    let mut provider = runtime.build_provider();
    match provider.refresh(request) {
        Ok(snapshot) => Ok(snapshot),
        Err(err) => {
            let Some(fallback) = fallback_request(request) else {
                return Err(err);
            };
            log::warn!(
                "[aft] go-overlay: {} refresh failed for {}: {}. Falling back to {}",
                request.provider_id(),
                request.root.display(),
                err,
                fallback.provider_id()
            );
            LocalHelperProvider {
                cache_dir: runtime.cache_dir.clone(),
            }
            .refresh(&fallback)
        }
    }
}

fn compute_env_hash(helper_path: Option<&Path>, backend: GoOverlayBackend) -> String {
    let mut hasher = Sha256::new();
    hasher.update(backend.as_str().as_bytes());
    hasher.update([0]);
    if let Some(path) = helper_path {
        hasher.update(path.to_string_lossy().as_bytes());
        if let Ok(meta) = fs::metadata(path) {
            hasher.update([0]);
            hasher.update(meta.len().to_string().as_bytes());
            if let Ok(modified) = meta.modified() {
                if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                    hasher.update([0]);
                    hasher.update(duration.as_nanos().to_string().as_bytes());
                }
            }
        }
    }
    for key in ["GOOS", "GOARCH", "GOFLAGS", "CGO_ENABLED", "GOWORK"] {
        hasher.update([0]);
        hasher.update(key.as_bytes());
        hasher.update([0]);
        if let Ok(value) = std::env::var(key) {
            hasher.update(value.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

fn iso8601_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{now}")
}

fn snapshot_meta(
    provider_id: &str,
    provider_version: String,
    request: &GoOverlayRequest,
) -> GoOverlaySnapshotMeta {
    GoOverlaySnapshotMeta {
        provider_id: provider_id.to_string(),
        provider_version,
        schema_version: HELPER_SCHEMA_VERSION,
        feature_hash: request.feature_hash.clone(),
        env_hash: request.env_hash.clone(),
        source_fingerprint: request.source_fingerprint.clone(),
        produced_at: iso8601_now(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CachedSnapshotEnvelope {
    meta: GoOverlaySnapshotMeta,
    output: HelperOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum CachedSnapshotFile {
    Envelope(CachedSnapshotEnvelope),
    Legacy(HelperOutput),
}

fn read_cached_snapshot(cache_dir: &Path, request: &GoOverlayRequest) -> Option<GoOverlaySnapshot> {
    let path = go_helper::cache_file_path(cache_dir);
    let raw = fs::read_to_string(path).ok()?;
    let parsed: CachedSnapshotFile = serde_json::from_str(&raw).ok()?;
    match parsed {
        CachedSnapshotFile::Envelope(envelope) => {
            if envelope.output.root != request.root.to_string_lossy() {
                return None;
            }
            if envelope.meta.matches_request(request) {
                Some(GoOverlaySnapshot {
                    meta: envelope.meta,
                    output: envelope.output,
                })
            } else {
                None
            }
        }
        CachedSnapshotFile::Legacy(_) => None,
    }
}

pub fn write_cached_snapshot(
    cache_dir: &Path,
    snapshot: &GoOverlaySnapshot,
) -> Result<(), HelperError> {
    fs::create_dir_all(cache_dir).map_err(|e| HelperError::Io(format!("mkdir cache: {e}")))?;
    let path = go_helper::cache_file_path(cache_dir);
    let body = serde_json::to_vec_pretty(&CachedSnapshotEnvelope {
        meta: snapshot.meta.clone(),
        output: snapshot.output.clone(),
    })
    .map_err(|e| HelperError::Io(format!("serialize cache: {e}")))?;
    fs::write(path, body).map_err(|e| HelperError::Io(format!("write cache: {e}")))?;
    write_helper_input_hash(cache_dir, &snapshot_identity(snapshot))
        .map_err(|e| HelperError::Io(format!("write helper hash: {e}")))?;
    Ok(())
}

fn snapshot_identity(snapshot: &GoOverlaySnapshot) -> String {
    let mut hasher = Sha256::new();
    hasher.update(snapshot.meta.provider_id.as_bytes());
    hasher.update([0]);
    hasher.update(snapshot.meta.feature_hash.as_bytes());
    hasher.update([0]);
    hasher.update(snapshot.meta.env_hash.as_bytes());
    hasher.update([0]);
    hasher.update(snapshot.meta.source_fingerprint.as_bytes());
    format!("{:x}", hasher.finalize())
}

struct LocalHelperProvider {
    cache_dir: PathBuf,
}

impl GoOverlayProvider for LocalHelperProvider {
    fn load_snapshot(&mut self, request: &GoOverlayRequest) -> Option<GoOverlaySnapshot> {
        read_cached_snapshot(&self.cache_dir, request)
    }

    fn refresh(&mut self, request: &GoOverlayRequest) -> Result<GoOverlaySnapshot, HelperError> {
        let output = go_helper::resolve_for_root(&request.root, request.timeout, request.flags)?;
        Ok(GoOverlaySnapshot {
            meta: snapshot_meta(
                LOCAL_PROVIDER_ID,
                request
                    .helper_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "unknown".to_string()),
                request,
            ),
            output,
        })
    }

    fn invalidate(
        &mut self,
        _request: &GoOverlayRequest,
        _invalidation: &GoOverlayInvalidation,
    ) -> Result<(), HelperError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SidecarInfo {
    provider_id: String,
    provider_version: String,
    schema_version: u32,
    addr: String,
    pid: u32,
    started_at: String,
}

#[derive(Debug, Deserialize)]
struct SidecarResponseRaw {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    result: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct HelloBody {
    provider_id: String,
    schema_version: u32,
}

#[derive(Debug, Deserialize)]
struct StatusBody {
    has_snapshot: bool,
    stale: bool,
}

#[derive(Debug, Deserialize)]
struct RefreshBody {
    #[serde(default)]
    job_id: Option<String>,
    state: String,
    snapshot_state: String,
}

#[derive(Debug, Deserialize)]
struct JobStatusBody {
    job_id: String,
    state: String,
    #[serde(default)]
    error: Option<String>,
    snapshot_state: String,
}

#[derive(Debug, Deserialize)]
struct SnapshotBody {
    snapshot: HelperOutput,
    #[serde(default)]
    stale: Option<bool>,
    root: String,
    feature_hash: String,
    env_hash: String,
    fingerprint: String,
    #[serde(default)]
    last_refreshed_at: Option<String>,
}

struct AftGoSidecarProvider {
    cache_dir: PathBuf,
}

impl AftGoSidecarProvider {
    fn sidecar_dir(&self, request: &GoOverlayRequest) -> PathBuf {
        self.cache_dir
            .join("go-overlay")
            .join(project_hash(&request.root))
            .join(SIDECAR_PROVIDER_ID)
    }

    fn sidecar_info_path(&self, request: &GoOverlayRequest) -> PathBuf {
        self.sidecar_dir(request).join("sidecar-info.json")
    }

    fn load_sidecar_info(&self, request: &GoOverlayRequest) -> Option<SidecarInfo> {
        let path = self.sidecar_info_path(request);
        let raw = fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn hello(&self, addr: &str) -> Result<HelloBody, HelperError> {
        let response: SidecarResponseRaw = send_sidecar_request(
            addr,
            &serde_json::json!({ "method": "hello" }),
            SIDECAR_RPC_TIMEOUT,
        )?;
        if response.ok {
            decode_sidecar_result(response.result)
        } else {
            Err(HelperError::Io(
                response
                    .error
                    .unwrap_or_else(|| "sidecar hello failed".to_string()),
            ))
        }
    }

    fn ensure_sidecar(&self, request: &GoOverlayRequest) -> Result<SidecarInfo, HelperError> {
        if let Some(info) = self.load_sidecar_info(request) {
            if let Ok(hello) = self.hello(&info.addr) {
                if hello.provider_id == SIDECAR_PROVIDER_ID
                    && hello.schema_version == HELPER_SCHEMA_VERSION
                {
                    return Ok(info);
                }
            }
        }

        let helper = request
            .helper_path
            .as_ref()
            .ok_or(HelperError::HelperNotFound)?;
        let info_path = self.sidecar_info_path(request);
        if let Some(parent) = info_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| HelperError::Io(format!("mkdir sidecar dir: {e}")))?;
        }

        let _ = fs::remove_file(&info_path);
        Command::new(helper)
            .arg("--sidecar")
            .arg("--root")
            .arg(&request.root)
            .arg("--sidecar-info-file")
            .arg(&info_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| HelperError::Io(format!("spawn sidecar: {e}")))?;

        let deadline = Instant::now() + SIDECAR_START_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(info) = self.load_sidecar_info(request) {
                if let Ok(hello) = self.hello(&info.addr) {
                    if hello.provider_id == SIDECAR_PROVIDER_ID
                        && hello.schema_version == HELPER_SCHEMA_VERSION
                    {
                        return Ok(info);
                    }
                }
            }
            thread::sleep(Duration::from_millis(50));
        }

        Err(HelperError::Io(
            "timed out waiting for AFT-Go sidecar".to_string(),
        ))
    }

    fn snapshot_from_body(
        &self,
        request: &GoOverlayRequest,
        info: &SidecarInfo,
        body: SnapshotBody,
    ) -> Result<GoOverlaySnapshot, HelperError> {
        if body.root != request.root.to_string_lossy() {
            return Err(HelperError::ParseFailed(
                "sidecar returned snapshot for unexpected root".to_string(),
            ));
        }
        if body.feature_hash != request.feature_hash || body.env_hash != request.env_hash {
            return Err(HelperError::ParseFailed(
                "sidecar returned snapshot for unexpected feature/env identity".to_string(),
            ));
        }
        let mut snapshot = GoOverlaySnapshot {
            meta: snapshot_meta(&info.provider_id, info.provider_version.clone(), request),
            output: body.snapshot,
        };
        snapshot.meta.source_fingerprint = body.fingerprint;
        if let Some(refreshed_at) = body.last_refreshed_at {
            snapshot.meta.produced_at = refreshed_at;
        }
        Ok(snapshot)
    }

    fn status_request(
        &self,
        info: &SidecarInfo,
        request: &GoOverlayRequest,
    ) -> Result<StatusBody, HelperError> {
        let response: SidecarResponseRaw = send_sidecar_request(
            &info.addr,
            &serde_json::json!({
                "method": "status",
                "root": request.root,
                "fingerprint": request.source_fingerprint,
                "env_hash": request.env_hash,
                "features": {
                    "no_call_context": request.flags.no_call_context,
                    "no_return_analysis": request.flags.no_return_analysis,
                },
            }),
            SIDECAR_RPC_TIMEOUT,
        )?;
        if response.ok {
            decode_sidecar_result(response.result)
        } else {
            Err(HelperError::Io(
                response
                    .error
                    .unwrap_or_else(|| "sidecar status failed".to_string()),
            ))
        }
    }

    fn get_snapshot_request(
        &self,
        info: &SidecarInfo,
        request: &GoOverlayRequest,
    ) -> Result<SnapshotBody, HelperError> {
        let response: SidecarResponseRaw = send_sidecar_request(
            &info.addr,
            &serde_json::json!({
                "method": "get_snapshot",
                "root": request.root,
                "fingerprint": request.source_fingerprint,
                "env_hash": request.env_hash,
                "features": {
                    "no_call_context": request.flags.no_call_context,
                    "no_return_analysis": request.flags.no_return_analysis,
                },
            }),
            SIDECAR_SNAPSHOT_TIMEOUT,
        )?;
        if response.ok {
            decode_sidecar_result(response.result)
        } else {
            Err(HelperError::Io(response.error.unwrap_or_else(|| {
                "sidecar get_snapshot failed".to_string()
            })))
        }
    }

    fn poll_refresh_job(
        &self,
        info: &SidecarInfo,
        request: &GoOverlayRequest,
        job_id: &str,
    ) -> Result<(), HelperError> {
        let deadline = Instant::now() + request.timeout;
        loop {
            let response: SidecarResponseRaw = send_sidecar_request(
                &info.addr,
                &serde_json::json!({
                    "method": "job_status",
                    "job_id": job_id,
                }),
                SIDECAR_RPC_TIMEOUT,
            )?;
            if !response.ok {
                return Err(HelperError::Io(
                    response
                        .error
                        .unwrap_or_else(|| "sidecar job_status failed".to_string()),
                ));
            }
            let body: JobStatusBody = decode_sidecar_result(response.result)?;
            if body.job_id != job_id {
                return Err(HelperError::ParseFailed(format!(
                    "sidecar job_status returned unexpected job id: {}",
                    body.job_id
                )));
            }
            match body.state.as_str() {
                "done" => {
                    if body.snapshot_state == "fresh" {
                        return Ok(());
                    }
                    return Err(HelperError::Io(
                        "sidecar finished refresh without a fresh snapshot".to_string(),
                    ));
                }
                "failed" => {
                    return Err(HelperError::Io(
                        body.error
                            .unwrap_or_else(|| "sidecar refresh job failed".to_string()),
                    ));
                }
                "queued" | "running" => {
                    if Instant::now() >= deadline {
                        return Err(HelperError::Io(format!(
                            "helper exceeded timeout of {}s",
                            request.timeout.as_secs()
                        )));
                    }
                    let sleep_for = SIDECAR_JOB_POLL_INTERVAL
                        .min(deadline.saturating_duration_since(Instant::now()));
                    thread::sleep(sleep_for);
                }
                other => {
                    return Err(HelperError::ParseFailed(format!(
                        "unknown sidecar job state: {other}"
                    )));
                }
            }
        }
    }
}

impl GoOverlayProvider for AftGoSidecarProvider {
    fn load_snapshot(&mut self, request: &GoOverlayRequest) -> Option<GoOverlaySnapshot> {
        let info = self.ensure_sidecar(request).ok()?;
        if let Ok(status) = self.status_request(&info, request) {
            if status.has_snapshot && !status.stale {
                if let Ok(snapshot_body) = self.get_snapshot_request(&info, request) {
                    if !snapshot_body.stale.unwrap_or(false) {
                        let snapshot = self
                            .snapshot_from_body(request, &info, snapshot_body)
                            .ok()?;
                        if snapshot.meta.matches_request(request) {
                            return Some(snapshot);
                        }
                    }
                }
            }
        }
        read_cached_snapshot(&self.cache_dir, request)
    }

    fn refresh(&mut self, request: &GoOverlayRequest) -> Result<GoOverlaySnapshot, HelperError> {
        let info = self.ensure_sidecar(request)?;
        let response: SidecarResponseRaw = send_sidecar_request(
            &info.addr,
            &serde_json::json!({
                "method": "refresh",
                "root": request.root,
                "timeout_ms": request.timeout.as_millis(),
                "fingerprint": request.source_fingerprint,
                "env_hash": request.env_hash,
                "features": {
                    "no_call_context": request.flags.no_call_context,
                    "no_return_analysis": request.flags.no_return_analysis,
                },
            }),
            SIDECAR_RPC_TIMEOUT,
        )?;
        if !response.ok {
            return Err(HelperError::Io(
                response
                    .error
                    .unwrap_or_else(|| "sidecar refresh failed".to_string()),
            ));
        }

        if let Ok(body) = decode_sidecar_result::<SnapshotBody>(response.result.clone()) {
            let snapshot = self.snapshot_from_body(request, &info, body)?;
            if snapshot.meta.matches_request(request) {
                return Ok(snapshot);
            }
            return Err(HelperError::ParseFailed(
                "sidecar returned incompatible snapshot".to_string(),
            ));
        }

        let accepted: RefreshBody = decode_sidecar_result(response.result)?;
        if accepted.state == "done" && accepted.snapshot_state != "fresh" {
            return Err(HelperError::Io(
                "sidecar completed refresh without a fresh snapshot".to_string(),
            ));
        }
        if accepted.state != "done" {
            let job_id = accepted.job_id.as_deref().ok_or_else(|| {
                HelperError::ParseFailed("sidecar refresh missing job_id".to_string())
            })?;
            self.poll_refresh_job(&info, request, job_id)?;
        }

        let body = self.get_snapshot_request(&info, request)?;
        if body.stale.unwrap_or(false) {
            return Err(HelperError::Io(
                "sidecar finished refresh without a fresh snapshot".to_string(),
            ));
        }
        let snapshot = self.snapshot_from_body(request, &info, body)?;
        if snapshot.meta.matches_request(request) {
            Ok(snapshot)
        } else {
            Err(HelperError::ParseFailed(
                "sidecar returned incompatible snapshot".to_string(),
            ))
        }
    }

    fn invalidate(
        &mut self,
        request: &GoOverlayRequest,
        invalidation: &GoOverlayInvalidation,
    ) -> Result<(), HelperError> {
        let Some(info) = self.load_sidecar_info(request) else {
            return Ok(());
        };
        let response: SidecarResponseRaw = send_sidecar_request(
            &info.addr,
            &serde_json::json!({
                "method": "invalidate",
                "root": request.root,
                "fingerprint": invalidation.source_fingerprint,
                "env_hash": request.env_hash,
                "features": {
                    "no_call_context": request.flags.no_call_context,
                    "no_return_analysis": request.flags.no_return_analysis,
                },
                "dirty_files": invalidation.dirty_files,
                "dirty_packages": invalidation.dirty_packages,
                "module_dirty": invalidation.module_dirty,
            }),
            SIDECAR_RPC_TIMEOUT,
        )?;
        if response.ok {
            Ok(())
        } else {
            Err(HelperError::Io(
                response
                    .error
                    .unwrap_or_else(|| "sidecar invalidate failed".to_string()),
            ))
        }
    }
}

fn send_sidecar_request<T: Serialize, R: DeserializeOwned>(
    addr: &str,
    request: &T,
    read_timeout: Duration,
) -> Result<R, HelperError> {
    let addr: SocketAddr = addr
        .parse()
        .map_err(|e| HelperError::Io(format!("parse sidecar addr: {e}")))?;
    let mut stream = TcpStream::connect_timeout(&addr, SIDECAR_CONNECT_TIMEOUT)
        .map_err(|e| HelperError::Io(format!("connect sidecar: {e}")))?;
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|e| HelperError::Io(format!("set sidecar read timeout: {e}")))?;
    stream
        .set_write_timeout(Some(SIDECAR_CONNECT_TIMEOUT))
        .map_err(|e| HelperError::Io(format!("set sidecar write timeout: {e}")))?;
    serde_json::to_writer(&mut stream, request)
        .map_err(|e| HelperError::Io(format!("encode sidecar request: {e}")))?;
    stream
        .write_all(b"\n")
        .map_err(|e| HelperError::Io(format!("write sidecar request: {e}")))?;
    stream
        .flush()
        .map_err(|e| HelperError::Io(format!("flush sidecar request: {e}")))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| HelperError::Io(format!("read sidecar response: {e}")))?;
    serde_json::from_str(&line)
        .map_err(|e| HelperError::ParseFailed(format!("sidecar response parse: {e}")))
}

fn decode_sidecar_result<T: DeserializeOwned>(
    result: Option<serde_json::Value>,
) -> Result<T, HelperError> {
    let value = result.ok_or_else(|| {
        HelperError::ParseFailed("sidecar response missing result body".to_string())
    })?;
    serde_json::from_value(value)
        .map_err(|e| HelperError::ParseFailed(format!("decode sidecar result: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn source_fingerprint_changes_with_go_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let go_file = dir.path().join("main.go");
        std::fs::write(&go_file, "package main\nfunc main() {}\n").unwrap();
        let before = compute_source_fingerprint(dir.path());
        std::fs::write(dir.path().join("go.mod"), "module example.com/x\ngo 1.22\n").unwrap();
        let after = compute_source_fingerprint(dir.path());
        assert_ne!(before, after);
    }

    #[test]
    fn cached_snapshot_requires_provider_metadata_match() {
        let dir = tempfile::tempdir().unwrap();
        let request = GoOverlayRequest::new(
            dir.path().join("project"),
            Duration::from_secs(1),
            HelperFlags::default(),
            GoOverlayBackend::LocalHelper,
        );
        std::fs::create_dir_all(&request.root).unwrap();
        let snapshot = GoOverlaySnapshot {
            meta: snapshot_meta(LOCAL_PROVIDER_ID, "test".to_string(), &request),
            output: HelperOutput {
                version: HELPER_SCHEMA_VERSION,
                root: request.root.to_string_lossy().into_owned(),
                edges: vec![],
                skipped: vec![],
                returns: vec![],
            },
        };
        write_cached_snapshot(dir.path(), &snapshot).unwrap();
        let hit = read_cached_snapshot(dir.path(), &request).unwrap();
        assert_eq!(hit, snapshot);

        let mismatched = GoOverlayRequest::new(
            request.root.clone(),
            Duration::from_secs(1),
            HelperFlags {
                no_call_context: true,
                no_return_analysis: false,
            },
            GoOverlayBackend::LocalHelper,
        );
        assert!(read_cached_snapshot(dir.path(), &mismatched).is_none());
    }

    #[test]
    fn build_invalidation_collects_packages_and_module_files() {
        let root = PathBuf::from("/tmp/example");
        let changed = vec![
            root.join("pkg/service/main.go"),
            root.join("go.mod"),
            root.join("pkg/service/helper.go"),
        ];
        let invalidation = build_invalidation(&root, &changed);
        assert!(invalidation.module_dirty);
        assert_eq!(invalidation.dirty_files.len(), 2);
        assert_eq!(invalidation.dirty_packages, vec!["pkg/service".to_string()]);
    }

    #[test]
    fn feature_hash_matches_sidecar_feature_set_shape() {
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(
                br#"{"dispatches":true,"implements":true,"writes":true,"call_context":true,"return_analysis":true}"#,
            );
            format!("{:x}", hasher.finalize())
        };
        assert_eq!(compute_feature_hash(HelperFlags::default()), expected);
    }

    #[test]
    fn sidecar_backend_falls_back_to_local_helper_cache() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("go.mod"), "module example.com/x\ngo 1.22\n").unwrap();

        let local_request = GoOverlayRequest::new(
            root.clone(),
            Duration::from_secs(1),
            HelperFlags::default(),
            GoOverlayBackend::LocalHelper,
        );
        let snapshot = GoOverlaySnapshot {
            meta: snapshot_meta(LOCAL_PROVIDER_ID, "test".to_string(), &local_request),
            output: HelperOutput {
                version: HELPER_SCHEMA_VERSION,
                root: root.to_string_lossy().into_owned(),
                edges: vec![],
                skipped: vec![],
                returns: vec![],
            },
        };
        write_cached_snapshot(dir.path(), &snapshot).unwrap();

        let mut sidecar_request = GoOverlayRequest::new(
            root,
            Duration::from_secs(1),
            HelperFlags::default(),
            GoOverlayBackend::AftGoSidecar,
        );
        sidecar_request.helper_path = None;
        let runtime =
            GoOverlayRuntimeConfig::new(GoOverlayBackend::AftGoSidecar, dir.path().into());
        let loaded = load_available_snapshot(&runtime, &sidecar_request).unwrap();
        assert_eq!(loaded.meta.provider_id, LOCAL_PROVIDER_ID);
    }

    #[test]
    fn sidecar_provider_loads_snapshot_and_receives_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("go.mod"), "module example.com/x\ngo 1.22\n").unwrap();

        let request = GoOverlayRequest::new(
            root.clone(),
            Duration::from_secs(1),
            HelperFlags::default(),
            GoOverlayBackend::AftGoSidecar,
        );
        let info_path = dir
            .path()
            .join("go-overlay")
            .join(project_hash(&root))
            .join(SIDECAR_PROVIDER_ID)
            .join("sidecar-info.json");
        std::fs::create_dir_all(info_path.parent().unwrap()).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let methods: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let methods_bg = Arc::clone(&methods);
        let root_text = root.to_string_lossy().into_owned();
        let feature_hash = request.feature_hash.clone();
        let env_hash = request.env_hash.clone();
        let fingerprint = request.source_fingerprint.clone();

        std::fs::write(
            &info_path,
            serde_json::to_string(&SidecarInfo {
                provider_id: SIDECAR_PROVIDER_ID.to_string(),
                provider_version: "0.1.0".to_string(),
                schema_version: HELPER_SCHEMA_VERSION,
                addr: addr.to_string(),
                pid: 99999,
                started_at: "now".to_string(),
            })
            .unwrap(),
        )
        .unwrap();

        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if stop_bg.load(Ordering::Relaxed) {
                    break;
                }
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(err) if err.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(err) => panic!("accept failed: {err}"),
                };
                stream.set_nonblocking(false).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                let method = req["method"].as_str().unwrap().to_string();
                methods_bg.lock().unwrap().push(method.clone());
                let response = match method.as_str() {
                    "hello" => serde_json::json!({
                        "ok": true,
                        "method": "hello",
                        "result": {
                            "provider_id": SIDECAR_PROVIDER_ID,
                            "provider_version": "0.1.0",
                            "schema_version": HELPER_SCHEMA_VERSION,
                            "capabilities": ["hello", "status", "refresh", "get_snapshot", "invalidate", "shutdown"],
                            "default_root": root_text,
                        }
                    }),
                    "status" => serde_json::json!({
                        "ok": true,
                        "method": "status",
                        "result": {
                            "root": root_text,
                            "feature_hash": feature_hash,
                            "env_hash": env_hash,
                            "fingerprint": fingerprint,
                            "requested_fingerprint": req["fingerprint"].as_str().unwrap_or_default(),
                            "has_snapshot": true,
                            "stale": false,
                            "last_refreshed_at": "now",
                        }
                    }),
                    "get_snapshot" => serde_json::json!({
                        "ok": true,
                        "method": "get_snapshot",
                        "result": {
                            "snapshot": {
                                "version": HELPER_SCHEMA_VERSION,
                                "root": root_text,
                                "edges": [],
                                "skipped": [],
                                "returns": [],
                            },
                            "root": root_text,
                            "feature_hash": feature_hash,
                            "env_hash": env_hash,
                            "fingerprint": fingerprint,
                            "last_refreshed_at": "now",
                            "module_dirty": false,
                        }
                    }),
                    "invalidate" => serde_json::json!({
                        "ok": true,
                        "method": "invalidate",
                        "result": {
                            "marked": 1,
                            "module_dirty": req["module_dirty"].as_bool().unwrap_or(false),
                            "source_fingerprint": req["fingerprint"].as_str().unwrap_or_default(),
                        }
                    }),
                    other => panic!("unexpected method {other}"),
                };
                serde_json::to_writer(&mut stream, &response).unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
            }
        });

        let runtime =
            GoOverlayRuntimeConfig::new(GoOverlayBackend::AftGoSidecar, dir.path().into());
        let snapshot = load_available_snapshot(&runtime, &request).unwrap();
        assert_eq!(snapshot.meta.provider_id, SIDECAR_PROVIDER_ID);
        invalidate_provider(
            &runtime,
            &request,
            &GoOverlayInvalidation {
                dirty_files: vec!["main.go".to_string()],
                dirty_packages: vec!["".to_string()],
                module_dirty: false,
                source_fingerprint: request.source_fingerprint.clone(),
            },
        )
        .unwrap();

        handle.join().unwrap();
        assert_eq!(
            methods.lock().unwrap().clone(),
            vec!["hello", "status", "get_snapshot", "invalidate"]
        );
    }

    #[test]
    fn sidecar_provider_refreshes_via_job_status_polling() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("go.mod"), "module example.com/x\ngo 1.22\n").unwrap();

        let request = GoOverlayRequest::new(
            root.clone(),
            Duration::from_secs(2),
            HelperFlags::default(),
            GoOverlayBackend::AftGoSidecar,
        );
        let info_path = dir
            .path()
            .join("go-overlay")
            .join(project_hash(&root))
            .join(SIDECAR_PROVIDER_ID)
            .join("sidecar-info.json");
        std::fs::create_dir_all(info_path.parent().unwrap()).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let methods: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let methods_bg = Arc::clone(&methods);
        let job_status_calls = Arc::new(Mutex::new(0usize));
        let job_status_calls_bg = Arc::clone(&job_status_calls);
        let root_text = root.to_string_lossy().into_owned();
        let feature_hash = request.feature_hash.clone();
        let env_hash = request.env_hash.clone();
        let fingerprint = request.source_fingerprint.clone();

        std::fs::write(
            &info_path,
            serde_json::to_string(&SidecarInfo {
                provider_id: SIDECAR_PROVIDER_ID.to_string(),
                provider_version: "0.1.0".to_string(),
                schema_version: HELPER_SCHEMA_VERSION,
                addr: addr.to_string(),
                pid: 99999,
                started_at: "now".to_string(),
            })
            .unwrap(),
        )
        .unwrap();

        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if stop_bg.load(Ordering::Relaxed) {
                    break;
                }
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(err) if err.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(err) => panic!("accept failed: {err}"),
                };
                stream.set_nonblocking(false).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                let method = req["method"].as_str().unwrap().to_string();
                methods_bg.lock().unwrap().push(method.clone());
                let response = match method.as_str() {
                    "hello" => serde_json::json!({
                        "ok": true,
                        "method": "hello",
                        "result": {
                            "provider_id": SIDECAR_PROVIDER_ID,
                            "provider_version": "0.1.0",
                            "schema_version": HELPER_SCHEMA_VERSION,
                            "capabilities": ["hello", "status", "refresh", "job_status", "get_snapshot", "invalidate", "shutdown"],
                            "default_root": root_text,
                        }
                    }),
                    "refresh" => serde_json::json!({
                        "ok": true,
                        "method": "refresh",
                        "result": {
                            "job_id": "job-1",
                            "state": "running",
                            "snapshot_state": "missing",
                            "root": root_text,
                            "feature_hash": feature_hash,
                            "env_hash": env_hash,
                            "fingerprint": fingerprint,
                        }
                    }),
                    "job_status" => {
                        let mut calls = job_status_calls_bg.lock().unwrap();
                        *calls += 1;
                        let state = if *calls == 1 { "running" } else { "done" };
                        let snapshot_state = if *calls == 1 { "missing" } else { "fresh" };
                        serde_json::json!({
                            "ok": true,
                            "method": "job_status",
                            "result": {
                                "job_id": "job-1",
                                "state": state,
                                "root": root_text,
                                "feature_hash": feature_hash,
                                "env_hash": env_hash,
                                "fingerprint": fingerprint,
                                "snapshot_state": snapshot_state,
                            }
                        })
                    }
                    "get_snapshot" => serde_json::json!({
                        "ok": true,
                        "method": "get_snapshot",
                        "result": {
                            "snapshot": {
                                "version": HELPER_SCHEMA_VERSION,
                                "root": root_text,
                                "edges": [],
                                "skipped": [],
                                "returns": [],
                            },
                            "stale": false,
                            "root": root_text,
                            "feature_hash": feature_hash,
                            "env_hash": env_hash,
                            "fingerprint": fingerprint,
                            "last_refreshed_at": "now",
                            "module_dirty": false,
                        }
                    }),
                    other => panic!("unexpected method {other}"),
                };
                serde_json::to_writer(&mut stream, &response).unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
            }
        });

        thread::sleep(Duration::from_millis(20));

        let mut provider = AftGoSidecarProvider {
            cache_dir: dir.path().into(),
        };
        let snapshot = provider.refresh(&request).unwrap();
        stop.store(true, Ordering::Relaxed);
        assert_eq!(snapshot.meta.provider_id, SIDECAR_PROVIDER_ID);
        assert!(snapshot.meta.matches_request(&request));

        handle.join().unwrap();
        let observed = methods.lock().unwrap().clone();
        assert!(
            observed.starts_with(&[
                "hello".to_string(),
                "refresh".to_string(),
                "job_status".to_string(),
                "job_status".to_string(),
                "get_snapshot".to_string(),
            ]),
            "unexpected sidecar call order: {observed:?}"
        );
    }
}
