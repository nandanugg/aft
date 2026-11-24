//! Known-answer tests for `AdapterGraph`, `finish_reply` and the parent-folder wrap.
//!
//! Each test builds a small checkout, builds upstream's own call graph over it, and lays a
//! `FixtureOverlay` over it. No runner and no registry are needed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::callgraph_store::{CallGraphRead, StoreNode};
use crate::commands::callgraph_store_adapter::{call_tree_result, callers_result, serialized_value};
use crate::commands::data_callgraph::lang_adapters::contract::{
    parse_output, AdapterState, AdapterStatus, Overlay, RegistryEntry,
};
use crate::commands::data_callgraph::lang_adapters::fixture::FixtureOverlay;
use crate::commands::data_callgraph::lang_adapters::reply::finish_reply;
use crate::commands::data_callgraph::DataGraph;
use crate::data_layer::query::{SliceReader, SliceSource};
use crate::protocol::{RawRequest, Response};

use super::AdapterGraph;

// ---------------------------------------------------------------------------
// Fixture: one Go package. `Run` calls `s.ext.CreateTx()`, a call upstream cannot resolve
// (`ext` is a field of an imported type), and `helper()`, which upstream resolves itself.
// ---------------------------------------------------------------------------

const A_GO: &str = "package p\n\nimport \"example.com/other\"\n\ntype svc struct {\n\text other.Store\n}\n\nfunc (s *svc) Run() {\n\ts.ext.CreateTx()\n\thelper()\n}\n\nfunc helper() {}\n";
const B_GO: &str = "package p\n\ntype pg struct{}\n\nfunc (pg) CreateTx() {}\n";
const MOCK_GO: &str = "package mocks\n\ntype Mock struct{}\n\nfunc (Mock) CreateTx() {}\n";
const STUB_TEST_GO: &str = "package p\n\ntype stub struct{}\n\nfunc (stub) CreateTx() {}\n";

struct Checkout {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn checkout(files: &[(&str, &str)]) -> Checkout {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    Checkout { _dir: dir, root }
}

/// Upstream's call graph over `root`, as the commands see it (`DataGraph`).
fn data_graph(root: &Path) -> DataGraph {
    let memory = crate::data_layer::trace_measure::memory_reader(root);
    let slices: Arc<dyn SliceSource + Send + Sync> = Arc::new(memory.source().clone());
    SliceReader::new(slices, root.to_path_buf(), "test".to_string())
}

/// 1-based (line, column) of byte `plus` into the first `needle` in `text`.
fn pos(text: &str, needle: &str, plus: usize) -> (u32, u32) {
    let at = text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in the file")) + plus;
    let line = text[..at].matches('\n').count() + 1;
    let col = at - text[..at].rfind('\n').map_or(0, |newline| newline + 1) + 1;
    (line as u32, col as u32)
}

fn go_entry(hidden: &[&str]) -> RegistryEntry {
    serde_json::from_value(json!({
        "contract": 1, "language": "go", "root_markers": ["go.mod"], "command": ["adapter"],
        "hidden_tags": hidden,
    }))
    .unwrap()
}

/// The target record of a declared name: `at` is the text that starts the declaration, `plus`
/// the distance from its start to the name.
fn target(file: &str, text: &str, at: &str, plus: usize, tags: &[&str]) -> Value {
    let (line, col) = pos(text, at, plus);
    json!({"file": file, "line": line, "col": col, "name": "CreateTx", "kind": "interface", "tags": tags})
}

fn site(file: &str, text: &str, call: &str, targets: Vec<Value>) -> Value {
    // The open parenthesis of `call` (for example `s.ext.CreateTx(`).
    let (line, col) = pos(text, call, call.len() - 1);
    json!({"type": "site", "file": file, "line": line, "col": col, "name": "CreateTx", "targets": targets})
}

/// A fresh overlay over `root` made of these records, with `analysed` as its coverage.
fn overlay(root: &Path, hidden: &[&str], analysed: &[&str], sites: Vec<Value>) -> Overlay {
    let target_count: usize = sites.iter().map(|site| site["targets"].as_array().unwrap().len()).sum();
    let mut lines = vec![
        json!({"type": "header", "contract": 1, "language": "go", "tool": "test"}),
        json!({"type": "analysed", "files": analysed}),
    ];
    let site_count = sites.len();
    lines.extend(sites);
    lines.push(json!({
        "type": "end", "sites": site_count, "targets": target_count, "facts": 0,
        "analysed_files": analysed.len(), "unanalysed_files": 0
    }));
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    let entry = go_entry(hidden);
    let output = parse_output(text.as_bytes(), &entry).unwrap_or_else(|violation| panic!("{violation}"));
    Arc::new(FixtureOverlay::from_output(root, output, entry.hidden_tags.clone()).unwrap())
}

fn fresh(overlay: Overlay) -> (Option<Overlay>, AdapterStatus) {
    (Some(overlay), AdapterStatus::new("go", AdapterState::Fresh { sites: 1, facts: 0 }))
}

fn node(graph: &impl CallGraphRead, file: &str, symbol: &str) -> StoreNode {
    graph.nodes_for(Path::new(file), symbol).unwrap().into_iter().next().unwrap_or_else(|| panic!("no node {symbol} in {file}"))
}

fn call_to_create_tx(sites: &[crate::callgraph_store::StoreCallSite]) -> bool {
    sites.iter().any(|site| site.target_file == "b.go" && site.target_symbol.ends_with("CreateTx"))
}

/// The overlay of the common case: the one call upstream cannot resolve gets its interface
/// implementation `pg.CreateTx` and a mock that the registry hides. `helper()` gets its own
/// target, which upstream already has.
fn common_overlay(root: &Path) -> Overlay {
    overlay(
        root,
        &["mock"],
        &["a.go", "b.go", "mocks/m.go"],
        vec![
            site(
                "a.go",
                A_GO,
                "s.ext.CreateTx(",
                vec![
                    target("b.go", B_GO, "func (pg) CreateTx", "func (pg) ".len(), &[]),
                    target("mocks/m.go", MOCK_GO, "func (Mock) CreateTx", "func (Mock) ".len(), &["mock"]),
                ],
            ),
            site("a.go", A_GO, "helper(", vec![target("a.go", A_GO, "func helper", "func ".len(), &[])]),
        ],
    )
}

/// `site` with the adapter's tags on the call.
fn tagged_site(file: &str, text: &str, call: &str, tags: &[&str], targets: Vec<Value>) -> Value {
    let mut site = site(file, text, call, targets);
    site["tags"] = json!(tags);
    site
}

fn files() -> Vec<(&'static str, &'static str)> {
    vec![("a.go", A_GO), ("b.go", B_GO), ("mocks/m.go", MOCK_GO)]
}

// ---------------------------------------------------------------------------

#[test]
fn an_empty_overlay_answers_as_upstream_does() {
    let repo = checkout(&files());
    let stock = data_graph(&repo.root);
    let empty = overlay(&repo.root, &["mock"], &["a.go", "b.go", "mocks/m.go"], vec![]);
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(empty)]);
    let run = node(&stock, "a.go", "Run");
    let create_tx = node(&stock, "b.go", "CreateTx");
    let go = crate::parser::LangId::Go;
    let targets = [(create_tx.file.clone(), create_tx.symbol.clone()), (run.file.clone(), run.symbol.clone())];

    assert_eq!(wrapped.outgoing_calls_of(&run).unwrap(), stock.outgoing_calls_of(&run).unwrap());
    assert_eq!(wrapped.unresolved_calls_of(&run).unwrap(), stock.unresolved_calls_of(&run).unwrap());
    assert_eq!(wrapped.direct_callers_of(Path::new("b.go"), &create_tx.symbol).unwrap(), stock.direct_callers_of(Path::new("b.go"), &create_tx.symbol).unwrap());
    assert_eq!(wrapped.direct_callers_for_symbols(&targets).unwrap(), stock.direct_callers_for_symbols(&targets).unwrap());
    assert_eq!(wrapped.direct_caller_counts_of(&targets).unwrap(), stock.direct_caller_counts_of(&targets).unwrap());
    assert_eq!(wrapped.direct_callers_bounded(&targets, 100).unwrap(), stock.direct_callers_bounded(&targets, 100).unwrap());
    assert_eq!(wrapped.outgoing_calls_for_symbols(&targets).unwrap(), stock.outgoing_calls_for_symbols(&targets).unwrap());
    assert_eq!(
        wrapped.unresolved_call_site_count_named("CreateTx", go, false).unwrap(),
        stock.unresolved_call_site_count_named("CreateTx", go, false).unwrap()
    );
    assert_eq!(
        wrapped.unresolved_call_sites_named("CreateTx", go, false, 5).unwrap(),
        stock.unresolved_call_sites_named("CreateTx", go, false, 5).unwrap()
    );
    assert_eq!(wrapped.unresolved_call_site_count(go, false).unwrap(), stock.unresolved_call_site_count(go, false).unwrap());
}

#[test]
fn adapter_edges_show_in_callers_and_callees_and_leave_the_unresolved_list() {
    let repo = checkout(&files());
    let stock = data_graph(&repo.root);
    let run = node(&stock, "a.go", "Run");
    assert!(
        stock.unresolved_calls_of(&run).unwrap().iter().any(|call| call.symbol == "CreateTx"),
        "the fixture must hold a call upstream cannot resolve"
    );
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(common_overlay(&repo.root))]);
    let create_tx = node(&wrapped, "b.go", "CreateTx");

    // Callers of the implementation: `Run`, through the adapter.
    let callers = wrapped.direct_callers_of(Path::new("b.go"), &create_tx.symbol).unwrap();
    assert_eq!(callers.len(), 1, "{callers:#?}");
    assert_eq!(callers[0].caller.symbol, run.symbol);
    assert_eq!(callers[0].line, pos(A_GO, "s.ext.CreateTx(", 0).0);
    assert_eq!(callers[0].provenance, "adapter:go (interface)");
    let counts = wrapped.direct_caller_counts_of(&[(create_tx.file.clone(), create_tx.symbol.clone())]).unwrap();
    assert_eq!(counts.values().copied().collect::<Vec<_>>(), [1]);

    // Callees of `Run`: the implementation, once. The mock is hidden. `helper` is upstream's.
    let callees = wrapped.outgoing_calls_of(&run).unwrap();
    assert_eq!(callees.iter().filter(|site| site.target_file == "b.go").count(), 1, "{callees:#?}");
    assert!(callees.iter().all(|site| !site.target_file.contains("mocks")), "mock targets must stay hidden");
    assert_eq!(callees.iter().filter(|site| site.target_symbol == "helper").count(), 1, "an upstream edge is not added twice");
    let batch = wrapped.outgoing_calls_for_symbols(&[(run.file.clone(), run.symbol.clone())]).unwrap();
    assert!(call_to_create_tx(&batch[&(run.file.clone(), run.symbol.clone())]));

    // The call is no longer unresolved. The batch reader sees the same edge.
    assert!(wrapped.unresolved_calls_of(&run).unwrap().iter().all(|call| call.symbol != "CreateTx"));
    let bounded = wrapped.direct_callers_bounded(&[(create_tx.file.clone(), create_tx.symbol.clone())], 100).unwrap();
    assert_eq!(bounded.0.values().next().unwrap().len(), 1);

    // The hidden mock was met and counted, and the served edge is recorded for the text.
    assert_eq!(wrapped.hidden_counts(), [("go".to_string(), 1)]);
    assert!(wrapped.served_edges().iter().any(|edge| edge.label() == "adapter:go (interface)" && edge.target_file == "b.go"));
}

#[test]
fn a_named_count_is_lowered_only_when_every_node_of_that_name_is_analysed() {
    let go = crate::parser::LangId::Go;
    // Analysed files hold every node named CreateTx (pg and the mock): the count drops to 0.
    let repo = checkout(&files());
    let stock = data_graph(&repo.root);
    assert_eq!(stock.unresolved_call_site_count_named("CreateTx", go, false).unwrap(), 1);
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(common_overlay(&repo.root))]);
    assert_eq!(wrapped.unresolved_call_site_count_named("CreateTx", go, false).unwrap(), 0);
    assert_eq!(wrapped.unresolved_call_sites_named("CreateTx", go, false, 5).unwrap().0, 0);
    // The count for the whole language is upstream's, never lowered.
    assert_eq!(wrapped.unresolved_call_site_count(go, false).unwrap(), stock.unresolved_call_site_count(go, false).unwrap());

    // A node of that name in a file the adapter did not analyse (a test stub) keeps the count.
    let mut with_stub = files();
    with_stub.push(("stub_test.go", STUB_TEST_GO));
    let repo = checkout(&with_stub);
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(common_overlay(&repo.root))]);
    assert_eq!(wrapped.unresolved_call_site_count_named("CreateTx", go, true).unwrap(), 1);
    assert_eq!(wrapped.unresolved_call_sites_named("CreateTx", go, true, 5).unwrap().0, 1);
}

#[test]
fn an_analysed_call_with_only_outside_callees_leaves_the_unresolved_list() {
    // Option (a): the adapter analysed `s.ext.CreateTx()` and every callee is outside the root.
    let repo = checkout(&files());
    let overlay = overlay(
        &repo.root,
        &["mock"],
        &["a.go", "b.go", "mocks/m.go"],
        vec![site("a.go", A_GO, "s.ext.CreateTx(", vec![])],
    );
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(overlay)]);
    let run = node(&wrapped, "a.go", "Run");
    assert!(wrapped.unresolved_calls_of(&run).unwrap().iter().all(|call| call.symbol != "CreateTx"));
    assert!(wrapped.outgoing_calls_of(&run).unwrap().iter().all(|site| site.target_file != "b.go"));
    assert_eq!(wrapped.unresolved_call_site_count_named("CreateTx", crate::parser::LangId::Go, false).unwrap(), 0);
}

#[test]
fn a_status_that_does_not_serve_edges_changes_nothing() {
    let repo = checkout(&files());
    let stock = data_graph(&repo.root);
    let failed = AdapterStatus::new(
        "go",
        AdapterState::Failed { reason: "exit 2".into(), at_unix_s: 1, retry_after_unix_s: 2, stderr_tail: vec![] },
    );
    // Even an overlay in hand is not served unless the status says so.
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![(Some(common_overlay(&repo.root)), failed)]);
    let run = node(&stock, "a.go", "Run");
    assert_eq!(wrapped.outgoing_calls_of(&run).unwrap(), stock.outgoing_calls_of(&run).unwrap());
    assert_eq!(wrapped.unresolved_calls_of(&run).unwrap(), stock.unresolved_calls_of(&run).unwrap());
}

fn request(command: &str) -> RawRequest {
    RawRequest {
        id: "known-answer".into(),
        command: command.into(),
        lsp_hints: None,
        session_id: None,
        params: json!({}),
    }
}

#[test]
fn finish_reply_adds_status_text_and_marks_adapter_targets() {
    let repo = checkout(&files());
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(common_overlay(&repo.root))]);
    let file = repo.root.join("a.go");

    let tree = call_tree_result(&wrapped, &file, "Run", 3, false).unwrap();
    let reply = Response::success("known-answer", serialized_value("known-answer", "call_tree", &tree).unwrap());
    let reply = finish_reply(&request("call_tree"), "call_tree", &wrapped, reply);
    assert!(reply.success);
    assert_eq!(reply.data["adapters"][0]["language"], "go");
    assert_eq!(reply.data["adapters"][0]["status"], "fresh");
    assert!(reply.data.get("adapter_reason").is_none(), "{:#}", reply.data);
    assert!(reply.data.get("complete").is_none(), "a fresh status must not add a complete flag: {:#}", reply.data);
    let text = reply.data["text"].as_str().unwrap();
    assert!(text.contains("CreateTx") && text.contains("adapter:go (interface)"), "{text}");
    assert!(text.contains("adapter go: fresh"), "{text}");
    assert!(text.contains("1 target tagged mock left out"), "{text}");
    // The JSON itself carries no mark and no mock.
    let json = reply.data.to_string();
    assert!(!json.contains("mocks/"), "{json}");
    assert!(!reply.data["children"].to_string().contains("adapter:go"), "{json}");

    // The callers of the implementation name `Run`, marked in the text.
    let callers = callers_result(&wrapped, &repo.root.join("b.go"), "CreateTx", 1, false).unwrap();
    let reply = Response::success("known-answer", serialized_value("known-answer", "callers", &callers).unwrap());
    let reply = finish_reply(&request("callers"), "callers", &wrapped, reply);
    let text = reply.data["text"].as_str().unwrap();
    assert!(text.contains("Run") && text.contains("via adapter:go (interface)"), "{text}");
}

#[test]
fn finish_reply_says_why_when_the_adapter_is_not_fresh() {
    let repo = checkout(&files());
    let building = AdapterStatus::new("go", AdapterState::Building { waited_ms: 20_000 });
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![(None, building)]);
    let file = repo.root.join("a.go");
    let tree = call_tree_result(&wrapped, &file, "Run", 3, false).unwrap();
    let reply = Response::success("known-answer", serialized_value("known-answer", "call_tree", &tree).unwrap());
    let reply = finish_reply(&request("call_tree"), "call_tree", &wrapped, reply);
    assert_eq!(reply.data["complete"], false);
    assert!(reply.data["adapter_reason"].as_str().unwrap().starts_with("go: building"), "{:#}", reply.data);
    assert_eq!(reply.data["adapters"][0]["status"], "building");
    assert!(reply.data["text"].as_str().unwrap().contains("adapter go: building"));

    // A failed reply passes through untouched.
    let failure = Response::error("known-answer", "symbol_not_found", "no such symbol");
    let failure = finish_reply(&request("callers"), "callers", &wrapped, failure);
    assert_eq!(failure.data, json!({"code": "symbol_not_found", "message": "no such symbol"}));
}

// ---------------------------------------------------------------------------
// Parent folder: the wrap in `parent_query/mod.rs` must show adapter edges too. Stock
// parent-folder mode never answers one-shot ("still loading"), so this runs in-process.
// ---------------------------------------------------------------------------

const HELPER_RS: &str = "pub fn real_run() {}\n\npub fn fire_it(handler: &Handler) {\n    handler.fire();\n}\n\npub struct Handler;\n";

fn git(root: &Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    crate::test_env::apply_hermetic_git_env(command.current_dir(root));
    let output = command.args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn rust_overlay(root: &Path) -> Overlay {
    let (line, col) = pos(HELPER_RS, "handler.fire(", "handler.fire".len());
    let (target_line, target_col) = pos(HELPER_RS, "fn real_run", "fn ".len());
    let lines = [
        json!({"type": "header", "contract": 1, "language": "rust", "tool": "test"}),
        json!({"type": "analysed", "files": ["src/lib.rs"]}),
        json!({"type": "site", "file": "src/lib.rs", "line": line, "col": col, "name": "fire", "targets": [
            {"file": "src/lib.rs", "line": target_line, "col": target_col, "name": "real_run", "kind": "dynamic"}]}),
        json!({"type": "end", "sites": 1, "targets": 1, "facts": 0, "analysed_files": 1, "unanalysed_files": 0}),
    ];
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    let entry: RegistryEntry = serde_json::from_value(
        json!({"contract": 1, "language": "rust", "root_markers": ["Cargo.toml"], "command": ["adapter"]}),
    )
    .unwrap();
    let output = parse_output(text.as_bytes(), &entry).unwrap_or_else(|violation| panic!("{violation}"));
    Arc::new(FixtureOverlay::from_output(root, output, vec![]).unwrap())
}

#[test]
fn parent_folder_reply_has_adapter_edges() {
    crate::parent_search::set_default_refresh_interval(Duration::from_millis(100));
    let temp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let parent_root = base.join("projects");
    let child = parent_root.join("alpha");
    let storage = base.join("storage");
    std::fs::create_dir_all(&storage).unwrap();
    // Two committed crates make `projects` a folder of repositories, not a checkout. Both are
    // complete, so no child build is left running when the test ends.
    for name in ["alpha", "beta"] {
        let crate_root = parent_root.join(name);
        std::fs::create_dir_all(crate_root.join("src")).unwrap();
        std::fs::write(
            crate_root.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        std::fs::write(crate_root.join("src/lib.rs"), HELPER_RS).unwrap();
        git(&crate_root, &["init", "-q"]);
        git(&crate_root, &["config", "user.email", "parent@example.test"]);
        git(&crate_root, &["config", "user.name", "Parent Test"]);
        git(&crate_root, &["add", "."]);
        git(&crate_root, &["commit", "-q", "-m", "initial"]);
    }

    let ctx = Arc::new(crate::context::AppContext::new(
        Box::new(crate::parser::TreeSitterProvider::new()),
        crate::config::Config { storage_dir: Some(storage.clone()), ..crate::config::Config::default() },
    ));
    let configure: RawRequest = serde_json::from_value(json!({
        "id": "configure", "command": "configure", "harness": "opencode",
        "project_root": parent_root, "storage_dir": storage,
        "config": [{"tier": "user", "source": "/u/aft.jsonc", "doc": json!({
            "indexes": {"trigram": true, "semantic": false, "callgraph": true}}).to_string()}],
    }))
    .unwrap();
    let response = {
        // The guard holds the process-wide environment lock: keep it for `configure` only.
        let _git = crate::test_env::hermetic_git_env_guard();
        crate::commands::configure::handle_configure(&configure, &ctx)
    };
    assert!(response.success, "configure failed: {response:?}");
    crate::runtime_drain::drain_deferred_configure_maintenance(&ctx);
    let deadline = Instant::now() + Duration::from_secs(180);
    let session = loop {
        crate::runtime_drain::drain_deferred_configure_maintenance(&ctx);
        if let Some(session) = crate::parent_search::session_for(&ctx) {
            break session;
        }
        assert!(Instant::now() < deadline, "no parent session started");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(session.wait_rounds(1, Duration::from_secs(180)), "the first load never ended");

    let ask = || {
        let req: RawRequest = serde_json::from_value(json!({
            "id": "parent-callers", "command": "callers",
            "file": "alpha/src/lib.rs", "symbol": "real_run",
        }))
        .unwrap();
        crate::commands::parent_query::route(&req, &ctx).expect("a parent folder answers callers itself")
    };
    let settled = |ask: &dyn Fn() -> Response| loop {
        let answer = ask();
        if answer.success {
            return answer;
        }
        assert!(Instant::now() < deadline, "never settled: {:#}", answer.data);
        std::thread::sleep(Duration::from_millis(50));
    };

    // Without an overlay the child's own graph has no caller of `real_run`.
    let plain = settled(&ask);
    assert!(!plain.data.to_string().contains("fire_it"), "{:#}", plain.data);

    // With the fixture overlay the same reply names `fire_it`, which only the adapter knows.
    let answers = vec![(
        Some(rust_overlay(&child)),
        AdapterStatus::new("rust", AdapterState::Fresh { sites: 1, facts: 0 }),
    )];
    super::TEST_ANSWERS.with(|cell| *cell.borrow_mut() = Some(answers));
    let with_overlay = settled(&ask);
    super::TEST_ANSWERS.with(|cell| *cell.borrow_mut() = None);
    assert!(with_overlay.data.to_string().contains("fire_it"), "{:#}", with_overlay.data);
    assert!(with_overlay.data.to_string().contains("alpha/src/lib.rs"), "{:#}", with_overlay.data);

    // Leave nothing behind for the tests that run after this one: stop the parent session.
    drop(session);
    crate::parent_search::release_context(&ctx);
}

// ---------------------------------------------------------------------------
// Call-site tags in the text
// ---------------------------------------------------------------------------

/// `Run` calls `s.ext.CreateTx()` (the adapter adds `pg.CreateTx`, the call is in a loop and in an
/// error branch) and `helper()` (upstream resolves it; the call is deferred).
fn tagged_overlay(root: &Path) -> Overlay {
    overlay(
        root,
        &["mock"],
        &["a.go", "b.go", "mocks/m.go"],
        vec![
            tagged_site(
                "a.go",
                A_GO,
                "s.ext.CreateTx(",
                &["in_loop", "in_error_branch"],
                vec![target("b.go", B_GO, "func (pg) CreateTx", "func (pg) ".len(), &[])],
            ),
            tagged_site(
                "a.go",
                A_GO,
                "helper(",
                &["in_defer"],
                vec![target("a.go", A_GO, "func helper", "func ".len(), &[])],
            ),
        ],
    )
}

fn text_of(wrapped: &AdapterGraph, operation: &str, reply: Value) -> String {
    let reply = Response::success("known-answer", reply);
    let reply = finish_reply(&request(operation), operation, wrapped, reply);
    reply.data["text"].as_str().unwrap().to_string()
}

#[test]
fn site_tags_show_in_call_tree_text_next_to_the_adapter_mark() {
    let repo = checkout(&files());
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(tagged_overlay(&repo.root))]);
    let tree = call_tree_result(&wrapped, &repo.root.join("a.go"), "Run", 3, false).unwrap();
    let text = text_of(&wrapped, "call_tree", serialized_value("known-answer", "call_tree", &tree).unwrap());
    // An added edge: the adapter mark, then the tags.
    assert!(text.contains("CreateTx adapter:go (interface) [in_loop, in_error_branch]"), "{text}");
    // An edge upstream resolved itself: the tags alone.
    assert!(text.contains("helper [in_defer]"), "{text}");
    // Only the added edge is counted as an adapter edge.
    assert!(text.contains("1 adapter edge considered"), "{text}");
    // The tags are text only.
    assert!(!serde_json::to_string(&tree).unwrap().contains("in_loop"));
}

#[test]
fn site_tags_show_in_callers_text() {
    let repo = checkout(&files());
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(tagged_overlay(&repo.root))]);

    let callers = callers_result(&wrapped, &repo.root.join("b.go"), "CreateTx", 1, false).unwrap();
    let text = text_of(&wrapped, "callers", serialized_value("known-answer", "callers", &callers).unwrap());
    assert!(text.contains("via adapter:go (interface) [in_loop, in_error_branch]"), "{text}");

    let callers = callers_result(&wrapped, &repo.root.join("a.go"), "helper", 1, false).unwrap();
    let text = text_of(&wrapped, "callers", serialized_value("known-answer", "callers", &callers).unwrap());
    assert!(text.contains("Run") && text.contains("via [in_defer]"), "{text}");
}

#[test]
fn a_site_without_tags_is_marked_as_before() {
    let repo = checkout(&files());
    let wrapped = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(common_overlay(&repo.root))]);
    let tree = call_tree_result(&wrapped, &repo.root.join("a.go"), "Run", 3, false).unwrap();
    let text = text_of(&wrapped, "call_tree", serialized_value("known-answer", "call_tree", &tree).unwrap());
    assert!(text.contains("adapter:go (interface)") && !text.contains("[in_"), "{text}");
}

// ---------------------------------------------------------------------------
// The mapping memo
// ---------------------------------------------------------------------------

#[test]
fn the_memo_serves_the_same_mapping_until_a_slice_or_the_overlay_changes() {
    use crate::data_layer::trace_measure::memory_reader;

    let repo = checkout(&files());
    let overlay = common_overlay(&repo.root);
    let first = memory_reader(&repo.root);
    let source = first.source();

    let kept = super::memo::file_sites(&overlay, source, "a.go").expect("a.go has sites");
    assert!(Arc::ptr_eq(&kept, &super::memo::file_sites(&overlay, source, "a.go").unwrap()), "same inputs: the memo answers");
    assert!(!kept.entries.is_empty() && kept.entries.iter().all(|entry| entry.mapped.is_some()));

    // Another overlay object (a rebuilt overlay, other hidden tags) is another memo entry.
    let other = common_overlay(&repo.root);
    assert!(!Arc::ptr_eq(&kept, &super::memo::file_sites(&other, source, "a.go").unwrap()));

    // The caller file is parsed again (it changed, or its slice was evicted): a new mapping.
    let reparsed = memory_reader(&repo.root);
    assert!(!Arc::ptr_eq(&kept, &super::memo::file_sites(&overlay, reparsed.source(), "a.go").unwrap()));

    // The checkout changes: `b.go` gains a blank line, so `pg.CreateTx` moves down and the target
    // position the overlay holds is wrong now. The new mapping must see that, not the old one.
    let before = super::memo::file_sites(&overlay, source, "a.go").unwrap();
    let shifted = checkout(&[("a.go", A_GO), ("b.go", &format!("\n{B_GO}")), ("mocks/m.go", MOCK_GO)]);
    let moved = memory_reader(&shifted.root);
    let after = super::memo::file_sites(&overlay, moved.source(), "a.go").unwrap();
    assert!(!Arc::ptr_eq(&before, &after));
    assert!(
        after.entries.iter().any(|entry| !entry.all_targets_mapped),
        "the target position no longer holds a function in the new b.go"
    );
}

#[test]
fn a_memoised_answer_equals_the_first_answer() {
    let repo = checkout(&files());
    let overlay = common_overlay(&repo.root);
    let first = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(Arc::clone(&overlay))]);
    let run = node(&first, "a.go", "Run");
    let cold = (first.outgoing_calls_of(&run).unwrap(), first.unresolved_calls_of(&run).unwrap());
    let second = AdapterGraph::with_overlays(data_graph(&repo.root), vec![fresh(overlay)]);
    let warm = (second.outgoing_calls_of(&run).unwrap(), second.unresolved_calls_of(&run).unwrap());
    assert_eq!(cold, warm);
}
