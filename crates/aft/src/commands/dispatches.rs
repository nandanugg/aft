use crate::callgraph::DispatchesResult;
use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

/// Handle a `dispatches` request.
///
/// Forward lookup by dispatch key: "what handler is registered under this key?"
///
/// Expects:
/// - `key` (string, required) — the dispatch key to look up (e.g. task type name, URL path)
/// - `prefix` (bool, optional, default false) — if true, treat `key` as a prefix
///
/// Returns:
/// ```json
/// {
///   "key": "TypeTask",
///   "handlers": [
///     {
///       "handler": { "file": "server/handler.go", "symbol": "HandleTask" },
///       "registered_by": { "file": "server/register.go", "symbol": "startServer", "line": 42 }
///     }
///   ]
/// }
/// ```
///
/// Returns error if:
/// - required params missing
/// - call graph not initialized (configure not called)
pub fn handle_dispatches(req: &RawRequest, ctx: &AppContext) -> Response {
    let key = match req.params.get("key").and_then(|v| v.as_str()) {
        Some(k) => k.to_string(),
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "dispatches: missing required param 'key'",
            );
        }
    };

    let prefix_mode = req
        .params
        .get("prefix")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if let Some(resp) = ctx.require_go_overlay_project(&req.id, "dispatches") {
        return resp;
    }
    let mut cg_ref = ctx.callgraph().borrow_mut();
    let graph = match cg_ref.as_mut() {
        Some(g) => g,
        None => {
            return Response::error(
                &req.id,
                "not_configured",
                "dispatches: project not configured — send 'configure' first",
            );
        }
    };

    if prefix_mode {
        // Return a list of results grouped by matched key.
        let matches = graph.find_by_dispatch_key_prefix(&key);
        // Flatten into a list of DispatchesResult, one per matched key.
        let results: Vec<DispatchesResult> = matches
            .into_iter()
            .map(|(k, handlers)| DispatchesResult { key: k, handlers })
            .collect();
        let text = results
            .iter()
            .map(|r| r.render_text())
            .collect::<Vec<_>>()
            .join("");
        let mut json = serde_json::json!({ "prefix": key, "results": results });
        if let Some(obj) = json.as_object_mut() {
            obj.insert("text".to_string(), serde_json::Value::String(text));
        }
        Response::success(&req.id, json)
    } else {
        let handlers = graph.find_by_dispatch_key(&key);
        let result = DispatchesResult {
            key: key.clone(),
            handlers,
        };
        let text = result.render_text();
        let mut result_json = serde_json::to_value(&result).unwrap_or_default();
        if let Some(obj) = result_json.as_object_mut() {
            obj.insert("text".to_string(), serde_json::Value::String(text));
        }
        Response::success(&req.id, result_json)
    }
}
