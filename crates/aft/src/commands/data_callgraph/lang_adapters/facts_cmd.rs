//! `aft facts …` (owner: R1).
//!
//! ```text
//! aft facts [--root <repo>] [--json]                       list the declared fact kinds
//! aft facts <kind> --label <key> [--prefix] [--root <repo>] [--include-tag <t>] [--json]
//! aft facts <kind> <file> <symbol> [--by subject|object|either] [--depth N]
//!                                   [--root <repo>] [--include-tag <t>] [--json]
//! ```
//! A symbol is found with upstream's parser in that one file (no checkout build):
//! its line range matches fact positions (identity is position). A symbol the parser
//! does not list (fields and variables have no upstream node) falls back to the facts
//! in that file whose `subject_name` / `object_name` ends with the given name.
//! `--depth N` follows object -> subject chains of one kind. Facts with a tag in
//! the kind's `hidden_tags` are left out, with a count, unless `--include-tag`
//! names that tag. `--json` prints `{"facts": […], "hidden": N, "adapters": […]}`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::adapters_cmd::{args, print_status, root_arg};
use super::contract::{AdapterStatus, AliasBy, FactDecl, FactLookup, FactQuery, FactRecord, FactSide, FactSymbolLookup, Overlay, Registry, RegistryEntry};
use super::query::overlay_for_query;
use super::registry;

/// Entry point for `aft facts …`; returns the exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    let parsed = match args::parse(args, &["root", "label", "by", "depth", "include-tag"], &["prefix", "json"]) {
        Ok(parsed) => parsed,
        Err(problem) => return usage(&problem),
    };
    let json = parsed.has("json");
    let loaded = match registry::load() {
        Ok(loaded) => loaded,
        Err(problem) => return fail(json, 1, &problem),
    };
    match parsed.positional.as_slice() {
        [] => list_kinds(&loaded.registry, parsed.value("root"), json),
        [kind] if parsed.value("label").is_some() => {
            let lookup = Lookup::Label { key: parsed.value("label").unwrap_or_default().to_string(), prefix: parsed.has("prefix") };
            query(&loaded.registry, kind, lookup, None, &parsed)
        }
        [kind, file, symbol] if parsed.value("label").is_none() => {
            let side = match parsed.value("by").unwrap_or("either") {
                "subject" => FactSide::Subject,
                "object" => FactSide::Object,
                "either" => FactSide::Either,
                other => return usage(&format!("--by must be subject, object or either, not {other:?}")),
            };
            query(&loaded.registry, kind, Lookup::Symbol { file: file.clone(), symbol: symbol.clone(), side }, Some(file), &parsed)
        }
        _ => usage("use `aft facts <kind> --label <key> [--prefix]` or `aft facts <kind> <file> <symbol>`"),
    }
}

fn usage(problem: &str) -> i32 {
    eprintln!("aft facts: {problem}");
    eprintln!("usage: aft facts [--root <repo>] [--json]");
    eprintln!("       aft facts <kind> --label <key> [--prefix] [--root <repo>] [--include-tag <t>] [--json]");
    eprintln!("       aft facts <kind> <file> <symbol> [--by subject|object|either] [--depth N] [--include-tag <t>] [--json]");
    2
}

/// An error in the form the caller asked for. Returns `code`.
fn fail(json: bool, code: i32, problem: &str) -> i32 {
    if json {
        println!("{}", serde_json::json!({ "error": problem }));
    } else {
        eprintln!("aft facts: {problem}");
    }
    code
}

enum Lookup {
    Label { key: String, prefix: bool },
    Symbol { file: String, symbol: String, side: FactSide },
}

// ---------------------------------------------------------------------------
// aft facts  (kinds)
// ---------------------------------------------------------------------------

fn list_kinds(registry: &Registry, root: Option<&str>, json: bool) -> i32 {
    let entries: Vec<&RegistryEntry> = match root {
        Some(root) => match root_arg(Some(root)) {
            Ok(root) => registry::entries_for_root(registry, &root),
            Err(problem) => return fail(json, 1, &problem),
        },
        None => registry.adapters.iter().collect(),
    };
    let kinds: Vec<serde_json::Value> = entries
        .iter()
        .flat_map(|entry| {
            entry.facts.iter().map(|fact| {
                serde_json::json!({ "language": entry.language, "kind": fact.kind, "description": fact.description, "aliases": fact.aliases })
            })
        })
        .collect();
    if json {
        println!("{}", serde_json::json!({ "kinds": kinds }));
        return 0;
    }
    if kinds.is_empty() {
        println!("no fact kinds are declared{}", if root.is_some() { " for this root" } else { "" });
    }
    for entry in &entries {
        for fact in &entry.facts {
            println!("{} ({}): {}", fact.kind, entry.language, fact.description);
            for alias in &fact.aliases {
                println!("    alias {}: {}", alias.name, alias_usage(&fact.kind, alias.by));
            }
        }
    }
    0
}

fn alias_usage(kind: &str, by: AliasBy) -> String {
    match by {
        AliasBy::Label => format!("aft facts {kind} --label <key> [--prefix]"),
        AliasBy::Subject => format!("aft facts {kind} <file> <symbol> --by subject"),
        AliasBy::Object => format!("aft facts {kind} <file> <symbol> --by object"),
        AliasBy::Either => format!("aft facts {kind} <file> <symbol>"),
    }
}

// ---------------------------------------------------------------------------
// aft facts <kind> …
// ---------------------------------------------------------------------------

/// The nearest directory at or above `start` that holds `.git`.
fn git_root_above(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_dir() { Some(start) } else { start.parent() };
    while let Some(current) = dir {
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        dir = current.parent();
    }
    None
}

/// An absolute path for `file`: as given when absolute, else under `root` when that exists, else under the current directory.
fn file_path(file: &str, root: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(file);
    if path.is_absolute() {
        return path;
    }
    if let Some(root) = root {
        if root.join(&path).exists() {
            return root.join(path);
        }
    }
    std::env::current_dir().unwrap_or_default().join(path)
}

fn query(registry: &Registry, kind: &str, lookup: Lookup, file: Option<&String>, parsed: &args::Parsed) -> i32 {
    let json = parsed.has("json");
    let depth: u32 = match parsed.value("depth").map(str::parse) {
        None => 1,
        Some(Ok(depth)) if depth >= 1 => depth,
        Some(_) => return usage("--depth must be a whole number of 1 or more"),
    };
    let include: Vec<String> = parsed.all("include-tag").into_iter().map(str::to_string).collect();

    // The root: --root, else the git work tree of the file, else of the current directory.
    let file_abs = file.map(|file| file_path(file, parsed.value("root").and_then(|root| root_arg(Some(root)).ok()).as_deref()));
    let root = match parsed.value("root") {
        Some(root) => root_arg(Some(root)),
        None => {
            let start = file_abs.clone().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            git_root_above(&start).and_then(|root| root.canonicalize().ok()).ok_or_else(|| {
                "cannot find the repository: give --root <repo> (no .git above the file or the current directory)".to_string()
            })
        }
    };
    let root = match root {
        Ok(root) => root,
        Err(problem) => return fail(json, 2, &problem),
    };

    let at_root = registry::entries_for_root(registry, &root);
    let entries: Vec<&RegistryEntry> = at_root.iter().copied().filter(|entry| entry.fact(kind).is_some()).collect();
    if entries.is_empty() {
        let mut declared: Vec<&str> = at_root.iter().flat_map(|entry| entry.facts.iter().map(|fact| fact.kind.as_str())).collect();
        declared.dedup();
        let problem = if at_root.is_empty() {
            format!("no adapter applies to {} (no registry entry has a root marker there)", root.display())
        } else {
            format!("no adapter at {} declares the fact kind {kind:?} (declared: {})", root.display(), declared.join(", "))
        };
        return fail(json, 2, &problem);
    }

    // The positions of the symbol, from upstream's parser, once.
    let mut note: Option<String> = None;
    let symbol_lookups: Option<SymbolLookups> = match (&lookup, &file_abs) {
        (Lookup::Symbol { symbol, side, .. }, Some(file_abs)) => match symbol_lookups(&root, file_abs, symbol, *side) {
            Ok(found) => {
                if !found.found {
                    note = Some(format!("{symbol:?} is not in the parser's symbol list for that file; matched fact names in that file"));
                }
                Some(found)
            }
            Err(problem) => return fail(json, 2, &problem),
        },
        _ => None,
    };

    // One shared wait for all languages: the longest query wait of the entries.
    let started = Instant::now();
    let total_wait = entries.iter().map(|entry| entry.query_wait()).max().unwrap_or(Duration::ZERO);
    let mut found: Vec<(FactRecord, u32)> = Vec::new();
    // Hidden facts, counted by the tag that hides them (the first hidden tag of each fact).
    let mut hidden: BTreeMap<String, usize> = BTreeMap::new();
    let mut statuses: Vec<AdapterStatus> = Vec::new();
    for entry in &entries {
        let decl = entry.fact(kind).expect("entries were filtered by kind");
        let (overlay, status) = overlay_for_query(&root, &entry.language, total_wait.saturating_sub(started.elapsed()));
        statuses.push(status);
        let Some(overlay) = overlay else { continue };
        let lookups: Vec<FactLookup> = match (&lookup, &symbol_lookups) {
            (Lookup::Label { key, prefix }, _) => vec![FactLookup::Label { key: key.clone(), prefix: *prefix }],
            (_, Some(found)) => scoped_lookups(found, decl.lookup),
            (_, None) => Vec::new(),
        };
        let mut facts = first_hop(&overlay, kind, &lookups);
        if let (Lookup::Symbol { side, .. }, Some(SymbolLookups { name_filter: Some(wanted), .. })) = (&lookup, &symbol_lookups) {
            facts.retain(|fact| side_name_matches(fact, *side, wanted));
        }
        let mut hops: Vec<(FactRecord, u32)> = facts.drain(..).map(|fact| (fact, 0)).collect();
        follow_chains(&overlay, kind, depth, &mut hops);
        for (fact, hop) in hops {
            if let Some(tag) = hiding_tag(&fact, decl, &include) {
                *hidden.entry(tag.to_string()).or_default() += 1;
            } else if !found.iter().any(|(known, _)| *known == fact) {
                found.push((fact, hop));
            }
        }
    }
    print_answer(&root, kind, &found, &hidden, &statuses, note.as_deref(), depth > 1, json);
    0
}

/// The facts of `lookups` in `overlay`, each once, in overlay order per lookup.
fn first_hop(overlay: &Overlay, kind: &str, lookups: &[FactLookup]) -> Vec<FactRecord> {
    let mut facts: Vec<FactRecord> = Vec::new();
    for lookup in lookups {
        for fact in overlay.facts(&FactQuery { kind: kind.to_string(), lookup: lookup.clone() }) {
            if !facts.contains(&fact) {
                facts.push(fact);
            }
        }
    }
    facts
}

/// Adds the facts reached by following object -> subject: a fact whose subject is
/// at the exact position where an earlier fact's object is. At most `depth - 1` more hops.
fn follow_chains(overlay: &Overlay, kind: &str, depth: u32, facts: &mut Vec<(FactRecord, u32)>) {
    let mut frontier: Vec<FactRecord> = facts.iter().map(|(fact, _)| fact.clone()).collect();
    for hop in 1..depth {
        let mut next: Vec<FactRecord> = Vec::new();
        for fact in &frontier {
            let at = FactLookup::Position { side: FactSide::Subject, file: fact.object.file.clone(), first_line: fact.object.line, last_line: fact.object.line };
            for candidate in overlay.facts(&FactQuery { kind: kind.to_string(), lookup: at }) {
                let same_place = candidate.subject == fact.object;
                if same_place && !facts.iter().any(|(known, _)| *known == candidate) {
                    facts.push((candidate.clone(), hop));
                    next.push(candidate);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
}

/// The tag that hides the fact: one the kind hides and `--include-tag` does not name. None: shown.
fn hiding_tag<'a>(fact: &'a FactRecord, decl: &FactDecl, include: &[String]) -> Option<&'a str> {
    fact.tags.iter().find(|tag| decl.hidden_tags.contains(tag) && !include.contains(tag)).map(String::as_str)
}

/// A fact is hidden when it has a tag the kind hides and `--include-tag` does not name that tag.
fn is_hidden(fact: &FactRecord, decl: &FactDecl, include: &[String]) -> bool {
    hiding_tag(fact, decl, include).is_some()
}

/// How to find the facts of `symbol` in `file`.
#[derive(Debug)]
struct SymbolLookups {
    lookups: Vec<FactLookup>,
    /// The parser found the symbol in the file: the lookups are its line ranges (identity is position).
    found: bool,
    /// With `found`: for each lookup, the line of the declaration itself (the range can start at a doc
    /// comment or an attribute).
    declaration_lines: Vec<u32>,
    /// Without `found`: the lookup is the whole file, and only facts whose name on that side ends
    /// with this name (`::`-separated, for example `CallbackMetadata` or `OrderDetails::CallbackMetadata`)
    /// are kept. Names are labels the adapter chooses (`OrderDetails.CallbackMetadata`), so the match
    /// is on the last segments.
    name_filter: Option<String>,
}

/// The line ranges of `symbol` in `file` (upstream's parser). When the parser does not list the
/// symbol (for example a struct field, or a language it does not know), the fallback is every fact
/// whose side is in that file, filtered by name (see `name_filter`).
fn symbol_lookups(root: &Path, file_abs: &Path, symbol: &str, side: FactSide) -> Result<SymbolLookups, String> {
    let canonical = file_abs.canonicalize().map_err(|error| format!("cannot read {}: {error}", file_abs.display()))?;
    let relative = canonical
        .strip_prefix(root)
        .map_err(|_| format!("{} is not inside the root {}", canonical.display(), root.display()))?
        .to_string_lossy()
        .replace('\\', "/");
    // Upstream's parser on this one file. A language it does not know gives no ranges: the name fallback still works.
    let symbols = crate::parser::FileParser::new().extract_symbols(&canonical).unwrap_or_default();
    let source = std::fs::read_to_string(&canonical).unwrap_or_default();
    let wanted = symbol.replace('.', "::");
    let mut lookups: Vec<FactLookup> = Vec::new();
    let mut declaration_lines: Vec<u32> = Vec::new();
    let mut found = false;
    for candidate in &symbols {
        let scoped = if candidate.scope_chain.is_empty() {
            candidate.name.clone()
        } else {
            format!("{}::{}", candidate.scope_chain.join("::"), candidate.name)
        };
        if candidate.name == symbol || scoped == wanted {
            found = true;
            // Upstream ranges are 0-based; positions in facts are 1-based.
            let (first_line, last_line) = (candidate.range.start_line + 1, candidate.range.end_line + 1);
            lookups.push(FactLookup::Position { side, file: relative.clone(), first_line, last_line });
            declaration_lines.push(declaration_line(&source, first_line, last_line, candidate.signature.as_deref()));
        }
    }
    if found {
        return Ok(SymbolLookups { lookups, found, declaration_lines, name_filter: None });
    }
    let whole_file = FactLookup::Position { side, file: relative, first_line: 1, last_line: u32::MAX };
    Ok(SymbolLookups { lookups: vec![whole_file], found, declaration_lines: Vec::new(), name_filter: Some(wanted) })
}

/// The line of the declaration in `first..=last` (1-based): the line where the parser's signature starts,
/// else the first line that is neither blank nor a comment or attribute, else `first`.
fn declaration_line(source: &str, first: u32, last: u32, signature: Option<&str>) -> u32 {
    let squeeze = |text: &str| -> String { text.chars().filter(|c| !c.is_whitespace()).collect() };
    let is_comment = |text: &str| text.starts_with("//") || text.starts_with("/*") || text.starts_with('*') || text.starts_with('#');
    let head: Option<String> = signature
        .and_then(|signature| signature.lines().next())
        .map(|line| squeeze(line).chars().take(40).collect::<String>())
        .filter(|head| !head.is_empty());
    let lines: Vec<&str> = source.lines().collect();
    let line_at = |number: u32| -> Option<&str> { lines.get((number as usize).checked_sub(1)?).map(|text| text.trim()) };
    if let Some(head) = &head {
        for number in first..=last.min(lines.len() as u32) {
            let text = squeeze(line_at(number).unwrap_or(""));
            if !text.is_empty() && (text.starts_with(head.as_str()) || head.starts_with(&text)) {
                return number;
            }
        }
    }
    (first..=last.min(lines.len() as u32))
        .find(|number| line_at(*number).is_some_and(|text| !text.is_empty() && !is_comment(text)))
        .unwrap_or(first)
}

/// The parser's line ranges, narrowed to the declaration line unless the kind looks `inside`
/// (registry `lookup`). The whole-file fallback (symbol not found) is kept as it is.
fn scoped_lookups(found: &SymbolLookups, scope: FactSymbolLookup) -> Vec<FactLookup> {
    if !found.found || scope == FactSymbolLookup::Inside {
        return found.lookups.clone();
    }
    found
        .lookups
        .iter()
        .zip(&found.declaration_lines)
        .map(|(lookup, line)| match lookup {
            FactLookup::Position { side, file, .. } => FactLookup::Position { side: *side, file: file.clone(), first_line: *line, last_line: *line },
            other => other.clone(),
        })
        .collect()
}

/// True when the fact's name on `side` ends with `wanted` at a segment boundary
/// (`.` and `::` both separate segments).
fn side_name_matches(fact: &FactRecord, side: FactSide, wanted: &str) -> bool {
    let matches = |name: &Option<String>| -> bool {
        let Some(name) = name else { return false };
        let name = name.replace('.', "::");
        name == wanted || name.ends_with(&format!("::{wanted}"))
    };
    match side {
        FactSide::Subject => matches(&fact.subject_name),
        FactSide::Object => matches(&fact.object_name),
        FactSide::Either => matches(&fact.subject_name) || matches(&fact.object_name),
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn print_answer(root: &Path, kind: &str, facts: &[(FactRecord, u32)], hidden: &BTreeMap<String, usize>, statuses: &[AdapterStatus], note: Option<&str>, chains: bool, json: bool) {
    if json {
        let facts_json: Vec<serde_json::Value> = facts
            .iter()
            .map(|(fact, hop)| {
                let mut value = serde_json::to_value(fact).unwrap_or(serde_json::Value::Null);
                if chains {
                    value["hop"] = serde_json::json!(hop);
                }
                value
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "root": root.display().to_string(), "kind": kind, "facts": facts_json, "hidden": hidden.values().sum::<usize>(), "hidden_by_tag": hidden, "note": note, "adapters": statuses })
        );
        return;
    }
    for (fact, hop) in facts {
        println!("{}", fact_line(fact, *hop, chains));
    }
    if facts.is_empty() {
        println!("no {kind} facts found");
    }
    if !hidden.is_empty() {
        println!("{}", hidden_line(kind, hidden));
    }
    if let Some(note) = note {
        println!("note: {note}");
    }
    for status in statuses {
        print_status(status);
    }
}

/// One fact as a text line: `file:line:col name -> file:line:col name  label=…  via=…  tags=…  hop=…`.
fn fact_line(fact: &FactRecord, hop: u32, chains: bool) -> String {
    let name = |name: &Option<String>| name.as_ref().map(|name| format!(" {name}")).unwrap_or_default();
    let label = fact.label.as_ref().map(|label| format!("  label={label}")).unwrap_or_default();
    let via = fact.via.as_ref().map(|via| format!("  via={via}")).unwrap_or_default();
    let tags = if fact.tags.is_empty() { String::new() } else { format!("  tags={}", fact.tags.join(",")) };
    let hop = if chains { format!("  hop={hop}") } else { String::new() };
    format!(
        "{}:{}:{}{} -> {}:{}:{}{}{label}{via}{tags}{hop}",
        fact.subject.file,
        fact.subject.line,
        fact.subject.col,
        name(&fact.subject_name),
        fact.object.file,
        fact.object.line,
        fact.object.col,
        name(&fact.object_name)
    )
}

/// `5 writes fact(s) hidden: init 5. Show them with --include-tag init`.
fn hidden_line(kind: &str, hidden: &BTreeMap<String, usize>) -> String {
    let total: usize = hidden.values().sum();
    let counts: Vec<String> = hidden.iter().map(|(tag, count)| format!("{tag} {count}")).collect();
    let flags: Vec<String> = hidden.keys().map(|tag| format!("--include-tag {tag}")).collect();
    format!("{total} {kind} fact(s) hidden: {}. Show them with {}", counts.join(", "), flags.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(kind: &str, subject: (u32, u32), object: (u32, u32), tags: &[&str]) -> FactRecord {
        let pos = |(line, col): (u32, u32)| super::super::contract::Pos { file: "a.go".into(), line, col };
        FactRecord {
            kind: kind.into(),
            subject: pos(subject),
            subject_name: None,
            object: pos(object),
            object_name: None,
            label: None,
            via: None,
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
        }
    }

    #[test]
    fn a_fact_line_shows_label_and_via() {
        let mut registers = fact("registers", (10, 5), (20, 6), &[]);
        registers.subject_name = Some("Setup".into());
        registers.object_name = Some("HandleRefund".into());
        registers.label = Some("job:refund".into());
        registers.via = Some("(*asynq.ServeMux).HandleFunc".into());
        assert_eq!(
            fact_line(&registers, 0, false),
            "a.go:10:5 Setup -> a.go:20:6 HandleRefund  label=job:refund  via=(*asynq.ServeMux).HandleFunc"
        );
        let without = fact("registers", (1, 1), (2, 1), &[]);
        assert_eq!(fact_line(&without, 0, false), "a.go:1:1 -> a.go:2:1");
    }

    #[test]
    fn via_is_in_the_json_of_a_fact_only_when_it_is_set() {
        let mut registers = fact("registers", (1, 1), (2, 1), &[]);
        assert!(serde_json::to_value(&registers).unwrap().get("via").is_none());
        registers.via = Some("pb.RegisterOrderServiceServer".into());
        assert_eq!(serde_json::to_value(&registers).unwrap()["via"], "pb.RegisterOrderServiceServer");
        let parsed: FactRecord = serde_json::from_value(serde_json::json!({"kind":"registers","subject":{"file":"a.go","line":1,"col":1},"object":{"file":"a.go","line":2,"col":1},"via":"x.Y"})).unwrap();
        assert_eq!(parsed.via.as_deref(), Some("x.Y"));
    }

    fn decl(hidden: &[&str]) -> FactDecl {
        FactDecl { kind: "writes".into(), description: "d".into(), hidden_tags: hidden.iter().map(|tag| tag.to_string()).collect(), aliases: Vec::new(), lookup: FactSymbolLookup::Declaration }
    }

    #[test]
    fn the_hidden_line_names_each_tag_and_its_flag() {
        let hidden = BTreeMap::from([("init".to_string(), 5), ("mock".to_string(), 1)]);
        assert_eq!(hidden_line("writes", &hidden), "6 writes fact(s) hidden: init 5, mock 1. Show them with --include-tag init --include-tag mock");
    }

    #[test]
    fn a_hidden_tag_hides_a_fact_unless_it_is_included() {
        let init = fact("writes", (1, 1), (2, 1), &["init"]);
        let plain = fact("writes", (1, 1), (2, 1), &[]);
        assert!(is_hidden(&init, &decl(&["init"]), &[]));
        assert!(!is_hidden(&init, &decl(&["init"]), &["init".to_string()]));
        assert!(!is_hidden(&plain, &decl(&["init"]), &[]));
        assert!(!is_hidden(&init, &decl(&[]), &[]));
    }

    #[test]
    fn a_function_symbol_matches_its_declaration_line_unless_the_kind_looks_inside() {
        // a queue server file (point 2): startQueueServer registers job handlers
        // written as function literals in its body. dispatched_by on it must not list them.
        let range = vec![FactLookup::Position { side: FactSide::Object, file: "s.go".into(), first_line: 60, last_line: 270 }];
        let found = SymbolLookups { lookups: range.clone(), found: true, declaration_lines: vec![61], name_filter: None };
        let declaration = scoped_lookups(&found, FactSymbolLookup::Declaration);
        assert_eq!(declaration, [FactLookup::Position { side: FactSide::Object, file: "s.go".into(), first_line: 61, last_line: 61 }]);
        assert_eq!(scoped_lookups(&found, FactSymbolLookup::Inside), range, "flows: parameters and locals");
        let fallback = SymbolLookups { lookups: range.clone(), found: false, declaration_lines: Vec::new(), name_filter: Some("x".into()) };
        assert_eq!(scoped_lookups(&fallback, FactSymbolLookup::Declaration), range, "the whole-file fallback stays");
    }

    #[test]
    fn the_declaration_line_skips_doc_comments() {
        // a service file: the parser's range starts at the doc comment.
        let source = "package p\n\n// processOrderBatch is the handler of the topic.\n// It reads one message.\nfunc (s *service) processOrderBatch(ctx context.Context) (err error) {\n\treturn nil\n}\n";
        let signature = Some("func (s *service) processOrderBatch(ctx context.Context) (err error)");
        assert_eq!(declaration_line(source, 3, 7, signature), 5);
        assert_eq!(declaration_line(source, 3, 7, None), 5, "no signature: the first line that is not a comment");
        assert_eq!(declaration_line(source, 5, 7, signature), 5);
    }

    #[test]
    fn the_name_fallback_matches_the_last_name_segments() {
        // writers of a struct field (point 2, case A11): the adapter names it `OrderDetails.CallbackMetadata`.
        let mut write = fact("writes", (59, 29), (21, 2), &["init"]);
        write.object_name = Some("OrderDetails.CallbackMetadata".into());
        assert!(side_name_matches(&write, FactSide::Object, "CallbackMetadata"));
        assert!(side_name_matches(&write, FactSide::Object, "OrderDetails::CallbackMetadata"));
        assert!(!side_name_matches(&write, FactSide::Object, "Metadata"), "only whole segments match");
        assert!(!side_name_matches(&write, FactSide::Object, "Other::CallbackMetadata"));
        assert!(!side_name_matches(&write, FactSide::Subject, "CallbackMetadata"), "the subject has no name here");
    }

    #[test]
    fn symbols_are_found_by_name_or_scoped_name_in_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.go"), "package p\n\ntype I interface{ M() }\n\ntype T struct{}\n\nfunc (T) M() {}\n\nfunc F() {}\n").unwrap();
        // Found by the parser: position only (a name lookup would also match other files).
        let found = symbol_lookups(&root, &root.join("a.go"), "F", FactSide::Either).unwrap();
        assert!(found.found);
        assert_eq!(found.lookups, [FactLookup::Position { side: FactSide::Either, file: "a.go".into(), first_line: 9, last_line: 9 }]);
        // Not found: the whole file is the lookup, and the caller filters by name.
        let missing = symbol_lookups(&root, &root.join("a.go"), "Nope", FactSide::Object).unwrap();
        assert!(!missing.found);
        assert_eq!(missing.lookups, [FactLookup::Position { side: FactSide::Object, file: "a.go".into(), first_line: 1, last_line: u32::MAX }]);
        assert_eq!(missing.name_filter.as_deref(), Some("Nope"));
        let scoped = symbol_lookups(&root, &root.join("a.go"), "T::M", FactSide::Subject).unwrap();
        assert!(scoped.found, "scoped name T::M: {scoped:?}");
        let error = symbol_lookups(&root, Path::new("/etc/hosts"), "x", FactSide::Either).unwrap_err();
        assert!(error.contains("not inside the root"), "{error}");
    }

    #[test]
    fn the_kinds_list_names_every_alias_with_its_command() {
        assert_eq!(alias_usage("registers", AliasBy::Label), "aft facts registers --label <key> [--prefix]");
        assert_eq!(alias_usage("implements", AliasBy::Object), "aft facts implements <file> <symbol> --by object");
    }
}
