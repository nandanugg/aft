use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::GoOverlayBackend;
use crate::context::AppContext;
use crate::go_helper::HelperFlags;
use crate::go_overlay::{
    refresh_now, write_cached_snapshot, GoOverlayRequest, GoOverlayRuntimeConfig,
};
use crate::persistent_cache::project_hash;
use crate::protocol::{RawRequest, Response};
use crate::search_index::resolve_cache_dir;

const DEFAULT_LEASE_TTL_SECS: u64 = 30 * 60;
const DEFAULT_WARM_TIMEOUT_SECS: u64 = 5 * 60;
const SIDECAR_PROVIDER_DIR: &str = "aft-go-sidecar";
const SIDECAR_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeaseRecord {
    session_id: String,
    client: Option<String>,
    project_root: String,
    updated_at_epoch_secs: u64,
    ttl_secs: u64,
}

#[derive(Debug, Deserialize)]
struct SidecarInfo {
    addr: String,
}

#[derive(Debug, Deserialize)]
struct SidecarResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

pub fn handle_go_overlay_session_open(req: &RawRequest, ctx: &AppContext) -> Response {
    match open_session(req, ctx) {
        Ok(data) => Response::success(&req.id, data),
        Err(message) => Response::error(&req.id, "go_overlay_session_failed", message),
    }
}

pub fn handle_go_overlay_session_touch(req: &RawRequest, ctx: &AppContext) -> Response {
    match touch_session(req, ctx) {
        Ok(data) => Response::success(&req.id, data),
        Err(message) => Response::error(&req.id, "go_overlay_session_failed", message),
    }
}

pub fn handle_go_overlay_session_close(req: &RawRequest, ctx: &AppContext) -> Response {
    match close_session(req, ctx) {
        Ok(data) => Response::success(&req.id, data),
        Err(message) => Response::error(&req.id, "go_overlay_session_failed", message),
    }
}

fn open_session(req: &RawRequest, ctx: &AppContext) -> Result<serde_json::Value, String> {
    let input = SessionInput::from_request(req, ctx)?;
    if input.backend != GoOverlayBackend::AftGoSidecar {
        return Ok(serde_json::json!({
            "project_root": input.project_root.display().to_string(),
            "provider": input.backend.as_str(),
            "skipped": true,
            "reason": "go_overlay_provider_is_not_sidecar",
        }));
    }
    if !is_go_project(&input.project_root) {
        return Ok(serde_json::json!({
            "project_root": input.project_root.display().to_string(),
            "provider": input.backend.as_str(),
            "skipped": true,
            "reason": "project_is_not_go_module_or_workspace",
        }));
    }

    let lease_count = with_bootstrap_lock(&input.sidecar_dir, || {
        prune_stale_leases(&input.leases_dir)?;
        write_lease(
            &input.leases_dir,
            req.session(),
            input.client.as_deref(),
            &input.project_root,
            input.lease_ttl_secs,
        )?;
        count_live_leases(&input.leases_dir)
    })?;

    let runtime = GoOverlayRuntimeConfig::new(input.backend, input.cache_dir.clone());
    let request = input.overlay_request();
    let snapshot = refresh_now(&runtime, &request).map_err(|err| err.to_string())?;
    if snapshot.meta.provider_id != request.provider_id() {
        return Err(format!(
            "go overlay warmup used {} instead of {}",
            snapshot.meta.provider_id,
            request.provider_id()
        ));
    }
    write_cached_snapshot(&input.cache_dir, &snapshot).map_err(|err| err.to_string())?;

    Ok(serde_json::json!({
        "project_root": input.project_root.display().to_string(),
        "provider": input.backend.as_str(),
        "skipped": false,
        "lease_count": lease_count,
        "edges": snapshot.output.edges.len(),
        "provider_id": snapshot.meta.provider_id,
        "produced_at": snapshot.meta.produced_at,
    }))
}

fn touch_session(req: &RawRequest, ctx: &AppContext) -> Result<serde_json::Value, String> {
    let input = SessionInput::from_request(req, ctx)?;
    if input.backend != GoOverlayBackend::AftGoSidecar || !is_go_project(&input.project_root) {
        return Ok(serde_json::json!({
            "project_root": input.project_root.display().to_string(),
            "provider": input.backend.as_str(),
            "skipped": true,
        }));
    }

    let lease_count = with_bootstrap_lock(&input.sidecar_dir, || {
        prune_stale_leases(&input.leases_dir)?;
        write_lease(
            &input.leases_dir,
            req.session(),
            input.client.as_deref(),
            &input.project_root,
            input.lease_ttl_secs,
        )?;
        count_live_leases(&input.leases_dir)
    })?;

    Ok(serde_json::json!({
        "project_root": input.project_root.display().to_string(),
        "provider": input.backend.as_str(),
        "lease_count": lease_count,
        "touched": true,
    }))
}

fn close_session(req: &RawRequest, ctx: &AppContext) -> Result<serde_json::Value, String> {
    let input = SessionInput::from_request(req, ctx)?;
    if input.backend != GoOverlayBackend::AftGoSidecar || !is_go_project(&input.project_root) {
        return Ok(serde_json::json!({
            "project_root": input.project_root.display().to_string(),
            "provider": input.backend.as_str(),
            "skipped": true,
        }));
    }

    let remaining_leases = with_bootstrap_lock(&input.sidecar_dir, || {
        prune_stale_leases(&input.leases_dir)?;
        remove_lease(&input.leases_dir, req.session())?;
        prune_stale_leases(&input.leases_dir)?;
        count_live_leases(&input.leases_dir)
    })?;

    let mut sidecar_stopped = false;
    if remaining_leases == 0 {
        sidecar_stopped = shutdown_sidecar(&input.sidecar_info_path)?;
    }

    Ok(serde_json::json!({
        "project_root": input.project_root.display().to_string(),
        "provider": input.backend.as_str(),
        "remaining_leases": remaining_leases,
        "sidecar_stopped": sidecar_stopped,
    }))
}

struct SessionInput {
    project_root: PathBuf,
    backend: GoOverlayBackend,
    cache_dir: PathBuf,
    sidecar_dir: PathBuf,
    sidecar_info_path: PathBuf,
    leases_dir: PathBuf,
    lease_ttl_secs: u64,
    warm_timeout: Duration,
    flags: HelperFlags,
    client: Option<String>,
}

impl SessionInput {
    fn from_request(req: &RawRequest, ctx: &AppContext) -> Result<Self, String> {
        let project_root = req
            .params
            .get("project_root")
            .or_else(|| req.params.get("root"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing required param 'project_root'".to_string())
            .map(PathBuf::from)?;
        let project_root = fs::canonicalize(&project_root).unwrap_or(project_root);

        let backend = req
            .params
            .get("go_overlay_provider")
            .or_else(|| req.params.get("backend"))
            .and_then(|v| v.as_str())
            .map(|name| {
                GoOverlayBackend::from_name(name)
                    .ok_or_else(|| format!("invalid go_overlay_provider: {name}"))
            })
            .transpose()?
            .unwrap_or(ctx.config().go_overlay_backend);

        let lease_ttl_secs = req
            .params
            .get("lease_ttl_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_LEASE_TTL_SECS);

        let warm_timeout_secs = req
            .params
            .get("warm_timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_WARM_TIMEOUT_SECS);

        let cache_dir = resolve_cache_dir(&project_root, ctx.config().storage_dir.as_deref());
        let sidecar_dir = cache_dir
            .join("go-overlay")
            .join(project_hash(&project_root))
            .join(SIDECAR_PROVIDER_DIR);
        let sidecar_info_path = sidecar_dir.join("sidecar-info.json");
        let leases_dir = sidecar_dir.join("leases");

        Ok(Self {
            project_root,
            backend,
            cache_dir,
            sidecar_dir,
            sidecar_info_path,
            leases_dir,
            lease_ttl_secs,
            warm_timeout: Duration::from_secs(warm_timeout_secs),
            flags: HelperFlags {
                no_call_context: !ctx.config().emit_call_context,
                no_return_analysis: !ctx.config().emit_return_analysis,
            },
            client: req
                .params
                .get("client")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned),
        })
    }

    fn overlay_request(&self) -> GoOverlayRequest {
        GoOverlayRequest::new(
            self.project_root.clone(),
            self.warm_timeout,
            self.flags,
            self.backend,
        )
    }
}

fn is_go_project(root: &Path) -> bool {
    root.join("go.mod").is_file() || root.join("go.work").is_file()
}

fn with_bootstrap_lock<T>(
    sidecar_dir: &Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    fs::create_dir_all(sidecar_dir).map_err(|err| format!("create sidecar dir: {err}"))?;
    let lock_dir = sidecar_dir.join("bootstrap.lock");
    let deadline = Instant::now() + LOCK_WAIT_TIMEOUT;
    loop {
        match fs::create_dir(&lock_dir) {
            Ok(()) => break,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if Instant::now() >= deadline {
                    return Err("timed out waiting for session bootstrap lock".to_string());
                }
                thread::sleep(LOCK_RETRY_INTERVAL);
            }
            Err(err) => return Err(format!("create bootstrap lock: {err}")),
        }
    }

    let result = f();
    let _ = fs::remove_dir(&lock_dir);
    result
}

fn write_lease(
    leases_dir: &Path,
    session_id: &str,
    client: Option<&str>,
    project_root: &Path,
    ttl_secs: u64,
) -> Result<(), String> {
    fs::create_dir_all(leases_dir).map_err(|err| format!("create leases dir: {err}"))?;
    let record = LeaseRecord {
        session_id: session_id.to_string(),
        client: client.map(ToOwned::to_owned),
        project_root: project_root.display().to_string(),
        updated_at_epoch_secs: epoch_secs_now(),
        ttl_secs,
    };
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|err| format!("serialize lease record: {err}"))?;
    let path = lease_path(leases_dir, session_id);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).map_err(|err| format!("write lease file: {err}"))?;
    fs::rename(&tmp, &path).map_err(|err| format!("commit lease file: {err}"))?;
    Ok(())
}

fn remove_lease(leases_dir: &Path, session_id: &str) -> Result<(), String> {
    let path = lease_path(leases_dir, session_id);
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("remove lease file: {err}")),
    }
}

fn prune_stale_leases(leases_dir: &Path) -> Result<(), String> {
    if !leases_dir.exists() {
        return Ok(());
    }
    let now = epoch_secs_now();
    for entry in fs::read_dir(leases_dir).map_err(|err| format!("read leases dir: {err}"))? {
        let entry = entry.map_err(|err| format!("read lease entry: {err}"))?;
        let path = entry.path();
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => {
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let record: LeaseRecord = match serde_json::from_str(&raw) {
            Ok(record) => record,
            Err(_) => {
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let expired = record.updated_at_epoch_secs.saturating_add(record.ttl_secs) <= now;
        if expired {
            let _ = fs::remove_file(&path);
        }
    }
    Ok(())
}

fn count_live_leases(leases_dir: &Path) -> Result<usize, String> {
    if !leases_dir.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in fs::read_dir(leases_dir).map_err(|err| format!("read leases dir: {err}"))? {
        let entry = entry.map_err(|err| format!("read lease entry: {err}"))?;
        if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            count += 1;
        }
    }
    Ok(count)
}

fn lease_path(leases_dir: &Path, session_id: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    leases_dir.join(format!("{digest}.json"))
}

fn epoch_secs_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn shutdown_sidecar(info_path: &Path) -> Result<bool, String> {
    let raw = match fs::read_to_string(info_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(format!("read sidecar info: {err}")),
    };
    let info: SidecarInfo =
        serde_json::from_str(&raw).map_err(|err| format!("parse sidecar info: {err}"))?;
    let addr: SocketAddr = info
        .addr
        .parse()
        .map_err(|err| format!("parse sidecar addr: {err}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, SIDECAR_CONNECT_TIMEOUT)
        .map_err(|err| format!("connect sidecar: {err}"))?;
    stream
        .set_read_timeout(Some(SIDECAR_CONNECT_TIMEOUT))
        .map_err(|err| format!("set sidecar read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(SIDECAR_CONNECT_TIMEOUT))
        .map_err(|err| format!("set sidecar write timeout: {err}"))?;
    serde_json::to_writer(&mut stream, &serde_json::json!({ "method": "shutdown" }))
        .map_err(|err| format!("encode sidecar shutdown request: {err}"))?;
    stream
        .write_all(b"\n")
        .map_err(|err| format!("write sidecar shutdown request: {err}"))?;
    stream
        .flush()
        .map_err(|err| format!("flush sidecar shutdown request: {err}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|err| format!("read sidecar shutdown response: {err}"))?;
    let response: SidecarResponse =
        serde_json::from_str(&line).map_err(|err| format!("parse sidecar response: {err}"))?;
    if !response.ok {
        return Err(response
            .error
            .unwrap_or_else(|| "sidecar shutdown failed".to_string()));
    }
    let _ = fs::remove_file(info_path);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_round_trip_prunes_stale_entries() {
        let dir = tempfile::tempdir().unwrap();
        let leases_dir = dir.path().join("leases");
        fs::create_dir_all(&leases_dir).unwrap();

        write_lease(
            &leases_dir,
            "session-a",
            Some("codex"),
            Path::new("/tmp/proj"),
            60,
        )
        .unwrap();
        let stale = LeaseRecord {
            session_id: "stale".to_string(),
            client: Some("codex".to_string()),
            project_root: "/tmp/proj".to_string(),
            updated_at_epoch_secs: 1,
            ttl_secs: 1,
        };
        fs::write(
            lease_path(&leases_dir, "stale"),
            serde_json::to_vec(&stale).unwrap(),
        )
        .unwrap();

        prune_stale_leases(&leases_dir).unwrap();
        assert_eq!(count_live_leases(&leases_dir).unwrap(), 1);

        remove_lease(&leases_dir, "session-a").unwrap();
        assert_eq!(count_live_leases(&leases_dir).unwrap(), 0);
    }

    #[test]
    fn go_project_detection_requires_module_or_workspace_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_go_project(dir.path()));
        fs::write(dir.path().join("go.mod"), "module example.com/x\ngo 1.22\n").unwrap();
        assert!(is_go_project(dir.path()));
    }
}
