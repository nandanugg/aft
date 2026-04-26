use std::path::Path;

use super::query_support::{filter_call_tree, resolve_symbol_query};
use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

/// Handle a `call_tree` request.
///
/// Expects:
/// - `file` (string, required) — path to the source file
/// - `symbol` (string, required) — name of the symbol to trace
/// - `depth` (number, optional, default 5) — max traversal depth
/// - `exclude_tests` (bool, optional, default false) — when true, prunes child
///   nodes whose file path looks like a test file/directory.
///
/// Returns a nested call tree with fields: `name`, `file`, `line`,
/// `signature`, `resolved`, `children`.
///
/// Returns error if:
/// - required params missing
/// - call graph not initialized (configure not called)
/// - symbol not found in the file
pub fn handle_call_tree(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "call_tree: missing required param 'file'",
            );
        }
    };

    let raw_symbol = match req.params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "call_tree: missing required param 'symbol'",
            );
        }
    };

    let depth = req
        .params
        .get("depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .min(100) as usize;
    let exclude_tests = req
        .params
        .get("exclude_tests")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let file_path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };
    if let Some(resp) = ctx.require_go_overlay(&req.id, "call_tree", &file_path) {
        return resp;
    }

    let mut cg_ref = ctx.callgraph().borrow_mut();
    let graph = match cg_ref.as_mut() {
        Some(g) => g,
        None => {
            return Response::error(
                &req.id,
                "not_configured",
                "call_tree: project not configured — send 'configure' first",
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
            // Check if the symbol exists in the file (as a call-site container or exported symbol)
            let has_symbol = data.calls_by_symbol.contains_key(&symbol)
                || data.exported_symbols.contains(&symbol)
                || data.symbol_metadata.contains_key(&symbol);
            if !has_symbol {
                return Response::error(
                    &req.id,
                    "symbol_not_found",
                    format!("call_tree: symbol '{}' not found in {}", raw_symbol, file),
                );
            }
        }
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    }

    match graph.forward_tree(&file_path, &symbol, depth) {
        Ok(mut tree) => {
            filter_call_tree(&mut tree, exclude_tests);
            let text = tree.render_text();
            let mut tree_json = serde_json::to_value(&tree).unwrap_or_default();
            if let Some(obj) = tree_json.as_object_mut() {
                obj.insert("text".to_string(), serde_json::Value::String(text));
            }
            Response::success(&req.id, tree_json)
        }
        Err(e) => Response::error(&req.id, e.code(), e.to_string()),
    }
}
