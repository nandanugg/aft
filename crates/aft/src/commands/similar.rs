//! `aft similar` command — find lexically similar symbols.
//!
//! Five-layer similarity: TF-IDF cosine + synonym expansion + call-graph co-citation.
//!
//! # Protocol
//!
//! Request params:
//! - `file` (string, required) — path to the file containing the target symbol.
//! - `symbol` (string, required) — identifier name to search for.
//! - `top` (number, optional, default 10) — how many results to return.
//! - `dict` (bool, optional, default false) — enable synonym dict expansion.
//! - `explain` (bool, optional, default false) — include per-candidate breakdown.
//! - `min_score` (float, optional, default 0.15) — minimum score to include.
//!
//! Returns a `SimilarityResult` JSON object.

use std::collections::HashSet;
use std::path::Path;

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};
use crate::search_index::resolve_cache_dir;
use crate::similarity::{query as run_query, SimilarityIndex, SimilarityQuery, SymbolRef};

/// Handle a `similar` request.
pub fn handle_similar(req: &RawRequest, ctx: &AppContext) -> Response {
    // --- Required params ---
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "similar: missing required param 'file'",
            )
        }
    };

    let symbol = match req.params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "similar: missing required param 'symbol'",
            )
        }
    };

    // --- Optional params ---
    let top = req
        .params
        .get("top")
        .and_then(|v| v.as_u64())
        .unwrap_or(10)
        .min(200) as usize;

    let use_dict = req
        .params
        .get("dict")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let explain = req
        .params
        .get("explain")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let min_score = req
        .params
        .get("min_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.15) as f32;

    // --- Validate project is configured ---
    let config = ctx.config();
    let project_root = match &config.project_root {
        Some(r) => r.clone(),
        None => {
            return Response::error(
                &req.id,
                "not_configured",
                "similar: project not configured — send 'configure' first",
            )
        }
    };
    let weights = config.similarity_weights;
    let similarity_enabled = config.similarity_enabled;
    drop(config);

    if !similarity_enabled {
        return Response::error(
            &req.id,
            "disabled",
            "similar: similarity is disabled in config ([similarity] enabled = false)",
        );
    }

    // --- Load or build the similarity index ---
    let index = get_or_build_index(ctx, &project_root, weights);
    let index = match index {
        Some(i) => i,
        None => {
            return Response::error(
                &req.id,
                "index_not_ready",
                "similar: similarity index not yet built — retry after configure completes",
            )
        }
    };

    // Resolve file path
    let file_path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    // Build query
    let q = SimilarityQuery {
        file: file_path.clone(),
        symbol: symbol.to_string(),
        top,
        use_dict,
        min_score,
        explain,
        weights,
    };

    // If co-citation is needed, enrich the index with live callgraph data
    // before running the query. We do a lightweight callee extraction pass.
    let working_index = if weights.2 > 0.0 {
        enrich_with_callgraph(index, &file_path, symbol, ctx)
            .unwrap_or_else(|| {
                // Reload from context (enrich_with_callgraph consumes index)
                get_or_build_index(ctx, &{
                    let cfg = ctx.config();
                    cfg.project_root.clone().unwrap_or_default()
                }, weights)
                .expect("index should be available after we just loaded it")
            })
    } else {
        index
    };

    match run_query(&working_index, &q) {
        Ok(result) => {
            let text = result.render_text();
            let mut json = serde_json::to_value(&result).unwrap_or_default();
            if let Some(obj) = json.as_object_mut() {
                obj.insert("text".to_string(), serde_json::Value::String(text));
            }
            Response::success(&req.id, json)
        }
        Err(e) => Response::error(&req.id, "symbol_not_found", e),
    }
}

/// Get the similarity index from context, or try to load from disk, or build synchronously.
fn get_or_build_index(
    ctx: &AppContext,
    project_root: &Path,
    weights: (f32, f32, f32),
) -> Option<SimilarityIndex> {
    // 1. Check in-memory cache
    {
        let idx_ref = ctx.similarity_index().borrow();
        if idx_ref.is_some() {
            return idx_ref.clone();
        }
    }

    // 2. Try loading from disk
    let config = ctx.config();
    let storage_dir = config.storage_dir.clone();
    drop(config);

    let cache_dir = resolve_cache_dir(project_root, storage_dir.as_deref());
    if let Some(cached) = SimilarityIndex::read_from_disk(&cache_dir) {
        *ctx.similarity_index().borrow_mut() = Some(cached.clone());
        return Some(cached);
    }

    // 3. Build synchronously (first call, no background index yet)
    log::info!("[aft-similarity] building index synchronously on first `aft similar` call");
    let index = crate::commands::configure::build_similarity_index(project_root, weights)?;

    // Write to disk for future configure loads
    if let Err(e) = index.write_to_disk(&cache_dir) {
        log::warn!("[aft-similarity] failed to write index to disk: {}", e);
    }

    *ctx.similarity_index().borrow_mut() = Some(index.clone());
    Some(index)
}

/// Enrich the index with live callee data from the callgraph for co-citation scoring.
///
/// Extracts the callee set for the target symbol and all candidates from the callgraph's
/// cached file data. Returns `None` if the callgraph has no useful data.
fn enrich_with_callgraph(
    mut index: SimilarityIndex,
    file: &Path,
    symbol: &str,
    ctx: &AppContext,
) -> Option<SimilarityIndex> {
    let mut cg_ref = ctx.callgraph().borrow_mut();
    let graph = cg_ref.as_mut()?;

    // Build the target file to get its call data
    let file_data = graph.build_file(file).ok()?;

    // Extract callees for the target symbol
    let target_callees: HashSet<String> = file_data
        .calls_by_symbol
        .get(symbol)
        .map(|sites| {
            sites
                .iter()
                .map(|site| site.callee_name.clone())
                .collect()
        })
        .unwrap_or_default();

    if !target_callees.is_empty() {
        let target_ref = SymbolRef {
            file: file.to_path_buf(),
            symbol: symbol.to_string(),
        };
        index.callees.insert(target_ref, target_callees);
    }

    Some(index)
}
