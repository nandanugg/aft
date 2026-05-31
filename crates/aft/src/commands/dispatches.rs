use crate::callgraph::DispatchesResult;
use crate::context::AppContext;
use crate::output::{graph_response, OutputFormat};
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

    let max_files = ctx.config().max_callgraph_files;
    if prefix_mode {
        // Return a list of results grouped by matched key.
        let matches = graph.find_by_dispatch_key_prefix(&key, max_files);
        // Flatten into a list of DispatchesResult, one per matched key.
        let results: Vec<DispatchesResult> = matches
            .into_iter()
            .map(|(k, handlers)| DispatchesResult { key: k, handlers })
            .collect();
        if OutputFormat::from_request(req) == OutputFormat::Compact {
            return graph_response(req, &results);
        }
        Response::success(
            &req.id,
            serde_json::json!({ "prefix": key, "results": results }),
        )
    } else {
        let handlers = graph.find_by_dispatch_key(&key, max_files);
        let result = DispatchesResult {
            key: key.clone(),
            handlers,
        };
        graph_response(req, &result)
    }
}
