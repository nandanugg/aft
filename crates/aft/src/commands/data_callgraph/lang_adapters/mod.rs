//! Language adapters (fork-only code; upstream does not have this module).
//!
//! An external program per language (the "adapter") type-checks a repository and
//! prints contract-v1 records (see `docs/adapters/CONTRACT.md`). A registry entry
//! names the program. This module runs adapters, stores their output as an overlay
//! per (repository, language, commit), and lays adapter call edges over upstream's
//! own call graph at query time. Upstream's tree-sitter graph stays the base for
//! every language; adapters only add.
//!
//! Seams (fixed in C0; the owner task fills the body, the signature stays):
//! - `languages_for_root(root) -> Vec<String>` and
//!   `overlay_for_query(root, language, wait) -> (Option<Overlay>, AdapterStatus)`
//!   in `query.rs` (owner R1), called only by R2's graph wrapper.
//! - `finish_reply(req, operation, graph, response) -> Response` in `reply.rs`
//!   (owner R2), called once per graph command reply.
//! - `route_subcommand(args)` below, called from `main.rs` before upstream's help
//!   guard. It routes `aft adapters|facts|cli|guide`.
//!
//! File owners (no file has two owners):
//! - C0: `mod.rs`, `contract.rs`, `fixture.rs`
//! - R1: `registry.rs`, `key.rs`, `runner.rs`, `store.rs`, `query.rs`, `adapters_cmd.rs`, `facts_cmd.rs`
//! - R2: `graph.rs`, `mapping.rs`, `reply.rs`
//! - W1: `cli.rs`, `guide.rs`

// C0 skeleton: many shared items are used only once R1, R2 and W1 fill their files.
#![allow(dead_code, unused_imports)]

pub(crate) mod contract;
pub(crate) mod fixture;

pub(crate) mod adapters_cmd;
pub(crate) mod facts_cmd;
pub(crate) mod key;
pub(crate) mod query;
pub(crate) mod registry;
pub(crate) mod runner;
pub(crate) mod store;

pub(crate) mod graph;
pub(crate) mod mapping;
pub(crate) mod reply;

pub(crate) mod cli;
pub(crate) mod guide;

pub(crate) use contract::{AdapterStatus, Overlay};
pub(crate) use query::{languages_for_root, overlay_for_query};
pub(crate) use reply::finish_reply;

/// The subcommands this module owns, with the task that implements each.
const SUBCOMMANDS: &[&str] = &["adapters", "facts", "cli", "guide"];

/// Routes `aft adapters|facts|cli|guide …` and returns the process exit code.
/// Returns `None` for every other first argument, so upstream's own dispatch
/// (help guard, `index`, `warmup`, NDJSON server, …) runs unchanged.
pub fn route_subcommand(args: Vec<std::ffi::OsString>) -> Option<i32> {
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let first = args.get(1)?.as_str();
    if !SUBCOMMANDS.contains(&first) {
        return None;
    }
    let rest = &args[2..];
    Some(match first {
        "adapters" => adapters_cmd::run(rest),
        "facts" => facts_cmd::run(rest),
        "cli" => cli::run(rest),
        _ => guide::run(rest),
    })
}

/// The message every C0 stub entry point prints, so a test can tell a stub
/// from a real failure.
pub(crate) fn not_implemented(command: &str, owner: &str) -> i32 {
    eprintln!("aft {command}: not implemented yet (C0 stub; owner task {owner})");
    NOT_IMPLEMENTED_EXIT
}

/// Exit code of a C0 stub entry point.
pub(crate) const NOT_IMPLEMENTED_EXIT: i32 = 78;
