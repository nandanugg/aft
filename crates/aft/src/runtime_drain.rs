use crate as aft;
use crate::context::{AppContext, SemanticIndexEvent, SemanticIndexStatus, SemanticRefreshRequest};
use crate::log_ctx;
use crate::lsp::client::LspEvent;
use crate::protocol::PushFrame;
use crate::watcher_filter::{watcher_path_is_infra_skip, WatcherDispatchEvent};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub fn drain_configure_warning_events(ctx: &AppContext) {
    for (generation, frame) in ctx.drain_configure_warnings() {
        if ctx.configure_generation() != generation {
            aft::slog_info!(
                "dropping stale configure_warnings for generation {} (current {})",
                generation,
                ctx.configure_generation()
            );
            continue;
        }

        if let Some(sender) = ctx.progress_sender_handle() {
            sender(PushFrame::ConfigureWarnings(frame));
        }
    }
}

pub fn drain_inspect_events(ctx: &AppContext) {
    let drained = ctx.inspect_manager().drain_completions();
    // Watcher-driven Tier-2 scans complete via the reuse path, which bypasses
    // `result_rx`/`drain_completions`. Poll the manager's reuse counter so a
    // background scan still refreshes the bar (#3) — otherwise the counts and
    // `~` marker would only update on a manual `aft_inspect`.
    let reuse_completed = ctx.take_new_reuse_completions();
    // A completed background Tier-2 scan refreshes the agent status-bar counts
    // to the freshly-persisted aggregate, and clears the stale marker — so the
    // bar reflects the new numbers on the next tool result without waiting for
    // an explicit aft_inspect call.
    if drained > 0 || reuse_completed {
        if let Some(project_root) = ctx.config().project_root.clone() {
            let (dead_code, unused_exports, duplicates) = ctx
                .inspect_manager()
                .latest_tier2_counts(ctx.inspect_dir(), project_root);
            // Don't clear the `~` stale marker until the whole serial Tier-2
            // cycle has drained — while any category is still in flight the
            // already-persisted categories may predate the latest edit, so
            // claiming fresh would be premature (#20). `None` counts preserve
            // the last-known value rather than fabricating a `0` (#1).
            let stale = ctx.inspect_manager().tier2_any_in_flight();
            ctx.update_status_bar_tier2(dead_code, unused_exports, duplicates, None, stale);
            // Push the refreshed snapshot so the sidebar reflects the new Tier-2
            // counts immediately. `update_status_bar_tier2` only mutates the
            // in-memory counts (which the agent status bar reads live on each
            // tool result); the push-driven sidebar would otherwise keep showing
            // the pre-population snapshot — where `status_bar` was null and the
            // Code Health section stayed hidden — until some unrelated event
            // happened to emit a status frame.
            ctx.status_emitter().signal(ctx.build_status_snapshot());
        }
    }
}

/// Drain all background build-completion receivers in standalone order.
///
/// Search installs first so watcher/pending updates apply to the freshest index,
/// followed by callgraph store and semantic index completion.
pub fn drain_build_completions(ctx: &AppContext) {
    drain_search_index_events(ctx);
    drain_callgraph_store_events(ctx);
    drain_semantic_index_events(ctx);
}

/// Return true when any background build-completion receiver is currently set.
///
/// Each receiver is checked under its own short lock; no lock is held while
/// checking the next subsystem.
pub fn any_build_in_flight(ctx: &AppContext) -> bool {
    {
        let rx = ctx
            .search_index_rx()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if rx.is_some() {
            return true;
        }
    }

    {
        let rx = ctx.callgraph_store_rx().lock();
        if rx.is_some() {
            return true;
        }
    }

    {
        let rx = ctx.semantic_index_rx().lock();
        rx.is_some()
    }
}

pub fn watcher_path_is_ignored_by_current_matcher(ctx: &AppContext, path: &Path) -> bool {
    if watcher_path_is_infra_skip(path) {
        return true;
    }

    if let Some(matcher) = ctx.gitignore() {
        if path.starts_with(matcher.path()) {
            let is_dir = path.is_dir();
            return matcher
                .matched_path_or_any_parents(path, is_dir)
                .is_ignore();
        }
    }

    false
}

fn replay_search_index_pending_updates(
    ctx: &AppContext,
    index: &mut crate::search_index::SearchIndex,
    pending_paths: Vec<std::path::PathBuf>,
) {
    for path in pending_paths {
        if path.exists() {
            if watcher_path_is_ignored_by_current_matcher(ctx, &path) {
                index.remove_file(&path);
            } else {
                index.update_file(&path);
            }
        } else {
            index.remove_file(&path);
        }
    }
}

pub fn watcher_path_is_semantic_source(path: &Path) -> bool {
    crate::semantic_index::is_semantic_indexed_extension(path)
}

pub fn mark_semantic_corpus_refresh_success(ctx: &AppContext) {
    ctx.clear_all_semantic_refresh_retry_attempts();
    ctx.reset_semantic_refresh_circuit_after_success();
}

pub fn drain_search_index_events(ctx: &AppContext) {
    let (latest, disconnected) = {
        let rx_ref = ctx
            .search_index_rx()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(rx) = rx_ref.as_ref() else {
            return;
        };

        let mut latest = None;
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(index) => latest = Some(index),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        (latest, disconnected)
    };

    let mut status_changed = false;
    let mut installed_index = false;
    if let Some(mut index) = latest {
        let pending_paths = ctx.take_pending_search_index_paths();
        if !pending_paths.is_empty() {
            replay_search_index_pending_updates(ctx, &mut index, pending_paths);
        }
        *ctx.search_index()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(index);
        installed_index = true;
        status_changed = true;
    }

    if disconnected || installed_index {
        *ctx.search_index_rx()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        if disconnected && !installed_index {
            let _ = ctx.take_pending_search_index_paths();
        }
        status_changed = true;
    }

    if status_changed {
        ctx.status_emitter().signal(ctx.build_status_snapshot());
    }
}

/// Install a background-built callgraph store once its cold build completes.
/// Mirrors `drain_search_index_events`: drains the receiver, installs the
/// freshest store, replays paths that changed during the build, and clears the
/// receiver. On build failure (channel disconnected with nothing installed) the
/// receiver is cleared so a later op can retry the cold build.
pub fn drain_callgraph_store_events(ctx: &AppContext) {
    let (latest, disconnected) = {
        let rx_ref = ctx.callgraph_store_rx().lock();
        let Some(rx) = rx_ref.as_ref() else {
            return;
        };

        let mut latest = None;
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(store) => latest = Some(store),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        (latest, disconnected)
    };

    let mut status_changed = false;
    let mut installed = false;
    if let Some(store) = latest {
        // Replay source files that changed while the cold build was running so
        // the freshly-installed store reflects mid-build edits.
        let pending = ctx.take_pending_callgraph_store_paths();
        if !pending.is_empty() {
            if let Err(error) = store.refresh_files(&pending) {
                crate::slog_warn!(
                    "callgraph store post-build pending refresh failed: {}",
                    error
                );
                if let Err(mark_error) = store.mark_files_stale(&pending) {
                    crate::slog_warn!(
                        "failed to mark callgraph store files stale after post-build refresh failure: {}",
                        mark_error
                    );
                }
            }
        }
        *ctx.callgraph_store()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(store));
        installed = true;
        status_changed = true;
    }

    if disconnected || installed {
        *ctx.callgraph_store_rx().lock() = None;
        if disconnected && !installed {
            // Build failed: discard pending paths (no store to apply them to);
            // a later op restarts the build and re-walks the project.
            let _ = ctx.take_pending_callgraph_store_paths();
        }
        status_changed = true;
    }

    if status_changed {
        ctx.status_emitter().signal(ctx.build_status_snapshot());
    }
}

pub fn drain_semantic_index_events(ctx: &AppContext) {
    let (events, disconnected) = {
        let rx_ref = ctx.semantic_index_rx().lock();
        let Some(rx) = rx_ref.as_ref() else {
            return;
        };

        let mut events = Vec::new();
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(event) => events.push(event),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        (events, disconnected)
    };

    if events.is_empty() && !disconnected {
        return;
    }

    let mut keep_receiver = true;
    let mut status_changed = false;
    let mut replay_refresh_paths = Vec::new();
    let mut replay_corpus_refresh = false;
    for event in events {
        match event {
            SemanticIndexEvent::Progress {
                stage,
                files,
                entries_done,
                entries_total,
            } => {
                *ctx.semantic_index_status()
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    SemanticIndexStatus::Building {
                        stage,
                        files,
                        entries_done,
                        entries_total,
                    };
                // Push progress to the sidebar. Without this, a long rebuild
                // (e.g. a slow local embedding backend re-indexing after a prior
                // failure) leaves the sidebar showing the stale prior state —
                // "failed" with an old error — for the entire build, even though
                // it is actively embedding. Progress transitions are exactly
                // when the user needs to see "building".
                status_changed = true;
            }
            SemanticIndexEvent::Ready(mut index) => {
                mark_semantic_corpus_refresh_success(ctx);
                let pending_paths = ctx.take_pending_semantic_index_paths();
                for path in pending_paths {
                    if watcher_path_is_semantic_source(&path) {
                        index.invalidate_file(&path);
                        replay_refresh_paths.push(path);
                    }
                }
                replay_corpus_refresh = ctx.take_pending_semantic_corpus_refresh();
                *ctx.semantic_index()
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(index);
                *ctx.semantic_index_status()
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    SemanticIndexStatus::ready();
                keep_receiver = false;
                status_changed = true;
            }
            SemanticIndexEvent::Failed(error) => {
                let _ = ctx.take_pending_semantic_index_paths();
                let _ = ctx.take_pending_semantic_corpus_refresh();
                *ctx.semantic_index()
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                ctx.clear_semantic_refresh_worker();
                *ctx.semantic_index_status()
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    SemanticIndexStatus::Failed(error);
                keep_receiver = false;
                status_changed = true;
            }
        }
    }

    if disconnected && keep_receiver {
        let _ = ctx.take_pending_semantic_index_paths();
        let _ = ctx.take_pending_semantic_corpus_refresh();
        *ctx.semantic_index()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        ctx.clear_semantic_refresh_worker();
        *ctx.semantic_index_status()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = SemanticIndexStatus::Failed(
            "semantic index build worker disconnected before reporting completion".to_string(),
        );
        keep_receiver = false;
        status_changed = true;
    }

    if !keep_receiver {
        *ctx.semantic_index_rx().lock() = None;
    }

    if replay_corpus_refresh {
        if ctx.canonical_cache_root_opt().is_some() {
            *ctx.semantic_index_status()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                SemanticIndexStatus::Building {
                    stage: "refreshing_corpus".to_string(),
                    files: None,
                    entries_done: None,
                    entries_total: None,
                };
            let sent = ctx
                .semantic_refresh_sender()
                .is_some_and(|sender| sender.send(SemanticRefreshRequest::Corpus).is_ok());
            if !sent {
                *ctx.semantic_index_status()
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    SemanticIndexStatus::Failed(
                        "semantic corpus refresh worker unavailable".to_string(),
                    );
            }
            status_changed = true;
        }
    } else if !replay_refresh_paths.is_empty() {
        {
            let mut status = ctx
                .semantic_index_status()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(&*status, SemanticIndexStatus::Ready { .. }) {
                for path in &replay_refresh_paths {
                    status.add_refreshing_file(path.clone());
                }
                status_changed = true;
            }
        }
        let sent = ctx.semantic_refresh_sender().is_some_and(|sender| {
            sender
                .send(SemanticRefreshRequest::Files {
                    paths: replay_refresh_paths.clone(),
                })
                .is_ok()
        });
        if !sent {
            crate::slog_warn!(
                "semantic refresh worker unavailable; dropping {} replayed file(s)",
                replay_refresh_paths.len()
            );
            let mut status = ctx
                .semantic_index_status()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for path in &replay_refresh_paths {
                status.cancel_refreshing_file(path);
            }
            status_changed = true;
        }
    }

    if status_changed {
        ctx.status_emitter().signal(ctx.build_status_snapshot());
    }
}

/// Source file extensions that the call graph supports.
const SOURCE_EXTENSIONS: &[&str] = &[
    "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "pyi", "rs", "go",
];

pub const WATCHER_BATCH_INLINE_CAP: usize = 256;

/// A `tsconfig.json` / `jsconfig.json` (including variant names like
/// `tsconfig.base.json`). A change to any of these can shift TypeScript build
/// membership (which files `tsc` checks), so the status-bar membership cache
/// must be invalidated. Deliberately broad on the variant suffix and ignorant
/// of `extends` graphs: the cache is cleared wholesale on a match, and base
/// configs almost always follow the `tsconfig*.json` naming. Non-standard base
/// names are covered on the next `tsconfig.json` change or `configure`.
pub fn watcher_path_is_tsconfig(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| {
            n == "tsconfig.json"
                || n == "jsconfig.json"
                || ((n.starts_with("tsconfig.") || n.starts_with("jsconfig."))
                    && n.ends_with(".json"))
        })
        .unwrap_or(false)
}

pub fn watcher_path_is_source(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| SOURCE_EXTENSIONS.contains(&ext))
}

/// A file the callgraph STORE would have indexed at cold-build time. The store
/// indexes every file `walk_project_files` yields (i.e. any detected language),
/// not just the trigram `SOURCE_EXTENSIONS` set. Gating the store's watcher
/// refresh on the narrower trigram set left edits to Java/C/C++/C#/Kotlin/Ruby/
/// PHP/… (all of which the store extracts calls for) serving stale results until
/// a full rebuild. Mirror cold-build exactly so refresh coverage == index
/// coverage.
pub fn watcher_path_is_callgraph_indexed(path: &std::path::Path) -> bool {
    aft::parser::detect_language(path).is_some()
}

pub fn semantic_corpus_refresh_in_progress(ctx: &AppContext) -> bool {
    let status = ctx
        .semantic_index_status()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    matches!(
        &*status,
        SemanticIndexStatus::Building { stage, .. } if stage == "refreshing_corpus"
    )
}

#[cfg(debug_assertions)]
pub fn delay_search_rebuild_publish_for_debug() {
    let Some(delay_ms) = std::env::var("AFT_TEST_SEARCH_REBUILD_PUBLISH_DELAY_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    else {
        return;
    };
    thread::sleep(Duration::from_millis(delay_ms));
}

#[cfg(not(debug_assertions))]
pub fn delay_search_rebuild_publish_for_debug() {}

pub fn spawn_search_corpus_refresh(
    ctx: &AppContext,
    root: std::path::PathBuf,
    config: Arc<aft::config::Config>,
) {
    {
        let mut search_index = ctx
            .search_index()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = search_index.as_mut() {
            index.ready = false;
        }
    }

    let (tx, rx): (
        crossbeam_channel::Sender<aft::search_index::SearchIndex>,
        crossbeam_channel::Receiver<aft::search_index::SearchIndex>,
    ) = crossbeam_channel::unbounded();
    *ctx.search_index_rx()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rx);
    ctx.reset_symbol_cache();

    let is_worktree_bridge = ctx.is_worktree_bridge();
    let session_id = log_ctx::current_session();
    thread::spawn(move || {
        log_ctx::with_session(session_id, || {
            let cache_dir =
                aft::search_index::resolve_cache_dir(&root, config.storage_dir.as_deref());
            let _cache_lock = if is_worktree_bridge {
                None
            } else {
                match aft::search_index::CacheLock::acquire(&cache_dir) {
                    Ok(lock) => Some(lock),
                    Err(error) => {
                        aft::slog_warn!(
                            "failed to acquire search cache lock for ignore refresh: {}",
                            error
                        );
                        None
                    }
                }
            };
            let index = aft::search_index::SearchIndex::build_with_limit(
                &root,
                config.search_index_max_file_size,
            );
            delay_search_rebuild_publish_for_debug();
            if !is_worktree_bridge {
                index.write_to_disk(&cache_dir, index.stored_git_head());
            }
            let _ = tx.send(index);
        });
    });
}

pub fn refresh_project_corpus(
    ctx: &AppContext,
    reason: &str,
    invalidate_ignore_paths: bool,
) -> bool {
    let Some(root) = ctx.canonical_cache_root_opt() else {
        return false;
    };
    let config = ctx.config();
    let mut status_changed = false;

    if invalidate_ignore_paths {
        if let Some(graph) = ctx.callgraph().lock().as_mut() {
            graph.invalidate_file(&root.join(".gitignore"));
            graph.invalidate_file(&root.join(".aftignore"));
        }
    }

    if !ctx.is_worktree_bridge() {
        // Do NOT cold-build the callgraph store synchronously here. This function
        // runs on the single-threaded dispatch loop from `drain_watcher_events`,
        // which fires before EVERY request (and on idle ticks). A full O(repo)
        // `refresh_corpus` (= `cold_build`: parse all files + resolve refs +
        // rewrite SQLite) blocks ALL queued requests — including `configure` and
        // `bash` — for its entire duration, which exceeds the 30s transport
        // timeout on a large repo. On a long-lived bridge (OpenCode Desktop) an
        // FSEvents overflow triggers this drain, so the user sees configure/bash
        // time out (regression: the watcher-overflow path that calls this is new
        // in 0.39.1; the ignore-rule path that also calls this had the same
        // latent inline block, just rarely triggered).
        //
        // Instead, drop the resident store and force a BACKGROUND rebuild: the
        // next `callgraph_store_for_ops()` spawns the cold build off-thread and
        // returns `Building` (callgraph ops + dead_code projection already handle
        // `Building`/unavailable gracefully). This mirrors the search/semantic
        // refreshes below, which are already async. A build already in flight
        // keeps running; the resident drop + force flag make the next op converge
        // to a fresh full rebuild.
        // Mirror the original "act only when the callgraph is actually loaded or
        // building" guard, but reschedule instead of inline-building.
        let callgraph_store_resident = {
            let guard = ctx
                .callgraph_store()
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.is_some()
        };
        if callgraph_store_resident || ctx.callgraph_store_rx().lock().is_some() {
            *ctx.callgraph_store()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            ctx.mark_callgraph_store_force_rebuild();
            status_changed = true;
            aft::slog_info!(
                "callgraph store scheduled for background rebuild after {}",
                reason
            );
        }
    }

    if config.search_index {
        spawn_search_corpus_refresh(ctx, root.clone(), config.clone());
        status_changed = true;
        aft::slog_info!("started search index refresh after {}", reason);
    }

    if config.semantic_search {
        if let Some(sender) = ctx.semantic_refresh_sender() {
            *ctx.semantic_index_status()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                SemanticIndexStatus::Building {
                    stage: "refreshing_corpus".to_string(),
                    files: None,
                    entries_done: None,
                    entries_total: None,
                };
            match sender.send(SemanticRefreshRequest::Corpus) {
                Ok(()) => {
                    status_changed = true;
                }
                Err(error) => {
                    *ctx.semantic_index_status()
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        SemanticIndexStatus::Failed(format!(
                            "semantic corpus refresh worker unavailable: {error}"
                        ));
                    status_changed = true;
                }
            }
        } else if ctx.semantic_index_rx().lock().is_some() {
            ctx.mark_pending_semantic_corpus_refresh();
        }
    }

    status_changed
}

pub fn refresh_corpus_after_ignore_change(ctx: &AppContext) -> bool {
    refresh_project_corpus(ctx, "ignore-rule change", true)
}

pub fn refresh_project_after_watcher_rescan(ctx: &AppContext) -> bool {
    let Some(root) = ctx.canonical_cache_root_opt() else {
        return false;
    };
    ctx.clear_pending_index_updates();
    ctx.reset_symbol_cache();
    let _ = ctx.mark_status_bar_tier2_stale();
    ctx.clear_tsconfig_membership_cache();
    let mut status_changed = true;

    if ctx.callgraph().lock().is_some() {
        *ctx.callgraph().lock() = Some(aft::callgraph::CallGraph::new(root));
    }

    status_changed |= refresh_project_corpus(ctx, "watcher overflow", false);
    status_changed
}

pub fn refresh_callgraph_store_for_watcher(
    ctx: &AppContext,
    changed: &HashSet<std::path::PathBuf>,
) {
    if ctx.is_worktree_bridge() {
        return;
    }
    let source_paths = changed
        .iter()
        .filter(|path| watcher_path_is_callgraph_indexed(path))
        .cloned()
        .collect::<Vec<_>>();
    if source_paths.is_empty() {
        return;
    }
    // Converge to the current generation before writing: if another process
    // published a newer one, drop our stale store so the changed paths get
    // recorded as pending and replayed against the fresh store (rather than
    // incrementally written into a superseded generation).
    ctx.revalidate_callgraph_store_generation();
    let store = {
        let guard = ctx
            .callgraph_store()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.as_ref().map(Arc::clone)
    };
    let Some(store) = store else {
        // Store not resident yet. If a cold build is in flight, record the
        // changed paths so they're replayed once the freshly-built store lands
        // (otherwise mid-build edits would be silently lost). If no build is
        // running, there's nothing to refresh.
        if ctx.callgraph_store_rx().lock().is_some() {
            ctx.add_pending_callgraph_store_paths(source_paths);
        }
        return;
    };
    if let Err(error) = store.refresh_files(&source_paths) {
        aft::slog_warn!("callgraph store refresh failed: {}", error);
        match store.mark_files_stale(&source_paths) {
            Ok(marked) => aft::slog_warn!(
                "marked {} callgraph store file(s) stale after refresh failure",
                marked.len()
            ),
            Err(mark_error) => aft::slog_warn!(
                "failed to mark callgraph store files stale after refresh failure: {}",
                mark_error
            ),
        }
    }
}

/// Drain pre-filtered watcher events and apply cache invalidations on the
/// dispatch thread. The watcher filter thread owns notify receive/decode,
/// metadata filtering, ignore matching, root-deleted detection, and path
/// coalescing; this drain only reacts to compact control events and surviving
/// paths because the cache/index state below is not Send.
pub fn drain_watcher_events(ctx: &AppContext) {
    let mut changed: HashSet<std::path::PathBuf> = HashSet::new();
    let mut ignore_file_changed = false;
    let mut rescan_required = false;
    let mut watcher_failed = None;
    let mut root_deleted = false;

    {
        let rx_ref = ctx.watcher_rx().lock();
        let rx = match rx_ref.as_ref() {
            Some(rx) => rx,
            None => {
                ctx.tick_tier2_refresh_scheduler(0);
                return; // No watcher configured
            }
        };

        loop {
            match rx.try_recv() {
                Ok(WatcherDispatchEvent::Paths(paths)) => {
                    if !rescan_required {
                        changed.extend(paths);
                    }
                }
                Ok(WatcherDispatchEvent::RescanRequired) => {
                    rescan_required = true;
                    changed.clear();
                }
                Ok(WatcherDispatchEvent::IgnoreRulesChanged { path }) => {
                    ignore_file_changed = true;
                    log::debug!(
                        "watcher: ignore rules changed at {}, rebuilding matcher",
                        path.display()
                    );
                    if !rescan_required {
                        ctx.rebuild_gitignore();
                    }
                }
                Ok(WatcherDispatchEvent::RootDeleted) => {
                    root_deleted = true;
                    break;
                }
                Ok(WatcherDispatchEvent::Error(error)) => {
                    watcher_failed = Some(error);
                    break;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    watcher_failed = Some("watcher channel disconnected".to_string());
                    break;
                }
            }
        }
    }

    let mut watcher_status_changed = false;
    if root_deleted {
        ctx.stop_watcher_runtime();
        let _ = ctx.add_degraded_reason("project_root_deleted".to_string());
        aft::slog_warn!(
            "project root deleted; dropping watcher to avoid delete-storm: {:?}",
            ctx.canonical_cache_root_opt()
        );
        watcher_status_changed = true;
        changed.clear();
        rescan_required = false;
    } else if let Some(error) = watcher_failed {
        ctx.stop_watcher_runtime();
        let _ = ctx.add_degraded_reason("watcher_unavailable".to_string());
        aft::slog_warn!(
            "file watcher unavailable; continuing without live external-change invalidation: {}",
            error
        );
        watcher_status_changed = true;
        rescan_required = false;
    }

    let mut status_changed = watcher_status_changed;
    let mut project_corpus_refresh_requested = false;
    if rescan_required {
        aft::slog_warn!("watcher overflow: forcing project rescan");
        ctx.rebuild_gitignore();
        status_changed |= refresh_project_after_watcher_rescan(ctx);
        project_corpus_refresh_requested = true;
        changed.clear();
    } else if ignore_file_changed {
        status_changed |= refresh_corpus_after_ignore_change(ctx);
        project_corpus_refresh_requested = true;
    }

    let scheduler_changed_path_count = if rescan_required {
        aft::inspect::tier2_scheduler::TIER2_REFRESH_STORM_PATH_THRESHOLD + 1
    } else if ignore_file_changed {
        changed.len().max(1)
    } else {
        changed.len()
    };
    if changed.is_empty() {
        if status_changed {
            ctx.status_emitter().signal(ctx.build_status_snapshot());
        }
        ctx.tick_tier2_refresh_scheduler(scheduler_changed_path_count);
        return;
    }

    // A real source change makes the last-known Tier-2 counts stale until the
    // next background scan reconciles them — surface that in the status bar
    // immediately (the `~` marker) so the agent never reads them as live.
    if ctx.mark_status_bar_tier2_stale() {
        status_changed = true;
    }

    // A tsconfig change can shift which files `tsc` checks, which is the policy
    // the status-bar E/W count filters on. Clear the membership cache wholesale
    // so the next bar count re-resolves from disk (handles new nested configs,
    // edited `extends` parents, and deletions without per-key bookkeeping).
    if changed.iter().any(|path| watcher_path_is_tsconfig(path)) {
        ctx.clear_tsconfig_membership_cache();
        status_changed = true;
    }

    let oversized_inline_batch = changed.len() > WATCHER_BATCH_INLINE_CAP;
    if oversized_inline_batch {
        aft::slog_warn!(
            "watcher batch of {} paths exceeds inline cap {}; scheduling corpus refresh",
            changed.len(),
            WATCHER_BATCH_INLINE_CAP
        );
        if !project_corpus_refresh_requested {
            status_changed |= refresh_project_corpus(ctx, "oversized watcher batch", false);
        }
    }

    let search_build_in_progress = {
        let search_index_rx = ctx
            .search_index_rx()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        search_index_rx.is_some()
    };
    if !oversized_inline_batch && search_build_in_progress {
        ctx.add_pending_search_index_paths(changed.iter().cloned());
    }
    let semantic_source_paths = changed
        .iter()
        .filter(|path| aft::runtime_drain::watcher_path_is_semantic_source(path))
        .cloned()
        .collect::<Vec<_>>();
    let semantic_build_in_progress = ctx.semantic_index_rx().lock().is_some();
    let semantic_corpus_refresh_in_progress = semantic_corpus_refresh_in_progress(ctx);
    if !oversized_inline_batch
        && (semantic_build_in_progress || semantic_corpus_refresh_in_progress)
        && !semantic_source_paths.is_empty()
    {
        ctx.add_pending_semantic_index_paths(semantic_source_paths.clone());
    }

    if let Ok(mut symbol_cache) = ctx.symbol_cache().write() {
        for path in &changed {
            symbol_cache.invalidate(path);
        }
    }

    // Phase 2: invalidate each changed file in the call graph
    let mut graph_ref = ctx.callgraph().lock();
    if let Some(graph) = graph_ref.as_mut() {
        for path in &changed {
            if watcher_path_is_source(path) {
                graph.invalidate_file(path);
            }
        }
    }
    drop(graph_ref);

    let mut semantic_refresh_paths = Vec::new();
    if !oversized_inline_batch {
        refresh_callgraph_store_for_watcher(ctx, &changed);

        {
            let mut index_ref = ctx
                .search_index()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(index) = index_ref.as_mut() {
                for path in &changed {
                    if path.exists() {
                        index.update_file(path);
                    } else {
                        index.remove_file(path);
                    }
                }
            }
        }

        let stale_paths = {
            let mut semantic_index_ref = ctx
                .semantic_index()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut stale_paths = Vec::new();
            if let Some(index) = semantic_index_ref.as_mut() {
                for path in &semantic_source_paths {
                    index.invalidate_file(path);
                    stale_paths.push(path.clone());
                }
            }
            stale_paths
        };
        if !stale_paths.is_empty() {
            let mut status = ctx
                .semantic_index_status()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(&*status, SemanticIndexStatus::Ready { .. }) {
                for path in &stale_paths {
                    status.add_refreshing_file(path.clone());
                }
                semantic_refresh_paths = stale_paths;
                status_changed = true;
            }
        }
    }

    // A vanished file's LSP diagnostics would otherwise linger in the warm set
    // forever (no server republishes for a path that no longer exists),
    // inflating the error/warning counts in the status bar and `aft_inspect`.
    // Clear them here so every deletion source is covered (AFT delete, `rm`,
    // `git checkout`, branch switch) — not just the delete command. The agent
    // status bar reads E/W live from the warm set on each response, so clearing
    // the store is sufficient; the next tool call's bar reflects the new count.
    //
    // Not gated on the trigram `SOURCE_EXTENSIONS` set: any registered LSP
    // server (Bash, YAML, Solidity, Vue, C/C++, custom servers, …) can publish
    // diagnostics for files outside that set, and gating on it left their
    // diagnostics stranded after deletion. `clear_for_file` is a cheap no-op
    // when the store holds nothing for the path, so clearing unconditionally
    // for every vanished path is safe.
    for path in &changed {
        if !path.exists() && ctx.lsp_clear_diagnostics_for_file(path) {
            status_changed = true;
        }
    }

    if !semantic_refresh_paths.is_empty() {
        let sent = ctx.semantic_refresh_sender().is_some_and(|sender| {
            sender
                .send(SemanticRefreshRequest::Files {
                    paths: semantic_refresh_paths.clone(),
                })
                .is_ok()
        });
        if !sent {
            aft::slog_warn!(
                "semantic refresh worker unavailable; dropping {} refreshing file(s)",
                semantic_refresh_paths.len()
            );
            let mut status = ctx
                .semantic_index_status()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for path in &semantic_refresh_paths {
                status.cancel_refreshing_file(path);
            }
            status_changed = true;
        }
    }

    aft::slog_info!("invalidated {} files", changed.len());
    if status_changed {
        ctx.status_emitter().signal(ctx.build_status_snapshot());
    }
    ctx.tick_tier2_refresh_scheduler(scheduler_changed_path_count);
}

pub fn drain_lsp_events(ctx: &AppContext) {
    let drained = {
        let mut lsp = ctx.lsp();
        lsp.drain_events()
    };
    let mut status_changed = drained.diagnostics_changed;
    for event in drained.events {
        match event {
            LspEvent::Notification {
                server_kind,
                root,
                method,
                params,
            } => {
                log::debug!(
                    "[aft-lsp] notification {:?} {} {} {}",
                    server_kind,
                    root.display(),
                    method,
                    params.unwrap_or(serde_json::Value::Null)
                );
            }
            LspEvent::ServerRequest {
                server_kind,
                root,
                id,
                method,
                params,
            } => {
                log::debug!(
                    "[aft-lsp] request {:?} {} {:?} {} {}",
                    server_kind,
                    root.display(),
                    id,
                    method,
                    params.unwrap_or(serde_json::Value::Null)
                );
            }
            LspEvent::ServerExited { server_kind, root } => {
                aft::slog_info!("exited {:?} {}", server_kind, root.display());
                status_changed = true;
            }
        }
    }
    if status_changed {
        ctx.status_emitter().signal(ctx.build_status_snapshot());
    }
}
