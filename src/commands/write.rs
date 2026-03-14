//! Handler for the `write` command: full file write with auto-backup.

use std::path::Path;

use crate::context::AppContext;
use crate::edit;
use crate::protocol::{RawRequest, Response};

/// Handle a `write` request.
///
/// Params:
///   - `file` (string, required) — target file path
///   - `content` (string, required) — content to write
///   - `create_dirs` (bool, optional, default false) — create parent dirs if missing
///
/// Returns: `{ file, created, syntax_valid?, backup_id? }`
pub fn handle_write(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "write: missing required param 'file'",
            );
        }
    };

    let content = match req.params.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "write: missing required param 'content'",
            );
        }
    };

    let create_dirs = req
        .params
        .get("create_dirs")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let path = Path::new(file);
    let existed = path.exists();

    // Auto-backup existing file before overwriting
    let backup_id = match edit::auto_backup(ctx, path, "write: pre-write backup") {
        Ok(id) => id,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    // Create parent directories if requested
    if create_dirs {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return Response::error(
                        &req.id,
                        "invalid_request",
                        format!("write: failed to create directories: {}", e),
                    );
                }
            }
        }
    }

    // Write content to file
    if let Err(e) = std::fs::write(path, content) {
        return Response::error(
            &req.id,
            "invalid_request",
            format!("write: failed to write file: {}", e),
        );
    }

    eprintln!("[aft] write: {}", file);

    // Attempt syntax validation
    let syntax_valid = match edit::validate_syntax(path) {
        Ok(Some(valid)) => Some(valid),
        Ok(None) => None, // unsupported language
        Err(_) => None,   // validation failed, don't report
    };

    let mut result = serde_json::json!({
        "file": file,
        "created": !existed,
    });

    if let Some(valid) = syntax_valid {
        result["syntax_valid"] = serde_json::json!(valid);
    }

    if let Some(ref id) = backup_id {
        result["backup_id"] = serde_json::json!(id);
    }

    Response::success(&req.id, result)
}
