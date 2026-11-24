---
name: update-upstream
description: Move our vendored aft (upstream cortexkit/aft plus our language-adapter layer) to a new upstream release. Explains what the vendored aft is, how and when it runs, where its files are, who uses it, and how to classify an upstream range as none, minor or major before you merge it. Use when upstream has a new tag, or when you must check whether an upstream change breaks the adapter layer.
---

# update-upstream

This skill has two jobs.

1. Explain the vendored aft: what, how, when, where, who.
2. Give the upgrade procedure: checks, classification, merge, fixes, tests.

`S` below means the tree of our fork (the repository that holds this file). `U` means upstream
`cortexkit/aft`.

## What is the vendored aft

aft is a code-navigation tool for agents. It builds a call graph with tree-sitter and answers
`callers`, `call_tree`, `impact`, `trace_to`, `trace_to_symbol`, `trace_data` and more.

Our copy is exactly two things:

1. **Upstream `cortexkit/aft` at one release tag.** Example: `v0.67.0` (tagged 2026-11-23).
   We do not patch upstream behaviour.
2. **Our commits on top.** Nothing else is allowed in the fork:
   - the language-adapter system: `crates/aft/src/commands/data_callgraph/lang_adapters/`
     (registry, key, runner, overlay store, `AdapterGraph`, `finish_reply`);
   - the adapters themselves, one directory per language: `adapters/go/` (Go) and `adapters/js/` (JavaScript);
   - the commands `aft adapters`, `aft facts`, `aft cli` (the one-shot client; it also maps the fact aliases and
     `search` / `similar` to upstream commands) and `aft guide`;
   - the install scripts and the shell shim in `scripts/adapters/` (`install.sh`, `uninstall.sh`, `aft`);
   - this skill, and the adapter docs in `docs/adapters/` (`CONTRACT.md`, `GUIDE.md`, `registry.example.json`);
   - tests in `tests/adapters/`.

The words used in this skill:

- **Adapter**: a separate program for one language. It uses that language's own tools (for Go: the type
  checker) and prints records about calls tree-sitter cannot resolve. It knows nothing about aft internals.
- **Adapter layer**: our code inside aft that runs adapters, stores their records and merges them into
  aft's answers. It is the module `lang_adapters/` and its few connection points in upstream files.
- **Seam**: one upstream file where the adapter layer connects to the core. The list is in "Where".
- **Core**: upstream aft without our commits.

## How it works

Upstream's tree-sitter graph is the base for every language. Adapters only add. An adapter never removes
an upstream edge.

1. A registry (one JSON file) names each adapter: language, root marker files (for example `go.mod`),
   command, timeouts. To add a language you add one registry entry and one adapter that speaks the contract.
   Nothing else changes.
2. **Contract v1** is the agreed input and output of an adapter. The adapter gets a repository root.
   It prints NDJSON on stdout: `header`, `analysed`, `site`, `fact`, `unanalysed`, `end`. Identity is a
   position (file, line, column), never a name. Read `docs/adapters/CONTRACT.md` for the people form and
   `crates/aft/src/commands/data_callgraph/lang_adapters/contract.rs` for the same rules in Rust.
3. The runner starts an adapter detached, in its own process group, with a timeout and a cooldown after a
   failure. At most 2 adapter runs run at the same time on the machine. A hang or crash costs one
   restart and never blocks a query or another run.
4. Records are stored per repository, language and git commit (SQLite). A root is searchable by an adapter
   only when it is a git work tree with a HEAD commit. A dirty tree (tracked files changed) gets no
   adapter data. The reply says why. Upstream answers still work.
5. **Read side.** Upstream reads its graph through the `CallGraphRead` trait (31 methods at v0.67.0).
   Our `AdapterGraph` implements the same trait and wraps the real reader. It merges the overlay in the
   methods that return edges, callers, callees or unresolved sites, and delegates all others. The
   core hands the reader out through `ops_graph` / `OpsGraph` in `commands/data_callgraph.rs`.
6. **Reply side.** Each of the six graph commands calls `data_callgraph::finish_reply(response, &graph, op)`
   once: `lang_adapters::finish_reply(req, "<command>", &snapshot, <upstream reply>)` wraps the upstream reply
   expression. It adds the adapter status, sets `complete:false` when needed, and renders text with upstream's
   `subc_format::format_callgraph`. Parent-folder replies get the adapter edges but no `finish_reply` (no
   status line); `aft cli` always uses one repository root, so it never uses parent-folder mode.
7. A guard test, `callgraph_read_methods_are_all_reviewed`, compares the trait source with a reviewed
   table. A new upstream method fails the test until someone decides: overlay or delegate.

Background and measurements: `docs/adapters/CONTRACT.md` and the spike notes (section 7 lists the seams
with their churn).

## When it runs and when it updates

- Upstream releases about every week. Dates seen: `v0.66.0` 2026-11-17, `v0.67.0` tag 2026-11-23.
- Our upgrades are manual. Nothing moves the fork by itself.
- Adapters run per repository commit. The first graph query on a commit that has no stored data starts a
  detached run; `aft adapters build --root <repo>` starts one on purpose. A reply waits at most
  `query_wait_s` (20 s, one deadline for the whole reply). If the run is not done by then, the reply answers
  from upstream and says `building`.
- A new commit gives a new key, so a new run. A failed or partial run waits `failure_cooldown_s` (600 s).
- **Planned, not built yet:** the consumer install/update flow. The plan is a daily `git pull` of the
  consumer's repositories, then an aft check that decides which repositories need a new adapter run.
  Until it exists, the first query after a pull starts the run, or run `aft adapters build --root <repo>`.

## Where everything lives

Seam files (upstream files we touch or read; all paths are under `crates/`):

| File | What we do there |
|---|---|
| `aft/src/commands/data_callgraph.rs` | `OpsGraph` becomes our wrapper; our module is a child of this file |
| `aft/src/commands/mod.rs` | one word: `pub mod data_callgraph` |
| `aft/src/commands/parent_query/mod.rs` | wrap the reader for parent-folder queries (1 line) |
| `aft/src/commands/{callers,call_tree,impact,trace_to,trace_to_symbol,trace_data}.rs` | one `finish_reply` call each |
| `aft/src/main.rs` | 5 lines: `lang_adapters::route_subcommand(...)` routes `adapters`, `facts`, `cli`, `guide` BEFORE the help guard (`cli::help::handle_first_arg`, in main.rs since v0.67.0) |
| `aft-parse/src/callgraph_store/mod.rs` | read only: the `CallGraphRead` trait (before v0.67.0: `aft/src/callgraph_store/mod.rs`) |
| `aft/src/data_layer/query.rs` | read only: `SliceReader` / `SliceSource`, and the rule for unresolved calls |
| `aft/src/data_layer/slice.rs` | read only: `NodeView` / `RefView` / `FileSlice` |
| `aft/src/subc_format.rs` | read only: `format_callgraph` |
| `aft-base/src/protocol.rs` | read only: `Response` / `RawRequest` |
| `aft-base/src/error.rs` | read only: the `ambiguous_symbol` message text (`candidates: [A, B]`) that `aft cli` parses to retry a bare name |
| `Cargo.lock` | read only. We use only crates already in it: `rusqlite`, `sha2`, `libc`, `serde`, `serde_json`; `tempfile` in tests |

Not touched on purpose: `aft/src/data_layer/callgraph_store_adapter.rs` and `aft/src/lib.rs` (the hottest
files), and any `Cargo.toml`.

Upstream items our code USES (it does not change them). Two kinds:

| Kind | Items | When upstream changes them |
|---|---|---|
| Compile-checked | `storage_path::storage_dir`; `parser::{FileParser::extract_symbols, LangId, TreeSitterProvider}`; `callgraph_store::{CallGraphRead, StoreCallSite, …}`; `data_layer::query::{SliceReader, SliceSource}`, `data_layer::slice`; `subc_format::{format_callgraph, format_response_with_context, FormatContext}`; `protocol::Response`; `commands::data_callgraph::{DataGraph, OpsGraph}`; tests only: `test_env`, `parent_search`, `commands::configure::handle_configure`, `commands::parent_query::route`, `context::AppContext`, `config::Config` | The build fails: follow the errors (minor) |
| Runtime only (compiles, then breaks) | NDJSON request words: `configure` + `project_root` + `harness`, command names, `file`, `symbol`, `toSymbol`, `toFile`, `expression`, `depth`, `includeTests`, `start_line`, `limit`, `pattern`, `path`, `query` (`semantic_search`); reply words: `success`, `code`, `message`, `text`; the `ambiguous_symbol` message text; the OUTLINE TEXT that `cli.rs` parses (`type X interface`, `func (r *T) name(`, `func name(`) | Section 4b of the script counts the words; accept_W1 and the real-world case set show a broken parse. Minor, unless the protocol itself goes |
| Runtime only: semantic search install | `install.sh` pins ONNX Runtime (`ort_version`, NuGet package, sha256) and the model `all-MiniLM-L6-v2` (`model_repo` + revision); upstream's `ort_pin.rs` (`PINNED_ORT_LIBRARIES`) must accept that ONNX Runtime version. The shim sets `ORT_DYLIB_PATH` and `FASTEMBED_CACHE_DIR` and passes `aft warmup …` to the binary | Section 4b of the script counts the words and checks the pinned version; accept_W1 shows a break ("ONNX Runtime not found", or warmup prints the `aft cli` usage). Update the pins in `install.sh` (minor) |

Our files:

- `crates/aft/src/commands/data_callgraph/lang_adapters/` the adapter system.
- `adapters/go/` (Go module `aft-go-adapter`; deps: `golang.org/x/tools`, `x/mod`, `x/sync`) and `adapters/js/`
  (Node built-ins and a vendored `acorn`) — the adapters.
- `scripts/adapters/` install, uninstall and the shell shim.
- `docs/adapters/` contract, guide, example registry.
- `tests/adapters/` acceptance scripts, fake adapters, fixtures.
- `.agents/skills/update-upstream/` this skill and `rebase-check.sh`.

Runtime places:

- Registry: `adapters-registry.json` in the directory of the resolved `aft` binary, or the file named by
  `$AFT_ADAPTERS_REGISTRY`. aft never reads a registry from a project root.
- Overlay storage: `<storage>/lang-adapters/overlays/<repo>-<hash>-<language>/` (SQLite, run records, locks),
  where `<storage>` is `$AFT_STORAGE_DIR`, else upstream's default storage root.
- Install: `scripts/adapters/install.sh --prefix <dir> (--release-url <url> | --binary <file>)` writes only inside
  `<dir>`: `<dir>/bin/aft` (shim, runs `aft cli`), `<dir>/libexec/aft-adapters/` (binary, adapters, registry with
  absolute paths), `<dir>/share/aft-adapters/install-manifest.txt` (what `uninstall.sh` removes). The consumer's install
  flow chooses `<dir>` (planned). `--guide-local <file>` also installs the consumer's guide addendum (`guide.local.md`).

## Who uses it

- The agents of a consumer project: they answer questions about code (callers and call trees), prove what
  the code does, and do on-call investigations (read-only incident work). They call `aft cli <command>`;
  they never run adapters by hand.
- Maintainers of the fork: they run this skill when upstream has a new tag.
- Adapter authors: anyone who adds a language. They read `docs/adapters/CONTRACT.md` and add a registry entry.

## Upgrade procedure

Work in a scratch branch or worktree. Never push before step 9.

### 1. Pre-checks

1. `git status --short` is empty before the merge (step 4). If not, commit your work first, or start from a
   clean worktree. The fact collector (step 2) reads only git objects, so it may run on any tree.
2. Write down the upstream tag our fork sits on (the base: `git describe --tags --abbrev=0 --match 'v*' $(git merge-base HEAD <target-tag>)`)
   and the upstream target tag. Both ends of the range in step 2 are UPSTREAM tags: with a fork commit as `a`,
   our own lines (for example `finish_reply`) look "gone" at `b` and the script says major for no reason.
3. Fetch upstream tags: `git fetch <upstream-remote> --tags`. The remote name is yours.
4. Take baseline numbers on the CURRENT tree, before the merge:
   - the four upstream test targets (see step 6): `938 passed, 0 failed, 11 ignored` at v0.67.0;
   - `tests/adapters/accept_*.sh` all pass.
   Put them in your report. If the baseline is not green, stop: you cannot tell old failures from new ones.

### 2. Run the fact collector

```bash
.agents/skills/update-upstream/rebase-check.sh --range <base-tag>..<target-tag>
```

It is read-only. It needs bash, POSIX tools, git and `/usr/bin/grep`. It prints: seam files touched with line counts; the
`CallGraphRead` method list at both ends; moves of seam files; changes to the seam symbols
(`OpsGraph`/`ops_graph`/`Access`, `SliceReader`/`SliceSource`/`FileSlice`, `NodeView`/`RefView`,
`format_callgraph`, the reply path in the six commands, the help guard in `main.rs`); `Cargo.lock` crate
changes; and one line `suggested: none|minor|major` with reasons.

The line is a hint from simple rules. The maintainer decides. Open every seam hunk it lists
(`git diff -M -U3 <a> <b> -- <file>`). When the script prints an older path for a file (the trait moved
from `crates/aft/` to `crates/aft-parse/` at v0.67.0), give both paths, or the diff of an older range is empty.
How to read it:
- `a=0` in section 4: the file or the symbol did not exist at `a` yet (for example the help guard before
  v0.67.0). It is not "no change".
- Section 2 says for each added `CallGraphRead` method whether it has a default body. A method WITHOUT one does
  not compile until `AdapterGraph` implements it. A method WITH one compiles, and `AdapterGraph` then uses the
  default silently: decide overlay or delegate for it too.
- The script's major rules: 5 or more trait methods removed; 40 or more trait body lines changed; 3000 or more
  changed lines in the file that keeps the trait (a store rewrite, as in 795838760); 5 or more files deleted under
  `callgraph_store/` or `data_layer/`; a seam file or a seam symbol gone. They are tuned on 795838760: when you
  disagree with a "major", say which rule fired and why it does not apply.

### 3. Classify

Use the severity table below. Write the class and one sentence per break area (the rows of the table in
step 5) in your report.

### 4. Merge or rebase

- none or minor: merge the upstream tag into the fork (`git merge <target-tag>`). Our commits stay as they are.
- major: stop. Do not merge to the main fork branch. Make a design note first (what the core now does, what
  our adapter layer must do instead), get a decision, then merge in a scratch branch.

### 5. Fix what the merge broke

Code to update, by break area:

| Break area | What to update |
|---|---|
| Trait `CallGraphRead` methods added or renamed | `AdapterGraph` in `lang_adapters/` (decide overlay or delegate for each); the reviewed table in the guard test |
| Trait moved to another crate or file | the trait path that the guard test reads (`graph.rs`); `use` paths in `lang_adapters/`; the seam table in this skill and `SEAMS` in `rebase-check.sh` |
| `OpsGraph` / `ops_graph` signature or `Access` changed | the wrap in `commands/data_callgraph.rs`; the wrap in `parent_query/mod.rs` |
| Command files changed their reply code | the one `finish_reply` call in each of the six files |
| `SliceReader` / `SliceSource` / `NodeView` / `RefView` changed | position mapping and the unresolved-count subtraction in `AdapterGraph` |
| `format_callgraph` signature changed | `finish_reply` |
| `main.rs` routing or help guard moved | our routing lines (`adapters`, `facts`, `cli`, `guide`); they must stay before the help guard |
| `Response` / `RawRequest` changed | `finish_reply` and `aft cli` (the NDJSON client) |
| The ambiguous-name message text changed (`aft-base/src/error.rs`) | `parse_ambiguity` in `lang_adapters/cli.rs` |
| `Cargo.lock` lost a crate we use | pick another crate that is already in the lock; do not change `Cargo.toml` |

### 6. Tests to run

Heavy jobs (a cargo build or test, an adapter run on a large repository): run at most two at once.
A cargo test build needs about 8 GB of disk per target directory.

1. Upstream's own targets. At v0.67.0 (upstream only) the four targets together gave 938 passed, 0 failed,
   11 ignored. The T2 filter also matches our tests under `commands::data_callgraph::lang_adapters`, so on the
   fork T2 is higher by the number of our tests:
   - T1 `cargo test --locked -p aft-parse --features test-hooks --lib` (120 passed)
   - T2 `cargo test --locked -p agent-file-tools --features test-hooks --lib -- data_layer:: callgraph_store:: commands::zoom commands::data_callgraph commands::parent_query commands::callers commands::call_tree commands::impact commands::trace` (677 passed)
   - T3 `cargo test --locked -p agent-file-tools --features test-hooks --test integration -- callgraph_test callgraph_restart_reuse_test trace_to_symbol_test outline_zoom_test` (114 passed)
   - T4 `cargo test --locked -p agent-file-tools --features test-hooks --test list_envelope -- list_envelope_callgraph list_envelope_trace` (27 passed)
   Pass means 0 failed and passed counts equal to the baseline, or higher only by tests we added.
   If upstream added tests, the count may be higher: list them by name.
2. The guard test: `cargo test --locked -p agent-file-tools --features test-hooks --lib callgraph_read_methods_are_all_reviewed`
   (in `lang_adapters/graph.rs`; it reads the trait source at run time from `aft-parse/src/callgraph_store/mod.rs`).
3. Our acceptance scripts: `for f in tests/adapters/accept_*.sh; do bash "$f"; done`. Every script must print
   `0 failed`. Some checks use private inputs; set them first (list in `tests/adapters/lib/common.sh`):
   `STOCK_AFT` (a release build of the upstream TARGET tag, in its own worktree and target dir — never a build of
   the fork), `AFT_ACCEPT_CASES` (the consumer's private case pack: real-repository values and fixtures),
   `ACCEPT_CLONES` (clean clones of the repositories that the pack names) and `CASE_REPOS_ROOT` (a folder that holds
   those repositories under `repos/`). `AFT_ACCEPT_HEAVY` can name a slot wrapper for heavy jobs. Without
   `AFT_ACCEPT_CASES`, the checks on real repositories print `SKIP`; a skip is not a pass, so run the full
   set before you release.
4. The real-world case set: about 60 questions on real repositories (every command, Go and JavaScript),
   each with an answer written from gopls and code reading BEFORE any aft run. It names private code, so it is
   kept privately by the consumer and given with `AFT_ACCEPT_CASES`, not in this public repository. Install
   the new build into a scratch prefix (`scripts/adapters/install.sh --prefix <dir> --binary <aft>`), run every
   case with the runner of the set, and compare each reply with its expected answer. A changed answer is a
   finding: decide whether upstream or our layer changed it before you merge. Unit tests miss these changes.

### 7. Update this skill

If a seam file moved or a method count changed, update the seam table here, `SEAMS` in `rebase-check.sh`, and
the replay cases (keep the old ones; add the new range).

### 8. Report

Report: range, suggested class, your class, break areas touched, tests with counts before and after, what you
did not run and why.

### 9. Push

Only after the report is accepted by the maintainer, and only with their consent. This skill never pushes.

## Severity table

"Adapter layer" means our code that talks to the core: `AdapterGraph`, `finish_reply`, the six command
calls, the routing in `main.rs`. "Major" means the way the adapter layer talks to the core must change.

| Class | Meaning | Examples | What you do |
|---|---|---|---|
| none | No seam file is touched in the range. Core and adapter layer are untouched. | A commit whose files are all outside `crates/` (docs); a change in an unrelated crate. A large `lib.rs` change (section 6 of the script) is still worth a look. | Merge. Run the tests once. |
| minor | Same mechanism, mechanical edits only. | Methods added or renamed in `CallGraphRead`; a seam file or crate moved; a signature changed; a `use` path changed; a new field in `Response`. | Merge, edit the places listed in step 5, run all tests. |
| major | A core mechanism change that forces a change in how our adapter layer talks to the core. | The graph reader or its storage replaced; `CallGraphRead` removed, or no longer gives positions or spans; the `OpsGraph` / `ops_graph` seam gone; the reply path (the six commands, `finish_reply` anchor, `format_callgraph`) gone; the SQLite store removed. | Stop. Write a design note. Redesign the adapter-layer part. Ask for a decision. |

When two classes seem to fit, pick the higher one and say why in the report.

## Replay cases (known answers)

Use these to check yourself. Run `rebase-check.sh` on each range, then classify by reading the facts.

| Range | Class | Why |
|---|---|---|
| `795838760^..795838760` (2026-11-07, "remove retired SQLite callgraph backend") | **major** | The SQLite call-graph store was removed. The trait text stayed, but the store behind it was replaced (about 36,000 lines of `callgraph_store/mod.rs` and 9 store files deleted). The old Go projection, which read that store, had to be redesigned. |
| `v0.64.0..v0.66.0`: `CallGraphRead` 29 -> 31 methods | **minor** | Same mechanism. Two methods added: `direct_callers_bounded` (no default body: must be implemented) and `unresolved_call_sites_named` (default body: would be used silently; we overlay it). The reply expression in the six commands changed (`serialized_response` -> `serialized_response_with_view`): the `finish_reply` call wraps the new expression. |
| `v0.66.0..v0.67.0`: the crate move `crates/aft` to `crates/aft-parse` | **minor** | Same mechanism, the trait file moved. Edits: the trait path the guard test reads, the `use` paths, the seam table. |
| A range that touches no seam file: a commit whose files are all outside `crates/` (find one: `git log --format=%h -n 300 <tag> -- docs/`, then keep a commit whose `git show --name-only --format= <sha>` lists only `docs/` files) | **none** | No seam file touched. Merge and run the tests once. |

`rebase-check.sh` must give `suggested: major` for the first row, `minor` for the next two, and `none` for the
last. If it does not, the rules in the script are wrong: fix the script, not the case. For a strict self-test,
give the tester this file without this table, and compare afterwards.

## Rules for this skill

- The fork holds upstream plus our commits only. A fix to upstream behaviour goes to upstream as a report.
- Touch as few upstream files as possible. A new seam needs a reason written in this skill.
- Rust code uses std and crates already in upstream's `Cargo.lock`. No `Cargo.toml` or `Cargo.lock` change.
- A failure of an adapter must stay cheap: a restart costs one run and blocks no query.
