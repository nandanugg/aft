use std::path::Path;

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

/// Handle a `writers` request.
///
/// "Who writes to this package-level variable?" — reverse lookup for
/// `writes` edges from the Go helper. Only cross-package writes are
/// returned; same-package writes are filtered at the helper side
/// (filter-at-source contract).
///
/// Expects:
/// - `file` (string, required) — path to the file declaring the variable
/// - `symbol` (string, required) — name of the package-level var/const
///
/// Returns:
/// ```json
/// {
///   "variable": "handlerRegistry",
///   "file": "server/registry.go",
///   "writers": [
///     { "file": "server/asynq.go", "symbol": "startServer", "line": 47 },
///     { "file": "server/registry.go", "symbol": "init", "line": 12 }
///   ],
///   "text": "..."
/// }
/// ```
///
/// Returns error if:
/// - required params missing
/// - call graph not initialized (configure not called)
pub fn handle_writers(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "writers: missing required param 'file'",
            );
        }
    };

    let symbol = match req.params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "writers: missing required param 'symbol'",
            );
        }
    };

    ctx.drain_go_helper();
    let mut cg_ref = ctx.callgraph().borrow_mut();
    let graph = match cg_ref.as_mut() {
        Some(g) => g,
        None => {
            return Response::error(
                &req.id,
                "not_configured",
                "writers: project not configured — send 'configure' first",
            );
        }
    };

    let file_path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };

    match graph.writers_of(&file_path, symbol) {
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
