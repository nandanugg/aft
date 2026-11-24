//! Query-time seam (owner: R1): `languages_for_root` and `overlay_for_query`.
//!
//! A query never fails because of an adapter and never waits for one for long.
//! Every problem becomes an `AdapterStatus`. The steps for one language:
//! 1. registry entry for the language, root marker present (else `skipped`);
//! 2. the root is a git work tree with a HEAD commit and a clean tree (else `skipped`); a clean
//!    verdict is remembered and confirmed by file metadata, not by `git status` (`key.rs`);
//! 3. the key (HEAD, build id, toolchain id) and the store lookup;
//! 4. an overlay exists: serve it (the opened overlay is kept, and the same `Arc` is served again
//!    while its file is unchanged: `store::open_overlay_shared`) (`fresh`, or `partial`; a partial overlay whose
//!    cooldown has passed also starts a new run, without waiting);
//! 5. no overlay: when no run is going on and one is due, start the detached
//!    runner; wait at most min(`wait`, the entry's `query_wait_s`); then answer
//!    `fresh`/`partial` if the run finished, `failed` if it failed, else
//!    `building` (or `stale` when an overlay of an older commit is stored).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::contract::{AdapterState, AdapterStatus, Overlay, Registry, RegistryEntry, CONTRACT_VERSION};
use super::key::{self, short};
use super::registry;
use super::runner;
use super::store::{self, KeyState, Layout};

/// How often a waiting query looks at the store.
const POLL: Duration = Duration::from_millis(50);

/// The language label of the status that reports a broken registry file.
pub(crate) const REGISTRY_LANGUAGE: &str = "registry";

/// The languages whose adapters apply to `root`: R2 asks `overlay_for_query` for
/// each of them, so every reply can state each language's status.
///
/// These are the registry entries whose `root_markers` exist at the top of `root`
/// (also when the root is not a git work tree: then the status says why). A
/// registry file that exists but is broken gives one status, language `registry`,
/// that says so; a missing registry file gives none.
pub(crate) fn languages_for_root(root: &Path) -> Vec<String> {
    match registry::load() {
        Ok(loaded) => registry::languages_at_root(&loaded.registry, root),
        Err(_) => vec![REGISTRY_LANGUAGE.to_string()],
    }
}

/// The overlay of `language` for the repository at `root`, and the status to
/// report. Waits at most `wait` for a run to finish (the smaller of `wait` and
/// the entry's `query_wait_s`).
pub(crate) fn overlay_for_query(root: &Path, language: &str, wait: Duration) -> (Option<Overlay>, AdapterStatus) {
    let loaded = match registry::load() {
        Ok(loaded) => loaded,
        Err(reason) => return (None, failed_now(language, reason)),
    };
    let setup = Setup {
        storage: crate::storage_path::storage_dir(None),
        registry: &loaded.registry,
        exe: std::env::current_exe().ok(),
    };
    resolve(&setup, root, language, Mode::Query { wait })
}

/// The status of `language` at `root` as it is now: nothing is started, nothing is waited for.
/// Used by `aft adapters status` and `build`.
pub(crate) fn status_now(registry: &Registry, root: &Path, language: &str) -> AdapterStatus {
    let setup = Setup { storage: crate::storage_path::storage_dir(None), registry, exe: None };
    resolve(&setup, root, language, Mode::Peek).1
}

/// Everything `resolve` needs from outside: tests build it with a temporary storage directory.
pub(crate) struct Setup<'a> {
    pub storage: PathBuf,
    pub registry: &'a Registry,
    /// The aft executable the detached runner is started from. `None`: no runner can start.
    pub exe: Option<PathBuf>,
}

#[derive(Clone, Copy)]
pub(crate) enum Mode {
    /// A query: start a run when one is due, wait up to `wait`.
    Query { wait: Duration },
    /// Look only.
    Peek,
}

fn failed_now(language: &str, reason: String) -> AdapterStatus {
    let now = store::now_unix_s();
    AdapterStatus::new(language, AdapterState::Failed { reason, at_unix_s: now, retry_after_unix_s: 0, stderr_tail: Vec::new() })
}

fn skipped(language: &str, reason: impl Into<String>) -> (Option<Overlay>, AdapterStatus) {
    (None, AdapterStatus::skipped(language, reason))
}

pub(crate) fn resolve(setup: &Setup<'_>, root: &Path, language: &str, mode: Mode) -> (Option<Overlay>, AdapterStatus) {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let entry = match registry::entry_for_root(setup.registry, &root, language) {
        Ok(entry) => entry,
        Err(reason) => return skipped(language, reason),
    };
    if entry.contract != CONTRACT_VERSION {
        return skipped(language, format!("the registry entry speaks contract {}; this build reads {CONTRACT_VERSION}", entry.contract));
    }
    let head = match key::check_root(&root) {
        Ok(head) => head,
        Err(reason) => return skipped(language, reason),
    };
    let key = match key::make_key(&setup.storage, entry, &root, &head) {
        Ok(key) => key,
        Err(reason) => return skipped(language, reason),
    };
    let layout = Layout::new(&store::adapters_root(&setup.storage), &root, language);
    let key_id = key.id();
    let state = store::key_state(&layout, &key_id);
    let now = store::now_unix_s();

    if state.meta.is_some() {
        // Served at once. A partial overlay whose cooldown has passed is retried in the background.
        if let Mode::Query { .. } = mode {
            if runner::needs_run(&state, entry, now) && !runner::is_locked(&layout.lock_path()) {
                if let Err(error) = start_runner(setup, &root, language, &layout) {
                    eprintln!("aft adapters: {error}");
                }
            }
        }
        return served(entry, &layout, &key_id, &state, language);
    }

    let due = runner::needs_run(&state, entry, now);
    if !due {
        // Only a failure record can hold a run back here.
        let record = state.failure.as_ref().expect("a key with no overlay is not due only after a failure");
        return (None, failed_status(language, entry, record));
    }
    let running = runner::is_locked(&layout.lock_path());
    let Mode::Query { wait } = mode else {
        return (None, peek_status(language, entry, &state, &head, running));
    };

    let started = Instant::now();
    let started_unix_s = now;
    if !running {
        if let Err(error) = start_runner(setup, &root, language, &layout) {
            return skipped(language, error);
        }
    }
    let limit = wait.min(entry.query_wait());
    while started.elapsed() < limit {
        let finished = layout.overlay_path(&key_id).exists()
            || store::read_failure(&layout, &key_id).map_or(false, |record| record.at_unix_s >= started_unix_s);
        if finished {
            break;
        }
        thread::sleep(POLL.min(limit.saturating_sub(started.elapsed())));
    }
    let state = store::key_state(&layout, &key_id);
    if state.meta.is_some() {
        return served(entry, &layout, &key_id, &state, language);
    }
    if let Some(record) = state.failure.as_ref().filter(|record| record.at_unix_s >= started_unix_s) {
        return (None, failed_status(language, entry, record));
    }
    let waited_ms = started.elapsed().as_millis() as u64;
    (None, building_status(language, &state, &head, waited_ms))
}

fn start_runner(setup: &Setup<'_>, root: &Path, language: &str, layout: &Layout) -> Result<(), String> {
    let Some(exe) = &setup.exe else {
        return Err("no aft executable to start the adapter runner from".to_string());
    };
    runner::spawn_detached(exe, &setup.storage, root, language, layout, false)
        .map_err(|error| format!("cannot start the adapter runner: {error}"))
}

/// The overlay file of this key, opened, with its fresh or partial status.
fn served(entry: &RegistryEntry, layout: &Layout, key_id: &str, state: &KeyState, language: &str) -> (Option<Overlay>, AdapterStatus) {
    let meta = state.meta.as_ref().expect("served is called with an overlay");
    let overlay = match store::open_overlay_shared(&layout.overlay_path(key_id), entry.hidden_tags.clone()) {
        Ok(overlay) => overlay,
        Err(reason) => return (None, failed_now(language, reason)),
    };
    let state = if meta.partial {
        AdapterState::Partial { sites: meta.counts.sites, facts: meta.counts.facts, unanalysed: meta.unanalysed.clone() }
    } else {
        AdapterState::Fresh { sites: meta.counts.sites, facts: meta.counts.facts }
    };
    (Some(overlay as Overlay), AdapterStatus::new(language, state))
}

fn failed_status(language: &str, entry: &RegistryEntry, record: &store::FailureRecord) -> AdapterStatus {
    AdapterStatus::new(
        language,
        AdapterState::Failed {
            reason: record.reason.clone(),
            at_unix_s: record.at_unix_s,
            retry_after_unix_s: record.at_unix_s + entry.failure_cooldown().as_secs(),
            stderr_tail: record.stderr_tail.clone(),
        },
    )
}

fn building_status(language: &str, state: &KeyState, head: &str, waited_ms: u64) -> AdapterStatus {
    match &state.other_head {
        Some(overlay_head) => AdapterStatus::new(language, AdapterState::Stale { overlay_head: short(overlay_head).to_string(), tree_head: short(head).to_string() }),
        None => AdapterStatus::new(language, AdapterState::Building { waited_ms }),
    }
}

/// The status when nothing is stored for the key and nothing is waited for.
fn peek_status(language: &str, entry: &RegistryEntry, state: &KeyState, head: &str, running: bool) -> AdapterStatus {
    if running {
        return building_status(language, state, head, 0);
    }
    if let Some(record) = &state.failure {
        return failed_status(language, entry, record);
    }
    if state.other_head.is_some() {
        return building_status(language, state, head, 0);
    }
    AdapterStatus::skipped(language, "no overlay is built yet and no run is going on; the next query or `aft adapters build` starts one")
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::contract::OverlayRead;
    use std::process::Command;

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

    struct Fixture {
        _dir: tempfile::TempDir,
        storage: PathBuf,
        root: PathBuf,
        registry: Registry,
    }

    fn fixture(fake: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        for entry in std::fs::read_dir(tests_dir().join("fixtures/tiny-mixed")).unwrap().flatten() {
            std::fs::copy(entry.path(), root.join(entry.file_name())).unwrap();
        }
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "fixture"]);
        let program = tests_dir().join("fake").join(fake).canonicalize().unwrap();
        let registry: Registry = serde_json::from_value(serde_json::json!({"adapters": [{
            "contract": 1, "language": "go", "root_markers": ["go.mod"],
            "command": [program.to_str().unwrap(), "--language", "go", "--root", "{root}"],
            "timeout_s": 20, "query_wait_s": 2, "hidden_tags": ["mock"],
            "facts": [{"kind": "implements", "description": "type -> interface"}]
        }]}))
        .unwrap();
        Fixture { storage: dir.path().join("storage"), root: root.canonicalize().unwrap(), registry, _dir: dir }
    }

    fn query(fixture: &Fixture, mode: Mode, exe: Option<&str>) -> (Option<Overlay>, AdapterStatus) {
        let setup = Setup { storage: fixture.storage.clone(), registry: &fixture.registry, exe: exe.map(PathBuf::from) };
        resolve(&setup, &fixture.root, "go", mode)
    }

    fn state_word(status: &AdapterStatus) -> String {
        serde_json::to_value(status).unwrap()["status"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_stored_overlay_is_served_without_a_run() {
        let fixture = fixture("ok.sh");
        let entry = &fixture.registry.adapters[0];
        let root = fixture.root.clone();
        assert_eq!(runner::run_one(&fixture.storage, &root, entry, false), runner::Outcome::Built { partial: false });
        // No executable to start a runner from: a stored overlay must not need one.
        let (overlay, status) = query(&fixture, Mode::Query { wait: Duration::ZERO }, None);
        assert_eq!(state_word(&status), "fresh");
        let overlay = overlay.expect("overlay");
        assert_eq!(overlay.counts().sites, 2);
        assert_eq!(overlay.sites_in_file("a.go").len(), 1);
        assert!(overlay.is_analysed("b.go"));
    }

    #[test]
    fn a_partial_overlay_is_served_with_the_unanalysed_names() {
        let fixture = fixture("partial.sh");
        runner::run_one(&fixture.storage, &fixture.root.clone(), &fixture.registry.adapters[0], false);
        let (overlay, status) = query(&fixture, Mode::Query { wait: Duration::ZERO }, None);
        assert!(overlay.is_some() && status.serves_edges());
        assert!(status.reason_line().contains("example.com/tiny/b"), "{}", status.reason_line());
    }

    #[test]
    fn a_failure_in_its_cooldown_is_reported_and_starts_nothing() {
        let fixture = fixture("crash.sh");
        runner::run_one(&fixture.storage, &fixture.root.clone(), &fixture.registry.adapters[0], false);
        let (overlay, status) = query(&fixture, Mode::Query { wait: Duration::ZERO }, None);
        assert!(overlay.is_none());
        match status.state {
            AdapterState::Failed { reason, at_unix_s, retry_after_unix_s, .. } => {
                assert!(reason.contains("signal 6"));
                assert_eq!(retry_after_unix_s, at_unix_s + 600);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_query_with_no_overlay_starts_the_runner_and_answers_building_at_once() {
        let fixture = fixture("ok.sh");
        // /usr/bin/true stands in for aft: the process starts and does nothing.
        let (overlay, status) = query(&fixture, Mode::Query { wait: Duration::ZERO }, Some("/usr/bin/true"));
        assert!(overlay.is_none());
        assert_eq!(state_word(&status), "building", "{status:?}");
        let layout = Layout::new(&store::adapters_root(&fixture.storage), &fixture.root, "go");
        assert!(layout.log_path().exists(), "the runner log is created at spawn");
        // Without an executable the problem is a status, not a failure of the query.
        let (_, status) = query(&fixture, Mode::Query { wait: Duration::ZERO }, None);
        assert_eq!(state_word(&status), "skipped");
    }

    #[test]
    fn a_peek_never_starts_anything() {
        let fixture = fixture("ok.sh");
        let (_, status) = query(&fixture, Mode::Peek, Some("/usr/bin/true"));
        assert_eq!(state_word(&status), "skipped");
        assert!(status.reason_line().contains("no overlay is built yet"));
        let layout = Layout::new(&store::adapters_root(&fixture.storage), &fixture.root, "go");
        assert!(!layout.log_path().exists());
    }

    #[test]
    fn the_wait_is_bounded_by_the_smaller_limit() {
        let fixture = fixture("ok.sh");
        let started = Instant::now();
        let (_, status) = query(&fixture, Mode::Query { wait: Duration::from_millis(300) }, Some("/usr/bin/true"));
        assert_eq!(state_word(&status), "building");
        assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
        if let AdapterState::Building { waited_ms } = status.state {
            assert!(waited_ms >= 250);
        }
    }

    #[test]
    fn a_root_that_cannot_be_served_says_why() {
        let fixture = fixture("ok.sh");
        // dirty tree
        std::fs::write(fixture.root.join("a.go"), "package tiny\n// local\n").unwrap();
        let (_, status) = query(&fixture, Mode::Query { wait: Duration::ZERO }, None);
        assert_eq!(state_word(&status), "skipped");
        assert!(status.reason_line().contains("local changes"));
        // marker absent
        std::fs::remove_file(fixture.root.join("go.mod")).unwrap();
        let (_, status) = query(&fixture, Mode::Query { wait: Duration::ZERO }, None);
        assert!(status.reason_line().contains("go.mod"), "{}", status.reason_line());
        // no entry for the language
        let setup = Setup { storage: fixture.storage.clone(), registry: &fixture.registry, exe: None };
        let (_, status) = resolve(&setup, &fixture.root, "rust", Mode::Peek);
        assert!(status.reason_line().contains("no registry entry"));
    }

    #[test]
    fn an_entry_with_another_contract_version_is_skipped() {
        let mut fixture = fixture("ok.sh");
        fixture.registry.adapters[0].contract = 2;
        let (_, status) = query(&fixture, Mode::Peek, None);
        assert!(status.reason_line().contains("contract 2"), "{}", status.reason_line());
    }

    #[test]
    fn an_overlay_of_an_older_commit_shows_as_stale_while_the_new_run_is_due() {
        let fixture = fixture("ok.sh");
        let root = fixture.root.clone();
        runner::run_one(&fixture.storage, &root, &fixture.registry.adapters[0], false);
        std::fs::write(root.join("b.go"), format!("{}\n// more\n", std::fs::read_to_string(root.join("b.go")).unwrap())).unwrap();
        git(&root, &["commit", "-q", "-am", "second"]);
        let (overlay, status) = query(&fixture, Mode::Query { wait: Duration::ZERO }, Some("/usr/bin/true"));
        assert!(overlay.is_none());
        assert_eq!(state_word(&status), "stale", "{status:?}");
    }

    #[test]
    fn a_module_in_a_subdirectory_of_a_git_work_tree_is_served_and_the_whole_tree_must_be_clean() {
        let fixture = fixture("ok.sh");
        // Move the module into svc/ and add a file elsewhere in the repository.
        let top = fixture.root.clone();
        let module = top.join("svc");
        std::fs::create_dir(&module).unwrap();
        for name in ["a.go", "b.go", "go.mod", "package.json"] {
            if top.join(name).exists() {
                std::fs::rename(top.join(name), module.join(name)).unwrap();
            }
        }
        std::fs::write(top.join("other.txt"), "x\n").unwrap();
        git(&top, &["add", "-A"]);
        git(&top, &["commit", "-q", "-m", "module below the top"]);
        let setup = Setup { storage: fixture.storage.clone(), registry: &fixture.registry, exe: None };
        let entry = &fixture.registry.adapters[0];
        assert_eq!(runner::run_one(&fixture.storage, &module, entry, false), runner::Outcome::Built { partial: false });
        let (overlay, status) = resolve(&setup, &module, "go", Mode::Query { wait: Duration::ZERO });
        assert_eq!(state_word(&status), "fresh", "{status:?}");
        assert_eq!(overlay.unwrap().sites_in_file("a.go").len(), 1);
        // The top level of the repository has no go.mod: not a root of this adapter.
        let (_, status) = resolve(&setup, &top, "go", Mode::Peek);
        assert!(status.reason_line().contains("go.mod"), "{}", status.reason_line());
        // A change to a tracked file outside the module makes the whole tree dirty.
        std::fs::write(top.join("other.txt"), "changed\n").unwrap();
        let (overlay, status) = resolve(&setup, &module, "go", Mode::Query { wait: Duration::ZERO });
        assert!(overlay.is_none());
        assert_eq!(state_word(&status), "skipped");
        assert!(status.reason_line().contains("local changes") && status.reason_line().contains("other.txt"), "{}", status.reason_line());
    }

    #[test]
    fn two_modules_of_one_repository_have_two_keys_and_two_stores() {
        let fixture = fixture("ok.sh");
        let entry = &fixture.registry.adapters[0];
        let head = key::read_head(&fixture.root).unwrap();
        let top_key = key::make_key(&fixture.storage, entry, &fixture.root, &head).unwrap();
        std::fs::create_dir(fixture.root.join("svc")).unwrap();
        let sub_key = key::make_key(&fixture.storage, entry, &fixture.root.join("svc"), &head).unwrap();
        assert_eq!(sub_key.module_path, "svc");
        assert_ne!(top_key.id(), sub_key.id());
        let root_layout = Layout::new(&store::adapters_root(&fixture.storage), &fixture.root, "go");
        let sub_layout = Layout::new(&store::adapters_root(&fixture.storage), &fixture.root.join("svc"), "go");
        assert_ne!(root_layout.dir, sub_layout.dir);
    }
}
