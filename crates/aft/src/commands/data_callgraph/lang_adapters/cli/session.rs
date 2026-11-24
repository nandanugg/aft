//! One aft process in NDJSON mode (the external protocol only): configure, then commands.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::{json, Value};

/// How long one reply may take. A cold index of a large repository can take minutes.
const DEFAULT_REPLY_TIMEOUT_S: u64 = 600;

/// How long the aft server may take to leave after its stdin closes.
const EXIT_GRACE: Duration = Duration::from_secs(5);

pub(crate) struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<Value>,
    next_id: u32,
    timeout: Duration,
}

impl Session {
    /// Starts this same executable with no arguments (NDJSON server mode) and configures it
    /// for `root`. Progress lines (`[aft] …`) on its stderr are passed to our stderr.
    pub(crate) fn start(root: &Path) -> Result<Session, String> {
        let exe = std::env::current_exe().map_err(|err| format!("cannot find the aft executable: {err}"))?;
        let mut child = Command::new(exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("cannot start aft: {err}"))?;
        let stdin = child.stdin.take().ok_or("no stdin on the aft process")?;
        let stdout = child.stdout.take().ok_or("no stdout on the aft process")?;
        let stderr = child.stderr.take().ok_or("no stderr on the aft process")?;

        let (sender, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if sender.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.starts_with("[aft]") {
                    eprintln!("{line}");
                }
            }
        });

        let mut session = Session { child, stdin: Some(stdin), replies, next_id: 0, timeout: reply_timeout() };
        let configured = session.send(json!({
            "command": "configure",
            "harness": "opencode",
            "project_root": root.to_string_lossy(),
        }))?;
        if configured.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(format!("configure failed: {}", reply_message(&configured)));
        }
        Ok(session)
    }

    /// Sends one command and returns its reply (the raw JSON object).
    pub(crate) fn send(&mut self, mut command: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = format!("cli{}", self.next_id);
        command["id"] = Value::String(id.clone());
        let mut line = command.to_string();
        line.push('\n');
        let stdin = self.stdin.as_mut().ok_or("the aft session is closed")?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|err| format!("cannot write to aft: {err}"))?;
        loop {
            match self.replies.recv_timeout(self.timeout) {
                Ok(reply) if reply.get("id").and_then(Value::as_str) == Some(id.as_str()) => return Ok(reply),
                Ok(_) => continue, // a frame for no request of ours (progress, notice)
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("no reply from aft within {} s", self.timeout.as_secs()));
                }
                Err(RecvTimeoutError::Disconnected) => return Err("aft exited before it replied".to_string()),
            }
        }
    }
}

/// How long one reply may take: `AFT_CLI_REPLY_TIMEOUT_S`, default 600 s.
pub(crate) fn reply_timeout() -> Duration {
    let seconds = std::env::var("AFT_CLI_REPLY_TIMEOUT_S").ok().and_then(|value| value.parse::<u64>().ok()).unwrap_or(DEFAULT_REPLY_TIMEOUT_S);
    Duration::from_secs(seconds)
}

impl Drop for Session {
    /// Closing stdin ends the server. If it has not left after `EXIT_GRACE`, kill it,
    /// so no aft process stays behind. (The detached adapter runner is not our child.)
    fn drop(&mut self) {
        self.stdin = None;
        let deadline = std::time::Instant::now() + EXIT_GRACE;
        while std::time::Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `code: message` of a failed reply.
pub(crate) fn reply_message(reply: &Value) -> String {
    let code = reply.get("code").and_then(Value::as_str).unwrap_or("error");
    let message = reply.get("message").and_then(Value::as_str).unwrap_or("command failed");
    format!("{code}: {message}")
}
