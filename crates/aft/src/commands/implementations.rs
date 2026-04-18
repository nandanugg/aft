use std::path::Path;

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

/// Handle an `implementations` request.
///
/// "Which concrete types implement this interface?"
///
/// Expects:
/// - `file` (string, required) — path to the file containing the interface declaration
/// - `symbol` (string, required) — the interface type name (not a method name)
/// - `include_mocks` (bool, optional, default false) — if true, include receivers
///   containing `Mock` and files under `mocks/` directories
///
/// Returns:
/// ```json
/// {
///   "interface": { "file": "settlement/store.go", "symbol": "SettlementStorer" },
///   "implementations": [
///     {
///       "receiver": "*store.settlementStore",
///       "pkg": "store",
///       "methods": [
///         { "name": "Create", "file": "store/settlement_store.go", "line": 125 }
///       ]
///     }
///   ]
/// }
/// ```
///
/// Returns error if:
/// - required params missing
/// - call graph not initialized (configure not called)
///
/// Note: when no Go helper data is available (no `aft-go-helper` on PATH or
/// project was not configured with `wait_for_helper=true`), the result may be
/// empty because tree-sitter alone cannot resolve interface implementations
/// across package boundaries.
pub fn handle_implementations(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "implementations: missing required param 'file'",
            );
        }
    };

    let symbol = match req.params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "implementations: missing required param 'symbol'",
            );
        }
    };

    let include_mocks = req
        .params
        .get("include_mocks")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    ctx.drain_go_helper();
    let mut cg_ref = ctx.callgraph().borrow_mut();
    let graph = match cg_ref.as_mut() {
        Some(g) => g,
        None => {
            return Response::error(
                &req.id,
                "not_configured",
                "implementations: project not configured — send 'configure' first",
            );
        }
    };

    let file_path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };

    match graph.implementations_of(&file_path, symbol, include_mocks) {
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
