use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use super::helpers::{fixture_path, AftProcess};

const SESSION_ID: &str = "tool-call-preview-session";

#[test]
fn tool_call_top_level_preview_does_not_mutate_edit_or_write_modes() {
    let dir = tempfile::tempdir().expect("temp project");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src dir");

    let append_path = root.join("src/append.txt");
    fs::write(&append_path, "alpha\n").expect("write append fixture");

    let replace_path = root.join("src/replace.txt");
    fs::write(&replace_path, "replace old value\n").expect("write replace fixture");

    let symbol_path = root.join("src/symbol.ts");
    fs::copy(fixture_path("sample.ts"), &symbol_path).expect("copy symbol fixture");
    let symbol_original = fs::read_to_string(&symbol_path).expect("read symbol fixture");

    let batch_path = root.join("src/batch.txt");
    fs::write(&batch_path, "one two three\n").expect("write batch fixture");

    let write_new_path = root.join("src/write-new.txt");
    assert!(
        !write_new_path.exists(),
        "new write fixture should start absent"
    );

    let write_overwrite_path = root.join("src/write-overwrite.txt");
    fs::write(&write_overwrite_path, "before write\n").expect("write overwrite fixture");

    let mut aft = AftProcess::spawn();
    let configure = aft.configure(root);
    assert_eq!(
        configure["success"], true,
        "configure failed: {configure:#}"
    );

    assert_preview_does_not_mutate(
        &mut aft,
        "edit append",
        "edit",
        json!({"filePath": "src/append.txt", "appendContent": "beta\n"}),
        &append_path,
        Some("alpha\n"),
    );

    assert_preview_does_not_mutate(
        &mut aft,
        "edit oldString",
        "edit",
        json!({"filePath": "src/replace.txt", "oldString": "old", "newString": "new"}),
        &replace_path,
        Some("replace old value\n"),
    );

    assert_preview_does_not_mutate(
        &mut aft,
        "edit symbol",
        "edit",
        json!({
            "filePath": "src/symbol.ts",
            "symbol": "greet",
            "content": "function greet(name: string): string {\n  return `Preview, ${name}!`;\n}",
        }),
        &symbol_path,
        Some(&symbol_original),
    );

    assert_preview_does_not_mutate(
        &mut aft,
        "edit batch",
        "edit",
        json!({
            "filePath": "src/batch.txt",
            "edits": [
                {"oldString": "one", "newString": "ONE"},
                {"oldString": "three", "newString": "THREE"},
            ],
        }),
        &batch_path,
        Some("one two three\n"),
    );

    assert_preview_does_not_mutate(
        &mut aft,
        "write create",
        "write",
        json!({"filePath": "src/write-new.txt", "content": "new file contents\n"}),
        &write_new_path,
        None,
    );

    assert_preview_does_not_mutate(
        &mut aft,
        "write overwrite",
        "write",
        json!({"filePath": "src/write-overwrite.txt", "content": "after write\n"}),
        &write_overwrite_path,
        Some("before write\n"),
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn agent_supplied_argument_preview_is_ignored_and_normal_tool_call_mutates() {
    let dir = tempfile::tempdir().expect("temp project");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src dir");

    let smuggled_path = root.join("src/smuggled.txt");
    fs::write(&smuggled_path, "before smuggle\n").expect("write smuggle fixture");

    let normal_path = root.join("src/normal.txt");
    fs::write(&normal_path, "before normal\n").expect("write normal fixture");

    let mut aft = AftProcess::spawn();
    let configure = aft.configure(root);
    assert_eq!(
        configure["success"], true,
        "configure failed: {configure:#}"
    );

    let smuggled = send_tool_call(
        &mut aft,
        "smuggled-argument-preview",
        "edit_match",
        json!({
            "file": smuggled_path.to_string_lossy(),
            "match": "before smuggle",
            "replacement": "after smuggle",
            "preview": true,
        }),
        false,
    );
    assert_eq!(
        smuggled["success"], true,
        "smuggled preview failed: {smuggled:#}"
    );
    assert_ne!(
        smuggled.get("preview").and_then(Value::as_bool),
        Some(true),
        "arguments.preview must not put the leaf in preview mode: {smuggled:#}"
    );
    assert_eq!(
        fs::read_to_string(&smuggled_path).expect("read smuggled file"),
        "after smuggle\n"
    );

    let normal = send_tool_call(
        &mut aft,
        "normal-no-preview",
        "edit",
        json!({"filePath": "src/normal.txt", "oldString": "before normal", "newString": "after normal"}),
        false,
    );
    assert_eq!(
        normal["success"], true,
        "normal tool_call failed: {normal:#}"
    );
    assert_ne!(
        normal.get("preview").and_then(Value::as_bool),
        Some(true),
        "normal tool_call must not be a preview: {normal:#}"
    );
    assert_eq!(
        fs::read_to_string(&normal_path).expect("read normal file"),
        "after normal\n"
    );

    let status = aft.shutdown();
    assert!(status.success());
}

fn assert_preview_does_not_mutate(
    aft: &mut AftProcess,
    label: &str,
    tool: &str,
    arguments: Value,
    path: &Path,
    expected_content: Option<&str>,
) {
    let response = send_tool_call(
        aft,
        &format!("preview-{}", label.replace(' ', "-")),
        tool,
        arguments,
        true,
    );

    assert_eq!(
        response["success"], true,
        "preview {label} should succeed: {response:#}"
    );
    assert_eq!(
        response["preview"], true,
        "preview {label} should mark the response as a preview: {response:#}"
    );
    let preview_diff = response["preview_diff"]
        .as_str()
        .unwrap_or_else(|| panic!("preview {label} missing preview_diff: {response:#}"));
    assert!(
        !preview_diff.is_empty(),
        "preview {label} returned an empty preview_diff"
    );
    assert!(
        response["diff"].is_object(),
        "preview {label} missing structured diff: {response:#}"
    );

    match expected_content {
        Some(content) => assert_eq!(
            fs::read_to_string(path).expect("read preview target"),
            content,
            "preview {label} must leave existing file content unchanged"
        ),
        None => assert!(
            !path.exists(),
            "preview {label} must not create the missing file {}",
            path.display()
        ),
    }
    assert_no_undo_entry(aft, label, path);
}

fn assert_no_undo_entry(aft: &mut AftProcess, label: &str, path: &Path) {
    let history = aft.send(
        &json!({
            "id": format!("history-after-{label}"),
            "command": "edit_history",
            "file": path.to_string_lossy(),
        })
        .to_string(),
    );
    assert_eq!(
        history["success"], true,
        "history failed for {label}: {history:#}"
    );
    let entries = history["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("history entries missing for {label}: {history:#}"));
    assert!(
        entries.is_empty(),
        "preview {label} must not create backup history: {history:#}"
    );

    let undo_preview = aft.send(
        &json!({
            "id": format!("undo-preview-after-{label}"),
            "command": "undo_preview",
            "file": path.to_string_lossy(),
        })
        .to_string(),
    );
    assert_eq!(
        undo_preview["success"], false,
        "preview {label} must not create undo state: {undo_preview:#}"
    );
    assert_eq!(undo_preview["code"], "no_undo_history");
}

fn send_tool_call(
    aft: &mut AftProcess,
    id: &str,
    name: &str,
    arguments: Value,
    preview: bool,
) -> Value {
    let mut request = json!({
        "id": id,
        "command": "tool_call",
        "session_id": SESSION_ID,
        "name": name,
        "arguments": arguments,
    });
    if preview {
        request["preview"] = json!(true);
    }
    aft.send(&request.to_string())
}
