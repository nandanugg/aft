use serde_json::json;

use super::helpers::AftProcess;

#[test]
fn configure_accepts_boolean_validate_on_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();

    let configure = aft.send(
        &json!({
            "id": "cfg-validate-bool",
            "command": "configure",
            "project_root": dir.path(),
            "validate_on_edit": true,
        })
        .to_string(),
    );
    assert_eq!(
        configure["success"], true,
        "configure should accept boolean validate_on_edit: {configure:?}"
    );

    let status = aft.send(r#"{"id":"status-validate-bool","command":"status"}"#);
    assert_eq!(status["success"], true, "status should succeed: {status:?}");
    assert_eq!(status["features"]["validate_on_edit"], "syntax");

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}

#[test]
fn configure_accepts_custom_lsp_servers() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();

    let configure = aft.send(
        &json!({
            "id": "cfg-lsp-custom",
            "command": "configure",
            "project_root": dir.path(),
            "experimental_lsp_ty": true,
            "lsp_servers": [{
                "id": "tinymist",
                "extensions": ["typ"],
                "binary": "tinymist",
                "args": [],
                "root_markers": [".git", "typst.toml"],
                "env": {
                    "TINYMIST_FONT_PATHS": "/tmp/fonts"
                },
                "initialization_options": {
                    "exportPdf": "never"
                },
                "disabled": false
            }],
            "disabled_lsp": ["Pyright"]
        })
        .to_string(),
    );

    assert_eq!(
        configure["success"], true,
        "configure should accept custom lsp server config: {configure:?}"
    );

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}

#[test]
fn configure_rejects_lsp_server_env_with_non_string_values() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();

    let configure = aft.send(
        &json!({
            "id": "cfg-lsp-bad-env",
            "command": "configure",
            "project_root": dir.path(),
            "lsp_servers": [{
                "id": "tinymist",
                "extensions": ["typ"],
                "binary": "tinymist",
                "env": {
                    "TINYMIST_FONT_PATHS": 42
                }
            }]
        })
        .to_string(),
    );

    assert_eq!(configure["success"], false);
    assert_eq!(configure["code"], "invalid_request");
    assert!(configure["message"]
        .as_str()
        .unwrap()
        .contains("env.TINYMIST_FONT_PATHS must be a string"));

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}

#[test]
fn configure_rejects_malformed_lsp_servers() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();

    let configure = aft.send(
        &json!({
            "id": "cfg-lsp-bad",
            "command": "configure",
            "project_root": dir.path(),
            "lsp_servers": [{
                "id": "tinymist",
                "extensions": [],
                "binary": "tinymist"
            }]
        })
        .to_string(),
    );

    assert_eq!(configure["success"], false);
    assert_eq!(configure["code"], "invalid_request");
    assert!(configure["message"]
        .as_str()
        .unwrap()
        .contains("extensions must not be empty"));

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}
