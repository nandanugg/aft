use crate as aft;
use crate::context::{AppContext, SemanticIndexEvent, SemanticIndexStatus, SemanticRefreshRequest};
use crate::lsp::client::LspEvent;
use crate::protocol::PushFrame;
use crate::watcher_filter::watcher_path_is_infra_skip;
use std::path::Path;
use std::sync::Arc;

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
