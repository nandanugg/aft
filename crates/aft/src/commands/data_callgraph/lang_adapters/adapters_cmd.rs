//! `aft adapters list|status|build` and the internal `aft adapters run` (owner: R1).
//!
//! ```text
//! aft adapters list
//! aft adapters status --root <repo> [--json]
//! aft adapters build  --root <repo> [--language <l>] [--force] [--json]
//! aft adapters run    --root <repo> --language <l> [--force]     (internal: the detached runner)
//! ```
//! `status` only looks. `build` runs the adapters of the root in this process and
//! waits for them (bounded by each entry's timeout plus the kill grace). `--json`
//! prints `{"root": …, "adapters": [AdapterStatus, …]}`.

pub(crate) mod args;

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use super::contract::{AdapterState, AdapterStatus, Registry, RegistryEntry, KILL_GRACE};
use super::query::status_now;
use super::registry;
use super::runner::{self, Outcome};
use super::store;

/// Extra time `build` waits for a run that another process started, on top of the entry's timeout.
const FOREIGN_RUN_SLACK: Duration = Duration::from_secs(60);

/// Entry point for `aft adapters …`; returns the exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    let Some(command) = args.first() else {
        return usage("a command is needed");
    };
    let rest = &args[1..];
    match command.as_str() {
        "list" => list(rest),
        "status" => status(rest),
        "build" => build(rest),
        "run" => run_detached(rest),
        other => usage(&format!("unknown command {other:?}")),
    }
}

fn usage(problem: &str) -> i32 {
    eprintln!("aft adapters: {problem}");
    eprintln!("usage: aft adapters list");
    eprintln!("       aft adapters status --root <repo> [--json]");
    eprintln!("       aft adapters build --root <repo> [--language <l>] [--force] [--json]");
    2
}

/// The root of a command: `--root`, else the current directory. Canonical.
pub(crate) fn root_arg(value: Option<&str>) -> Result<PathBuf, String> {
    let path = match value {
        Some(value) => PathBuf::from(value),
        None => std::env::current_dir().map_err(|error| format!("cannot read the current directory: {error}"))?,
    };
    path.canonicalize().map_err(|error| format!("root {} cannot be used: {error}", path.display()))
}

fn load_registry() -> Result<registry::Loaded, String> {
    registry::load()
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn list(args: &[String]) -> i32 {
    let parsed = match args::parse(args, &[], &["json"]) {
        Ok(parsed) => parsed,
        Err(problem) => return usage(&problem),
    };
    let loaded = match load_registry() {
        Ok(loaded) => loaded,
        Err(problem) => {
            eprintln!("aft adapters: {problem}");
            return 1;
        }
    };
    let location = loaded.path.as_ref().map(|path| path.display().to_string());
    if parsed.has("json") {
        println!("{}", serde_json::json!({ "registry": location, "adapters": loaded.registry.adapters }));
        return 0;
    }
    match &location {
        Some(path) => println!("registry: {path} ({} adapters)", loaded.registry.adapters.len()),
        None => println!("registry: no location (cannot find the aft executable and {} is not set)", registry::REGISTRY_ENV),
    }
    if loaded.registry.adapters.is_empty() {
        println!("no adapters registered. Set {} or put {} next to the aft executable.", registry::REGISTRY_ENV, registry::REGISTRY_FILE);
    }
    for entry in &loaded.registry.adapters {
        println!();
        println!("{}  (contract {})", entry.language, entry.contract);
        println!("  markers:  {}", entry.root_markers.join(", "));
        println!("  command:  {}", entry.command.join(" "));
        println!(
            "  limits:   run {} s, query wait {} s, failure cooldown {} s",
            entry.timeout().as_secs(),
            entry.query_wait().as_secs(),
            entry.failure_cooldown().as_secs()
        );
        if let Some(toolchain) = &entry.toolchain_id_command {
            println!("  toolchain id: {}", toolchain.join(" "));
        }
        if !entry.hidden_tags.is_empty() {
            println!("  hidden target tags: {}", entry.hidden_tags.join(", "));
        }
        for fact in &entry.facts {
            let aliases: Vec<String> = fact.aliases.iter().map(|alias| alias.name.clone()).collect();
            let aliases = if aliases.is_empty() { String::new() } else { format!(" (aliases: {})", aliases.join(", ")) };
            println!("  facts:    {}{aliases}: {}", fact.kind, fact.description);
        }
    }
    0
}

// ---------------------------------------------------------------------------
// status and build
// ---------------------------------------------------------------------------

/// One line per status, plus the detail of a failure.
pub(crate) fn print_status(status: &AdapterStatus) {
    println!("{}", status.reason_line());
    if let AdapterState::Failed { at_unix_s, retry_after_unix_s, stderr_tail, .. } = &status.state {
        let now = store::now_unix_s();
        let ago = now.saturating_sub(*at_unix_s);
        if *retry_after_unix_s > now {
            println!("  failed {ago} s ago; no new run for {} s (`aft adapters build --force` runs it now)", retry_after_unix_s - now);
        } else if *retry_after_unix_s > 0 {
            println!("  failed {ago} s ago; the next query starts a new run");
        }
        for line in stderr_tail {
            println!("  stderr: {line}");
        }
    }
}

fn print_statuses(root: &Path, statuses: &[AdapterStatus], json: bool) {
    if json {
        println!("{}", serde_json::json!({ "root": root.display().to_string(), "adapters": statuses }));
        return;
    }
    println!("root: {}", root.display());
    if statuses.is_empty() {
        println!("no adapter applies to this root (no registry entry has a root marker here)");
    }
    for status in statuses {
        print_status(status);
    }
}

/// The languages to work on at `root`: all with a marker, or the one asked for.
fn languages(registry: &Registry, root: &Path, wanted: Option<&str>) -> Result<Vec<String>, String> {
    let all = registry::languages_at_root(registry, root);
    match wanted {
        None => Ok(all),
        Some(language) if all.iter().any(|name| name == language) => Ok(vec![language.to_string()]),
        Some(language) => Err(format!("no adapter for language {language:?} applies to this root (applies: {})", all.join(", "))),
    }
}

fn status(args: &[String]) -> i32 {
    let parsed = match args::parse(args, &["root", "language"], &["json"]) {
        Ok(parsed) => parsed,
        Err(problem) => return usage(&problem),
    };
    let (root, loaded) = match (root_arg(parsed.value("root")), load_registry()) {
        (Ok(root), Ok(loaded)) => (root, loaded),
        (Err(problem), _) | (_, Err(problem)) => {
            eprintln!("aft adapters: {problem}");
            return 1;
        }
    };
    let languages = match languages(&loaded.registry, &root, parsed.value("language")) {
        Ok(languages) => languages,
        Err(problem) => return usage(&problem),
    };
    let statuses: Vec<AdapterStatus> = languages.iter().map(|language| status_now(&loaded.registry, &root, language)).collect();
    print_statuses(&root, &statuses, parsed.has("json"));
    0
}

fn build(args: &[String]) -> i32 {
    let parsed = match args::parse(args, &["root", "language"], &["json", "force"]) {
        Ok(parsed) => parsed,
        Err(problem) => return usage(&problem),
    };
    let (root, loaded) = match (root_arg(parsed.value("root")), load_registry()) {
        (Ok(root), Ok(loaded)) => (root, loaded),
        (Err(problem), _) | (_, Err(problem)) => {
            eprintln!("aft adapters: {problem}");
            return 1;
        }
    };
    let languages = match languages(&loaded.registry, &root, parsed.value("language")) {
        Ok(languages) => languages,
        Err(problem) => return usage(&problem),
    };
    let force = parsed.has("force");
    let json = parsed.has("json");
    let storage = crate::storage_path::storage_dir(None);

    // Each language runs in its own thread; the machine-wide slots limit how many really run.
    let outcomes: Vec<Outcome> = thread::scope(|scope| {
        let handles: Vec<_> = languages
            .iter()
            .map(|language| {
                let entry = registry::entry_for_root(&loaded.registry, &root, language).expect("language comes from this registry and root");
                let (storage, root) = (&storage, &root);
                scope.spawn(move || run_and_wait(storage, root, entry, force))
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| Outcome::Failed("the build thread panicked".to_string()))).collect()
    });

    let statuses: Vec<AdapterStatus> = languages.iter().map(|language| status_now(&loaded.registry, &root, language)).collect();
    if !json {
        for (language, outcome) in languages.iter().zip(&outcomes) {
            println!("{language}: {}", describe(outcome));
        }
    }
    print_statuses(&root, &statuses, json);
    let any_failed = statuses.iter().any(|status| matches!(status.state, AdapterState::Failed { .. }));
    if any_failed {
        1
    } else {
        0
    }
}

/// `run_one`, and when another process holds the run lock, wait for it to finish
/// (bounded by the entry's timeout, the kill grace and some slack).
fn run_and_wait(storage: &Path, root: &Path, entry: &RegistryEntry, force: bool) -> Outcome {
    let give_up = Instant::now() + entry.timeout() + KILL_GRACE + FOREIGN_RUN_SLACK;
    loop {
        match runner::run_one(storage, root, entry, force) {
            Outcome::AlreadyRunning if Instant::now() < give_up => thread::sleep(Duration::from_millis(250)),
            outcome => return outcome,
        }
    }
}

fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Built { partial: false } => "built".to_string(),
        Outcome::Built { partial: true } => "built (partial)".to_string(),
        Outcome::Failed(reason) => format!("the run failed: {reason}"),
        Outcome::NothingToDo(why) => format!("not started: {why}"),
        Outcome::AlreadyRunning => "another run is still going on after the wait limit".to_string(),
        Outcome::Skipped(reason) => format!("skipped: {reason}"),
        Outcome::NoSlot => format!("no run slot became free ({} adapter runs at most at the same time on this machine)", runner::global_run_limit()),
        Outcome::Discarded(why) => format!("result thrown away: {why}"),
    }
}

// ---------------------------------------------------------------------------
// run (internal: the detached runner of a query)
// ---------------------------------------------------------------------------

fn run_detached(args: &[String]) -> i32 {
    let parsed = match args::parse(args, &["root", "language"], &["force"]) {
        Ok(parsed) => parsed,
        Err(problem) => return usage(&problem),
    };
    let (Some(root), Some(language)) = (parsed.value("root"), parsed.value("language")) else {
        return usage("run needs --root and --language");
    };
    let root = match root_arg(Some(root)) {
        Ok(root) => root,
        Err(problem) => {
            eprintln!("aft adapters run: {problem}");
            return 1;
        }
    };
    let loaded = match load_registry() {
        Ok(loaded) => loaded,
        Err(problem) => {
            eprintln!("aft adapters run: {problem}");
            return 1;
        }
    };
    let entry = match registry::entry_for_root(&loaded.registry, &root, language) {
        Ok(entry) => entry,
        Err(problem) => {
            eprintln!("aft adapters run: {problem}");
            return 1;
        }
    };
    let storage = crate::storage_path::storage_dir(None);
    let outcome = runner::run_one(&storage, &root, entry, parsed.has("force"));
    // This goes to the runner log (stderr).
    eprintln!("aft adapters run: {} at {}: {}", entry.language, root.display(), describe(&outcome));
    0
}
