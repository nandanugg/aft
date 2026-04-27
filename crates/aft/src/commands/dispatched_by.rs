use std::path::Path;

use super::query_support::resolve_symbol_query;
use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

/// Handle a `dispatched_by` request.
///
/// Reverse lookup for dispatch edges: "who passes `<symbol>` as a function value?"
///
/// Expects:
/// - `file` (string, required) — path to the file containing the target symbol
/// - `symbol` (string, required) — name of the handler function to look up
///
/// Returns:
/// ```json
/// {
///   "symbol": "HandleTask",
///   "file": "server/handler.go",
///   "dispatched_by": [
///     {
///       "caller": { "file": "server/register.go", "symbol": "startServer", "line": 42 },
///       "nearby_string": "TypeTask"
///     }
///   ]
/// }
/// ```
///
/// Returns error if:
/// - required params missing
/// - call graph not initialized (configure not called)
/// - symbol not found in the file
pub fn handle_dispatched_by(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "dispatched_by: missing required param 'file'",
            );
        }
    };

    let raw_symbol = match req.params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "dispatched_by: missing required param 'symbol'",
            );
        }
    };

    let file_path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };
    if let Some(resp) = ctx.require_go_overlay(&req.id, "dispatched_by", &file_path) {
        return resp;
    }

    let mut cg_ref = ctx.callgraph().borrow_mut();
    let graph = match cg_ref.as_mut() {
        Some(g) => g,
        None => {
            return Response::error(
                &req.id,
                "not_configured",
                "dispatched_by: project not configured — send 'configure' first",
            );
        }
    };
    let symbol = match resolve_symbol_query(ctx, &file_path, raw_symbol) {
        Ok(symbol) => symbol,
        Err(err) => return Response::error(&req.id, err.code(), err.to_string()),
    };

    // Build file data first to check if the symbol exists.
    match graph.build_file(&file_path) {
        Ok(data) => {
            let has_symbol = data.calls_by_symbol.contains_key(&symbol)
                || data.exported_symbols.contains(&symbol)
                || data.symbol_metadata.contains_key(&symbol);
            if !has_symbol {
                return Response::error(
                    &req.id,
                    "symbol_not_found",
                    format!(
                        "dispatched_by: symbol '{}' not found in {}",
                        raw_symbol, file
                    ),
                );
            }
        }
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    }

    let max_files = ctx.config().max_callgraph_files;
    match graph.dispatched_by(&file_path, &symbol, max_files) {
        Ok(result) => {
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
