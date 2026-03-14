//! Integration tests for the aft binary's persistent protocol.
//!
//! These tests spawn the compiled binary as a child process and communicate
//! over stdin/stdout via NDJSON. They prove the process reliability contract:
//! - 100+ sequential commands without failure
//! - Recovery from malformed JSON input
//! - Structured errors for unknown commands
//! - Clean shutdown on stdin EOF

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

/// A handle to a running aft process with piped I/O.
///
/// Uses a persistent `BufReader` over stdout so sequential reads
/// don't lose buffered data between calls.
struct AftProcess {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl AftProcess {
    /// Spawn the aft binary with piped stdin/stdout/stderr.
    fn spawn() -> Self {
        let binary = env!("CARGO_BIN_EXE_aft");
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn aft binary");

        let stdout = child.stdout.take().expect("stdout handle");
        let reader = BufReader::new(stdout);

        AftProcess { child, reader }
    }

    /// Send a raw line and read back the JSON response.
    fn send(&mut self, request: &str) -> serde_json::Value {
        let stdin = self.child.stdin.as_mut().expect("stdin handle");
        writeln!(stdin, "{}", request).expect("write to stdin");
        stdin.flush().expect("flush stdin");

        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read from stdout");
        assert!(
            !line.is_empty(),
            "expected a response line but got EOF from aft"
        );
        serde_json::from_str(line.trim()).expect("parse response JSON")
    }

    /// Send a raw line that should produce no response (e.g. empty line).
    /// Verifies the process is still alive by sending a follow-up ping.
    fn send_silent(&mut self, request: &str) {
        let stdin = self.child.stdin.as_mut().expect("stdin handle");
        writeln!(stdin, "{}", request).expect("write to stdin");
        stdin.flush().expect("flush stdin");
    }

    /// Close stdin and wait for the process to exit. Returns the exit status.
    fn shutdown(mut self) -> std::process::ExitStatus {
        drop(self.child.stdin.take());
        self.child.wait().expect("wait for process exit")
    }

    /// Read stderr contents after process exits.
    fn stderr_output(mut self) -> (std::process::ExitStatus, String) {
        drop(self.child.stdin.take());
        let status = self.child.wait().expect("wait for process exit");
        let mut stderr_content = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            use std::io::Read;
            stderr.read_to_string(&mut stderr_content).ok();
        }
        (status, stderr_content)
    }
}

#[test]
fn test_sequential_commands() {
    let mut aft = AftProcess::spawn();
    let total_commands = 120;

    for i in 0..total_commands {
        let id = format!("seq-{}", i);

        if i % 3 == 0 {
            // ping
            let resp = aft.send(&format!(r#"{{"id":"{}","command":"ping"}}"#, id));
            assert_eq!(resp["id"], id, "ping response id mismatch at {}", i);
            assert_eq!(resp["ok"], true, "ping should succeed at {}", i);
            assert_eq!(resp["command"], "pong", "ping should return pong at {}", i);
        } else if i % 3 == 1 {
            // version
            let resp = aft.send(&format!(r#"{{"id":"{}","command":"version"}}"#, id));
            assert_eq!(resp["id"], id, "version response id mismatch at {}", i);
            assert_eq!(resp["ok"], true, "version should succeed at {}", i);
            assert!(
                resp["version"].is_string(),
                "version should include version string at {}",
                i
            );
        } else {
            // echo
            let msg = format!("message-{}", i);
            let resp = aft.send(&format!(
                r#"{{"id":"{}","command":"echo","message":"{}"}}"#,
                id, msg
            ));
            assert_eq!(resp["id"], id, "echo response id mismatch at {}", i);
            assert_eq!(resp["ok"], true, "echo should succeed at {}", i);
            assert_eq!(resp["message"], msg, "echo message mismatch at {}", i);
        }
    }

    // Verify clean shutdown after the long run
    let status = aft.shutdown();
    assert!(
        status.success(),
        "process should exit 0 after {} commands, got {:?}",
        total_commands,
        status
    );

    eprintln!(
        "[test] test_sequential_commands: sent and verified {} commands",
        total_commands
    );
}

#[test]
fn test_malformed_json_recovery() {
    let mut aft = AftProcess::spawn();

    // 1. Garbage text → parse error response
    let resp = aft.send("this is not json at all");
    assert_eq!(resp["id"], "_parse_error", "parse error should use sentinel id");
    assert_eq!(resp["ok"], false, "parse error should be ok: false");
    assert_eq!(resp["code"], "parse_error", "parse error should have code");
    assert!(
        resp["message"].as_str().unwrap().contains("failed to parse"),
        "parse error message should describe failure"
    );

    // 2. Valid command after garbage → proves recovery
    let resp = aft.send(r#"{"id":"after-garbage","command":"ping"}"#);
    assert_eq!(resp["id"], "after-garbage");
    assert_eq!(resp["ok"], true, "process should recover after garbage input");
    assert_eq!(resp["command"], "pong");

    // 3. Empty line → should be skipped, no response
    aft.send_silent("");
    // Verify process is still alive with a follow-up command
    let resp = aft.send(r#"{"id":"after-empty","command":"ping"}"#);
    assert_eq!(resp["id"], "after-empty");
    assert_eq!(resp["ok"], true, "process should survive empty line");

    // 4. Whitespace-only line → also skipped
    aft.send_silent("   ");
    let resp = aft.send(r#"{"id":"after-whitespace","command":"ping"}"#);
    assert_eq!(resp["id"], "after-whitespace");
    assert_eq!(resp["ok"], true, "process should survive whitespace line");

    // 5. Partial/truncated JSON → parse error
    let resp = aft.send(r#"{"id":"partial","command":"pin"#);
    assert_eq!(resp["id"], "_parse_error");
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["code"], "parse_error");

    // 6. Valid command after partial JSON → recovery
    let resp = aft.send(r#"{"id":"after-partial","command":"version"}"#);
    assert_eq!(resp["id"], "after-partial");
    assert_eq!(resp["ok"], true, "process should recover after partial JSON");

    // 7. Valid JSON but missing required fields → parse error
    let resp = aft.send(r#"{"foo":"bar"}"#);
    assert_eq!(resp["id"], "_parse_error");
    assert_eq!(resp["ok"], false);

    // 8. Recovery after missing-fields error
    let resp = aft.send(r#"{"id":"after-missing","command":"ping"}"#);
    assert_eq!(resp["id"], "after-missing");
    assert_eq!(resp["ok"], true, "process should recover after missing fields");

    let status = aft.shutdown();
    assert!(status.success());

    eprintln!("[test] test_malformed_json_recovery: all 8 recovery scenarios passed");
}

#[test]
fn test_unknown_command() {
    let mut aft = AftProcess::spawn();

    // Unknown command → structured error
    let resp = aft.send(r#"{"id":"unk1","command":"nonexistent"}"#);
    assert_eq!(resp["id"], "unk1");
    assert_eq!(resp["ok"], false, "unknown command should return ok: false");
    assert_eq!(
        resp["code"], "unknown_command",
        "error code should be unknown_command"
    );
    assert!(
        resp["message"].as_str().unwrap().contains("nonexistent"),
        "error message should mention the command name"
    );

    // Process should still be alive after unknown command
    let resp = aft.send(r#"{"id":"unk2","command":"ping"}"#);
    assert_eq!(resp["id"], "unk2");
    assert_eq!(resp["ok"], true, "process should continue after unknown command");

    // Another unknown command with different name
    let resp = aft.send(r#"{"id":"unk3","command":"foobar"}"#);
    assert_eq!(resp["id"], "unk3");
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["code"], "unknown_command");
    assert!(resp["message"].as_str().unwrap().contains("foobar"));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn test_clean_shutdown() {
    let mut aft = AftProcess::spawn();

    // Send a few commands to confirm the process is alive
    for i in 0..5 {
        let resp = aft.send(&format!(r#"{{"id":"sd-{}","command":"ping"}}"#, i));
        assert_eq!(resp["id"], format!("sd-{}", i));
        assert_eq!(resp["ok"], true);
    }

    // Close stdin → process should exit cleanly
    let (status, stderr) = aft.stderr_output();

    assert!(
        status.success(),
        "expected exit code 0 on stdin EOF, got {:?}",
        status
    );

    // Verify stderr contains the expected lifecycle messages
    assert!(
        stderr.contains("[aft] started"),
        "stderr should contain startup banner, got: {}",
        stderr
    );
    assert!(
        stderr.contains("[aft] stdin closed, shutting down"),
        "stderr should contain shutdown banner, got: {}",
        stderr
    );
}
