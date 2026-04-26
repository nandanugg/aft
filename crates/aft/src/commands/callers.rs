use std::path::Path;

use super::query_support::{filter_callers_result, resolve_symbol_query};
use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

/// Handle a `callers` request.
///
/// Expects:
/// - `file` (string, required) — path to the source file containing the target symbol
/// - `symbol` (string, required) — name of the symbol to find callers for
/// - `depth` (number, optional, default 1) — recursive depth (1 = direct callers only)
/// - `via_interface` (bool, optional, default false) — if true, also include
///   concrete types from the ImplementationIndex when `symbol` is an interface method.
///   Allows "who actually runs when this interface method is called?" in one query.
/// - `include_mocks` (bool, optional, default false) — when `via_interface=true`,
///   controls whether mock implementations are included in the result.
/// - `exclude_tests` (bool, optional, default false) — when true, drops caller
///   groups whose file path looks like a test file/directory.
///
/// Returns callers grouped by file with fields: `symbol`, `file`,
/// `callers` (array of `{ file, callers: [{ symbol, line }] }`),
/// `total_callers`, `scanned_files`.
///
/// Returns error if:
/// - required params missing
/// - call graph not initialized (configure not called)
/// - symbol not found in the file
pub fn handle_callers(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "callers: missing required param 'file'",
            );
        }
    };

    let raw_symbol = match req.params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "callers: missing required param 'symbol'",
            );
        }
    };

    let depth = req
        .params
        .get("depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .min(100) as usize;

    let via_interface = req
        .params
        .get("via_interface")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let include_mocks = req
        .params
        .get("include_mocks")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let exclude_tests = req
        .params
        .get("exclude_tests")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let file_path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };
    if let Some(resp) = ctx.require_go_overlay(&req.id, "callers", &file_path) {
        return resp;
    }

    let mut cg_ref = ctx.callgraph().borrow_mut();
    let graph = match cg_ref.as_mut() {
        Some(g) => g,
        None => {
            return Response::error(
                &req.id,
                "not_configured",
                "callers: project not configured — send 'configure' first",
            );
        }
    };
    let symbol = match resolve_symbol_query(ctx, &file_path, raw_symbol) {
        Ok(symbol) => symbol,
        Err(err) => return Response::error(&req.id, err.code(), err.to_string()),
    };

    // Build file data first to check if the symbol exists
    match graph.build_file(&file_path) {
        Ok(data) => {
            let has_symbol = data.calls_by_symbol.contains_key(&symbol)
                || data.exported_symbols.contains(&symbol)
                || data.symbol_metadata.contains_key(&symbol);
            if !has_symbol {
                return Response::error(
                    &req.id,
                    "symbol_not_found",
                    format!("callers: symbol '{}' not found in {}", raw_symbol, file),
                );
            }
        }
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    }

    let result = if via_interface {
        graph.callers_of_via_interface(&file_path, &symbol, depth, include_mocks)
    } else {
        graph.callers_of(&file_path, &symbol, depth)
    };

    match result {
        Ok(mut result) => {
            filter_callers_result(&mut result, exclude_tests);
            let text = result.render_text();
            let mut result_json = serde_json::to_value(&result).unwrap_or_default();
            if let Some(obj) = result_json.as_object_mut() {
                obj.insert("text".to_string(), serde_json::Value::String(text));
            }
            Response::success(&req.id, result_json)
        }
        Err(e) => Response::error(&req.id, e.code(), e.to_string()),
    }
}
