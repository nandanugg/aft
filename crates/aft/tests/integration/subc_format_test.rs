//! Golden parity tests for subc_format::format_response (TS formatter reference).

use aft::protocol::Response;
use aft::subc_format::format_response;
use serde_json::json;

fn ok(id: &str, data: serde_json::Value) -> Response {
    Response::success(id, data)
}

fn err(id: &str, code: &str, message: &str) -> Response {
    Response::error(id, code, message)
}

#[test]
fn edit_rolled_back() {
    let r = ok(
        "1",
        json!({ "rolled_back": true, "replacements": 1, "diff": { "additions": 1, "deletions": 1 } }),
    );
    assert_eq!(
        format_response("edit", &r, false),
        "Edit rolled back: the change produced invalid syntax, so the file was left unchanged."
    );
}

#[test]
fn edit_files_modified_plural() {
    let r = ok("1", json!({ "files_modified": 3 }));
    assert_eq!(
        format_response("edit", &r, false),
        "Applied edits to 3 files."
    );
}

#[test]
fn edit_glob_total_files() {
    let r = ok("1", json!({ "total_files": 3, "total_replacements": 7 }));
    assert_eq!(
        format_response("edit", &r, false),
        "Edited 3 files (7 replacements)."
    );
}

#[test]
fn write_created_with_counts() {
    let r = ok(
        "1",
        json!({ "created": true, "diff": { "additions": 10, "deletions": 0 } }),
    );
    assert_eq!(
        format_response("write", &r, false),
        "Created file (+10/-0)."
    );
}

#[test]
fn edit_plain_with_edits_applied() {
    let r = ok(
        "1",
        json!({ "edits_applied": 2, "diff": { "additions": 4, "deletions": 1 } }),
    );
    assert_eq!(
        format_response("edit", &r, false),
        "Edited (+4/-1, 2 edits)."
    );
}

#[test]
fn edit_replacements_gt_one() {
    let r = ok(
        "1",
        json!({ "replacements": 3, "diff": { "additions": 3, "deletions": 0 } }),
    );
    assert_eq!(
        format_response("edit", &r, false),
        "Edited (+3/-0, 3 replacements)."
    );
}

#[test]
fn edit_formatted_reformatted_text() {
    let r = ok(
        "1",
        json!({
            "replacements": 1,
            "formatted": true,
            "diff": { "additions": 2, "deletions": 1 },
            "reformatted": { "text": "fn main() {\n    let x = 1;\n}" }
        }),
    );
    let out = format_response("edit", &r, false);
    assert!(out.contains("reflowed your edit"));
    assert!(out.contains("fn main()"));
    assert!(!out.ends_with(" Auto-formatted."));
}

#[test]
fn edit_formatted_extensive() {
    let r = ok(
        "1",
        json!({
            "replacements": 1,
            "formatted": true,
            "diff": { "additions": 1, "deletions": 1 },
            "reformatted": { "extensive": true }
        }),
    );
    assert_eq!(
        format_response("edit", &r, false),
        "Edited (+1/-1). Auto-formatted — extensive reflow; re-read the file before your next anchored edit."
    );
}

#[test]
fn edit_formatted_plain_suffix() {
    let r = ok(
        "1",
        json!({
            "replacements": 1,
            "formatted": true,
            "diff": { "additions": 1, "deletions": 1 }
        }),
    );
    assert_eq!(
        format_response("edit", &r, false),
        "Edited (+1/-1). Auto-formatted."
    );
}

#[test]
fn read_directory_entries() {
    let r = ok("1", json!({ "entries": ["src/", "Cargo.toml"] }));
    assert_eq!(format_response("read", &r, false), "src/\nCargo.toml");
}

#[test]
fn read_binary_message() {
    let r = ok(
        "1",
        json!({ "binary": true, "message": "Binary file (42 bytes), cannot display as text" }),
    );
    assert_eq!(
        format_response("read", &r, false),
        "Binary file (42 bytes), cannot display as text"
    );
}

#[test]
fn read_truncated_footer_when_agent_did_not_specify_range() {
    let r = ok(
        "1",
        json!({
            "content": "1: line\n",
            "truncated": true,
            "start_line": 1,
            "end_line": 100,
            "total_lines": 500
        }),
    );
    assert_eq!(
        format_response("read", &r, false),
        "1: line\n\n(Showing lines 1-100 of 500. Use startLine/endLine to read other sections.)"
    );
}

#[test]
fn read_no_footer_when_agent_specified_range() {
    let r = ok(
        "1",
        json!({
            "content": "1: line\n",
            "truncated": true,
            "start_line": 1,
            "end_line": 100,
            "total_lines": 500
        }),
    );
    assert_eq!(format_response("read", &r, true), "1: line\n");
}

#[test]
fn search_text_plus_honesty_note() {
    let r = ok(
        "1",
        json!({
            "text": "1. foo.rs:10 — bar",
            "fully_degraded": true,
            "complete": false
        }),
    );
    assert_eq!(
        format_response("search", &r, false),
        "1. foo.rs:10 — bar\nSearch status: fully degraded; partial/incomplete."
    );
}

#[test]
fn inspect_text_plus_diagnostics() {
    let r = ok(
        "1",
        json!({
            "text": "todos: 2",
            "summary": { "diagnostics": { "errors": 1, "warnings": 2, "info": 0, "hints": 0 } },
            "details": {
                "diagnostics": [
                    {
                        "file": "a.rs",
                        "line": 1,
                        "column": 2,
                        "severity": "error",
                        "message": "boom",
                        "source": "rustc"
                    }
                ]
            }
        }),
    );
    let out = format_response("inspect", &r, false);
    assert!(out.starts_with("todos: 2"));
    assert!(out.contains("diagnostics: 1 errors, 2 warnings, 0 info, 0 hints"));
    assert!(out.contains("a.rs:1:2 error boom [rustc]"));
}

#[test]
fn outline_partial_footer() {
    let r = ok(
        "1",
        json!({
            "text": "tree",
            "complete": false,
            "unchecked_files": ["z.rs"]
        }),
    );
    assert_eq!(
        format_response("outline", &r, false),
        "tree\n\n⚠ Partial result: 1 files in this directory were not indexed.\nUnchecked files:\n  z.rs"
    );
}

#[test]
fn status_text_passthrough() {
    let r = ok("1", json!({ "text": "indexes ready" }));
    assert_eq!(format_response("status", &r, false), "indexes ready");
}

#[test]
fn error_code_and_message_not_json() {
    let r = err("1", "invalid_request", "edit: missing filePath");
    assert_eq!(
        format_response("edit", &r, false),
        "invalid_request: edit: missing filePath"
    );
    assert!(!format_response("edit", &r, false).contains('{'));
}
