use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use crossbeam_channel::unbounded;
use notify::{RecursiveMode, Watcher};

use crate::callgraph::CallGraph;
use crate::config::{GoOverlayBackend, SemanticBackend, SemanticBackendConfig};
use crate::context::{AppContext, SemanticIndexEvent, SemanticIndexStatus};
use crate::go_helper;
use crate::go_overlay::{
    load_available_snapshot, refresh_now, spawn_refresh, GoOverlayRequest, GoOverlayRuntimeConfig,
    DEFAULT_GO_OVERLAY_TIMEOUT,
};
use crate::protocol::{RawRequest, Response};
use crate::search_index::{
    build_path_filters, current_git_head, resolve_cache_dir, walk_project_files, SearchIndex,
};
use crate::semantic_index::SemanticIndex;
use crate::similarity::{SimilarityIndex, SymbolRef, SynonymDict};

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    normalized
}

fn validate_storage_dir(raw: &str) -> Result<PathBuf, String> {
    let storage_dir = PathBuf::from(raw);
    if !storage_dir.is_absolute() {
        return Err("configure: storage_dir must be an absolute path".to_string());
    }

    let normalized = normalize_absolute_path(&storage_dir);
    if normalized
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("configure: storage_dir must not escape via '..' traversal".to_string());
    }

    Ok(normalized)
}

fn parse_semantic_config(
    value: &serde_json::Value,
    current: &SemanticBackendConfig,
) -> Result<SemanticBackendConfig, String> {
    let Some(obj) = value.as_object() else {
        return Err("configure: semantic must be an object".to_string());
    };

    let mut semantic = current.clone();

    if let Some(raw) = obj.get("backend") {
        let name = raw
            .as_str()
            .ok_or_else(|| "configure: semantic.backend must be a string".to_string())?
            .trim();
        semantic.backend = SemanticBackend::from_name(name)
            .ok_or_else(|| format!("configure: unsupported semantic.backend '{name}'"))?;
    }
    if let Some(raw) = obj.get("model") {
        semantic.model = raw
            .as_str()
            .ok_or_else(|| "configure: semantic.model must be a string".to_string())?
            .trim()
            .to_string();
    }
    if let Some(raw) = obj.get("base_url") {
        let base_url = raw
            .as_str()
            .ok_or_else(|| "configure: semantic.base_url must be a string".to_string())?
            .trim()
            .to_string();
        semantic.base_url = if base_url.is_empty() {
            None
        } else {
            Some(base_url)
        };
    }
    if let Some(raw) = obj.get("api_key_env") {
        let api_key_env = raw
            .as_str()
            .ok_or_else(|| "configure: semantic.api_key_env must be a string".to_string())?
            .trim()
            .to_string();
        semantic.api_key_env = if api_key_env.is_empty() {
            None
        } else {
            Some(api_key_env)
        };
    }
    if let Some(raw) = obj.get("timeout_ms") {
        let timeout_ms = raw.as_u64().ok_or_else(|| {
            "configure: semantic.timeout_ms must be an unsigned integer".to_string()
        })?;
        semantic.timeout_ms = timeout_ms;
    }
    if let Some(raw) = obj.get("max_batch_size") {
        let max_batch_size = raw.as_u64().ok_or_else(|| {
            "configure: semantic.max_batch_size must be an unsigned integer".to_string()
        })?;
        semantic.max_batch_size = usize::try_from(max_batch_size)
            .map_err(|_| "configure: semantic.max_batch_size is too large".to_string())?;
    }

    Ok(semantic)
}

/// Handle a `configure` request.
///
/// Expects `project_root` (string, required) — absolute path to the project root.
/// Sets the project root on `Config`, initializes the `CallGraph` with that root,
/// spawns a file watcher for live invalidation, and returns success with the
/// configured path.
///
/// Stderr log: `[aft] project root set: <path>`
/// Stderr log: `[aft] watcher started: <path>`
pub fn handle_configure(req: &RawRequest, ctx: &AppContext) -> Response {
    let root = match req.params.get("project_root").and_then(|v| v.as_str()) {
        Some(r) => r,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "configure: missing required param 'project_root'",
            );
        }
    };

    let root_path = PathBuf::from(root);
    if !root_path.is_dir() {
        return Response::error(
            &req.id,
            "invalid_request",
            format!("configure: project_root is not a directory: {}", root),
        );
    }

    // Set project root on config
    ctx.config_mut().project_root = Some(root_path.clone());

    // Optional feature flags from plugin config
    // Optional feature flags from plugin config
    if let Some(v) = req.params.get("format_on_edit").and_then(|v| v.as_bool()) {
        ctx.config_mut().format_on_edit = v;
    }
    if let Some(v) = req.params.get("validate_on_edit").and_then(|v| v.as_str()) {
        ctx.config_mut().validate_on_edit = Some(v.to_string());
    }
    // Per-language formatter overrides: { "typescript": "biome", "python": "ruff" }
    if let Some(v) = req.params.get("formatter").and_then(|v| v.as_object()) {
        for (lang, tool) in v {
            if let Some(tool_str) = tool.as_str() {
                ctx.config_mut()
                    .formatter
                    .insert(lang.clone(), tool_str.to_string());
            }
        }
    }
    // Restrict file operations to project root (default: false)
    if let Some(v) = req
        .params
        .get("restrict_to_project_root")
        .and_then(|v| v.as_bool())
    {
        ctx.config_mut().restrict_to_project_root = v;
    }
    // Per-language checker overrides: { "typescript": "tsc", "python": "pyright" }
    if let Some(v) = req.params.get("checker").and_then(|v| v.as_object()) {
        for (lang, tool) in v {
            if let Some(tool_str) = tool.as_str() {
                ctx.config_mut()
                    .checker
                    .insert(lang.clone(), tool_str.to_string());
            }
        }
    }

    if let Some(v) = req
        .params
        .get("experimental_search_index")
        .and_then(|v| v.as_bool())
    {
        ctx.config_mut().experimental_search_index = v;
    }
    if let Some(v) = req
        .params
        .get("experimental_semantic_search")
        .and_then(|v| v.as_bool())
    {
        ctx.config_mut().experimental_semantic_search = v;
    }
    if let Some(v) = req
        .params
        .get("search_index_max_file_size")
        .and_then(|v| v.as_u64())
    {
        ctx.config_mut().search_index_max_file_size = v;
    }
    // [callgraph] enable_dispatch_edges — drop dispatches/goroutine/defer edges
    // when false. Env var `AFT_DISABLE_DISPATCH_EDGES=1` is the kill switch
    // (set at Config::default() time); this param lets the caller override
    // it per-session. When the env var is set to "1", this param cannot
    // re-enable it (env var wins).
    if let Some(v) = req
        .params
        .get("enable_dispatch_edges")
        .and_then(|v| v.as_bool())
    {
        // Only honour if env-var kill switch is not active.
        if std::env::var("AFT_DISABLE_DISPATCH_EDGES").as_deref() != Ok("1") {
            ctx.config_mut().enable_dispatch_edges = v;
        }
    }
    // [callgraph] enable_implementation_edges — build the ImplementationIndex from
    // `implements` edges. Env var `AFT_DISABLE_IMPLEMENTATION_EDGES=1` is the kill switch.
    if let Some(v) = req
        .params
        .get("enable_implementation_edges")
        .and_then(|v| v.as_bool())
    {
        // Only honour if env-var kill switch is not active.
        if std::env::var("AFT_DISABLE_IMPLEMENTATION_EDGES").as_deref() != Ok("1") {
            ctx.config_mut().enable_implementation_edges = v;
        }
    }
    // [callgraph] enable_writes_edges — include cross-package variable-write edges.
    // Env var `AFT_DISABLE_WRITES_EDGES=1` is the kill switch.
    if let Some(v) = req
        .params
        .get("enable_writes_edges")
        .and_then(|v| v.as_bool())
    {
        if std::env::var("AFT_DISABLE_WRITES_EDGES").as_deref() != Ok("1") {
            ctx.config_mut().enable_writes_edges = v;
        }
    }
    // [callgraph] emit_call_context — annotate edges with caller-context booleans.
    // Env var `AFT_DISABLE_CALL_CONTEXT=1` is the kill switch.
    if let Some(v) = req
        .params
        .get("emit_call_context")
        .and_then(|v| v.as_bool())
    {
        if std::env::var("AFT_DISABLE_CALL_CONTEXT").as_deref() != Ok("1") {
            ctx.config_mut().emit_call_context = v;
        }
    }
    // [callgraph] emit_return_analysis — per-return path-condition analysis.
    // Env var `AFT_DISABLE_RETURN_ANALYSIS=1` is the kill switch.
    if let Some(v) = req
        .params
        .get("emit_return_analysis")
        .and_then(|v| v.as_bool())
    {
        if std::env::var("AFT_DISABLE_RETURN_ANALYSIS").as_deref() != Ok("1") {
            ctx.config_mut().emit_return_analysis = v;
        }
    }
    if let Some(v) = req.params.get("storage_dir").and_then(|v| v.as_str()) {
        let storage_dir = match validate_storage_dir(v) {
            Ok(path) => path,
            Err(error) => {
                return Response::error(&req.id, "invalid_request", error);
            }
        };
        ctx.config_mut().storage_dir = Some(storage_dir.clone());
        ctx.backup().borrow_mut().set_storage_dir(storage_dir);
    }
    if let Some(v) = req.params.get("semantic") {
        let current = ctx.config().semantic.clone();
        let semantic = match parse_semantic_config(v, &current) {
            Ok(config) => config,
            Err(error) => {
                return Response::error(&req.id, "invalid_request", error);
            }
        };
        ctx.config_mut().semantic = semantic;
    }
    // `no_cache: true` disables the persistent call-graph cache for this session.
    if let Some(v) = req.params.get("no_cache").and_then(|v| v.as_bool()) {
        ctx.config_mut().cache_enabled = !v;
    }
    if let Some(v) = req
        .params
        .get("go_overlay_provider")
        .and_then(|v| v.as_str())
    {
        let backend = match GoOverlayBackend::from_name(v) {
            Some(backend) => backend,
            None => {
                return Response::error(
                    &req.id,
                    "invalid_request",
                    format!("configure: invalid go_overlay_provider: {v}"),
                );
            }
        };
        ctx.config_mut().go_overlay_backend = backend;
    }

    // [similarity] config section
    if let Some(v) = req.params.get("similarity") {
        if let Some(obj) = v.as_object() {
            if let Some(enabled) = obj.get("enabled").and_then(|v| v.as_bool()) {
                ctx.config_mut().similarity_enabled = enabled;
            }
            if let Some(auto_build) = obj.get("auto_build_index").and_then(|v| v.as_bool()) {
                ctx.config_mut().similarity_auto_build_index = auto_build;
            }
        }
    }

    let experimental_search_index = ctx.config().experimental_search_index;
    let experimental_semantic_search = ctx.config().experimental_semantic_search;
    let search_index_max_file_size = ctx.config().search_index_max_file_size;
    let semantic_config = ctx.config().semantic.clone();
    let similarity_enabled = ctx.config().similarity_enabled;
    let similarity_auto_build = ctx.config().similarity_auto_build_index;
    let _similarity_weights = ctx.config().similarity_weights;

    *ctx.search_index().borrow_mut() = None;
    *ctx.search_index_rx().borrow_mut() = None;
    *ctx.semantic_index().borrow_mut() = None;
    *ctx.semantic_index_rx().borrow_mut() = None;
    *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::Disabled;
    *ctx.semantic_embedding_model().borrow_mut() = None;
    *ctx.similarity_index().borrow_mut() = None;

    let storage_dir = ctx.config().storage_dir.clone();

    if experimental_search_index {
        let cache_dir = resolve_cache_dir(&root_path, storage_dir.as_deref());
        let current_head = current_git_head(&root_path);
        let mut baseline = SearchIndex::read_from_disk(&cache_dir);

        if let Some(index) = baseline.as_mut() {
            if current_head.is_some() && index.stored_git_head() == current_head.as_deref() {
                *ctx.search_index().borrow_mut() = Some(index.clone());
            } else {
                index.set_ready(false);
                *ctx.search_index().borrow_mut() = Some(index.clone());
            }
        }

        let (tx, rx): (
            crossbeam_channel::Sender<(SearchIndex, crate::parser::SymbolCache)>,
            crossbeam_channel::Receiver<(SearchIndex, crate::parser::SymbolCache)>,
        ) = unbounded();
        *ctx.search_index_rx().borrow_mut() = Some(rx);

        let root_clone = root_path.clone();
        thread::spawn(move || {
            let index = SearchIndex::rebuild_or_refresh(
                &root_clone,
                search_index_max_file_size,
                current_head,
                baseline,
            );
            index.write_to_disk(&cache_dir, index.stored_git_head());

            // Pre-warm symbol cache from indexed files
            let mut symbol_cache = crate::parser::SymbolCache::new();
            let mut parser = crate::parser::FileParser::new();
            for file_entry in &index.files {
                if let Ok(mtime) = std::fs::metadata(&file_entry.path).and_then(|m| m.modified()) {
                    if let Ok(symbols) = parser.extract_symbols(&file_entry.path) {
                        symbol_cache.insert(file_entry.path.clone(), mtime, symbols);
                    }
                }
            }
            log::info!(
                "[aft] pre-warmed symbol cache: {} files",
                symbol_cache.len()
            );

            let _ = tx.send((index, symbol_cache));
        });
    }

    if experimental_semantic_search {
        *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::Building {
            stage: "queued".to_string(),
            files: None,
            entries_done: None,
            entries_total: None,
        };
        let (tx, rx): (
            crossbeam_channel::Sender<SemanticIndexEvent>,
            crossbeam_channel::Receiver<SemanticIndexEvent>,
        ) = unbounded();
        *ctx.semantic_index_rx().borrow_mut() = Some(rx);

        let root_clone = root_path.clone();
        let semantic_storage = storage_dir.clone();
        let semantic_project_key = crate::search_index::project_cache_key(&root_path);
        let semantic_config = semantic_config.clone();
        let tx_progress = tx.clone();
        thread::spawn(move || {
            let build_result =
                catch_unwind(AssertUnwindSafe(|| -> Result<SemanticIndex, String> {
                    let _ = tx_progress.send(SemanticIndexEvent::Progress {
                        stage: "initializing_embedding_model".to_string(),
                        files: None,
                        entries_done: None,
                        entries_total: None,
                    });
                    let mut model =
                        crate::semantic_index::EmbeddingModel::from_config(&semantic_config)?;
                    let fingerprint = model.fingerprint(&semantic_config)?;
                    let fingerprint_key = fingerprint.as_string();

                    if let Some(ref dir) = semantic_storage {
                        if let Some(cached) = SemanticIndex::read_from_disk(
                            dir,
                            &semantic_project_key,
                            Some(&fingerprint_key),
                        ) {
                            let stale_count = cached.count_stale_files();
                            if stale_count == 0 {
                                let _ = tx_progress.send(SemanticIndexEvent::Progress {
                                    stage: "loaded_cached_index".to_string(),
                                    files: None,
                                    entries_done: Some(cached.entry_count()),
                                    entries_total: Some(cached.entry_count()),
                                });
                                return Ok(cached);
                            }

                            log::info!(
                                "[aft] semantic index: {} stale files, rebuilding",
                                stale_count
                            );
                        }
                    }

                    let filters = build_path_filters(&[], &[]).unwrap_or_default();
                    let files = walk_project_files(&root_clone, &filters);
                    let _ = tx_progress.send(SemanticIndexEvent::Progress {
                        stage: "scanned_project_files".to_string(),
                        files: Some(files.len()),
                        entries_done: None,
                        entries_total: None,
                    });

                    // Cap file count to prevent OOM on huge project roots (e.g., /home/user).
                    // fastembed model (~200MB) + embeddings + batch buffers can exceed memory
                    // on constrained systems when indexing tens of thousands of files.
                    const MAX_SEMANTIC_FILES: usize = 10_000;
                    if files.len() > MAX_SEMANTIC_FILES {
                        log::warn!(
                            "[aft] skipping semantic index: {} files exceeds limit of {}. \
                             Open a specific project directory instead of a large root.",
                            files.len(),
                            MAX_SEMANTIC_FILES
                        );
                        return Err(format!(
                            "too many files ({}) for semantic indexing (max {})",
                            files.len(),
                            MAX_SEMANTIC_FILES
                        ));
                    }

                    let mut embed = |texts: Vec<String>| model.embed(texts);

                    let _ = tx_progress.send(SemanticIndexEvent::Progress {
                        stage: "extracting_symbols".to_string(),
                        files: Some(files.len()),
                        entries_done: None,
                        entries_total: None,
                    });
                    let mut progress = |done: usize, total: usize| {
                        let _ = tx_progress.send(SemanticIndexEvent::Progress {
                            stage: "embedding_symbols".to_string(),
                            files: Some(files.len()),
                            entries_done: Some(done),
                            entries_total: Some(total),
                        });
                    };
                    let index = SemanticIndex::build_with_progress(
                        &root_clone,
                        &files,
                        &mut embed,
                        semantic_config.max_batch_size.max(1),
                        &mut progress,
                    )?;
                    let mut index = index;
                    index.set_fingerprint(fingerprint);
                    log::info!(
                        "[aft] built semantic index: {} files, {} entries",
                        files.len(),
                        index.len()
                    );
                    let _ = tx_progress.send(SemanticIndexEvent::Progress {
                        stage: "persisting_index".to_string(),
                        files: Some(files.len()),
                        entries_done: Some(index.len()),
                        entries_total: Some(index.len()),
                    });

                    if let Some(ref dir) = semantic_storage {
                        index.write_to_disk(dir, &semantic_project_key);
                    }

                    Ok(index)
                }));

            let event = match build_result {
                Ok(Ok(index)) => SemanticIndexEvent::Ready(index),
                Ok(Err(error)) => {
                    log::warn!("[aft] failed to build semantic index: {}", error);
                    SemanticIndexEvent::Failed(error)
                }
                Err(_) => {
                    let error = "semantic index build panicked".to_string();
                    log::warn!("[aft] {}", error);
                    SemanticIndexEvent::Failed(error)
                }
            };

            let _ = tx.send(event);
        });
    }

    // Load similarity index from disk cache if available (if enabled).
    // If not cached, the first `aft similar` call builds it synchronously.
    // We do not background-build here because:
    //   1. AppContext is not Send — we can't update ctx from a thread.
    //   2. A background thread that writes to disk would race with the first
    //      `aft similar` call's synchronous build in the same session.
    // The `aft similar` handler handles build-on-demand with disk caching.
    let _ = similarity_auto_build; // config accepted, not used for background pre-build
    if similarity_enabled {
        let cache_dir = resolve_cache_dir(&root_path, storage_dir.as_deref());
        if let Some(cached) = SimilarityIndex::read_from_disk(&cache_dir) {
            *ctx.similarity_index().borrow_mut() = Some(cached);
            log::info!("[aft-similarity] loaded cached index from disk");
        }
    }

    // Initialize call graph with the project root, and enable on-disk
    // parse caching so repeated CLI invocations skip re-parsing files
    // whose mtime hasn't changed.
    // `no_cache` inverts `cache_enabled`: persistent cache is ON by default
    // unless overridden via `--no-cache`, `AFT_DISABLE_CACHE=1`, or
    // `configure { "no_cache": true }`.
    let no_cache = !ctx.config().cache_enabled;
    let mut graph = CallGraph::new(root_path.clone(), no_cache);
    // Propagate feature-flag settings from Config into the graph.
    graph.enable_dispatch_edges = ctx.config().enable_dispatch_edges;
    graph.enable_implementation_edges = ctx.config().enable_implementation_edges;
    graph.enable_writes_edges = ctx.config().enable_writes_edges;
    let parse_cache_root = resolve_cache_dir(&root_path, storage_dir.as_deref());
    graph.set_parse_cache_dir(parse_cache_root);
    *ctx.callgraph().borrow_mut() = Some(graph);

    // Go helper strategy:
    //   1. Try reading cache synchronously — if present, install immediately
    //      so the first query has resolved interface-dispatch edges.
    //   2. If caller set wait_for_helper=true (CLI mode), and we don't have a
    //      fresh cache, run the helper synchronously and block configure until
    //      it finishes. This is needed because CLI processes exit right after
    //      replying to the command, killing any background thread mid-run.
    //   3. Otherwise (daemon mode), spawn the helper in a background thread
    //      and let AppContext drain it lazily on first query.
    let helper_root = root_path.clone();
    let helper_cache = resolve_cache_dir(&root_path, storage_dir.as_deref());
    let helper_runtime =
        GoOverlayRuntimeConfig::new(ctx.config().go_overlay_backend, helper_cache.clone());

    let helper_request = GoOverlayRequest::new(
        helper_root.clone(),
        DEFAULT_GO_OVERLAY_TIMEOUT,
        go_helper::HelperFlags {
            no_call_context: !ctx.config().emit_call_context,
            no_return_analysis: !ctx.config().emit_return_analysis,
        },
        ctx.config().go_overlay_backend,
    );

    let had_cache = if let Some(cached) = load_available_snapshot(&helper_runtime, &helper_request)
    {
        log::info!(
            "[aft] go-helper: loaded {} cached edges from {} via {}",
            cached.output.edges.len(),
            helper_cache.display(),
            cached.meta.provider_id
        );
        ctx.install_go_helper(cached);
        true
    } else {
        false
    };

    let wait_for_helper = req
        .params
        .get("wait_for_helper")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if wait_for_helper && !had_cache {
        // CLI path: block for helper so same-process queries see resolved
        // interface-dispatch edges. Bounded by the configured Go overlay timeout
        // run_helper (plus whatever packages.Load takes on cold cache).
        match refresh_now(&helper_runtime, &helper_request) {
            Ok(data) => {
                log::info!(
                    "[aft] go-helper: {} edges (sync), {} skipped pkgs via {}",
                    data.output.edges.len(),
                    data.output.skipped.len(),
                    data.meta.provider_id
                );
                if let Err(e) = crate::go_overlay::write_cached_snapshot(&helper_cache, &data) {
                    log::debug!("[aft] go-helper cache write failed: {e}");
                }
                ctx.install_go_helper(data);
            }
            Err(e) => {
                // Silent fallback by design — non-Go project, missing go,
                // missing helper binary, build errors, etc. Tree-sitter
                // still handles same-file and same-package resolution.
                log::debug!("[aft] go-helper sync run unavailable: {e}");
            }
        }
    } else {
        // Daemon path (or cache hit — still refresh for next time).
        ctx.mark_go_overlay_refreshing();
        *ctx.go_helper_rx().borrow_mut() = Some(spawn_refresh(
            helper_runtime.clone(),
            helper_request.clone(),
        ));
    }

    // Drop old watcher/receiver before creating new ones (re-configure)
    *ctx.watcher().borrow_mut() = None;
    *ctx.watcher_rx().borrow_mut() = None;

    // Spawn file watcher for live invalidation
    let (tx, rx) = mpsc::channel();
    match notify::recommended_watcher(tx) {
        Ok(mut w) => {
            if let Err(e) = w.watch(&root_path, RecursiveMode::Recursive) {
                log::debug!(
                    "[aft] watcher watch error: {} — callers will work with stale data",
                    e
                );
            } else {
                log::info!("watcher started: {}", root_path.display());
            }
            *ctx.watcher().borrow_mut() = Some(w);
            *ctx.watcher_rx().borrow_mut() = Some(rx);
        }
        Err(e) => {
            log::debug!(
                "[aft] watcher init failed: {} — callers will work with stale data",
                e
            );
        }
    }

    log::info!("project root set: {}", root_path.display());

    Response::success(
        &req.id,
        serde_json::json!({ "project_root": root_path.display().to_string() }),
    )
}

/// Build a similarity index by scanning all project files and extracting symbols.
///
/// Returns `None` on failures (graceful). Designed to be called from a background thread.
pub fn build_similarity_index(
    project_root: &std::path::Path,
    weights: (f32, f32, f32),
) -> Option<SimilarityIndex> {
    use rayon::prelude::*;
    use std::collections::HashSet;

    let _ = weights; // weights stored in index config, not in the index itself

    let filters = match build_path_filters(&[], &[]) {
        Ok(f) => f,
        Err(_) => return None,
    };
    let files = walk_project_files(project_root, &filters);
    if files.is_empty() {
        return None;
    }

    log::info!("[aft-similarity] building index: {} files", files.len());
    let t0 = std::time::Instant::now();

    // Parse files in parallel (rayon). Each thread gets its own FileParser (not Send).
    // We use par_iter and thread_local parsers via a rayon scope.
    let symbol_data: Vec<(SymbolRef, HashSet<String>)> = files
        .par_iter()
        .flat_map(|file| {
            // Each rayon thread gets its own FileParser (thread-local allocation)
            let mut parser = crate::parser::FileParser::new();
            match parser.extract_symbols(file) {
                Ok(symbols) => symbols
                    .into_iter()
                    .filter(|sym| sym.name.len() >= 2)
                    .map(|sym| {
                        (
                            SymbolRef {
                                file: file.to_path_buf(),
                                symbol: sym.name,
                            },
                            HashSet::new(),
                        )
                    })
                    .collect::<Vec<_>>(),
                Err(_) => Vec::new(),
            }
        })
        .collect();

    if symbol_data.is_empty() {
        return None;
    }

    log::info!("[aft-similarity] tokenizing {} symbols", symbol_data.len());

    // Load synonym dict from project root
    let synonyms = SynonymDict::load(project_root);
    if !synonyms.is_empty() {
        log::info!(
            "[aft-similarity] loaded synonym dict ({} entries)",
            synonyms.map.len()
        );
    }

    let index = SimilarityIndex::build(symbol_data, synonyms);
    let elapsed = t0.elapsed();
    log::info!(
        "[aft-similarity] built index: {} symbols, {:.1}ms",
        index.symbol_count,
        elapsed.as_secs_f64() * 1000.0
    );

    Some(index)
}

#[cfg(test)]
mod tests {
    use super::validate_storage_dir;
    use std::path::PathBuf;

    #[test]
    fn validate_storage_dir_requires_absolute_paths() {
        assert!(validate_storage_dir("relative/cache").is_err());
    }

    #[test]
    fn validate_storage_dir_normalizes_safe_parents() {
        let base = std::env::temp_dir();
        let path = base.join("aft-config-test").join("..").join("cache");
        assert_eq!(
            validate_storage_dir(path.to_str().unwrap()).unwrap(),
            base.join("cache")
        );
    }

    #[test]
    fn validate_storage_dir_rejects_relative_with_dotdot() {
        // Relative paths with .. are rejected (not absolute)
        assert!(validate_storage_dir("../../../etc/passwd").is_err());
    }

    #[test]
    fn validate_storage_dir_accepts_absolute_with_dotdot_that_normalizes() {
        // /../../cache normalizes to /cache which is a valid absolute path
        let mut path = PathBuf::from(std::path::MAIN_SEPARATOR.to_string());
        path.push("..");
        path.push("..");
        path.push("cache");
        assert!(validate_storage_dir(path.to_str().unwrap()).is_ok());
    }
}
