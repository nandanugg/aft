//! Contract v1: the shared types every task meets at (owner: C0).
//!
//! - Registry entry (`RegistryEntry`): how a language plugs in.
//! - Adapter output records (`Record`) and their validation (`parse_output`).
//! - Adapter status (`AdapterStatus`): what every graph reply says per language.
//! - The overlay read interface (`OverlayRead`, `Overlay`): what R1 serves and R2 reads.
//!
//! The rules are written for people in `docs/adapters/CONTRACT.md`; this file is
//! the same rules for the compiler. Change both together.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The only contract version this build reads.
pub(crate) const CONTRACT_VERSION: u32 = 1;

/// Defaults for optional registry fields.
pub(crate) const DEFAULT_TIMEOUT_S: u64 = 120;
pub(crate) const DEFAULT_QUERY_WAIT_S: u64 = 20;
pub(crate) const DEFAULT_FAILURE_COOLDOWN_S: u64 = 600;

/// System limits (not registry fields).
/// At most this many adapter runs at the same time on the machine (decision B).
pub(crate) const GLOBAL_RUN_LIMIT: usize = 2;
/// SIGTERM to the process group, then SIGKILL after this grace.
pub(crate) const KILL_GRACE: Duration = Duration::from_secs(5);
/// Limit for `git status --porcelain --untracked-files=no` per query.
pub(crate) const TREE_CHECK_LIMIT: Duration = Duration::from_millis(1000);
/// Limit for the registry's `toolchain_id_command`.
pub(crate) const TOOLCHAIN_ID_LIMIT: Duration = Duration::from_secs(5);
/// Lines of adapter stderr kept in a failure record.
pub(crate) const STDERR_TAIL_LINES: usize = 20;
/// Upstream's own cap: a named unresolved count at this value is a lower bound.
pub(crate) const UNRESOLVED_COUNT_CAP: usize = 1_000;

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// The registry file: read only from `$AFT_ADAPTERS_REGISTRY` or the install
/// location, never from a project root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registry {
    pub adapters: Vec<RegistryEntry>,
}

/// One language adapter. Required: `contract`, `language`, `root_markers`, `command`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryEntry {
    /// Contract version the adapter speaks. Per entry, so versions can coexist.
    pub contract: u32,
    /// Upstream's language label of the files (`lang_label`): `go`, `javascript`, …
    pub language: String,
    /// The adapter applies to a root that has one of these files at its top.
    pub root_markers: Vec<String>,
    /// argv; `{root}` is the only placeholder. No shell.
    pub command: Vec<String>,
    /// argv whose trimmed stdout names the toolchain (part of the overlay key).
    #[serde(default)]
    pub toolchain_id_command: Option<Vec<String>>,
    /// Variables added to the adapter's environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout_s: Option<u64>,
    #[serde(default)]
    pub query_wait_s: Option<u64>,
    #[serde(default)]
    pub failure_cooldown_s: Option<u64>,
    /// Target tags hidden in display unless asked (`--include-tag`). Counts still use them.
    #[serde(default)]
    pub hidden_tags: Vec<String>,
    /// Fact kinds this adapter may emit.
    #[serde(default)]
    pub facts: Vec<FactDecl>,
}

impl RegistryEntry {
    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S))
    }

    pub(crate) fn query_wait(&self) -> Duration {
        Duration::from_secs(self.query_wait_s.unwrap_or(DEFAULT_QUERY_WAIT_S))
    }

    pub(crate) fn failure_cooldown(&self) -> Duration {
        Duration::from_secs(self.failure_cooldown_s.unwrap_or(DEFAULT_FAILURE_COOLDOWN_S))
    }

    /// The declaration of fact kind `kind`, if this entry declares it.
    pub(crate) fn fact(&self, kind: &str) -> Option<&FactDecl> {
        self.facts.iter().find(|decl| decl.kind == kind)
    }
}

/// A fact kind an adapter may emit, with its command aliases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FactDecl {
    pub kind: String,
    /// One line, shown by `aft facts` and `aft guide`.
    pub description: String,
    /// Fact tags hidden unless asked (`--include-tag`).
    #[serde(default)]
    pub hidden_tags: Vec<String>,
    /// Old command names: `aft <alias> …` = `aft facts <kind> …` with a lookup mode.
    #[serde(default)]
    pub aliases: Vec<FactAlias>,
    /// Which positions `<file> <symbol>` matches (CONTRACT.md 2.3). Default: the declaration line.
    #[serde(default)]
    pub lookup: FactSymbolLookup,
}

/// How `aft facts <kind> <file> <symbol>` matches a symbol found by the parser.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FactSymbolLookup {
    /// The symbol itself: a fact side on the symbol's first line (its declaration). A function
    /// literal inside the body (a job handler it registers) is not the symbol.
    #[default]
    Declaration,
    /// Values inside the symbol: a fact side anywhere in its line range (`flows`: parameters and locals).
    Inside,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FactAlias {
    pub name: String,
    pub by: AliasBy,
}

/// How an alias looks facts up. `label` takes `<key> [--prefix]`; the others
/// take `<file> <symbol>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AliasBy {
    Label,
    Subject,
    Object,
    Either,
}

// ---------------------------------------------------------------------------
// Adapter output records (one JSON object per line on stdout)
// ---------------------------------------------------------------------------

/// A position: root-relative file (`/` separators), 1-based line, 1-based column
/// in UTF-8 bytes. A fact `object` outside the root has an absolute `file`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Pos {
    pub file: String,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Record {
    Header(HeaderRecord),
    Analysed(AnalysedRecord),
    Site(SiteRecord),
    Fact(FactRecord),
    Unanalysed(UnanalysedRecord),
    End(EndRecord),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HeaderRecord {
    pub contract: u32,
    pub language: String,
    pub tool: String,
    #[serde(default)]
    pub tool_version: String,
}

/// Files whose calls the adapter analysed and whose functions it considered as
/// targets. May appear more than once; the files are united.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AnalysedRecord {
    pub files: Vec<String>,
}

/// One analysed call. Every target among the analysed files is in `targets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SiteRecord {
    /// Caller file (root-relative).
    pub file: String,
    /// A position inside the call and outside every nested call (Go: the open parenthesis).
    pub line: u32,
    pub col: u32,
    /// The called name as written at the site (label only).
    pub name: String,
    /// `in_loop`, `in_error_branch`, `in_defer`, `in_goroutine`, …
    #[serde(default)]
    pub tags: Vec<String>,
    /// In-root targets; empty when every callee is outside the root.
    pub targets: Vec<TargetRecord>,
    /// Optional labels of callees outside the root (not shown in v1).
    #[serde(default)]
    pub external: Vec<ExternalLabel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TargetRecord {
    /// Position of the callee's declared name (root-relative file).
    pub file: String,
    pub line: u32,
    pub col: u32,
    /// Declared bare name (label only).
    pub name: String,
    /// Free label, shown as `adapter:<language> (<kind>)`: `static`, `concrete`, `interface`, …
    pub kind: String,
    /// For example `mock`.
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExternalLabel {
    pub name: String,
    pub package: String,
    #[serde(default)]
    pub module: Option<String>,
}

/// A fact. It does not feed the call graph; `aft facts` answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FactRecord {
    pub kind: String,
    pub subject: Pos,
    #[serde(default)]
    pub subject_name: Option<String>,
    pub object: Pos,
    #[serde(default)]
    pub object_name: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// The API through which a `registers` fact was registered, for example `(*asynq.ServeMux).HandleFunc`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Files the adapter could not analyse because of an error. Makes the run partial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UnanalysedRecord {
    pub files: Vec<String>,
    pub reason: String,
    /// The package or module that failed, when the language has one.
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EndRecord {
    pub sites: u64,
    pub targets: u64,
    pub facts: u64,
    pub analysed_files: u64,
    pub unanalysed_files: u64,
}

/// A validated adapter run.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AdapterOutput {
    pub header: HeaderRecord,
    pub analysed: BTreeSet<String>,
    pub sites: Vec<SiteRecord>,
    pub facts: Vec<FactRecord>,
    pub unanalysed: Vec<UnanalysedRecord>,
    pub end: EndRecord,
}

impl AdapterOutput {
    pub(crate) fn is_partial(&self) -> bool {
        !self.unanalysed.is_empty()
    }
}

/// Why an adapter's stdout broke the contract. The runner turns it into a
/// `failed` status: "contract violation at line N: …".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContractViolation {
    /// 1-based line of stdout; 0 when the problem is about the whole output.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ContractViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "contract violation: {}", self.message)
        } else {
            write!(f, "contract violation at line {}: {}", self.line, self.message)
        }
    }
}

fn violation(line: usize, message: impl Into<String>) -> ContractViolation {
    ContractViolation {
        line,
        message: message.into(),
    }
}

/// A root-relative path: not empty, not absolute, no `..`, no `\`.
pub(crate) fn is_root_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.split('/').any(|part| part == ".." || part.is_empty())
}

fn check_pos(line: usize, what: &str, file: &str, l: u32, c: u32, may_be_absolute: bool) -> Result<(), ContractViolation> {
    let file_ok = is_root_relative(file) || (may_be_absolute && file.starts_with('/'));
    if !file_ok {
        return Err(violation(line, format!("{what} file {file:?} is not a root-relative path")));
    }
    if l == 0 || c == 0 {
        return Err(violation(line, format!("{what} position {file}:{l}:{c} is not 1-based")));
    }
    Ok(())
}

/// Validates an adapter's whole stdout against `entry`. Every rule of
/// `docs/adapters/CONTRACT.md` "Output" and "Failure" that can be checked
/// without the repository files is checked here.
pub(crate) fn parse_output(stdout: &[u8], entry: &RegistryEntry) -> Result<AdapterOutput, ContractViolation> {
    let text = std::str::from_utf8(stdout).map_err(|error| violation(0, format!("stdout is not UTF-8: {error}")))?;
    if text.is_empty() {
        return Err(violation(0, "no output (no header and no end record)"));
    }
    if !text.ends_with('\n') {
        let last = text.lines().count();
        return Err(violation(last, "the last line is incomplete (no newline at the end)"));
    }
    let mut header: Option<HeaderRecord> = None;
    let mut analysed = BTreeSet::new();
    let mut sites = Vec::new();
    let mut facts = Vec::new();
    let mut unanalysed = Vec::new();
    let mut end: Option<EndRecord> = None;
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        if raw.trim().is_empty() {
            return Err(violation(number, "empty line"));
        }
        if end.is_some() {
            return Err(violation(number, "a record after the end record"));
        }
        let record: Record = serde_json::from_str(raw).map_err(|error| violation(number, format!("not a contract record: {error}")))?;
        if header.is_none() && !matches!(record, Record::Header(_)) {
            return Err(violation(number, "the first record must be the header"));
        }
        match record {
            Record::Header(record) => {
                if header.is_some() {
                    return Err(violation(number, "a second header"));
                }
                if record.contract != CONTRACT_VERSION || record.contract != entry.contract {
                    return Err(violation(
                        number,
                        format!(
                            "header contract {} does not match the registry entry ({}) and this build ({CONTRACT_VERSION})",
                            record.contract, entry.contract
                        ),
                    ));
                }
                if record.language != entry.language {
                    return Err(violation(
                        number,
                        format!("header language {:?} is not the registry language {:?}", record.language, entry.language),
                    ));
                }
                header = Some(record);
            }
            Record::Analysed(record) => {
                for file in record.files {
                    if !is_root_relative(&file) {
                        return Err(violation(number, format!("analysed file {file:?} is not a root-relative path")));
                    }
                    analysed.insert(file);
                }
            }
            Record::Site(record) => {
                check_pos(number, "site", &record.file, record.line, record.col, false)?;
                for target in &record.targets {
                    check_pos(number, "target", &target.file, target.line, target.col, false)?;
                }
                sites.push(record);
            }
            Record::Fact(record) => {
                if entry.fact(&record.kind).is_none() {
                    return Err(violation(number, format!("fact kind {:?} is not declared in the registry entry", record.kind)));
                }
                check_pos(number, "fact subject", &record.subject.file, record.subject.line, record.subject.col, false)?;
                check_pos(number, "fact object", &record.object.file, record.object.line, record.object.col, true)?;
                facts.push(record);
            }
            Record::Unanalysed(record) => {
                for file in &record.files {
                    if !is_root_relative(file) {
                        return Err(violation(number, format!("unanalysed file {file:?} is not a root-relative path")));
                    }
                }
                unanalysed.push(record);
            }
            Record::End(record) => end = Some(record),
        }
    }
    let Some(header) = header else {
        return Err(violation(0, "no header record"));
    };
    let Some(end) = end else {
        return Err(violation(0, "no end record (the adapter stopped early or crashed)"));
    };
    let counted = EndRecord {
        sites: sites.len() as u64,
        targets: sites.iter().map(|site| site.targets.len() as u64).sum(),
        facts: facts.len() as u64,
        analysed_files: analysed.len() as u64,
        unanalysed_files: unanalysed.iter().map(|record| record.files.len() as u64).sum(),
    };
    if counted != end {
        return Err(violation(0, format!("end record counts {end:?} do not match the records read {counted:?}")));
    }
    Ok(AdapterOutput {
        header,
        analysed,
        sites,
        facts,
        unanalysed,
        end,
    })
}

/// Byte offset of a 1-based (line, column-in-bytes) position in `text`, when the
/// column points at a byte of that line (not past its end).
pub(crate) fn line_col_to_offset(text: &str, line: u32, col: u32) -> Option<u64> {
    if line == 0 || col == 0 {
        return None;
    }
    let mut start = 0usize;
    for (index, content) in text.split('\n').enumerate() {
        if index + 1 == line as usize {
            let column = col as usize - 1;
            return (column < content.len()).then(|| (start + column) as u64);
        }
        start += content.len() + 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Adapter status (in every graph reply, per language)
// ---------------------------------------------------------------------------

/// What the adapter of one language contributed to a reply, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AdapterStatus {
    pub language: String,
    #[serde(flatten)]
    pub state: AdapterState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum AdapterState {
    /// The overlay for the current key is served.
    Fresh { sites: u64, facts: u64 },
    /// Served, but some files were not analysed (they keep upstream answers only).
    /// Gets the same cooldown + retry as a failure (decision H).
    Partial { sites: u64, facts: u64, unanalysed: Vec<String> },
    /// A run is going on; the query waited `waited_ms` and answers without adapter edges.
    Building { waited_ms: u64 },
    /// The stored overlay is for another commit; a run was started or is cooling down.
    Stale { overlay_head: String, tree_head: String },
    /// The last run for this key failed; no retry before `retry_after_unix_s` unless `--force`.
    Failed { reason: String, at_unix_s: u64, retry_after_unix_s: u64, stderr_tail: Vec<String> },
    /// No adapter data for this root: no commit, dirty tree, no registry entry,
    /// no root marker, tree check timed out, …
    Skipped { reason: String },
}

impl AdapterStatus {
    pub(crate) fn new(language: impl Into<String>, state: AdapterState) -> Self {
        Self {
            language: language.into(),
            state,
        }
    }

    pub(crate) fn skipped(language: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(language, AdapterState::Skipped { reason: reason.into() })
    }

    /// Fresh or partial: adapter edges are served and named counts may be lowered
    /// (under the coverage rule).
    pub(crate) fn serves_edges(&self) -> bool {
        matches!(self.state, AdapterState::Fresh { .. } | AdapterState::Partial { .. })
    }

    /// One line for `adapter_reason` and text output, for example
    /// `go: stale (overlay for 1a2b3c4, tree at 5d6e7f8)`.
    pub(crate) fn reason_line(&self) -> String {
        let detail = match &self.state {
            AdapterState::Fresh { sites, facts } => format!("fresh ({sites} sites, {facts} facts)"),
            AdapterState::Partial { sites, facts, unanalysed } => {
                format!("partial ({sites} sites, {facts} facts; not analysed: {})", unanalysed.join(", "))
            }
            AdapterState::Building { waited_ms } => format!("building (waited {waited_ms} ms; answer has no adapter edges)"),
            AdapterState::Stale { overlay_head, tree_head } => {
                format!("stale (overlay for {overlay_head}, tree at {tree_head})")
            }
            AdapterState::Failed { reason, .. } => format!("failed ({reason})"),
            AdapterState::Skipped { reason } => format!("skipped ({reason})"),
        };
        format!("{}: {detail}", self.language)
    }
}

// ---------------------------------------------------------------------------
// Overlay read interface (R1 serves it, R2 reads it)
// ---------------------------------------------------------------------------

/// A site with its caller byte offset (computed once from the file text at the
/// overlay's commit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OverlaySite {
    pub offset: u64,
    pub site: SiteRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub(crate) struct OverlayCounts {
    pub sites: u64,
    pub targets: u64,
    pub facts: u64,
    pub analysed_files: u64,
}

/// Which end of a fact a lookup matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FactSide {
    Subject,
    Object,
    Either,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FactLookup {
    /// `--label <key> [--prefix]`
    Label { key: String, prefix: bool },
    /// A symbol's lines in a file (from upstream's outline of that file).
    Position { side: FactSide, file: String, first_line: u32, last_line: u32 },
    /// `subject_name` / `object_name`, for fields and variables (no upstream node).
    Name { side: FactSide, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FactQuery {
    pub kind: String,
    pub lookup: FactLookup,
}

/// One validated overlay for (root, language, key). Read-only.
pub(crate) trait OverlayRead: Send + Sync {
    fn language(&self) -> &str;
    /// The adapter analysed `file`: its calls are covered and its functions were target candidates.
    fn is_analysed(&self, file: &str) -> bool;
    /// The registry's `hidden_tags` for targets (display only; counts use every target).
    fn hidden_tags(&self) -> &[String];
    /// Sites whose call is in `file`, by offset.
    fn sites_in_file(&self, file: &str) -> Vec<OverlaySite>;
    /// Sites with a target declared in `file` on a line in `first_line..=last_line`,
    /// each with the index of that target in `site.targets`.
    fn sites_targeting(&self, file: &str, first_line: u32, last_line: u32) -> Vec<(OverlaySite, usize)>;
    /// Facts of `query.kind` that match `query.lookup`. Tags are not filtered here.
    fn facts(&self, query: &FactQuery) -> Vec<FactRecord>;
    fn counts(&self) -> OverlayCounts;
}

/// The overlay handle `overlay_for_query` returns.
pub(crate) type Overlay = Arc<dyn OverlayRead>;

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> RegistryEntry {
        serde_json::from_value(serde_json::json!({
            "contract": 1,
            "language": "go",
            "root_markers": ["go.mod"],
            "command": ["adapter", "{root}"],
            "facts": [{"kind": "implements", "description": "type -> interface"}]
        }))
        .unwrap()
    }

    const OK: &str = concat!(
        r#"{"type":"header","contract":1,"language":"go","tool":"fake","tool_version":"0"}"#, "\n",
        r#"{"type":"analysed","files":["a.go","b.go"]}"#, "\n",
        r#"{"type":"site","file":"a.go","line":3,"col":7,"name":"M","tags":["in_loop"],"targets":[{"file":"b.go","line":5,"col":14,"name":"M","kind":"interface","tags":["mock"]}]}"#, "\n",
        r#"{"type":"site","file":"a.go","line":4,"col":9,"name":"Now","targets":[],"external":[{"name":"Now","package":"time"}]}"#, "\n",
        r#"{"type":"fact","kind":"implements","subject":{"file":"b.go","line":2,"col":6},"object":{"file":"/abs/dep.go","line":1,"col":6},"object_name":"I"}"#, "\n",
        r#"{"type":"end","sites":2,"targets":1,"facts":1,"analysed_files":2,"unanalysed_files":0}"#, "\n",
    );

    #[test]
    fn a_valid_output_parses_with_all_record_kinds() {
        let output = parse_output(OK.as_bytes(), &entry()).unwrap();
        assert_eq!(output.sites.len(), 2);
        assert_eq!(output.sites[0].targets[0].tags, vec!["mock"]);
        assert_eq!(output.facts[0].object_name.as_deref(), Some("I"));
        assert!(!output.is_partial());
    }

    fn fails(text: &str) -> String {
        parse_output(text.as_bytes(), &entry()).unwrap_err().to_string()
    }

    #[test]
    fn every_failure_rule_names_its_reason() {
        let without_end = OK.lines().take(5).map(|l| format!("{l}\n")).collect::<String>();
        assert!(fails(&without_end).contains("no end record"));
        assert!(fails(OK.trim_end()).contains("incomplete"));
        assert!(fails(&OK.replace(r#""contract":1,"language""#, r#""contract":2,"language""#)).contains("header contract 2"));
        assert!(fails(&OK.replace(r#""kind":"implements""#, r#""kind":"writes""#)).contains("not declared"));
        assert!(fails(&OK.replace(r#""sites":2"#, r#""sites":3"#)).contains("do not match"));
        assert!(fails(&OK.replace("analysed", "analyzed")).contains("not a contract record"));
        assert!(fails(&OK.replace(r#""file":"a.go","line":3"#, r#""file":"../a.go","line":3"#)).contains("root-relative"));
        assert!(fails("").contains("no output"));
    }

    #[test]
    fn positions_convert_to_byte_offsets() {
        let text = "package p\n\nfunc f() { g() }\n";
        assert_eq!(line_col_to_offset(text, 3, 13), Some(23));
        assert_eq!(&text[23..24], "(");
        assert_eq!(line_col_to_offset(text, 3, 99), None);
        assert_eq!(line_col_to_offset(text, 9, 1), None);
    }

    #[test]
    fn the_registry_example_parses_with_every_alias_mode() {
        // Read at run time: upstream's build_path_independence test forbids the compile-time macro.
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR for tests");
        let text = std::fs::read_to_string(std::path::Path::new(&manifest).join("../../docs/adapters/registry.example.json")).unwrap();
        let registry: Registry = serde_json::from_str(&text).unwrap();
        let go = registry.adapters.iter().find(|entry| entry.language == "go").unwrap();
        assert_eq!(go.env.get("GONOPROXY").map(String::as_str), Some("none"));
        assert_eq!(go.timeout(), Duration::from_secs(120));
        let registers = go.fact("registers").unwrap();
        assert_eq!(registers.aliases[0], FactAlias { name: "dispatches".into(), by: AliasBy::Label });
        assert_eq!(registers.aliases[1].by, AliasBy::Object);
        assert!(registry.adapters.iter().any(|entry| entry.language == "javascript" && entry.facts.is_empty()));
    }

    #[test]
    fn status_serializes_with_language_and_state() {
        let status = AdapterStatus::new("go", AdapterState::Stale { overlay_head: "a".into(), tree_head: "b".into() });
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["language"], "go");
        assert_eq!(json["status"], "stale");
        assert!(!status.serves_edges());
        assert_eq!(status.reason_line(), "go: stale (overlay for a, tree at b)");
    }
}
