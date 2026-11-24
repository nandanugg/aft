//! Adapter runner (owner: R1).
//!
//! One run = `run_one`: take the lock of the (root, language) pair, take one of
//! the machine-wide run slots (decision B), run the adapter under supervision,
//! validate its output, and store it (temporary file, then rename) or record the
//! failure. A query never runs this itself. It starts the detached runner
//! `aft adapters run --root <repo> --language <l>` (`spawn_detached`), which
//! lives in its own session and survives the one-shot process. `aft adapters
//! build` calls `run_one` in its own process and waits.
//!
//! Failure rules (CONTRACT.md section 6): a failed run and a partial run both
//! keep a record and are not started again for the same key before the entry's
//! `failure_cooldown_s`; `--force` starts at once; a new commit is a new key.

mod lock;
mod supervise;

use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use super::contract::{parse_output, RegistryEntry, GLOBAL_RUN_LIMIT, KILL_GRACE};
use super::key::{self, Key};
use super::store::{self, FailureRecord, KeyState, Layout, Meta};

pub(crate) use lock::is_locked;
use lock::{acquire_slot, try_lock_for};
use supervise::{run_adapter, Exit};

/// How long a run waits for a free machine-wide slot before it gives up (the next query starts it again).
const SLOT_WAIT: Duration = Duration::from_secs(15 * 60);
/// How long a runner tries for the (root, language) lock before it decides someone else holds it.
const LOCK_WAIT: Duration = Duration::from_millis(300);
/// The runner log grows only by appends; above this size a new runner starts it again.
const LOG_LIMIT_BYTES: u64 = 1 << 20;

/// What `run_one` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The overlay for the key is stored (fresh or partial).
    Built { partial: bool },
    /// The run failed; the record is stored. The text is the reason.
    Failed(String),
    /// No run was needed: the overlay exists, or the key is cooling down.
    NothingToDo(String),
    /// Another process holds the lock of this (root, language): it is running now.
    AlreadyRunning,
    /// The root cannot be served (no commit, dirty tree, …), so nothing ran.
    Skipped(String),
    /// No machine-wide slot became free in time.
    NoSlot,
    /// The run finished but HEAD or the tree changed meanwhile, so its result was thrown away.
    Discarded(String),
}

/// Whether a run for this key is due: nothing is stored, or the failure/partial
/// cooldown has passed (decision H).
pub(crate) fn needs_run(state: &KeyState, entry: &RegistryEntry, now_unix_s: u64) -> bool {
    let cooldown = entry.failure_cooldown().as_secs();
    let failed_at = state.failure.as_ref().map(|record| record.at_unix_s);
    match (&state.meta, failed_at) {
        (Some(meta), _) if !meta.partial => false,
        (Some(meta), failed_at) => now_unix_s >= meta.built_at_unix_s.max(failed_at.unwrap_or(0)) + cooldown,
        (None, Some(failed_at)) => now_unix_s >= failed_at + cooldown,
        (None, None) => true,
    }
}

/// `argv` of the adapter: the entry's command with `{root}` replaced.
pub(crate) fn adapter_args(entry: &RegistryEntry, root: &Path) -> Vec<String> {
    let root = root.to_string_lossy();
    entry.command[1..].iter().map(|arg| arg.replace("{root}", &root)).collect()
}

/// Runs the adapter of `entry` for `root` once, unless it is running or not due.
/// `root` must be canonical. `storage` is aft's storage directory.
pub(crate) fn run_one(storage: &Path, root: &Path, entry: &RegistryEntry, force: bool) -> Outcome {
    let head = match key::check_root(root) {
        Ok(head) => head,
        Err(reason) => return Outcome::Skipped(reason),
    };
    let key = match key::make_key(storage, entry, root, &head) {
        Ok(key) => key,
        Err(reason) => return Outcome::Skipped(reason),
    };
    let adapters_root = store::adapters_root(storage);
    let layout = Layout::new(&adapters_root, root, &entry.language);
    let key_id = key.id();

    let _run_lock = match try_lock_for(&layout.lock_path(), LOCK_WAIT) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Outcome::AlreadyRunning,
        Err(error) => return Outcome::Skipped(format!("cannot take the run lock {}: {error}", layout.lock_path().display())),
    };
    let state = store::key_state(&layout, &key_id);
    if !force && !needs_run(&state, entry, store::now_unix_s()) {
        return Outcome::NothingToDo("an overlay exists for this key, or its failure cooldown has not passed".to_string());
    }

    let _slot = match acquire_slot(&adapters_root.join("slots"), SLOT_WAIT) {
        Ok(Some(slot)) => slot,
        Ok(None) => return Outcome::NoSlot,
        Err(error) => return Outcome::Skipped(format!("cannot take a run slot: {error}")),
    };
    // A run that waited for a slot may be late: the key could have moved on.
    if key::read_head(root).ok().as_deref() != Some(key.head.as_str()) {
        return Outcome::Discarded("HEAD changed before the adapter started".to_string());
    }
    execute(root, entry, &key, &layout)
}

/// The adapter run itself, with the lock and a slot held.
fn execute(root: &Path, entry: &RegistryEntry, key: &Key, layout: &Layout) -> Outcome {
    let key_id = key.id();
    let fail = |reason: String, stderr_tail: Vec<String>| -> Outcome {
        let record = FailureRecord { reason: reason.clone(), at_unix_s: store::now_unix_s(), stderr_tail };
        if let Err(error) = store::write_failure(layout, &key_id, &record) {
            eprintln!("aft adapters: {error}");
        }
        eprintln!("aft adapters: {} run for {} failed: {reason}", entry.language, root.display());
        Outcome::Failed(reason)
    };

    let program = match key::resolve_program(&entry.command[0]) {
        Ok(program) => program,
        Err(reason) => return fail(reason, Vec::new()),
    };
    eprintln!("aft adapters: {} run for {} starts (key {key_id}, head {})", entry.language, root.display(), key::short(&key.head));
    let ran = match run_adapter(&program, &adapter_args(entry, root), root, &entry.env, entry.timeout(), KILL_GRACE) {
        Ok(ran) => ran,
        Err(reason) => return fail(reason, Vec::new()),
    };
    let reason = match ran.exit {
        Exit::Code(0) => None,
        Exit::Code(code) => Some(format!("adapter exited with code {code}")),
        Exit::Signal(signal) => Some(format!("adapter was killed by signal {signal}")),
        Exit::TimedOut(limit) => Some(format!("timeout after {} s", limit.as_secs())),
        Exit::OutputTooBig => Some("adapter printed more than 1 GiB on stdout".to_string()),
    };
    if let Some(reason) = reason {
        return fail(reason, ran.stderr_tail);
    }

    let mut output = match parse_output(&ran.stdout, entry) {
        Ok(output) => output,
        Err(violation) => return fail(violation.to_string(), ran.stderr_tail),
    };
    drop(ran.stdout);
    let sites = match store::sites_with_offsets(root, std::mem::take(&mut output.sites)) {
        Ok(sites) => sites,
        Err(reason) => return fail(reason, ran.stderr_tail),
    };
    // The repository must be as it was when the run began, or the positions mean nothing.
    if key::read_head(root).ok().as_deref() != Some(key.head.as_str()) {
        return Outcome::Discarded("HEAD changed while the adapter ran".to_string());
    }
    if key::tree_state(root) != key::TreeState::Clean {
        return Outcome::Discarded("tracked files changed while the adapter ran".to_string());
    }
    let meta = Meta::of_output(&output, key, store::now_unix_s());
    if let Err(reason) = store::write_overlay(&layout.overlay_path(&key_id), &meta, &output, &sites) {
        return fail(reason, ran.stderr_tail);
    }
    store::remove_other_keys(layout, &key_id);
    let _ = std::fs::remove_file(layout.failure_path(&key_id));
    eprintln!(
        "aft adapters: {} overlay stored for {}: {} sites, {} facts{}",
        entry.language,
        root.display(),
        meta.counts.sites,
        meta.counts.facts,
        if meta.partial { " (partial)" } else { "" }
    );
    Outcome::Built { partial: meta.partial }
}

/// Starts `aft adapters run --root <root> --language <language>` in its own
/// session, with stdio to the runner log, and does not wait for it. The caller
/// keeps no handle; a small thread reaps the child if this process lives on.
pub(crate) fn spawn_detached(exe: &Path, storage: &Path, root: &Path, language: &str, layout: &Layout, force: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(&layout.dir)?;
    let log_path = layout.log_path();
    let too_big = std::fs::metadata(&log_path).map(|meta| meta.len() > LOG_LIMIT_BYTES).unwrap_or(false);
    let log = OpenOptions::new().create(true).append(true).truncate(false).open(&log_path);
    let log = match (log, too_big) {
        (Ok(file), true) => {
            file.set_len(0)?;
            file
        }
        (Ok(file), false) => file,
        (Err(error), _) => return Err(error),
    };
    let mut command = Command::new(exe);
    command.args(["adapters", "run", "--root"]).arg(root).args(["--language", language]);
    if force {
        command.arg("--force");
    }
    // The runner must find the same storage directory as the process that started it.
    command.env("AFT_STORAGE_DIR", storage);
    command.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // Own session: no terminal hangup, no process-group signal of the caller reaches it.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The documented machine-wide limit, for messages.
pub(crate) fn global_run_limit() -> usize {
    GLOBAL_RUN_LIMIT
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tests_dir() -> PathBuf {
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR for tests")).join("../../tests/adapters")
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A committed copy of the tiny Go + JS repository.
    fn tiny_repo(dir: &Path) -> std::path::PathBuf {
        let root = dir.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        for entry in std::fs::read_dir(tests_dir().join("fixtures/tiny-mixed")).unwrap().flatten() {
            std::fs::copy(entry.path(), root.join(entry.file_name())).unwrap();
        }
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "fixture"]);
        root.canonicalize().unwrap()
    }

    fn entry(fake: &str, timeout_s: u64) -> RegistryEntry {
        let program = tests_dir().join("fake").join(fake).canonicalize().unwrap();
        serde_json::from_value(serde_json::json!({
            "contract": 1, "language": "go", "root_markers": ["go.mod"],
            "command": [program.to_str().unwrap(), "--language", "go", "--root", "{root}"],
            "timeout_s": timeout_s, "failure_cooldown_s": 600, "hidden_tags": ["mock"],
            "facts": [{"kind": "implements", "description": "type -> interface"}]
        }))
        .unwrap()
    }

    fn state_of(storage: &Path, root: &Path, entry: &RegistryEntry) -> (Layout, String, KeyState) {
        let key = key::make_key(storage, entry, root, &key::read_head(root).unwrap()).unwrap();
        let layout = Layout::new(&store::adapters_root(storage), root, &entry.language);
        let state = store::key_state(&layout, &key.id());
        (layout, key.id(), state)
    }

    #[test]
    fn a_good_run_is_stored_and_not_repeated_until_forced() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let root = tiny_repo(dir.path());
        let entry = entry("ok.sh", 20);
        assert_eq!(run_one(&storage, &root, &entry, false), Outcome::Built { partial: false });
        let (_, _, state) = state_of(&storage, &root, &entry);
        assert_eq!(state.meta.unwrap().counts.sites, 2);
        assert!(matches!(run_one(&storage, &root, &entry, false), Outcome::NothingToDo(_)));
        assert_eq!(run_one(&storage, &root, &entry, true), Outcome::Built { partial: false });
    }

    #[test]
    fn each_failure_rule_is_a_failure_record_with_its_reason() {
        let cases = [
            ("crash.sh", "signal 6"),
            ("exit_nonzero.sh", "exited with code 3"),
            ("no_end.sh", "no end record"),
            ("partial_line.sh", "incomplete"),
            ("wrong_contract.sh", "header contract 2"),
            ("undeclared_fact.sh", "not declared"),
            ("count_mismatch.sh", "do not match"),
        ];
        for (fake, expected) in cases {
            let dir = tempfile::tempdir().unwrap();
            let storage = dir.path().join("storage");
            let root = tiny_repo(dir.path());
            let entry = entry(fake, 20);
            let outcome = run_one(&storage, &root, &entry, false);
            assert!(matches!(&outcome, Outcome::Failed(reason) if reason.contains(expected)), "{fake}: {outcome:?}");
            let (_, _, state) = state_of(&storage, &root, &entry);
            assert!(state.meta.is_none(), "{fake}: a failed run must store no overlay");
            let record = state.failure.expect("failure record");
            assert!(record.reason.contains(expected));
            // Cooldown: no second run of the same key; --force runs.
            assert!(matches!(run_one(&storage, &root, &entry, false), Outcome::NothingToDo(_)), "{fake}");
            assert!(matches!(run_one(&storage, &root, &entry, true), Outcome::Failed(_)), "{fake}");
        }
    }

    #[test]
    fn the_stderr_tail_of_a_failure_keeps_the_last_twenty_lines() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let root = tiny_repo(dir.path());
        let entry = entry("exit_nonzero.sh", 20);
        run_one(&storage, &root, &entry, false);
        let (_, _, state) = state_of(&storage, &root, &entry);
        let tail = state.failure.unwrap().stderr_tail;
        assert_eq!(tail.len(), 20);
        assert_eq!(tail.first().unwrap(), "fake stderr line 11");
        assert_eq!(tail.last().unwrap(), "fake stderr line 30");
    }

    #[test]
    fn a_partial_run_is_served_and_retried_after_the_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let root = tiny_repo(dir.path());
        let entry = entry("partial.sh", 20);
        assert_eq!(run_one(&storage, &root, &entry, false), Outcome::Built { partial: true });
        let (_, _, state) = state_of(&storage, &root, &entry);
        let meta = state.meta.clone().unwrap();
        assert!(meta.partial);
        assert_eq!(meta.unanalysed, vec!["example.com/tiny/b".to_string()]);
        let built = meta.built_at_unix_s;
        assert!(!needs_run(&state, &entry, built + 599));
        assert!(needs_run(&state, &entry, built + 600));
        assert!(matches!(run_one(&storage, &root, &entry, false), Outcome::NothingToDo(_)));
    }

    #[test]
    fn a_new_commit_is_a_new_key_and_replaces_the_old_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let root = tiny_repo(dir.path());
        let entry = entry("ok.sh", 20);
        run_one(&storage, &root, &entry, false);
        let (layout, old_id, _) = state_of(&storage, &root, &entry);
        std::fs::write(root.join("b.go"), format!("{}\n// more\n", std::fs::read_to_string(root.join("b.go")).unwrap())).unwrap();
        git(&root, &["commit", "-q", "-am", "second"]);
        let (_, new_id, state) = state_of(&storage, &root, &entry);
        assert_ne!(old_id, new_id);
        assert!(state.meta.is_none() && state.other_head.is_some(), "the old overlay shows as stale");
        assert_eq!(run_one(&storage, &root, &entry, false), Outcome::Built { partial: false });
        assert!(layout.overlay_path(&new_id).exists() && !layout.overlay_path(&old_id).exists());
    }

    #[test]
    fn no_commit_and_a_dirty_tree_run_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let root = tiny_repo(dir.path());
        let entry = entry("ok.sh", 20);
        std::fs::write(root.join("a.go"), "package tiny\n// local\n").unwrap();
        assert!(matches!(run_one(&storage, &root, &entry, false), Outcome::Skipped(reason) if reason.contains("local changes")));
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        std::fs::write(plain.join("go.mod"), "module x\n").unwrap();
        assert!(matches!(run_one(&storage, &plain.canonicalize().unwrap(), &entry, false), Outcome::Skipped(reason) if reason.contains("commit")));
    }

    #[test]
    fn a_timeout_fails_the_run_and_leaves_no_child() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let root = tiny_repo(dir.path());
        let entry = entry("hang_child.sh", 1);
        let outcome = run_one(&storage, &root, &entry, false);
        assert!(matches!(&outcome, Outcome::Failed(reason) if reason.contains("timeout after 1 s")), "{outcome:?}");
        thread::sleep(Duration::from_millis(300));
        let alive = Command::new("pgrep").args(["-f", "sleep 86398"]).output().unwrap();
        assert!(alive.stdout.is_empty(), "the adapter's child is still alive");
    }

    #[test]
    fn a_held_lock_means_already_running() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let root = tiny_repo(dir.path());
        let entry = entry("ok.sh", 20);
        let layout = Layout::new(&store::adapters_root(&storage), &root, "go");
        let held = lock::try_lock(&layout.lock_path()).unwrap().unwrap();
        assert!(is_locked(&layout.lock_path()));
        assert_eq!(run_one(&storage, &root, &entry, false), Outcome::AlreadyRunning);
        drop(held);
        assert_eq!(run_one(&storage, &root, &entry, false), Outcome::Built { partial: false });
    }

    #[test]
    fn the_command_gets_the_root_in_place_of_the_placeholder() {
        let entry = entry("ok.sh", 20);
        let args = adapter_args(&entry, Path::new("/repo/x"));
        assert_eq!(args, vec!["--language", "go", "--root", "/repo/x"]);
    }
}
