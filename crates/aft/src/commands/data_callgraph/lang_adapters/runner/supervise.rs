//! Runs one adapter process under supervision (owner: R1).
//!
//! The adapter gets its own process group. Its stdout and stderr are read at the
//! same time (a full pipe must never stop it). When the time limit passes, the
//! whole group gets SIGTERM, then SIGKILL after the grace. No child may survive:
//! after the leader is gone the group gets SIGKILL once more for strays.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::super::contract::STDERR_TAIL_LINES;

/// An adapter that prints more than this is cut off: it is a runaway, not a result.
const MAX_STDOUT_BYTES: usize = 1 << 30;
/// Adapter stderr lines copied into the runner log (the tail is always kept).
const LOGGED_STDERR_LINES: usize = 5_000;
const LOGGED_LINE_CHARS: usize = 500;
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exit {
    Code(i32),
    Signal(i32),
    TimedOut(Duration),
    OutputTooBig,
}

pub(crate) struct Ran {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    /// The last `STDERR_TAIL_LINES` lines of stderr.
    pub stderr_tail: Vec<String>,
}

fn signal_group(pgid: i32, signal: i32) {
    // ESRCH (no such group) is the normal answer when everything is gone.
    unsafe {
        libc::kill(-pgid, signal);
    }
}

/// Runs `program args…` in `root` with stdin empty and `env` added to aft's own environment.
pub(crate) fn run_adapter(
    program: &Path,
    args: &[String],
    root: &Path,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    grace: Duration,
) -> Result<Ran, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(root)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("cannot start the adapter {}: {error}", program.display()))?;
    let pgid = child.id() as i32;
    let too_big = Arc::new(AtomicBool::new(false));

    let mut stdout_pipe = child.stdout.take().expect("stdout is piped");
    let flag = Arc::clone(&too_big);
    let stdout_reader = thread::spawn(move || {
        let mut bytes: Vec<u8> = Vec::new();
        let mut chunk = vec![0u8; 1 << 16];
        loop {
            match stdout_pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    // Over the limit: keep draining so the adapter is not blocked, store nothing more.
                    if bytes.len() + read > MAX_STDOUT_BYTES {
                        flag.store(true, Ordering::SeqCst);
                    } else {
                        bytes.extend_from_slice(&chunk[..read]);
                    }
                }
            }
        }
        bytes
    });
    let stderr_pipe = child.stderr.take().expect("stderr is piped");
    let stderr_reader = thread::spawn(move || {
        let mut tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL_LINES + 1);
        let mut reader = BufReader::new(stderr_pipe);
        let mut line: Vec<u8> = Vec::new();
        let mut logged = 0usize;
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&line).trim_end().to_string();
            if logged < LOGGED_STDERR_LINES {
                logged += 1;
                eprintln!("[adapter] {}", text.chars().take(LOGGED_LINE_CHARS).collect::<String>());
            }
            if tail.len() == STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(text);
        }
        tail.into_iter().collect::<Vec<String>>()
    });

    let started = Instant::now();
    let mut exit: Option<Exit> = None;
    let mut cut: Option<Exit> = None;
    while exit.is_none() {
        match child.try_wait() {
            Err(error) => return Err(format!("cannot wait for the adapter: {error}")),
            Ok(Some(status)) => {
                exit = Some(match (status.code(), status.signal()) {
                    (Some(code), _) => Exit::Code(code),
                    (None, Some(signal)) => Exit::Signal(signal),
                    (None, None) => Exit::Code(-1),
                });
            }
            Ok(None) => {
                if cut.is_none() && started.elapsed() >= timeout {
                    cut = Some(Exit::TimedOut(timeout));
                } else if cut.is_none() && too_big.load(Ordering::SeqCst) {
                    cut = Some(Exit::OutputTooBig);
                }
                if cut.is_some() {
                    exit = Some(stop_group(&mut child, pgid, grace, cut.expect("cut was just set")));
                } else {
                    thread::sleep(POLL);
                }
            }
        }
    }
    // The leader is gone. Strays in the group must not outlive the run (and must not hold the pipes).
    signal_group(pgid, libc::SIGKILL);
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr_tail = stderr_reader.join().unwrap_or_default();
    let exit = if too_big.load(Ordering::SeqCst) && cut.is_none() { Exit::OutputTooBig } else { exit.expect("loop ends with an exit") };
    Ok(Ran { exit, stdout, stderr_tail })
}

/// SIGTERM to the group, wait up to `grace` for the leader, then SIGKILL.
/// Returns `reason` (why the run was cut).
fn stop_group(child: &mut std::process::Child, pgid: i32, grace: Duration, reason: Exit) -> Exit {
    signal_group(pgid, libc::SIGTERM);
    let asked = Instant::now();
    while asked.elapsed() < grace {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return reason;
        }
        thread::sleep(POLL);
    }
    signal_group(pgid, libc::SIGKILL);
    let _ = child.wait();
    reason
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, timeout: Duration, grace: Duration) -> Ran {
        run_adapter(Path::new("/bin/sh"), &["-c".to_string(), script.to_string()], Path::new("/"), &BTreeMap::new(), timeout, grace).unwrap()
    }

    #[test]
    fn stdout_and_stderr_are_read_together_and_the_tail_is_kept() {
        // 300 KB on each stream: a reader that handled one stream at a time would deadlock.
        let ran = sh(
            "i=0; while [ $i -lt 3000 ]; do echo \"err line $i xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\" >&2; echo \"out $i xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"; i=$((i+1)); done; exit 3",
            Duration::from_secs(60),
            Duration::from_secs(1),
        );
        assert_eq!(ran.exit, Exit::Code(3));
        assert!(ran.stdout.len() > 250_000);
        assert_eq!(ran.stderr_tail.len(), STDERR_TAIL_LINES);
        assert!(ran.stderr_tail.last().unwrap().starts_with("err line 2999"));
    }

    #[test]
    fn a_timeout_ends_the_whole_group_including_children() {
        let marker = format!("sleep 86{}", 100 + std::process::id() % 800);
        let started = Instant::now();
        let ran = sh(&format!("{marker} & echo started; wait"), Duration::from_millis(300), Duration::from_secs(5));
        assert_eq!(ran.exit, Exit::TimedOut(Duration::from_millis(300)));
        assert!(started.elapsed() < Duration::from_secs(4));
        thread::sleep(Duration::from_millis(300));
        let alive = Command::new("pgrep").args(["-f", &marker]).output().unwrap();
        assert!(alive.stdout.is_empty(), "a child of the adapter is still alive");
    }

    #[test]
    fn a_program_that_ignores_sigterm_is_killed_after_the_grace() {
        let started = Instant::now();
        let ran = sh("trap '' TERM; while true; do sleep 1; done", Duration::from_millis(200), Duration::from_millis(600));
        assert!(matches!(ran.exit, Exit::TimedOut(_)));
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn a_stray_in_the_group_does_not_survive_a_normal_exit() {
        let marker = format!("sleep 85{}", 100 + std::process::id() % 800);
        let ran = sh(&format!("{marker} >/dev/null 2>&1 & exit 0"), Duration::from_secs(10), Duration::from_secs(1));
        assert_eq!(ran.exit, Exit::Code(0));
        thread::sleep(Duration::from_millis(300));
        let alive = Command::new("pgrep").args(["-f", &marker]).output().unwrap();
        assert!(alive.stdout.is_empty(), "a stray child is still alive");
    }
}
