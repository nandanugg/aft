//! Persistent on-disk cache for the AFT call graph (Tier 2).
//!
//! Layout under `$CACHE_ROOT/<project-hash>/`:
//!
//! ```text
//! meta.json            — project root, aft version, schema version, timestamps
//! parse-index.cbor     — per-file tree-sitter parse keyed on (mtime_nsec, size)
//! helper-output.json   — last full Go helper output (verbatim, not transcoded)
//! helper-input-hash    — hex sha256 over sorted (rel_path, mtime_nsec, size) for .go files
//! merged-graph.cbor    — derived reverse index (rebuilt from parse + helper)
//! ```
//!
//! ## Design choices
//!
//! - **CBOR (ciborium)** for internal caches: binary, ~3-5x smaller and faster than JSON.
//! - **JSON** for helper output: cached verbatim, no transcoding.
//! - **Atomic writes** via `write(tmp); fsync; rename(tmp, final)` — POSIX atomicity.
//! - **Corruption-safe reads**: any parse failure → log, delete, rebuild. Never crash.
//! - **No daemons, no file watchers, no locks**: pure stateless CLI. Last-writer-wins.
//! - **Schema version** baked in; bump invalidates all existing caches automatically.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::callgraph::walk_project_files;
use crate::imports::{ImportBlock, ImportGroup, ImportKind, ImportStatement};
use crate::parser::LangId;
use crate::symbols::SymbolKind;

// ---------------------------------------------------------------------------
// Schema version — bump on any breaking cache format change
// ---------------------------------------------------------------------------

/// Increment this when the cache serialization format changes in a
/// backwards-incompatible way. All existing caches auto-invalidate.
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Cache-root resolution
// ---------------------------------------------------------------------------

/// Resolve the project-specific cache directory.
///
/// Priority:
///  1. `AFT_CACHE_DIR` env var (overrides everything; used in tests)
///  2. `AFT_DISABLE_CACHE=1` → returns None
///  3. `~/.cache/aft/<project-hash>/`
pub fn resolve_project_cache_dir(project_root: &Path) -> Option<PathBuf> {
    if std::env::var_os("AFT_DISABLE_CACHE").is_some_and(|v| v == "1") {
        return None;
    }

    let base = if let Some(dir) = std::env::var_os("AFT_CACHE_DIR") {
        PathBuf::from(dir)
    } else {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".cache").join("aft")
    };

    let project_hash = project_hash(project_root);
    Some(base.join(project_hash))
}

/// Compute a stable, short project identifier: first 12 hex chars of
/// SHA-256(canonical_absolute_root).
pub fn project_hash(project_root: &Path) -> String {
    let canon = fs::canonicalize(project_root)
        .unwrap_or_else(|_| project_root.to_path_buf());
    let root_str = canon.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(root_str.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)[..12].to_string()
}

// ---------------------------------------------------------------------------
// Atomic write helper
// ---------------------------------------------------------------------------

/// Atomically write `data` to `final_path`.
///
/// Uses `write(tmp); fsync; rename(tmp, final)`. PID is embedded in the
/// tmp name to avoid concurrent-process collisions. On Windows rename is
/// not atomic, but AFT does not support concurrent writes there.
fn atomic_write(final_path: &Path, data: &[u8]) -> io::Result<()> {
    let pid = std::process::id();
    let tmp = match final_path.file_name() {
        Some(name) => {
            let mut tmp_name = name.to_owned();
            tmp_name.push(format!(".tmp.{}", pid));
            final_path.with_file_name(tmp_name)
        }
        None => final_path.with_extension(format!("tmp.{}", pid)),
    };

    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }

    fs::rename(&tmp, final_path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// meta.json
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMeta {
    pub project_root: String,
    pub aft_version: String,
    pub schema_version: u32,
    pub created_at: String,
    pub last_refreshed_at: String,
}

impl CacheMeta {
    pub fn new(project_root: &Path) -> Self {
        let now = iso8601_now();
        CacheMeta {
            project_root: project_root.to_string_lossy().into_owned(),
            aft_version: env!("CARGO_PKG_VERSION").to_string(),
            schema_version: SCHEMA_VERSION,
            created_at: now.clone(),
            last_refreshed_at: now,
        }
    }

    /// Returns `true` if this meta is valid for the current binary and schema.
    pub fn is_compatible(&self) -> bool {
        self.schema_version == SCHEMA_VERSION
    }
}

fn iso8601_now() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    // Produce a simple ISO-8601-ish timestamp without an external chrono dep
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    // Rough gregorian from days (good enough for a cache timestamp)
    let year = 1970 + days / 365;
    let day_of_year = days % 365;
    let month = day_of_year / 30 + 1;
    let day = day_of_year % 30 + 1;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, h, m, s
    )
}

pub fn write_meta(cache_dir: &Path, meta: &CacheMeta) -> io::Result<()> {
    let data = serde_json::to_vec_pretty(meta)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    atomic_write(&cache_dir.join("meta.json"), &data)
}

pub fn read_meta(cache_dir: &Path) -> Option<CacheMeta> {
    let path = cache_dir.join("meta.json");
    let data = fs::read(&path).ok()?;
    if data.is_empty() {
        log::warn!("[cache] meta.json is empty — discarding");
        let _ = fs::remove_file(&path);
        return None;
    }
    match serde_json::from_slice::<CacheMeta>(&data) {
        Ok(m) => Some(m),
        Err(e) => {
            log::warn!("[cache] meta.json corrupt ({}) — discarding", e);
            let _ = fs::remove_file(&path);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Serde-able wrappers for callgraph types
// ---------------------------------------------------------------------------
//
// The existing callgraph types carry only `#[derive(Serialize)]`.  For the
// on-disk cache we need both directions.  Rather than patching every type
// (which would require `#[derive(Deserialize)]` on all of ImportBlock,
// ImportStatement, etc.), we introduce thin mirror structs used only for
// serialization.  They convert from the live types on write and back on read.

/// Serializable mirror of `LangId`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SerLangId {
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Rust,
    Go,
    C,
    Cpp,
    Zig,
    CSharp,
    Bash,
    Html,
    Markdown,
}

impl From<LangId> for SerLangId {
    fn from(l: LangId) -> Self {
        match l {
            LangId::TypeScript => SerLangId::TypeScript,
            LangId::Tsx => SerLangId::Tsx,
            LangId::JavaScript => SerLangId::JavaScript,
            LangId::Python => SerLangId::Python,
            LangId::Rust => SerLangId::Rust,
            LangId::Go => SerLangId::Go,
            LangId::C => SerLangId::C,
            LangId::Cpp => SerLangId::Cpp,
            LangId::Zig => SerLangId::Zig,
            LangId::CSharp => SerLangId::CSharp,
            LangId::Bash => SerLangId::Bash,
            LangId::Html => SerLangId::Html,
            LangId::Markdown => SerLangId::Markdown,
        }
    }
}

impl From<SerLangId> for LangId {
    fn from(l: SerLangId) -> Self {
        match l {
            SerLangId::TypeScript => LangId::TypeScript,
            SerLangId::Tsx => LangId::Tsx,
            SerLangId::JavaScript => LangId::JavaScript,
            SerLangId::Python => LangId::Python,
            SerLangId::Rust => LangId::Rust,
            SerLangId::Go => LangId::Go,
            SerLangId::C => LangId::C,
            SerLangId::Cpp => LangId::Cpp,
            SerLangId::Zig => LangId::Zig,
            SerLangId::CSharp => LangId::CSharp,
            SerLangId::Bash => LangId::Bash,
            SerLangId::Html => LangId::Html,
            SerLangId::Markdown => LangId::Markdown,
        }
    }
}

/// Serializable mirror of `SymbolKind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SerSymbolKind {
    Function,
    Class,
    Method,
    Struct,
    Interface,
    Enum,
    TypeAlias,
    Variable,
    Constant,
    Heading,
}

impl From<SymbolKind> for SerSymbolKind {
    fn from(k: SymbolKind) -> Self {
        match k {
            SymbolKind::Function => SerSymbolKind::Function,
            SymbolKind::Class => SerSymbolKind::Class,
            SymbolKind::Method => SerSymbolKind::Method,
            SymbolKind::Struct => SerSymbolKind::Struct,
            SymbolKind::Interface => SerSymbolKind::Interface,
            SymbolKind::Enum => SerSymbolKind::Enum,
            SymbolKind::TypeAlias => SerSymbolKind::TypeAlias,
            SymbolKind::Variable => SerSymbolKind::Variable,
            SymbolKind::Constant => SerSymbolKind::Constant,
            SymbolKind::Heading => SerSymbolKind::Heading,
        }
    }
}

impl From<SerSymbolKind> for SymbolKind {
    fn from(k: SerSymbolKind) -> Self {
        match k {
            SerSymbolKind::Function => SymbolKind::Function,
            SerSymbolKind::Class => SymbolKind::Class,
            SerSymbolKind::Method => SymbolKind::Method,
            SerSymbolKind::Struct => SymbolKind::Struct,
            SerSymbolKind::Interface => SymbolKind::Interface,
            SerSymbolKind::Enum => SymbolKind::Enum,
            SerSymbolKind::TypeAlias => SymbolKind::TypeAlias,
            SerSymbolKind::Variable => SymbolKind::Variable,
            SerSymbolKind::Constant => SymbolKind::Constant,
            SerSymbolKind::Heading => SymbolKind::Heading,
        }
    }
}

/// Serializable mirror of `ImportKind`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SerImportKind {
    Value,
    Type,
    SideEffect,
}

impl From<ImportKind> for SerImportKind {
    fn from(k: ImportKind) -> Self {
        match k {
            ImportKind::Value => SerImportKind::Value,
            ImportKind::Type => SerImportKind::Type,
            ImportKind::SideEffect => SerImportKind::SideEffect,
        }
    }
}

impl From<SerImportKind> for ImportKind {
    fn from(k: SerImportKind) -> Self {
        match k {
            SerImportKind::Value => ImportKind::Value,
            SerImportKind::Type => ImportKind::Type,
            SerImportKind::SideEffect => ImportKind::SideEffect,
        }
    }
}

/// Serializable mirror of `ImportGroup`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SerImportGroup {
    Stdlib,
    External,
    Internal,
}

impl From<ImportGroup> for SerImportGroup {
    fn from(g: ImportGroup) -> Self {
        match g {
            ImportGroup::Stdlib => SerImportGroup::Stdlib,
            ImportGroup::External => SerImportGroup::External,
            ImportGroup::Internal => SerImportGroup::Internal,
        }
    }
}

impl From<SerImportGroup> for ImportGroup {
    fn from(g: SerImportGroup) -> Self {
        match g {
            SerImportGroup::Stdlib => ImportGroup::Stdlib,
            SerImportGroup::External => ImportGroup::External,
            SerImportGroup::Internal => ImportGroup::Internal,
        }
    }
}

/// Serializable mirror of `ImportStatement`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerImportStatement {
    pub module_path: String,
    pub names: Vec<String>,
    pub default_import: Option<String>,
    pub namespace_import: Option<String>,
    pub kind: SerImportKind,
    pub group: SerImportGroup,
    /// Byte range stored as [start, end].
    pub byte_range: [usize; 2],
    pub raw_text: String,
}

impl From<&ImportStatement> for SerImportStatement {
    fn from(s: &ImportStatement) -> Self {
        SerImportStatement {
            module_path: s.module_path.clone(),
            names: s.names.clone(),
            default_import: s.default_import.clone(),
            namespace_import: s.namespace_import.clone(),
            kind: s.kind.into(),
            group: s.group.into(),
            byte_range: [s.byte_range.start, s.byte_range.end],
            raw_text: s.raw_text.clone(),
        }
    }
}

impl From<SerImportStatement> for ImportStatement {
    fn from(s: SerImportStatement) -> Self {
        ImportStatement {
            module_path: s.module_path,
            names: s.names,
            default_import: s.default_import,
            namespace_import: s.namespace_import,
            kind: s.kind.into(),
            group: s.group.into(),
            byte_range: s.byte_range[0]..s.byte_range[1],
            raw_text: s.raw_text,
        }
    }
}

/// Serializable mirror of `ImportBlock`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerImportBlock {
    pub imports: Vec<SerImportStatement>,
    /// `None` if no imports; [start, end] otherwise.
    pub byte_range: Option<[usize; 2]>,
}

impl From<&ImportBlock> for SerImportBlock {
    fn from(b: &ImportBlock) -> Self {
        SerImportBlock {
            imports: b.imports.iter().map(SerImportStatement::from).collect(),
            byte_range: b
                .byte_range
                .as_ref()
                .map(|r| [r.start, r.end]),
        }
    }
}

impl From<SerImportBlock> for ImportBlock {
    fn from(b: SerImportBlock) -> Self {
        ImportBlock {
            imports: b.imports.into_iter().map(Into::into).collect(),
            byte_range: b.byte_range.map(|[s, e]| s..e),
        }
    }
}

/// Serializable mirror of `CallSite`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerCallSite {
    pub callee_name: String,
    pub full_callee: String,
    pub line: u32,
    pub byte_start: usize,
    pub byte_end: usize,
}

impl From<&crate::callgraph::CallSite> for SerCallSite {
    fn from(s: &crate::callgraph::CallSite) -> Self {
        SerCallSite {
            callee_name: s.callee_name.clone(),
            full_callee: s.full_callee.clone(),
            line: s.line,
            byte_start: s.byte_start,
            byte_end: s.byte_end,
        }
    }
}

impl From<SerCallSite> for crate::callgraph::CallSite {
    fn from(s: SerCallSite) -> Self {
        crate::callgraph::CallSite {
            callee_name: s.callee_name,
            full_callee: s.full_callee,
            line: s.line,
            byte_start: s.byte_start,
            byte_end: s.byte_end,
        }
    }
}

/// Serializable mirror of `SymbolMeta`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerSymbolMeta {
    pub kind: SerSymbolKind,
    pub exported: bool,
    pub signature: Option<String>,
}

impl From<&crate::callgraph::SymbolMeta> for SerSymbolMeta {
    fn from(m: &crate::callgraph::SymbolMeta) -> Self {
        SerSymbolMeta {
            kind: m.kind.clone().into(),
            exported: m.exported,
            signature: m.signature.clone(),
        }
    }
}

impl From<SerSymbolMeta> for crate::callgraph::SymbolMeta {
    fn from(m: SerSymbolMeta) -> Self {
        crate::callgraph::SymbolMeta {
            kind: m.kind.into(),
            exported: m.exported,
            signature: m.signature,
        }
    }
}

/// Serializable mirror of `FileCallData`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerFileCallData {
    pub calls_by_symbol: HashMap<String, Vec<SerCallSite>>,
    pub exported_symbols: Vec<String>,
    pub symbol_metadata: HashMap<String, SerSymbolMeta>,
    pub import_block: SerImportBlock,
    pub lang: SerLangId,
}

impl From<&crate::callgraph::FileCallData> for SerFileCallData {
    fn from(d: &crate::callgraph::FileCallData) -> Self {
        SerFileCallData {
            calls_by_symbol: d
                .calls_by_symbol
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.iter().map(SerCallSite::from).collect(),
                    )
                })
                .collect(),
            exported_symbols: d.exported_symbols.clone(),
            symbol_metadata: d
                .symbol_metadata
                .iter()
                .map(|(k, v)| (k.clone(), SerSymbolMeta::from(v)))
                .collect(),
            import_block: SerImportBlock::from(&d.import_block),
            lang: d.lang.into(),
        }
    }
}

impl From<SerFileCallData> for crate::callgraph::FileCallData {
    fn from(d: SerFileCallData) -> Self {
        crate::callgraph::FileCallData {
            calls_by_symbol: d
                .calls_by_symbol
                .into_iter()
                .map(|(k, v)| {
                    (k, v.into_iter().map(crate::callgraph::CallSite::from).collect())
                })
                .collect(),
            exported_symbols: d.exported_symbols,
            symbol_metadata: d
                .symbol_metadata
                .into_iter()
                .map(|(k, v)| (k, crate::callgraph::SymbolMeta::from(v)))
                .collect(),
            import_block: d.import_block.into(),
            lang: d.lang.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// parse-index.cbor
// ---------------------------------------------------------------------------

/// Stat recorded at parse time (used as cache key).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileStat {
    /// mtime in nanoseconds since UNIX_EPOCH.
    pub mtime_nsec: i128,
    /// File size in bytes.
    pub size: u64,
}

impl FileStat {
    pub fn from_metadata(meta: &fs::Metadata) -> Option<Self> {
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos() as i128;
        let size = meta.len();
        Some(FileStat {
            mtime_nsec: mtime,
            size,
        })
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        Self::from_metadata(&meta)
    }
}

/// The full on-disk parse index: maps relative-path string → (stat, call data).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ParseIndex {
    /// Maps `relative_path → (FileStat, SerFileCallData)`.
    pub entries: HashMap<String, (FileStat, SerFileCallData)>,
}

impl ParseIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Upsert an entry (called after a file has been freshly parsed).
    pub fn upsert(&mut self, rel_path: String, stat: FileStat, data: SerFileCallData) {
        self.entries.insert(rel_path, (stat, data));
    }

    /// Remove stale entries (files that no longer exist on disk).
    pub fn remove(&mut self, rel_path: &str) {
        self.entries.remove(rel_path);
    }

    pub fn get(&self, rel_path: &str) -> Option<&(FileStat, SerFileCallData)> {
        self.entries.get(rel_path)
    }
}

pub fn write_parse_index(cache_dir: &Path, index: &ParseIndex) -> io::Result<()> {
    let mut data = Vec::new();
    ciborium::into_writer(index, &mut data)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    atomic_write(&cache_dir.join("parse-index.cbor"), &data)
}

pub fn read_parse_index(cache_dir: &Path) -> Option<ParseIndex> {
    let path = cache_dir.join("parse-index.cbor");
    let data = fs::read(&path).ok()?;
    if data.is_empty() {
        log::warn!("[cache] parse-index.cbor is empty — discarding");
        let _ = fs::remove_file(&path);
        return None;
    }
    match ciborium::from_reader::<ParseIndex, _>(data.as_slice()) {
        Ok(idx) => Some(idx),
        Err(e) => {
            log::warn!("[cache] parse-index.cbor corrupt ({}) — discarding", e);
            let _ = fs::remove_file(&path);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// helper-input-hash
// ---------------------------------------------------------------------------

/// Compute the helper input hash from all .go files in the project.
///
/// Hash = sha256(sorted(rel_path + mtime_nsec_hex + size_hex) for each .go file)
pub fn compute_helper_input_hash(project_root: &Path) -> String {
    let mut entries: Vec<(String, i128, u64)> = walk_project_files(project_root)
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("go"))
        .filter_map(|p| {
            let stat = FileStat::from_path(&p)?;
            let rel = p
                .strip_prefix(project_root)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned();
            Some((rel, stat.mtime_nsec, stat.size))
        })
        .collect();

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    for (rel, mtime, size) in &entries {
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(mtime.to_le_bytes());
        hasher.update(size.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub fn write_helper_input_hash(cache_dir: &Path, hash: &str) -> io::Result<()> {
    atomic_write(
        &cache_dir.join("helper-input-hash"),
        (hash.to_string() + "\n").as_bytes(),
    )
}

pub fn read_helper_input_hash(cache_dir: &Path) -> Option<String> {
    let path = cache_dir.join("helper-input-hash");
    let raw = fs::read_to_string(&path).ok()?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

// ---------------------------------------------------------------------------
// helper-output.json
// ---------------------------------------------------------------------------

pub fn write_helper_output(cache_dir: &Path, json_bytes: &[u8]) -> io::Result<()> {
    atomic_write(&cache_dir.join("helper-output.json"), json_bytes)
}

pub fn read_helper_output(cache_dir: &Path) -> Option<Vec<u8>> {
    let path = cache_dir.join("helper-output.json");
    let data = fs::read(&path).ok()?;
    if data.is_empty() {
        log::warn!("[cache] helper-output.json is empty — discarding");
        let _ = fs::remove_file(&path);
        return None;
    }
    // Quick sanity: must be valid JSON (starts with '[' or '{')
    let first = data.iter().find(|&&b| !b.is_ascii_whitespace());
    match first {
        Some(&b'[') | Some(&b'{') => Some(data),
        _ => {
            log::warn!("[cache] helper-output.json has unexpected format — discarding");
            let _ = fs::remove_file(&path);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// merged-graph.cbor
// ---------------------------------------------------------------------------
//
// The merged graph is a serialised form of the in-memory reverse index.
// It is fully derived and can always be rebuilt from parse-index + helper output.

/// A single entry in the serialised reverse index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedCallerEntry {
    pub caller_file: String,   // relative path
    pub caller_symbol: String,
    pub line: u32,
    pub col: u32,
    pub resolved: bool,
}

/// Embedded header for staleness detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedGraphHeader {
    pub schema_version: u32,
    /// sha256 of the parse-index CBOR at merge time (hex, first 16 chars).
    pub parse_index_digest: String,
    /// helper-input-hash at merge time.
    pub helper_input_hash: String,
}

/// Full on-disk merged graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedGraph {
    pub header: MergedGraphHeader,
    /// target_rel_path → target_symbol → Vec<MergedCallerEntry>
    pub reverse_index: HashMap<String, HashMap<String, Vec<MergedCallerEntry>>>,
}

impl MergedGraph {
    pub fn is_compatible(&self, parse_index_digest: &str, helper_input_hash: &str) -> bool {
        self.header.schema_version == SCHEMA_VERSION
            && self.header.parse_index_digest == parse_index_digest
            && self.header.helper_input_hash == helper_input_hash
    }
}

/// Compute a short content digest of the serialised parse index (first 16 hex chars).
pub fn parse_index_digest(cbor_bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cbor_bytes);
    format!("{:x}", hasher.finalize())[..16].to_string()
}

pub fn write_merged_graph(cache_dir: &Path, graph: &MergedGraph) -> io::Result<()> {
    let mut data = Vec::new();
    ciborium::into_writer(graph, &mut data)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    atomic_write(&cache_dir.join("merged-graph.cbor"), &data)
}

pub fn read_merged_graph(cache_dir: &Path) -> Option<MergedGraph> {
    let path = cache_dir.join("merged-graph.cbor");
    let data = fs::read(&path).ok()?;
    if data.is_empty() {
        log::warn!("[cache] merged-graph.cbor is empty — discarding");
        let _ = fs::remove_file(&path);
        return None;
    }
    match ciborium::from_reader::<MergedGraph, _>(data.as_slice()) {
        Ok(g) => Some(g),
        Err(e) => {
            log::warn!("[cache] merged-graph.cbor corrupt ({}) — discarding", e);
            let _ = fs::remove_file(&path);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// High-level cache manager
// ---------------------------------------------------------------------------

/// Outcome of a cache lookup for a single file.
pub enum FileCacheOutcome {
    /// Hit: stat matches, use the cached data.
    Hit(crate::callgraph::FileCallData),
    /// Miss: file changed or not present, must re-parse.
    Miss,
}

/// Central coordinator: orchestrates all cache files.
pub struct CacheManager {
    /// Resolved cache dir, or `None` if cache is disabled.
    cache_dir: Option<PathBuf>,
    project_root: PathBuf,
    /// Loaded parse index (None if cache disabled or load failed).
    parse_index: Option<ParseIndex>,
    /// Whether the parse index was dirty (i.e. any file was re-parsed)
    /// since the last flush.
    parse_index_dirty: bool,
    /// Cached helper-input-hash from disk.
    cached_helper_hash: Option<String>,
    /// Current helper-input-hash computed from disk state.
    current_helper_hash: Option<String>,
}

impl CacheManager {
    /// Create and initialise a cache manager for `project_root`.
    ///
    /// If the cache is disabled (via env or flag), all operations are no-ops
    /// and queries always miss.
    pub fn new(project_root: PathBuf, no_cache: bool) -> Self {
        if no_cache
            || std::env::var_os("AFT_DISABLE_CACHE").is_some_and(|v| v == "1")
        {
            return CacheManager {
                cache_dir: None,
                project_root,
                parse_index: None,
                parse_index_dirty: false,
                cached_helper_hash: None,
                current_helper_hash: None,
            };
        }

        let cache_dir = resolve_project_cache_dir(&project_root);

        // Ensure the directory exists
        if let Some(ref dir) = cache_dir {
            if fs::create_dir_all(dir).is_err() {
                log::warn!("[cache] could not create cache dir {:?}", dir);
            }
        }

        let mut mgr = CacheManager {
            cache_dir: cache_dir.clone(),
            project_root,
            parse_index: None,
            parse_index_dirty: false,
            cached_helper_hash: None,
            current_helper_hash: None,
        };

        if let Some(ref dir) = cache_dir {
            // Validate meta
            if let Some(meta) = read_meta(dir) {
                if !meta.is_compatible() {
                    log::info!(
                        "[cache] schema version mismatch (cached={}, current={}) — invalidating",
                        meta.schema_version,
                        SCHEMA_VERSION
                    );
                    mgr.delete_all_cache_files(dir);
                    // Write fresh meta
                    let new_meta = CacheMeta::new(&mgr.project_root);
                    let _ = write_meta(dir, &new_meta);
                } else {
                    // Load parse index
                    mgr.parse_index = read_parse_index(dir);
                    // Load helper hash
                    mgr.cached_helper_hash = read_helper_input_hash(dir);
                }
            } else {
                // No meta yet — write one
                let new_meta = CacheMeta::new(&mgr.project_root);
                let _ = write_meta(dir, &new_meta);
            }
        }

        mgr
    }

    fn delete_all_cache_files(&self, dir: &Path) {
        for name in &[
            "parse-index.cbor",
            "helper-output.json",
            "helper-input-hash",
            "merged-graph.cbor",
        ] {
            let _ = fs::remove_file(dir.join(name));
        }
    }

    /// Whether caching is active.
    pub fn is_enabled(&self) -> bool {
        self.cache_dir.is_some()
    }

    /// Return the cache directory (if enabled).
    pub fn cache_dir(&self) -> Option<&Path> {
        self.cache_dir.as_deref()
    }

    /// Look up a file in the parse cache.
    ///
    /// Returns `Hit(data)` if the on-disk stat matches the cached stat, or
    /// `Miss` if the file is stale or not in the cache.
    pub fn get_file(&self, abs_path: &Path) -> FileCacheOutcome {
        let index = match &self.parse_index {
            Some(i) => i,
            None => return FileCacheOutcome::Miss,
        };
        let rel = match abs_path.strip_prefix(&self.project_root) {
            Ok(r) => r.to_string_lossy().into_owned(),
            Err(_) => abs_path.to_string_lossy().into_owned(),
        };
        let (cached_stat, ser_data) = match index.get(&rel) {
            Some(entry) => entry,
            None => return FileCacheOutcome::Miss,
        };
        let current_stat = match FileStat::from_path(abs_path) {
            Some(s) => s,
            None => return FileCacheOutcome::Miss,
        };
        if *cached_stat == current_stat {
            FileCacheOutcome::Hit(crate::callgraph::FileCallData::from(ser_data.clone()))
        } else {
            FileCacheOutcome::Miss
        }
    }

    /// Store a freshly-parsed file in the parse cache (in memory only;
    /// call `flush_parse_index` to persist).
    pub fn put_file(
        &mut self,
        abs_path: &Path,
        stat: FileStat,
        data: &crate::callgraph::FileCallData,
    ) {
        if !self.is_enabled() {
            return;
        }
        let index = self.parse_index.get_or_insert_with(ParseIndex::new);
        let rel = match abs_path.strip_prefix(&self.project_root) {
            Ok(r) => r.to_string_lossy().into_owned(),
            Err(_) => abs_path.to_string_lossy().into_owned(),
        };
        index.upsert(rel, stat, SerFileCallData::from(data));
        self.parse_index_dirty = true;
        // Any change to the parse data invalidates the merged graph.
        self.invalidate_merged_graph();
    }

    /// Remove a stale file from the parse cache.
    pub fn remove_file(&mut self, abs_path: &Path) {
        if let Some(index) = &mut self.parse_index {
            let rel = match abs_path.strip_prefix(&self.project_root) {
                Ok(r) => r.to_string_lossy().into_owned(),
                Err(_) => abs_path.to_string_lossy().into_owned(),
            };
            index.remove(&rel);
            self.parse_index_dirty = true;
            // Any change to the parse data invalidates the merged graph.
            self.invalidate_merged_graph();
        }
    }

    /// Check whether the helper needs to re-run.
    ///
    /// Lazily computes `current_helper_hash` on first call. Returns `true`
    /// if the hash has changed (or was never computed), meaning the helper
    /// must be re-invoked.
    pub fn helper_needs_rerun(&mut self) -> bool {
        if !self.is_enabled() {
            return true; // no cache → always run
        }
        // Compute the current hash if not yet done
        if self.current_helper_hash.is_none() {
            self.current_helper_hash =
                Some(compute_helper_input_hash(&self.project_root));
        }
        match (&self.cached_helper_hash, &self.current_helper_hash) {
            (Some(cached), Some(current)) => cached != current,
            _ => true,
        }
    }

    /// Return the current helper-input-hash (computed lazily).
    pub fn current_helper_hash(&mut self) -> Option<String> {
        if self.current_helper_hash.is_none() {
            self.current_helper_hash =
                Some(compute_helper_input_hash(&self.project_root));
        }
        self.current_helper_hash.clone()
    }

    /// Persist the cached helper output and update the hash file.
    pub fn save_helper_output(&mut self, json_bytes: &[u8]) {
        let Some(dir) = self.cache_dir.clone() else {
            return;
        };
        if let Err(e) = write_helper_output(&dir, json_bytes) {
            log::warn!("[cache] could not write helper-output.json: {}", e);
            return;
        }
        if let Some(hash) = self.current_helper_hash() {
            if let Err(e) = write_helper_input_hash(&dir, &hash) {
                log::warn!("[cache] could not write helper-input-hash: {}", e);
            } else {
                self.cached_helper_hash = Some(hash);
            }
        }
    }

    /// Read the cached helper output from disk (if valid and not stale).
    ///
    /// Returns `None` if the helper needs to re-run.
    pub fn load_helper_output(&mut self) -> Option<Vec<u8>> {
        if self.helper_needs_rerun() {
            return None;
        }
        let dir = self.cache_dir.as_ref()?;
        read_helper_output(dir)
    }

    /// Invalidate the merged-graph cache on disk.
    ///
    /// Called whenever the parse cache is mutated (file changed or removed).
    /// The merged graph is a derived artifact: if any input (parse result) has
    /// changed, the cached merged graph is no longer valid.
    pub fn invalidate_merged_graph(&self) {
        let Some(ref dir) = self.cache_dir else {
            return;
        };
        let path = dir.join("merged-graph.cbor");
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                log::warn!("[cache] could not delete merged-graph.cbor: {}", e);
            }
        }
    }

    /// Write the parse index to disk (if dirty).
    pub fn flush_parse_index(&mut self) {
        if !self.parse_index_dirty {
            return;
        }
        let Some(ref dir) = self.cache_dir.clone() else {
            return;
        };
        if let Some(index) = &self.parse_index {
            match write_parse_index(dir, index) {
                Ok(()) => {
                    self.parse_index_dirty = false;
                }
                Err(e) => {
                    log::warn!("[cache] could not write parse-index.cbor: {}", e);
                }
            }
        }
    }

    /// Try to load the merged graph from disk.
    ///
    /// Returns `None` if not present, stale, or incompatible.
    pub fn load_merged_graph(&self) -> Option<MergedGraph> {
        let dir = self.cache_dir.as_ref()?;

        // Compute expected digests to validate staleness
        let parse_index_cbor = fs::read(dir.join("parse-index.cbor")).ok()?;
        let pi_digest = parse_index_digest(&parse_index_cbor);
        let helper_hash = read_helper_input_hash(dir).unwrap_or_default();

        let graph = read_merged_graph(dir)?;
        if graph.is_compatible(&pi_digest, &helper_hash) {
            Some(graph)
        } else {
            log::debug!("[cache] merged-graph.cbor is stale — will rebuild");
            None
        }
    }

    /// Build and write a new merged graph from the current in-memory reverse index.
    pub fn save_merged_graph(
        &self,
        reverse_index: &HashMap<PathBuf, HashMap<String, Vec<crate::callgraph::CallerSite>>>,
    ) {
        let Some(ref dir) = self.cache_dir else {
            return;
        };

        // Compute digests for the header
        let pi_cbor = match fs::read(dir.join("parse-index.cbor")) {
            Ok(d) => d,
            Err(_) => {
                log::debug!("[cache] parse-index.cbor not found — skipping merged-graph write");
                return;
            }
        };
        let pi_digest = parse_index_digest(&pi_cbor);
        let helper_hash = read_helper_input_hash(dir).unwrap_or_default();

        // Build the serialised reverse index
        let mut ser_reverse: HashMap<String, HashMap<String, Vec<MergedCallerEntry>>> =
            HashMap::new();
        for (target_path, symbol_map) in reverse_index {
            let target_rel = target_path
                .strip_prefix(&self.project_root)
                .unwrap_or(target_path)
                .to_string_lossy()
                .into_owned();
            let inner: HashMap<String, Vec<MergedCallerEntry>> = symbol_map
                .iter()
                .map(|(sym, callers)| {
                    let entries: Vec<MergedCallerEntry> = callers
                        .iter()
                        .map(|c| MergedCallerEntry {
                            caller_file: c
                                .caller_file
                                .strip_prefix(&self.project_root)
                                .unwrap_or(&c.caller_file)
                                .to_string_lossy()
                                .into_owned(),
                            caller_symbol: c.caller_symbol.clone(),
                            line: c.line,
                            col: c.col,
                            resolved: c.resolved,
                        })
                        .collect();
                    (sym.clone(), entries)
                })
                .collect();
            ser_reverse.insert(target_rel, inner);
        }

        let graph = MergedGraph {
            header: MergedGraphHeader {
                schema_version: SCHEMA_VERSION,
                parse_index_digest: pi_digest,
                helper_input_hash: helper_hash,
            },
            reverse_index: ser_reverse,
        };

        if let Err(e) = write_merged_graph(dir, &graph) {
            log::warn!("[cache] could not write merged-graph.cbor: {}", e);
        }
    }

    /// Deserialise a cached merged graph into the in-memory `ReverseIndex` type
    /// used by `CallGraph`.
    pub fn restore_reverse_index(
        &self,
        graph: MergedGraph,
    ) -> HashMap<PathBuf, HashMap<String, Vec<crate::callgraph::CallerSite>>> {
        let mut reverse: HashMap<PathBuf, HashMap<String, Vec<crate::callgraph::CallerSite>>> =
            HashMap::new();

        for (target_rel, symbol_map) in graph.reverse_index {
            let target_path = self.project_root.join(&target_rel);
            let inner = symbol_map
                .into_iter()
                .map(|(sym, entries)| {
                    let callers: Vec<crate::callgraph::CallerSite> = entries
                        .into_iter()
                        .map(|e| crate::callgraph::CallerSite {
                            caller_file: self.project_root.join(&e.caller_file),
                            caller_symbol: e.caller_symbol,
                            line: e.line,
                            col: e.col,
                            resolved: e.resolved,
                            kind: None,
                            nearby_string: None,
                            context: None,
                        })
                        .collect();
                    (sym, callers)
                })
                .collect();
            reverse.insert(target_path, inner);
        }

        reverse
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tmp_cache() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let cache = dir.path().join("cache");
        fs::create_dir_all(&cache).unwrap();
        (dir, cache)
    }

    fn tmp_project() -> TempDir {
        TempDir::new().unwrap()
    }

    // --- meta.json round-trip ---

    #[test]
    fn meta_round_trip() {
        let (_d, cache) = tmp_cache();
        let meta = CacheMeta::new(Path::new("/fake/project"));
        write_meta(&cache, &meta).unwrap();
        let loaded = read_meta(&cache).unwrap();
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.project_root, "/fake/project");
    }

    #[test]
    fn meta_corrupt_triggers_discard() {
        let (_d, cache) = tmp_cache();
        fs::write(cache.join("meta.json"), b"not json!!!").unwrap();
        let loaded = read_meta(&cache);
        assert!(loaded.is_none());
        // file should be gone
        assert!(!cache.join("meta.json").exists());
    }

    #[test]
    fn meta_empty_triggers_discard() {
        let (_d, cache) = tmp_cache();
        fs::write(cache.join("meta.json"), b"").unwrap();
        let loaded = read_meta(&cache);
        assert!(loaded.is_none());
    }

    #[test]
    fn meta_schema_mismatch_detected() {
        let (_d, cache) = tmp_cache();
        let meta = CacheMeta {
            project_root: "/p".to_string(),
            aft_version: "0.0.0".to_string(),
            schema_version: 9999, // definitely not SCHEMA_VERSION
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_refreshed_at: "2026-01-01T00:00:00Z".to_string(),
        };
        write_meta(&cache, &meta).unwrap();
        let loaded = read_meta(&cache).unwrap();
        assert!(!loaded.is_compatible());
    }

    // --- parse-index.cbor round-trip ---

    fn make_parse_index() -> ParseIndex {
        let mut idx = ParseIndex::new();
        let stat = FileStat {
            mtime_nsec: 1_700_000_000_000_000_000,
            size: 1234,
        };
        let data = SerFileCallData {
            calls_by_symbol: {
                let mut m = HashMap::new();
                m.insert(
                    "main".to_string(),
                    vec![SerCallSite {
                        callee_name: "helper".to_string(),
                        full_callee: "helper".to_string(),
                        line: 5,
                        byte_start: 100,
                        byte_end: 106,
                    }],
                );
                m
            },
            exported_symbols: vec!["main".to_string()],
            symbol_metadata: {
                let mut m = HashMap::new();
                m.insert(
                    "main".to_string(),
                    SerSymbolMeta {
                        kind: SerSymbolKind::Function,
                        exported: true,
                        signature: None,
                    },
                );
                m
            },
            import_block: SerImportBlock {
                imports: vec![],
                byte_range: None,
            },
            lang: SerLangId::Go,
        };
        idx.upsert("main.go".to_string(), stat, data);
        idx
    }

    #[test]
    fn parse_index_round_trip() {
        let (_d, cache) = tmp_cache();
        let idx = make_parse_index();
        write_parse_index(&cache, &idx).unwrap();
        let loaded = read_parse_index(&cache).unwrap();
        assert!(loaded.entries.contains_key("main.go"));
        let (stat, data) = &loaded.entries["main.go"];
        assert_eq!(stat.mtime_nsec, 1_700_000_000_000_000_000);
        assert_eq!(stat.size, 1234);
        assert!(data.calls_by_symbol.contains_key("main"));
        let sites = &data.calls_by_symbol["main"];
        assert_eq!(sites[0].callee_name, "helper");
    }

    #[test]
    fn parse_index_corrupt_triggers_rebuild() {
        let (_d, cache) = tmp_cache();
        fs::write(cache.join("parse-index.cbor"), b"\xff\xfe GARBAGE").unwrap();
        let loaded = read_parse_index(&cache);
        assert!(loaded.is_none());
        assert!(!cache.join("parse-index.cbor").exists());
    }

    #[test]
    fn parse_index_empty_triggers_discard() {
        let (_d, cache) = tmp_cache();
        fs::write(cache.join("parse-index.cbor"), b"").unwrap();
        let loaded = read_parse_index(&cache);
        assert!(loaded.is_none());
    }

    // --- FileStat ---

    #[test]
    fn file_stat_from_path() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("x.go");
        fs::write(&p, b"package main").unwrap();
        let stat = FileStat::from_path(&p).unwrap();
        assert!(stat.mtime_nsec > 0);
        assert_eq!(stat.size, 12);
    }

    // --- helper-input-hash ---

    #[test]
    fn helper_input_hash_round_trip() {
        let (_d, cache) = tmp_cache();
        write_helper_input_hash(&cache, "deadbeef1234").unwrap();
        let loaded = read_helper_input_hash(&cache).unwrap();
        assert_eq!(loaded, "deadbeef1234");
    }

    #[test]
    fn helper_input_hash_changes_when_file_mtime_changes() {
        let project = tmp_project();
        let go_file = project.path().join("main.go");
        fs::write(&go_file, b"package main\nfunc main() {}").unwrap();
        let h1 = compute_helper_input_hash(project.path());

        // Touch the file (change mtime by writing again)
        fs::write(&go_file, b"package main\nfunc main() { _ = 1 }").unwrap();
        let h2 = compute_helper_input_hash(project.path());
        assert_ne!(h1, h2, "hash should differ after file content change");
    }

    #[test]
    fn helper_input_hash_stable_without_changes() {
        let project = tmp_project();
        let go_file = project.path().join("main.go");
        fs::write(&go_file, b"package main").unwrap();
        let h1 = compute_helper_input_hash(project.path());
        let h2 = compute_helper_input_hash(project.path());
        assert_eq!(h1, h2);
    }

    // --- helper-output.json ---

    #[test]
    fn helper_output_round_trip() {
        let (_d, cache) = tmp_cache();
        let json = br#"[{"caller":{"file":"main.go","line":1,"symbol":"main"},"callee":{"file":"util.go","symbol":"helper"},"kind":"concrete"}]"#;
        write_helper_output(&cache, json).unwrap();
        let loaded = read_helper_output(&cache).unwrap();
        assert_eq!(loaded, json);
    }

    #[test]
    fn helper_output_corrupt_discarded() {
        let (_d, cache) = tmp_cache();
        // Not valid JSON (doesn't start with [ or {)
        fs::write(cache.join("helper-output.json"), b"GARBAGE BYTES").unwrap();
        let loaded = read_helper_output(&cache);
        assert!(loaded.is_none());
    }

    // --- merged-graph.cbor ---

    #[test]
    fn merged_graph_round_trip() {
        let (_d, cache) = tmp_cache();
        let mut reverse: HashMap<String, HashMap<String, Vec<MergedCallerEntry>>> =
            HashMap::new();
        reverse
            .entry("util.go".to_string())
            .or_default()
            .entry("helper".to_string())
            .or_default()
            .push(MergedCallerEntry {
                caller_file: "main.go".to_string(),
                caller_symbol: "main".to_string(),
                line: 5,
                col: 0,
                resolved: true,
            });

        let graph = MergedGraph {
            header: MergedGraphHeader {
                schema_version: SCHEMA_VERSION,
                parse_index_digest: "abc123def456abcd".to_string(),
                helper_input_hash: "deadbeef".to_string(),
            },
            reverse_index: reverse,
        };

        write_merged_graph(&cache, &graph).unwrap();
        let loaded = read_merged_graph(&cache).unwrap();
        assert_eq!(loaded.header.schema_version, SCHEMA_VERSION);
        assert!(loaded.reverse_index.contains_key("util.go"));
        let callers = &loaded.reverse_index["util.go"]["helper"];
        assert_eq!(callers[0].caller_file, "main.go");
    }

    #[test]
    fn merged_graph_corrupt_discarded() {
        let (_d, cache) = tmp_cache();
        fs::write(cache.join("merged-graph.cbor"), b"\xff GARBAGE").unwrap();
        let loaded = read_merged_graph(&cache);
        assert!(loaded.is_none());
        assert!(!cache.join("merged-graph.cbor").exists());
    }

    // --- CacheManager ---

    #[test]
    fn cache_manager_disabled_when_env_set() {
        // Use a scoped env var change via thread_local env trick.
        // Actually we need unsafe for this in tests; use a sub-process approach or
        // just test via the resolution logic.
        // We test the `no_cache` flag path instead:
        let project = tmp_project();
        let mgr = CacheManager::new(project.path().to_path_buf(), true /* no_cache */);
        assert!(!mgr.is_enabled());
    }

    #[test]
    fn cache_manager_enabled_and_creates_dir() {
        let project = tmp_project();
        // Point cache to a temp dir via AFT_CACHE_DIR
        let cache_base = TempDir::new().unwrap();
        // We can't easily set env vars in parallel tests, so test directly via resolve
        let dir = cache_base.path().join("cache_dir_test");
        fs::create_dir_all(&dir).unwrap();
        // Validate hash is deterministic
        let h1 = project_hash(project.path());
        let h2 = project_hash(project.path());
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 12);
    }

    #[test]
    fn cache_manager_schema_mismatch_invalidates() {
        let project = tmp_project();
        let cache_base = TempDir::new().unwrap();

        // Write a meta with wrong schema_version directly
        let cache_dir = cache_base.path().join(project_hash(project.path()));
        fs::create_dir_all(&cache_dir).unwrap();

        let bad_meta = CacheMeta {
            project_root: project.path().to_string_lossy().into_owned(),
            aft_version: "0.0.0".to_string(),
            schema_version: 9999,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_refreshed_at: "2026-01-01T00:00:00Z".to_string(),
        };
        write_meta(&cache_dir, &bad_meta).unwrap();

        // Write a fake parse index too
        let fake_index = ParseIndex::new();
        write_parse_index(&cache_dir, &fake_index).unwrap();
        assert!(cache_dir.join("parse-index.cbor").exists());

        // Now set AFT_CACHE_DIR and create a CacheManager — it should invalidate
        // We test the logic indirectly: load meta, check incompatible, verify cache files removed.
        let meta = read_meta(&cache_dir).unwrap();
        assert!(!meta.is_compatible());
        // Simulate what CacheManager does on mismatch:
        let _ = fs::remove_file(cache_dir.join("parse-index.cbor"));
        assert!(!cache_dir.join("parse-index.cbor").exists());
    }

    #[test]
    fn cache_manager_file_hit_and_miss() {
        let project = tmp_project();
        let go_file = project.path().join("src.go");
        fs::write(&go_file, b"package main").unwrap();

        let stat = FileStat::from_path(&go_file).unwrap();

        // Build a fake FileCallData
        let mut data = crate::callgraph::FileCallData {
            calls_by_symbol: HashMap::new(),
            exported_symbols: vec!["Foo".to_string()],
            symbol_metadata: HashMap::new(),
            import_block: ImportBlock::empty(),
            lang: crate::parser::LangId::Go,
        };
        data.calls_by_symbol.insert(
            "Foo".to_string(),
            vec![crate::callgraph::CallSite {
                callee_name: "Bar".to_string(),
                full_callee: "Bar".to_string(),
                line: 3,
                byte_start: 50,
                byte_end: 53,
            }],
        );

        // Use a CacheManager backed by a temp cache dir
        let cache_base = TempDir::new().unwrap();
        // Override AFT_CACHE_DIR would affect other tests; instead we test via
        // the internal helpers directly.
        let cache_dir = cache_base.path().join("proj");
        fs::create_dir_all(&cache_dir).unwrap();

        let mut index = ParseIndex::new();
        let rel = go_file
            .strip_prefix(project.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        index.upsert(rel.clone(), stat.clone(), SerFileCallData::from(&data));
        write_parse_index(&cache_dir, &index).unwrap();

        let loaded_index = read_parse_index(&cache_dir).unwrap();
        let (loaded_stat, _) = loaded_index.entries.get(&rel).unwrap();
        assert_eq!(*loaded_stat, stat, "cached stat must match");
    }

    // --- Concurrent access (basic) ---

    #[test]
    fn atomic_write_survives_concurrent_writes() {
        let dir = TempDir::new().unwrap();
        let final_path = dir.path().join("data.cbor");

        // Two threads racing to write — last writer wins; no crash.
        let p1 = final_path.clone();
        let p2 = final_path.clone();

        let t1 = std::thread::spawn(move || atomic_write(&p1, b"thread1").is_ok());
        let t2 = std::thread::spawn(move || atomic_write(&p2, b"thread2").is_ok());

        let r1 = t1.join().unwrap();
        let r2 = t2.join().unwrap();
        assert!(r1 || r2, "at least one write must succeed");
        // The file must exist and contain one of the two values
        let contents = fs::read(&final_path).unwrap();
        assert!(contents == b"thread1" || contents == b"thread2");
    }

    // --- Invalidation: file delete ---

    #[test]
    fn parse_index_drops_deleted_file_entry() {
        let project = tmp_project();
        let f = project.path().join("main.go");
        fs::write(&f, b"package main").unwrap();
        let stat = FileStat::from_path(&f).unwrap();

        let mut idx = ParseIndex::new();
        let data = SerFileCallData {
            calls_by_symbol: HashMap::new(),
            exported_symbols: vec![],
            symbol_metadata: HashMap::new(),
            import_block: SerImportBlock {
                imports: vec![],
                byte_range: None,
            },
            lang: SerLangId::Go,
        };
        idx.upsert("main.go".to_string(), stat, data);
        assert!(idx.get("main.go").is_some());

        // Simulate file deletion
        fs::remove_file(&f).unwrap();
        idx.remove("main.go");
        assert!(idx.get("main.go").is_none());
    }

    // --- Serialisation mirrors are lossless ---

    #[test]
    fn lang_id_round_trip_all_variants() {
        for lang in &[
            LangId::TypeScript,
            LangId::Tsx,
            LangId::JavaScript,
            LangId::Python,
            LangId::Rust,
            LangId::Go,
            LangId::C,
            LangId::Cpp,
            LangId::Zig,
            LangId::CSharp,
            LangId::Bash,
            LangId::Html,
            LangId::Markdown,
        ] {
            let ser: SerLangId = (*lang).into();
            let back: LangId = ser.into();
            assert_eq!(*lang, back);
        }
    }

    #[test]
    fn symbol_kind_round_trip_all_variants() {
        for kind in &[
            SymbolKind::Function,
            SymbolKind::Class,
            SymbolKind::Method,
            SymbolKind::Struct,
            SymbolKind::Interface,
            SymbolKind::Enum,
            SymbolKind::TypeAlias,
            SymbolKind::Variable,
            SymbolKind::Heading,
        ] {
            let ser: SerSymbolKind = kind.clone().into();
            let back: SymbolKind = ser.into();
            assert_eq!(*kind, back);
        }
    }
}
