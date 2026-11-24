//! `aft cli <command> …` (owner: W1): the one-shot client. It finds the root,
//! refuses a root that is not a git work tree with a HEAD commit, starts aft in
//! NDJSON mode (external protocol only), retries an ambiguous bare name, maps
//! fact aliases to `aft facts`, and prints the reply text.
//!
//! Exit codes: 0 ok, 1 the command failed (reply `success:false`), 2 usage,
//! 3 root refused (not a git work tree with a HEAD commit).

mod root;
mod session;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::contract::AliasBy;
use super::{adapters_cmd, facts_cmd, guide, registry};
use root::RootError;
use session::{reply_message, reply_timeout, Session};

const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_ROOT_REFUSED: i32 = 3;

/// Default depth of `trace_data` (the same as the old wrapper).
const TRACE_DATA_DEFAULT_DEPTH: u32 = 5;
/// Default depth of `call_tree` in this client (upstream's default is 5). With adapter edges every
/// interface call expands to all its implementations, so depth 5 often reaches upstream's 50 KB
/// reply limit. `--depth N` asks for more.
const CALL_TREE_DEFAULT_DEPTH: u64 = 2;
/// The graph commands that take `--depth N` and `--include-tests`.
const GRAPH_FLAG_COMMANDS: &[&str] = &["callers", "call_tree", "impact", "trace_to", "trace_to_symbol"];
/// How long `search` and `similar` wait for upstream's semantic index to be built or attached:
/// `AFT_CLI_SEMANTIC_WAIT_S`, default 300 s.
const SEMANTIC_WAIT_DEFAULT_S: u64 = 300;
/// The pause between two asks while the semantic index is building.
const SEMANTIC_WAIT_STEP: Duration = Duration::from_millis(500);
/// A progress line on stderr at the start and then every this many seconds.
const SEMANTIC_PROGRESS_EVERY_S: u64 = 10;
/// An ambiguous name can be retried once per name argument at most (symbol, toSymbol).
const MAX_AMBIGUITY_RETRIES: usize = 2;

const USAGE: &str = "\
usage: aft cli <command> <args…> [--json]

Paths are relative to the current directory.
Symbols can be bare names (CreateOrder) or qualified (service::CreateOrder).

  outline <file|dir>                          structure without bodies
  zoom <file> [symbol]                        one symbol with its same-file calls
  read <file> [start_line] [limit]            lines with numbers
  grep <pattern> [path]                       indexed text search
  glob <pattern> [path]                       files by pattern
  callers <file> <symbol>                     who calls it (default depth 1)
  call_tree <file> <symbol>                   what it calls (default depth 2 here)
  impact <file> <symbol>                      what a change reaches
  trace_to <file> <symbol>                    how execution reaches it
  trace_to_symbol <file> <symbol> <toSymbol> [toFile]
      these five also take --depth N and --include-tests (test files are hidden by default)
  trace_data <file> <symbol> <expression> [depth]   (a field path such as req.Amount shows its root variable when it has no hops)
  search <query> [path]                       upstream semantic search (meaning and words)
  similar <file> <symbol>                     code like this symbol (upstream semantic search on its name)
  facts[<kind> (--label <key> [--prefix] | <file> <symbol> [--by subject|object|either])]
  adapters list|status|build [--root <repo>]
  guide                                       how to use aft here, registered languages and fact kinds
  <alias> …                                   a fact alias of the registry (see: aft guide)

--json prints the raw reply.";

/// Entry point for `aft cli …`; returns the exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let rest: Vec<String> = args.iter().filter(|arg| *arg != "--json").cloned().collect();
    let Some(command) = rest.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return EXIT_USAGE;
    };
    let operands = &rest[1..];
    match command {
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            EXIT_OK
        }
        "guide" => guide::run(operands),
        "adapters" => adapters_cmd::run(&with_json(operands, json)),
        "facts" => facts_command(operands, json),
        "outline" | "zoom" | "read" | "grep" | "glob" | "callers" | "call_tree" | "impact" | "trace_to"
        | "trace_to_symbol" | "trace_data" | "search" | "similar" => one_shot(command, operands, json),
        _ => alias_command(command, operands, json),
    }
}

fn with_json(operands: &[String], json: bool) -> Vec<String> {
    let mut args = operands.to_vec();
    if json {
        args.push("--json".to_string());
    }
    args
}

fn usage_error(message: &str) -> i32 {
    eprintln!("aft cli: {message}");
    EXIT_USAGE
}

fn root_error(error: RootError) -> i32 {
    match error {
        RootError::Usage(message) => usage_error(&message),
        RootError::NotSearchable(message) => {
            eprintln!("aft cli: {message}");
            EXIT_ROOT_REFUSED
        }
    }
}

/// The root for `anchor` (a path), checked against the git rule. The registry's root
/// markers count as language markers. A broken registry is not an error here: the
/// built-in graph still answers, and the reply names the registry problem.
fn searchable_root(anchor: &str) -> Result<PathBuf, RootError> {
    let target = root::absolute(anchor)?;
    let registry_markers: Vec<String> = registry::load()
        .map(|loaded| loaded.registry.adapters.into_iter().flat_map(|entry| entry.root_markers).collect())
        .unwrap_or_default();
    let found = root::find_root(&target, &registry_markers)?;
    root::require_git_commit(&found)?;
    Ok(found)
}

// ---------------------------------------------------------------------------
// Graph and basic commands: one aft session
// ---------------------------------------------------------------------------

/// The aft request for one command, and the path its root is found from.
struct Request {
    params: Value,
    anchor: String,
}

fn abs_string(path: &str) -> Result<String, RootError> {
    Ok(root::absolute(path)?.to_string_lossy().into_owned())
}

fn build_request(command: &str, operands: &[String]) -> Result<Request, RootError> {
    let usage = |text: &str| RootError::Usage(format!("usage: aft cli {command} {text}"));
    // `--depth N` and `--include-tests` (graph commands only) may stand anywhere; the rest are positional.
    let GraphFlags { rest: operands, depth, include_tests } = take_graph_flags(command, operands).map_err(|message| usage(&message))?;
    let operands = operands.as_slice();
    let arg = |index: usize| operands.get(index).map(String::as_str);
    let count = operands.len();
    let mut params = json!({ "command": command });
    let anchor;
    match command {
        "outline" => {
            if count != 1 {
                return Err(usage("<file|dir>"));
            }
            anchor = operands[0].clone();
            let abs = abs_string(&anchor)?;
            let key = if std::path::Path::new(&abs).is_dir() { "directory" } else { "file" };
            params[key] = json!(abs);
        }
        "zoom" => {
            if !(1..=2).contains(&count) {
                return Err(usage("<file> [symbol]"));
            }
            anchor = operands[0].clone();
            params["file"] = json!(abs_string(&anchor)?);
            if let Some(symbol) = arg(1) {
                params["symbol"] = json!(symbol);
            }
        }
        "read" => {
            if !(1..=3).contains(&count) {
                return Err(usage("<file> [start_line] [limit]"));
            }
            anchor = operands[0].clone();
            params["file"] = json!(abs_string(&anchor)?);
            for (index, key) in [(1, "start_line"), (2, "limit")] {
                if let Some(text) = arg(index) {
                    let number: u64 = text.parse().map_err(|_| usage(&format!("<file> [start_line] [limit]: {key} must be a number, got '{text}'")))?;
                    params[key] = json!(number);
                }
            }
        }
        "grep" | "glob" => {
            if !(1..=2).contains(&count) {
                return Err(usage("<pattern> [path]"));
            }
            anchor = arg(1).unwrap_or(".").to_string();
            params["pattern"] = json!(operands[0]);
            params["path"] = json!(abs_string(&anchor)?);
        }
        "callers" | "call_tree" | "impact" | "trace_to" => {
            if count != 2 {
                return Err(usage("<file> <symbol>"));
            }
            anchor = operands[0].clone();
            params["file"] = json!(abs_string(&anchor)?);
            params["symbol"] = json!(operands[1]);
            match depth {
                Some(depth) => params["depth"] = json!(depth),
                None if command == "call_tree" => params["depth"] = json!(CALL_TREE_DEFAULT_DEPTH),
                None => {}
            }
            if include_tests {
                params["includeTests"] = json!(true);
            }
        }
        "trace_to_symbol" => {
            if !(3..=4).contains(&count) {
                return Err(usage("<file> <symbol> <toSymbol> [toFile]"));
            }
            anchor = operands[0].clone();
            params["file"] = json!(abs_string(&anchor)?);
            params["symbol"] = json!(operands[1]);
            params["toSymbol"] = json!(operands[2]);
            if let Some(to_file) = arg(3) {
                params["toFile"] = json!(abs_string(to_file)?);
            }
            if let Some(depth) = depth {
                params["depth"] = json!(depth);
            }
            if include_tests {
                params["includeTests"] = json!(true);
            }
        }
        "trace_data" => {
            if !(3..=4).contains(&count) {
                return Err(usage("<file> <symbol> <expression> [depth]"));
            }
            anchor = operands[0].clone();
            params["file"] = json!(abs_string(&anchor)?);
            params["symbol"] = json!(operands[1]);
            params["expression"] = json!(operands[2]);
            let depth = match arg(3) {
                Some(text) => text.parse::<u32>().map_err(|_| usage(&format!("<file> <symbol> <expression> [depth]: depth must be a number, got '{text}'")))?,
                None => TRACE_DATA_DEFAULT_DEPTH,
            };
            params["depth"] = json!(depth);
        }
        "search" => {
            // Upstream's semantic search: it ranks code by meaning (when an embedding model is
            // installed) and by words. `path` limits the search to a directory or file.
            if !(1..=2).contains(&count) {
                return Err(usage("<query> [path]"));
            }
            anchor = arg(1).unwrap_or(".").to_string();
            params = json!({ "command": "semantic_search", "query": operands[0], "path": abs_string(&anchor)? });
        }
        "similar" => {
            // The old fork's own `similar` (stems, TF-IDF, synonyms, co-citation) is not restored.
            // Upstream's semantic search on the symbol's name answers the same question (user rule:
            // use upstream when it supports the need). The search covers the whole root.
            if count != 2 {
                return Err(usage("<file> <symbol>"));
            }
            anchor = operands[0].clone();
            abs_string(&anchor)?; // the file must exist
            let name = operands[1].rsplit("::").next().unwrap_or(&operands[1]);
            params = json!({ "command": "semantic_search", "query": name });
        }
        _ => unreachable!("one_shot is called only for the commands above"),
    }
    Ok(Request { params, anchor })
}

fn one_shot(command: &str, operands: &[String], json_output: bool) -> i32 {
    let request = match build_request(command, operands) {
        Ok(request) => request,
        Err(error) => return root_error(error),
    };
    let root = match searchable_root(&request.anchor) {
        Ok(root) => root,
        Err(error) => return root_error(error),
    };
    let mut session = match Session::start(&root) {
        Ok(session) => session,
        Err(message) => {
            eprintln!("aft cli: {message}");
            return EXIT_FAILED;
        }
    };
    let mut params = request.params;
    if command == "trace_to_symbol" {
        match prefer_concrete_to_symbol(&mut session, &mut params) {
            Ok(Some(note)) => println!("{note}"),
            Ok(None) => {}
            Err(message) => {
                eprintln!("aft cli: {message}");
                return EXIT_FAILED;
            }
        }
    }
    for _ in 0..=MAX_AMBIGUITY_RETRIES {
        let reply = match session.send(params.clone()) {
            Ok(reply) => reply,
            Err(message) => {
                eprintln!("aft cli: {message}");
                return EXIT_FAILED;
            }
        };
        if reply.get("code").and_then(Value::as_str) == Some("ambiguous_symbol") && reply.get("success") == Some(&json!(false)) {
            match resolve_ambiguity(&mut session, &mut params, &reply) {
                Ok(note) => {
                    println!("{note}");
                    continue; // same session, resend with the chosen name
                }
                Err(message) => {
                    eprintln!("aft cli: {message}");
                    return EXIT_FAILED;
                }
            }
        }
        // The wire command (for example `semantic_search` for `search`) picks upstream's text formatter.
        let wire_command = params["command"].as_str().unwrap_or(command);
        let mut reply = reply;
        if wire_command == "semantic_search" {
            match wait_for_semantic_index(&mut session, &params, reply, &root) {
                Ok(waited) => reply = waited,
                Err(message) => {
                    eprintln!("aft cli: {message}");
                    return EXIT_FAILED;
                }
            }
        }
        if command == "trace_data" {
            match retry_field_expression_as_root(&mut session, &params, &reply) {
                Ok(Some((note, root_reply))) => {
                    // With --json stdout stays one JSON reply, so the note goes to stderr.
                    if json_output {
                        eprintln!("{note}");
                    } else {
                        println!("{note}");
                    }
                    reply = root_reply;
                }
                Ok(None) => {}
                Err(message) => {
                    eprintln!("aft cli: {message}");
                    return EXIT_FAILED;
                }
            }
        }
        return print_reply(wire_command, &reply, json_output, &request.anchor);
    }
    eprintln!("aft cli: the name is still ambiguous after {MAX_AMBIGUITY_RETRIES} retries");
    EXIT_FAILED
}

// ---------------------------------------------------------------------------
// search and similar: wait for the semantic index
// ---------------------------------------------------------------------------

/// What to do with a `semantic_search` reply.
#[derive(Debug, PartialEq, Eq)]
enum SemanticWait {
    /// The reply is final: print it.
    Print,
    /// The index is still building and the limit has not passed: wait, then ask again.
    AskAgain,
    /// The index is still building and the limit has passed: print the lexical reply with a note.
    GiveUp,
}

/// `status` is the reply field `status` of upstream's `semantic_search` (it is `building` while the
/// semantic index is built or attached; the reply text then says "lexical fallback results follow").
fn semantic_wait_decision(status: Option<&str>, waited: Duration, limit: Duration) -> SemanticWait {
    match status {
        Some("building") if waited < limit => SemanticWait::AskAgain,
        Some("building") => SemanticWait::GiveUp,
        _ => SemanticWait::Print,
    }
}

/// The wait limit: `AFT_CLI_SEMANTIC_WAIT_S` (default 300 s), never more than the reply timeout.
fn semantic_wait_limit() -> Duration {
    let seconds = std::env::var("AFT_CLI_SEMANTIC_WAIT_S").ok().and_then(|value| value.parse::<u64>().ok()).unwrap_or(SEMANTIC_WAIT_DEFAULT_S);
    Duration::from_secs(seconds).min(reply_timeout())
}

/// `aft cli` is one-shot, so a reply with status `building` would be the lexical fallback every time.
/// While the status is `building`, this prints a progress line on stderr, waits, and sends the same
/// request again in the same session. At the limit the last (lexical) reply stands, with one note line.
fn wait_for_semantic_index(session: &mut Session, params: &Value, mut reply: Value, root: &std::path::Path) -> Result<Value, String> {
    let limit = semantic_wait_limit();
    let started = Instant::now();
    let mut last_progress_s: Option<u64> = None;
    loop {
        let waited = started.elapsed();
        match semantic_wait_decision(reply.get("status").and_then(Value::as_str), waited, limit) {
            SemanticWait::Print => return Ok(reply),
            SemanticWait::GiveUp => {
                eprintln!(
                    "note: the semantic index is still building after {} s; this is the lexical fallback. Finish it with: aft warmup --root {} --only semantic",
                    waited.as_secs(),
                    root.display()
                );
                return Ok(reply);
            }
            SemanticWait::AskAgain => {
                if last_progress_s.is_none_or(|last| waited.as_secs() >= last + SEMANTIC_PROGRESS_EVERY_S) {
                    eprintln!("aft: semantic index is building ({}s)…", waited.as_secs());
                    last_progress_s = Some(waited.as_secs());
                }
                std::thread::sleep(SEMANTIC_WAIT_STEP);
                reply = session.send(params.clone())?;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// trace_data on a field expression
// ---------------------------------------------------------------------------

/// Upstream's `trace_data` follows variables. For `req.Amount` it finds no hop, while `req` has some.
/// When a reply has no hop and the expression is a field path, this asks again with the root variable.
/// Returns the note line and the second reply when that reply has hops. Returns `None` (the first
/// reply stands) when the first reply failed or has hops, the expression is not a field path, or the
/// root variable has no hops either.
fn retry_field_expression_as_root(session: &mut Session, params: &Value, reply: &Value) -> Result<Option<(String, Value)>, String> {
    let has_no_hops = reply.get("success") == Some(&json!(true)) && reply.get("hops").and_then(Value::as_array).is_some_and(|hops| hops.is_empty());
    let expression = params["expression"].as_str().unwrap_or("");
    let Some(root) = field_path_root(expression) else { return Ok(None) };
    if !has_no_hops {
        return Ok(None);
    }
    let mut root_params = params.clone();
    root_params["expression"] = json!(root);
    let root_reply = session.send(root_params)?;
    let root_has_hops = root_reply.get("success") == Some(&json!(true)) && root_reply.get("hops").and_then(Value::as_array).is_some_and(|hops| !hops.is_empty());
    if !root_has_hops {
        return Ok(None);
    }
    Ok(Some((format!("note: {expression} has no hops of its own; showing {root} (the field moves with it, approximate)"), root_reply)))
}

/// The root variable of a field path: `req` for `req.Amount`, `req.Items`, `req.Items.Id`, `items[i].Amount`
/// and `m["k"].F`. `None` for a plain variable (`req`), and for anything with a call or a space
/// (`f(x).F`, `a + b`), which is not a path.
fn field_path_root(expression: &str) -> Option<&str> {
    let expression = expression.trim();
    let root_len = expression.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(expression.len());
    let (root, rest) = expression.split_at(root_len);
    let starts_like_a_name = root.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_');
    let continues_as_a_path = rest.starts_with('.') || rest.starts_with('[');
    let is_only_a_path = !rest.contains(|c: char| c == '(' || c == ')' || c.is_whitespace());
    (starts_like_a_name && continues_as_a_path && is_only_a_path).then_some(root)
}

/// Prints the reply: raw JSON with `--json`; else its `text`, else upstream's own formatter.
/// `target` is the path as the user typed it (upstream's zoom header shows it).
fn print_reply(command: &str, reply: &Value, json_output: bool, target: &str) -> i32 {
    let succeeded = reply.get("success").and_then(Value::as_bool) == Some(true);
    if json_output {
        println!("{reply}");
    } else if succeeded {
        println!("{}", reply_text(command, reply, target));
    } else {
        eprintln!("aft cli: {}", reply_message(reply));
    }
    if succeeded {
        EXIT_OK
    } else {
        EXIT_FAILED
    }
}

fn reply_text(command: &str, reply: &Value, target: &str) -> String {
    if let Some(text) = reply.get("text").and_then(Value::as_str) {
        return text.trim_end_matches('\n').to_string();
    }
    let id = reply.get("id").and_then(Value::as_str).unwrap_or("cli");
    let mut data = reply.clone();
    if let Some(object) = data.as_object_mut() {
        object.remove("id");
        object.remove("success");
    }
    let response = crate::protocol::Response::success(id, data);
    let context = crate::subc_format::FormatContext { zoom_target_label: Some(target.to_string()), ..Default::default() };
    let formatted = crate::subc_format::format_response_with_context(command, &response, &context);
    if formatted.trim().is_empty() {
        serde_json::to_string_pretty(reply).unwrap_or_else(|_| reply.to_string())
    } else {
        formatted.trim_end_matches('\n').to_string()
    }
}

/// The operands without the graph flags, and the flags.
struct GraphFlags {
    rest: Vec<String>,
    depth: Option<u64>,
    include_tests: bool,
}

/// Takes `--depth N` and `--include-tests` out of the operands. Only the commands in
/// `GRAPH_FLAG_COMMANDS` take them.
fn take_graph_flags(command: &str, operands: &[String]) -> Result<GraphFlags, String> {
    let mut flags = GraphFlags { rest: Vec::new(), depth: None, include_tests: false };
    let mut index = 0;
    while index < operands.len() {
        let operand = operands[index].as_str();
        let is_flag = operand == "--depth" || operand == "--include-tests";
        if is_flag && !GRAPH_FLAG_COMMANDS.contains(&command) {
            return Err(format!("{operand} is for {} only", GRAPH_FLAG_COMMANDS.join(", ")));
        }
        match operand {
            "--depth" => {
                let value = operands.get(index + 1).ok_or("--depth needs a number")?;
                let depth = value.parse::<u64>().ok().filter(|depth| *depth >= 1);
                flags.depth = Some(depth.ok_or(format!("--depth needs a whole number of 1 or more, got '{value}'"))?);
                index += 2;
            }
            "--include-tests" => {
                flags.include_tests = true;
                index += 1;
            }
            _ => {
                flags.rest.push(operands[index].clone());
                index += 1;
            }
        }
    }
    Ok(flags)
}

// ---------------------------------------------------------------------------
// Ambiguous bare names (spike section 6)
// ---------------------------------------------------------------------------

/// Upstream refuses a bare Go method name when the same file has an interface with that
/// method. This drops interface-member candidates (see below).
/// One candidate left: it is written into `params` and a note line is returned.
/// Otherwise an error message lists the candidates.
fn resolve_ambiguity(session: &mut Session, params: &mut Value, reply: &Value) -> Result<String, String> {
    let message = reply.get("message").and_then(Value::as_str).unwrap_or("");
    let (bare, candidates) = parse_ambiguity(message).ok_or_else(|| format!("ambiguous name, and the reply could not be read: {message}"))?;
    // Which argument was ambiguous: `symbol` (in `file`) or `toSymbol` (in `toFile`, else `file`).
    let (key, file_key) = if params["symbol"] == json!(bare) {
        ("symbol", "file")
    } else if params["toSymbol"] == json!(bare) {
        ("toSymbol", if params.get("toFile").is_some() { "toFile" } else { "file" })
    } else {
        return Err(format!("ambiguous name '{bare}' is not one of the arguments sent: {message}"));
    };
    let file = params[file_key].as_str().unwrap_or("").to_string();
    let outline = session.send(json!({ "command": "outline", "file": file }))?;
    let outline_text = outline.get("text").and_then(Value::as_str).unwrap_or("");
    let (chosen, note) = choose_candidate(&bare, &candidates, outline_text)?;
    params[key] = json!(chosen);
    Ok(note)
}

/// Picks the one real definition among upstream's candidates, from the file's outline text (Go).
/// Skipped: `I::name` with `I` an interface of the file; and the unqualified `name` when the file
/// declares an interface (trace_data names an interface method without its type) or has no
/// top-level `func name(` (trace_data also names a call site, for example `x.Iface.name(…)`).
/// One candidate left: it is chosen, with a note line. Otherwise an error lists the candidates.
fn choose_candidate(bare: &str, candidates: &[String], outline_text: &str) -> Result<(String, String), String> {
    let interfaces = interfaces_in_outline(outline_text);
    let has_top_level_function = defines_top_level_function(outline_text, bare);
    let mut skipped: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for candidate in candidates {
        match candidate.split_once("::") {
            Some((owner, _)) if interfaces.iter().any(|name| name == owner) => skipped.push(format!("interface member {owner}::")),
            None if candidate == bare && !interfaces.is_empty() => skipped.push("interface member".to_string()),
            None if candidate == bare && !has_top_level_function => {
                skipped.push("unqualified name with no top-level function in this file".to_string())
            }
            _ => kept.push(candidate.clone()),
        }
    }
    match kept.as_slice() {
        [chosen] if !skipped.is_empty() => {
            skipped.dedup();
            Ok((chosen.clone(), format!("note: {bare} → {chosen} ({} skipped)", skipped.join(", "))))
        }
        _ => Err(format!("'{bare}' is ambiguous. Use one of: {}", candidates.join(", "))),
    }
}

/// True when the outline text has a top-level Go function `name` (`func name(` or `func name[`),
/// not a method (`func (r *T) name(`).
fn defines_top_level_function(outline_text: &str, name: &str) -> bool {
    let plain = format!("func {name}(");
    let generic = format!("func {name}[");
    outline_text.lines().any(|line| line.contains(&plain) || line.contains(&generic))
}

/// Upstream does not report an ambiguous `toSymbol`: it takes a node of that name. When the target
/// file declares an interface with that method AND has exactly one concrete method of that name, the
/// interface member would be a valid end too (it is a call target since the Go adapter lists it), so
/// the concrete method is sent by its qualified name, as for an ambiguous `symbol`.
fn prefer_concrete_to_symbol(session: &mut Session, params: &mut Value) -> Result<Option<String>, String> {
    let Some(bare) = params["toSymbol"].as_str().map(str::to_string) else { return Ok(None) };
    if bare.contains("::") || bare.contains('.') {
        return Ok(None);
    }
    let file_key = if params.get("toFile").is_some() { "toFile" } else { "file" };
    let file = params[file_key].as_str().unwrap_or("").to_string();
    let outline = session.send(json!({ "command": "outline", "file": file }))?;
    let outline_text = outline.get("text").and_then(Value::as_str).unwrap_or("");
    let interfaces = interfaces_in_outline(outline_text);
    let owners = concrete_method_owners(outline_text, &bare);
    match owners.as_slice() {
        [owner] if !interfaces.is_empty() && !interfaces.contains(owner) => {
            let chosen = format!("{owner}::{bare}");
            params["toSymbol"] = json!(chosen);
            Ok(Some(format!("note: {bare} → {chosen} (interface member skipped)")))
        }
        _ => Ok(None),
    }
}

/// The receiver types of the Go methods named `name` in an outline text
/// (`func (r *T) name(`, `func (T) name[`).
fn concrete_method_owners(outline_text: &str, name: &str) -> Vec<String> {
    let mut owners: Vec<String> = Vec::new();
    for line in outline_text.lines() {
        let Some(after_func) = line.split_once("func (").map(|(_, rest)| rest) else { continue };
        let Some((receiver, rest)) = after_func.split_once(')') else { continue };
        let rest = rest.trim_start();
        if !(rest.starts_with(&format!("{name}(")) || rest.starts_with(&format!("{name}["))) {
            continue;
        }
        let type_word = receiver.split_whitespace().last().unwrap_or("");
        let owner = type_word.trim_start_matches('*').split('[').next().unwrap_or("").to_string();
        if !owner.is_empty() && !owners.contains(&owner) {
            owners.push(owner);
        }
    }
    owners
}

/// `symbol 'X' is ambiguous, candidates: [A, B]` → (`X`, [`A`, `B`]).
fn parse_ambiguity(message: &str) -> Option<(String, Vec<String>)> {
    let after_quote = message.split_once("symbol '")?.1;
    let bare = after_quote.split_once('\'')?.0.to_string();
    let list = message.split_once("candidates: [")?.1.split_once(']')?.0;
    let candidates = list.split(", ").map(|name| name.trim().to_string()).filter(|name| !name.is_empty()).collect();
    Some((bare, candidates))
}

/// Names declared as `type <Name> interface` in an outline text.
fn interfaces_in_outline(outline: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in outline.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        // `type Name interface …` and `type Name[T any] interface …`.
        if let Some(at) = words.iter().position(|word| *word == "type") {
            if let Some(name) = words.get(at + 1) {
                if words[at + 2..].contains(&"interface") {
                    names.push(name.split('[').next().unwrap_or(name).to_string());
                }
            }
        }
    }
    names
}

// ---------------------------------------------------------------------------
// Facts and fact aliases: handed to `aft facts` (in this process)
// ---------------------------------------------------------------------------

/// Flags that take a value. Everything else starting with `--` is a switch.
const VALUE_FLAGS: &[&str] = &["--label", "--depth", "--include-tag", "--by", "--root"];

/// `aft cli facts …` operands split into positional arguments and flags.
struct FactOperands {
    positionals: Vec<String>,
    /// Flags to pass on, with `--include-mocks` already written as `--include-tag mock`.
    flags: Vec<String>,
    root: Option<String>,
    has_label: bool,
}

fn split_fact_operands(operands: &[String]) -> Result<FactOperands, String> {
    let mut split = FactOperands { positionals: Vec::new(), flags: Vec::new(), root: None, has_label: false };
    let mut iter = operands.iter();
    while let Some(operand) = iter.next() {
        if operand == "--include-mocks" {
            split.flags.extend(["--include-tag".to_string(), "mock".to_string()]);
        } else if VALUE_FLAGS.contains(&operand.as_str()) {
            let value = iter.next().ok_or_else(|| format!("{operand} needs a value"))?;
            match operand.as_str() {
                "--root" => split.root = Some(value.clone()),
                "--label" => {
                    split.has_label = true;
                    split.flags.extend([operand.clone(), value.clone()]);
                }
                _ => split.flags.extend([operand.clone(), value.clone()]),
            }
        } else if operand.starts_with("--") {
            split.flags.push(operand.clone());
        } else {
            split.positionals.push(operand.clone());
        }
    }
    Ok(split)
}

/// `aft cli facts [<kind> …]`.
fn facts_command(operands: &[String], json_output: bool) -> i32 {
    let split = match split_fact_operands(operands) {
        Ok(split) => split,
        Err(message) => return usage_error(&message),
    };
    let Some(kind) = split.positionals.first() else {
        // `aft facts` lists the kinds. A root is passed only when the caller gave one.
        let mut args = split.flags.clone();
        if let Some(root) = &split.root {
            args.extend(["--root".to_string(), root.clone()]);
        }
        return facts_cmd::run(&with_json(&args, json_output));
    };
    // `<kind> --label <key>` has no path: the root comes from the current directory.
    // `<kind> <file> <symbol>`: the root comes from the file.
    let mut args = vec![kind.clone()];
    let anchor = if split.has_label {
        ".".to_string()
    } else {
        let (Some(file), Some(symbol)) = (split.positionals.get(1), split.positionals.get(2)) else {
            return usage_error("usage: aft cli facts <kind> (--label <key> [--prefix] | <file> <symbol> [--by subject|object|either])");
        };
        match abs_string(file) {
            Ok(abs) => args.extend([abs, symbol.clone()]),
            Err(error) => return root_error(error),
        }
        file.clone()
    };
    args.extend(split.flags.iter().cloned());
    run_facts(args, split.root, &anchor, json_output)
}

/// `aft cli <alias> …`: the registry says which fact kind and which lookup mode.
fn alias_command(name: &str, operands: &[String], json_output: bool) -> i32 {
    let registry = match registry::load() {
        Ok(loaded) => loaded.registry,
        Err(message) => return usage_error(&format!("unknown command '{name}', and {message}")),
    };
    let found = registry
        .adapters
        .iter()
        .flat_map(|entry| entry.facts.iter())
        .find_map(|fact| fact.aliases.iter().find(|alias| alias.name == name).map(|alias| (fact.kind.clone(), alias.by)));
    let Some((kind, by)) = found else {
        eprintln!("aft cli: unknown command '{name}'\n\n{USAGE}");
        return EXIT_USAGE;
    };
    let split = match split_fact_operands(operands) {
        Ok(split) => split,
        Err(message) => return usage_error(&message),
    };
    let usage = guide::alias_usage(&kind, name, by);
    let mut args = vec![kind];
    let anchor = match by {
        AliasBy::Label => {
            let [key] = split.positionals.as_slice() else {
                return usage_error(&format!("usage: {usage}"));
            };
            args.extend(["--label".to_string(), key.clone()]);
            ".".to_string()
        }
        AliasBy::Subject | AliasBy::Object | AliasBy::Either => {
            let [file, symbol] = split.positionals.as_slice() else {
                return usage_error(&format!("usage: {usage}"));
            };
            let side = match by {
                AliasBy::Subject => "subject",
                AliasBy::Object => "object",
                _ => "either",
            };
            match abs_string(file) {
                Ok(abs) => args.extend([abs, symbol.clone(), "--by".to_string(), side.to_string()]),
                Err(error) => return root_error(error),
            }
            file.clone()
        }
    };
    args.extend(split.flags.iter().cloned());
    run_facts(args, split.root, &anchor, json_output)
}

/// Adds `--root` (the given one, else the root found from `anchor`) and runs `aft facts`.
fn run_facts(mut args: Vec<String>, root: Option<String>, anchor: &str, json_output: bool) -> i32 {
    let root = match root {
        Some(root) => root,
        None => match searchable_root(anchor) {
            Ok(found) => found.to_string_lossy().into_owned(),
            // A label lookup has no file: its anchor is the current directory.
            Err(RootError::Usage(message)) if anchor == "." => {
                return usage_error(&format!("{message}\nA key has no file, so give the repository: add --root <repo>."));
            }
            Err(error) => return root_error(error),
        },
    };
    args.extend(["--root".to_string(), root]);
    facts_cmd::run(&with_json(&args, json_output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_upstream_ambiguity_message() {
        let message = "symbol 'CreateOrder' is ambiguous, candidates: [Service::CreateOrder, service::CreateOrder]";
        let (bare, candidates) = parse_ambiguity(message).unwrap();
        assert_eq!(bare, "CreateOrder");
        assert_eq!(candidates, ["Service::CreateOrder", "service::CreateOrder"]);
    }

    #[test]
    fn finds_interfaces_in_outline_text() {
        let outline = "service.go\n  E type Service interface 60:122\n  type service struct 124:167\n  type Repo[T any] interface 170:180\n";
        assert_eq!(interfaces_in_outline(outline), ["Service", "Repo"]);
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn interface_member_is_skipped() {
        let outline = "service.go\n  E type Service interface 60:122\n  type service struct 124:167\n    .E func (s *service) Create(ctx context.Context) error 130:140\n";
        let (chosen, note) = choose_candidate("Create", &names(&["Service::Create", "service::Create"]), outline).unwrap();
        assert_eq!(chosen, "service::Create");
        assert!(note.contains("interface member Service::"), "{note}");
    }

    #[test]
    fn call_site_name_without_a_top_level_function_is_skipped() {
        // trace_data on a handler file (point 2, case A6).
        let outline = "handler.go\n  type orderHandler struct 54:77\n    .E func (gh *orderHandler) CreateOrder(ctx context.Context) error 2824:2850\n";
        let candidates = names(&["CreateOrder", "orderHandler::CreateOrder"]);
        let (chosen, note) = choose_candidate("CreateOrder", &candidates, outline).unwrap();
        assert_eq!(chosen, "orderHandler::CreateOrder");
        assert!(note.contains("no top-level function"), "{note}");
    }

    #[test]
    fn a_real_top_level_function_and_a_method_stay_ambiguous() {
        let outline = "a.go\n  E func Run(x int) 3:5\n  type T struct 7:8\n    .E func (t T) Run() 9:10\n";
        assert!(choose_candidate("Run", &names(&["Run", "T::Run"]), outline).is_err());
    }

    #[test]
    fn concrete_method_owners_come_from_the_receiver() {
        // a store file (point 2, case A5): an interface and its implementation.
        let outline = "order_store.go\n  E type OrderStorer interface 20:60\n  type orderStore struct 62:65\n    .E func (ps *orderStore) GetByID(ctx context.Context, id string) (p Order, err error) 236:250\n    .func (orderStore) helper() 252:253\n  E func GetByIDs[T any](x T) 260:262\n";
        assert_eq!(concrete_method_owners(outline, "GetByID"), ["orderStore"]);
        assert_eq!(concrete_method_owners(outline, "helper"), ["orderStore"]);
        assert!(concrete_method_owners(outline, "GetByIDs").is_empty(), "a top-level function has no receiver");
    }

    #[test]
    fn graph_flags_are_taken_out_for_graph_commands_only() {
        let flags = take_graph_flags("call_tree", &names(&["a.go", "--depth", "3", "F", "--include-tests"])).unwrap();
        assert_eq!(flags.rest, ["a.go", "F"]);
        assert_eq!(flags.depth, Some(3));
        assert!(flags.include_tests);
        assert!(take_graph_flags("outline", &names(&["a.go", "--depth", "3"])).is_err());
        assert!(take_graph_flags("read", &names(&["a.go", "--include-tests"])).is_err());
        assert!(take_graph_flags("callers", &names(&["a.go", "F", "--depth", "0"])).is_err());
        let plain = take_graph_flags("callers", &names(&["a.go", "F"])).unwrap();
        assert_eq!((plain.depth, plain.include_tests), (None, false));
    }

    #[test]
    fn semantic_wait_decision_follows_status_and_limit() {
        let limit = Duration::from_secs(300);
        let at = |seconds| Duration::from_secs(seconds);
        assert_eq!(semantic_wait_decision(Some("building"), at(0), limit), SemanticWait::AskAgain);
        assert_eq!(semantic_wait_decision(Some("building"), at(299), limit), SemanticWait::AskAgain);
        assert_eq!(semantic_wait_decision(Some("building"), at(300), limit), SemanticWait::GiveUp);
        assert_eq!(semantic_wait_decision(Some("ready"), at(0), limit), SemanticWait::Print);
        assert_eq!(semantic_wait_decision(Some("partial"), at(500), limit), SemanticWait::Print);
        assert_eq!(semantic_wait_decision(None, at(0), limit), SemanticWait::Print);
        assert_eq!(semantic_wait_decision(Some("building"), at(0), Duration::ZERO), SemanticWait::GiveUp);
    }

    #[test]
    fn a_field_path_has_its_root_variable() {
        assert_eq!(field_path_root("req.Amount"), Some("req"));
        assert_eq!(field_path_root("req.Items.Id"), Some("req"));
        assert_eq!(field_path_root("items[i].Amount"), Some("items"));
        assert_eq!(field_path_root("m[\"k\"].F"), Some("m"));
        assert_eq!(field_path_root(" req.Amount "), Some("req"));
        assert_eq!(field_path_root("_x1.F"), Some("_x1"));
    }

    #[test]
    fn a_variable_or_a_computation_has_no_root_to_retry() {
        assert_eq!(field_path_root("req"), None);
        assert_eq!(field_path_root(""), None);
        assert_eq!(field_path_root("f(x).F"), None);
        assert_eq!(field_path_root("a + b.C"), None);
        assert_eq!(field_path_root("x[f(i)].F"), None);
        assert_eq!(field_path_root("&req.Amount"), None);
        assert_eq!(field_path_root(".F"), None);
        assert_eq!(field_path_root("1x.F"), None);
    }

    #[test]
    fn include_mocks_becomes_include_tag() {
        let operands: Vec<String> = ["a.go", "X", "--include-mocks"].iter().map(|s| s.to_string()).collect();
        let split = split_fact_operands(&operands).unwrap();
        assert_eq!(split.positionals, ["a.go", "X"]);
        assert_eq!(split.flags, ["--include-tag", "mock"]);
    }

    #[test]
    fn value_flag_without_value_is_a_usage_error() {
        let operands = vec!["--label".to_string()];
        assert!(split_fact_operands(&operands).is_err());
    }
}
