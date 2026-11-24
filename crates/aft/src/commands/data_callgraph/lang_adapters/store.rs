//! Overlay store (owner: R1).
//!
//! One SQLite file per (root, language, key) under aft's storage directory
//! (decision 10). The file is written to a temporary name and renamed only when
//! the whole run passed every check, so a reader never sees a half-written
//! overlay. Next to it, per key: a failure record (`<key>.failure.json`), the run
//! lock (`run.lock`) and the runner log (`run.log`).
//!
//! `SqliteOverlay` answers every `OverlayRead` method exactly like
//! `fixture::FixtureOverlay` does for the same output (a test compares them).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::contract::{
    line_col_to_offset, AdapterOutput, FactLookup, FactQuery, FactRecord, FactSide, OverlayCounts, OverlayRead, OverlaySite,
    SiteRecord,
};
use super::key::{hex, Key};

/// Bump when a table changes. A file with another version is deleted and built again.
const SCHEMA_VERSION: i64 = 1;

pub(crate) fn now_unix_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Where things live
// ---------------------------------------------------------------------------

/// The directory of all adapter state inside aft's storage directory.
pub(crate) fn adapters_root(storage: &Path) -> PathBuf {
    storage.join("lang-adapters")
}

/// The files of one (root, language) pair.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pub dir: PathBuf,
}

impl Layout {
    /// `root` must be the canonical path, so one repository has one directory.
    pub(crate) fn new(adapters_root: &Path, root: &Path, language: &str) -> Self {
        let readable = |text: &str| -> String {
            text.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(40).collect()
        };
        let base = root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let root_hash = hex(&Sha256::digest(root.as_os_str().as_encoded_bytes()))[..12].to_string();
        Self { dir: adapters_root.join("overlays").join(format!("{}-{root_hash}-{}", readable(&base), readable(language))) }
    }

    pub(crate) fn overlay_path(&self, key_id: &str) -> PathBuf {
        self.dir.join(format!("{key_id}.sqlite"))
    }

    pub(crate) fn failure_path(&self, key_id: &str) -> PathBuf {
        self.dir.join(format!("{key_id}.failure.json"))
    }

    pub(crate) fn lock_path(&self) -> PathBuf {
        self.dir.join("run.lock")
    }

    pub(crate) fn log_path(&self) -> PathBuf {
        self.dir.join("run.log")
    }
}

// ---------------------------------------------------------------------------
// Overlay metadata and failure record
// ---------------------------------------------------------------------------

/// What the store knows about an overlay without loading its sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Meta {
    pub language: String,
    pub head: String,
    pub build_id: String,
    pub toolchain_id: String,
    pub built_at_unix_s: u64,
    pub partial: bool,
    /// Names of the unanalysed packages or modules (else files).
    pub unanalysed: Vec<String>,
    pub tool: String,
    pub tool_version: String,
    pub counts: OverlayCounts,
}

impl Meta {
    /// Meta for a validated run that was built at `built_at_unix_s`.
    pub(crate) fn of_output(output: &AdapterOutput, key: &Key, built_at_unix_s: u64) -> Self {
        Self {
            language: output.header.language.clone(),
            head: key.head.clone(),
            build_id: key.build_id.clone(),
            toolchain_id: key.toolchain_id.clone(),
            built_at_unix_s,
            partial: output.is_partial(),
            unanalysed: unanalysed_names(output),
            tool: output.header.tool.clone(),
            tool_version: output.header.tool_version.clone(),
            counts: OverlayCounts {
                sites: output.end.sites,
                targets: output.end.targets,
                facts: output.end.facts,
                analysed_files: output.end.analysed_files,
            },
        }
    }
}

/// The unit of each `unanalysed` record, else its files; at most 20 names.
fn unanalysed_names(output: &AdapterOutput) -> Vec<String> {
    const SHOWN: usize = 20;
    let mut names: Vec<String> = Vec::new();
    for record in &output.unanalysed {
        let name = match &record.unit {
            Some(unit) => unit.clone(),
            None => record.files.join(", "),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if names.len() > SHOWN {
        let more = names.len() - SHOWN;
        names.truncate(SHOWN);
        names.push(format!("and {more} more"));
    }
    names
}

/// Why the last run for a key failed. The retry time is not stored: it is
/// `at_unix_s` plus the entry's cooldown when the record is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FailureRecord {
    pub reason: String,
    pub at_unix_s: u64,
    pub stderr_tail: Vec<String>,
}

pub(crate) fn write_failure(layout: &Layout, key_id: &str, record: &FailureRecord) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
    write_atomic(&layout.failure_path(key_id), &text)
}

pub(crate) fn read_failure(layout: &Layout, key_id: &str) -> Option<FailureRecord> {
    let bytes = std::fs::read(layout.failure_path(key_id)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temp, bytes).map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot move {} into place: {error}", temp.display())
    })
}

/// Everything stored for one key.
#[derive(Debug, Clone, Default)]
pub(crate) struct KeyState {
    /// The overlay for this key, when it exists and can be read.
    pub meta: Option<Meta>,
    pub failure: Option<FailureRecord>,
    /// HEAD of the newest overlay of another key, when there is one (for `stale`).
    pub other_head: Option<String>,
}

pub(crate) fn key_state(layout: &Layout, key_id: &str) -> KeyState {
    let path = layout.overlay_path(key_id);
    let meta = if path.exists() {
        match read_meta_cached(&path) {
            Ok(meta) => Some(meta),
            Err(error) => {
                // A damaged or old-format file is only a cache: drop it, and the next run rebuilds it.
                eprintln!("aft adapters: removing unreadable overlay {}: {error}", path.display());
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    } else {
        None
    };
    let other_head = if meta.is_none() { newest_other_head(layout, key_id) } else { None };
    KeyState { meta, failure: read_failure(layout, key_id), other_head }
}

fn newest_other_head(layout: &Layout, key_id: &str) -> Option<String> {
    let own = format!("{key_id}.sqlite");
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&layout.dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".sqlite") || name == own {
            continue;
        }
        let modified = entry.metadata().and_then(|meta| meta.modified()).unwrap_or(UNIX_EPOCH);
        if newest.as_ref().map_or(true, |(time, _)| modified > *time) {
            newest = Some((modified, entry.path()));
        }
    }
    read_meta(&newest?.1).ok().map(|meta| meta.head)
}

/// After a run that stored an overlay for `keep_key_id`: remove the overlays and
/// failure records of other keys. Failures to remove are not worth a failed run.
pub(crate) fn remove_other_keys(layout: &Layout, keep_key_id: &str) {
    let Ok(entries) = std::fs::read_dir(&layout.dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let owned = name.ends_with(".sqlite") || name.ends_with(".failure.json");
        if owned && !name.starts_with(keep_key_id) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// ---------------------------------------------------------------------------
// Sites with byte offsets
// ---------------------------------------------------------------------------

/// Turns every site position into a byte offset of its file under `root`, and
/// sorts the sites by (file, offset), the order `FixtureOverlay` uses. A site
/// outside its file is a contract violation (the message says so).
pub(crate) fn sites_with_offsets(root: &Path, sites: Vec<SiteRecord>) -> Result<Vec<OverlaySite>, String> {
    // Group by file (stable) so each file is read once and not kept.
    let mut sites = sites;
    sites.sort_by(|a, b| a.file.cmp(&b.file));
    let mut out: Vec<OverlaySite> = Vec::with_capacity(sites.len());
    let mut current: Option<(String, String)> = None;
    for site in sites {
        if current.as_ref().map_or(true, |(file, _)| *file != site.file) {
            let bytes = std::fs::read(root.join(&site.file))
                .map_err(|error| format!("contract violation: site file {} cannot be read in the repository: {error}", site.file))?;
            let text = String::from_utf8(bytes).map_err(|_| format!("contract violation: site file {} is not UTF-8", site.file))?;
            current = Some((site.file.clone(), text));
        }
        let text = &current.as_ref().expect("file text was just read").1;
        let offset = line_col_to_offset(text, site.line, site.col)
            .ok_or_else(|| format!("contract violation: site {}:{}:{} is not inside the file", site.file, site.line, site.col))?;
        out.push(OverlaySite { offset, site });
    }
    out.sort_by(|a, b| (&a.site.file, a.offset).cmp(&(&b.site.file, b.offset)));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

const TABLES: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE analysed (file TEXT PRIMARY KEY);
CREATE TABLE sites (id INTEGER PRIMARY KEY, file TEXT NOT NULL, off INTEGER NOT NULL, record TEXT NOT NULL);
CREATE TABLE targets (site_id INTEGER NOT NULL, idx INTEGER NOT NULL, file TEXT NOT NULL, line INTEGER NOT NULL);
CREATE TABLE facts (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, label TEXT,
    subj_file TEXT NOT NULL, subj_line INTEGER NOT NULL, subj_name TEXT,
    obj_file TEXT NOT NULL, obj_line INTEGER NOT NULL, obj_name TEXT, record TEXT NOT NULL);
";

const INDEXES: &str = "
CREATE INDEX sites_by_caller ON sites (file, off, id);
CREATE INDEX targets_by_file_line ON targets (file, line, site_id, idx);
CREATE INDEX facts_by_label ON facts (kind, label);
CREATE INDEX facts_by_subject ON facts (kind, subj_file, subj_line);
CREATE INDEX facts_by_object ON facts (kind, obj_file, obj_line);
CREATE INDEX facts_by_subject_name ON facts (kind, subj_name);
CREATE INDEX facts_by_object_name ON facts (kind, obj_name);
";

fn db_error(what: &str, error: rusqlite::Error) -> String {
    format!("overlay store: {what}: {error}")
}

/// Writes the overlay to `final_path` (temporary file, then rename). `sites` come
/// from `sites_with_offsets`; `output` supplies the rest.
pub(crate) fn write_overlay(final_path: &Path, meta: &Meta, output: &AdapterOutput, sites: &[OverlaySite]) -> Result<(), String> {
    if let Some(dir) = final_path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    }
    let temp = final_path.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let written = write_tables(&temp, meta, output, sites);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    std::fs::rename(&temp, final_path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot move the overlay into place: {error}")
    })
}

fn write_tables(path: &Path, meta: &Meta, output: &AdapterOutput, sites: &[OverlaySite]) -> Result<(), String> {
    let conn = Connection::open(path).map_err(|error| db_error("open", error))?;
    // The file is private until the rename, so no journal and no sync are needed.
    conn.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY;")
        .map_err(|error| db_error("pragma", error))?;
    conn.execute_batch(TABLES).map_err(|error| db_error("create tables", error))?;
    {
        let tx = conn.unchecked_transaction().map_err(|error| db_error("begin", error))?;
        let mut put_meta = tx.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)").map_err(|e| db_error("prepare meta", e))?;
        let pairs: Vec<(&str, String)> = vec![
            ("language", meta.language.clone()),
            ("head", meta.head.clone()),
            ("build_id", meta.build_id.clone()),
            ("toolchain_id", meta.toolchain_id.clone()),
            ("built_at_unix_s", meta.built_at_unix_s.to_string()),
            ("partial", (meta.partial as u8).to_string()),
            ("unanalysed", serde_json::to_string(&meta.unanalysed).map_err(|e| e.to_string())?),
            ("tool", meta.tool.clone()),
            ("tool_version", meta.tool_version.clone()),
            ("sites", meta.counts.sites.to_string()),
            ("targets", meta.counts.targets.to_string()),
            ("facts", meta.counts.facts.to_string()),
            ("analysed_files", meta.counts.analysed_files.to_string()),
        ];
        for (key, value) in pairs {
            put_meta.execute(params![key, value]).map_err(|e| db_error("insert meta", e))?;
        }
        let mut put_analysed = tx.prepare("INSERT INTO analysed (file) VALUES (?1)").map_err(|e| db_error("prepare analysed", e))?;
        for file in &output.analysed {
            put_analysed.execute(params![file]).map_err(|e| db_error("insert analysed", e))?;
        }
        let mut put_site = tx
            .prepare("INSERT INTO sites (id, file, off, record) VALUES (?1, ?2, ?3, ?4)")
            .map_err(|e| db_error("prepare site", e))?;
        let mut put_target = tx
            .prepare("INSERT INTO targets (site_id, idx, file, line) VALUES (?1, ?2, ?3, ?4)")
            .map_err(|e| db_error("prepare target", e))?;
        for (index, entry) in sites.iter().enumerate() {
            let id = index as i64 + 1;
            let record = serde_json::to_string(&entry.site).map_err(|e| e.to_string())?;
            put_site.execute(params![id, entry.site.file, entry.offset as i64, record]).map_err(|e| db_error("insert site", e))?;
            for (target_index, target) in entry.site.targets.iter().enumerate() {
                put_target.execute(params![id, target_index as i64, target.file, target.line]).map_err(|e| db_error("insert target", e))?;
            }
        }
        let mut put_fact = tx
            .prepare(
                "INSERT INTO facts (id, kind, label, subj_file, subj_line, subj_name, obj_file, obj_line, obj_name, record)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )
            .map_err(|e| db_error("prepare fact", e))?;
        for (index, fact) in output.facts.iter().enumerate() {
            let record = serde_json::to_string(fact).map_err(|e| e.to_string())?;
            put_fact
                .execute(params![
                    index as i64 + 1,
                    fact.kind,
                    fact.label,
                    fact.subject.file,
                    fact.subject.line,
                    fact.subject_name,
                    fact.object.file,
                    fact.object.line,
                    fact.object_name,
                    record
                ])
                .map_err(|e| db_error("insert fact", e))?;
        }
        drop((put_meta, put_analysed, put_site, put_target, put_fact));
        tx.commit().map_err(|error| db_error("commit", error))?;
    }
    conn.execute_batch(INDEXES).map_err(|error| db_error("create indexes", error))?;
    conn.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION};")).map_err(|error| db_error("set version", error))?;
    conn.close().map_err(|(_, error)| db_error("close", error))
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

fn open_read_only(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|error| db_error("open", error))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).map_err(|error| db_error("read version", error))?;
    if version != SCHEMA_VERSION {
        return Err(format!("overlay store: schema version {version}, this build reads {SCHEMA_VERSION}"));
    }
    Ok(conn)
}

fn read_meta_from(conn: &Connection) -> Result<Meta, String> {
    let mut values: HashMap<String, String> = HashMap::new();
    let mut statement = conn.prepare("SELECT key, value FROM meta").map_err(|error| db_error("read meta", error))?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|error| db_error("read meta", error))?;
    for row in rows {
        let (key, value) = row.map_err(|error| db_error("read meta", error))?;
        values.insert(key, value);
    }
    let text = |name: &str| values.get(name).cloned().ok_or_else(|| format!("overlay store: meta {name} is missing"));
    let number = |name: &str| -> Result<u64, String> { text(name)?.parse().map_err(|_| format!("overlay store: meta {name} is not a number")) };
    Ok(Meta {
        language: text("language")?,
        head: text("head")?,
        build_id: text("build_id")?,
        toolchain_id: text("toolchain_id")?,
        built_at_unix_s: number("built_at_unix_s")?,
        partial: number("partial")? != 0,
        unanalysed: serde_json::from_str(&text("unanalysed")?).map_err(|error| format!("overlay store: meta unanalysed: {error}"))?,
        tool: text("tool")?,
        tool_version: text("tool_version")?,
        counts: OverlayCounts {
            sites: number("sites")?,
            targets: number("targets")?,
            facts: number("facts")?,
            analysed_files: number("analysed_files")?,
        },
    })
}

pub(crate) fn read_meta(path: &Path) -> Result<Meta, String> {
    read_meta_from(&open_read_only(path)?)
}

/// The overlay of one key, served from its SQLite file.
pub(crate) struct SqliteOverlay {
    language: String,
    hidden_tags: Vec<String>,
    counts: OverlayCounts,
    conn: Mutex<Connection>,
}

/// Opens the overlay file. `hidden_tags` is the entry's list at query time.
pub(crate) fn open_overlay(path: &Path, hidden_tags: Vec<String>) -> Result<SqliteOverlay, String> {
    open_with_meta(path, hidden_tags).map(|(_, overlay)| overlay)
}

fn open_with_meta(path: &Path, hidden_tags: Vec<String>) -> Result<(Meta, SqliteOverlay), String> {
    let conn = open_read_only(path)?;
    let meta = read_meta_from(&conn)?;
    let overlay = SqliteOverlay { language: meta.language.clone(), hidden_tags, counts: meta.counts, conn: Mutex::new(conn) };
    Ok((meta, overlay))
}

// ---------------------------------------------------------------------------
// Opened overlays kept for the next query
// ---------------------------------------------------------------------------

/// Who an overlay file is: size, modification time, inode. A run writes the file under a
/// temporary name and renames it into place, so a rebuilt overlay has another identity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileId {
    size: u64,
    mtime: (i64, i64),
    inode: u64,
}

impl FileId {
    fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        Some(Self { size: meta.size(), mtime: (meta.mtime(), meta.mtime_nsec()), inode: meta.ino() })
    }
}

/// One overlay file as last read: its meta, and the opened overlay once a query asked for it.
///
/// VALIDITY RULE: an entry serves while the file still has the identity it had when it was read
/// and, for the overlay, while the registry's `hidden_tags` are the same. Anything else reads the
/// file again. A cached overlay is the SAME `Arc` for every query that uses it, and the graph
/// layer relies on that to keep its own per-file mappings (`graph/memo.rs`).
struct Opened {
    id: FileId,
    meta: Meta,
    overlay: Option<(Vec<String>, Arc<SqliteOverlay>)>,
}

/// Overlays kept at once. Each holds an open database connection.
const MAX_OPENED: usize = 8;

static OPENED: Mutex<BTreeMap<PathBuf, Opened>> = Mutex::new(BTreeMap::new());

fn opened_overlays() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Opened>> {
    OPENED.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Stores `entry` for `path`, first dropping entries whose file is gone or replaced.
fn keep_opened(opened: &mut BTreeMap<PathBuf, Opened>, path: &Path, entry: Opened) {
    opened.retain(|other, kept| other == path || FileId::of(other).as_ref() == Some(&kept.id));
    if opened.len() >= MAX_OPENED && !opened.contains_key(path) {
        opened.clear();
    }
    opened.insert(path.to_path_buf(), entry);
}

/// `read_meta`, answered from the kept entry while the file is the one that was read.
pub(crate) fn read_meta_cached(path: &Path) -> Result<Meta, String> {
    let Some(id) = FileId::of(path) else { return read_meta(path) };
    let mut opened = opened_overlays();
    if let Some(kept) = opened.get(path).filter(|kept| kept.id == id) {
        return Ok(kept.meta.clone());
    }
    let meta = read_meta(path)?;
    keep_opened(&mut opened, path, Opened { id, meta: meta.clone(), overlay: None });
    Ok(meta)
}

/// `open_overlay`, answered from the kept entry (the same `Arc`) while the file is the one that
/// was opened.
pub(crate) fn open_overlay_shared(path: &Path, hidden_tags: Vec<String>) -> Result<Arc<SqliteOverlay>, String> {
    let Some(id) = FileId::of(path) else { return open_overlay(path, hidden_tags).map(Arc::new) };
    let mut opened = opened_overlays();
    if let Some((tags, overlay)) = opened.get(path).filter(|kept| kept.id == id).and_then(|kept| kept.overlay.as_ref()) {
        if *tags == hidden_tags {
            return Ok(Arc::clone(overlay));
        }
    }
    let (meta, overlay) = open_with_meta(path, hidden_tags.clone())?;
    let overlay = Arc::new(overlay);
    keep_opened(&mut opened, path, Opened { id, meta, overlay: Some((hidden_tags, Arc::clone(&overlay))) });
    Ok(overlay)
}

impl SqliteOverlay {
    /// Runs one query; a database error is logged and answers nothing (the trait has no error path).
    fn rows<T>(&self, sql: &str, args: &[&dyn rusqlite::ToSql], map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>) -> Vec<T> {
        let conn = self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let result = (|| {
            let mut statement = conn.prepare_cached(sql)?;
            let rows = statement.query_map(args, map)?;
            rows.collect::<rusqlite::Result<Vec<T>>>()
        })();
        result.unwrap_or_else(|error| {
            eprintln!("aft adapters: overlay query failed ({}): {error}", self.language);
            Vec::new()
        })
    }
}

fn parse_site(record: &str) -> Option<SiteRecord> {
    serde_json::from_str(record).map_err(|error| eprintln!("aft adapters: unreadable site record in the overlay: {error}")).ok()
}

impl OverlayRead for SqliteOverlay {
    fn language(&self) -> &str {
        &self.language
    }

    fn is_analysed(&self, file: &str) -> bool {
        !self.rows("SELECT 1 FROM analysed WHERE file = ?1", &[&file], |row| row.get::<_, i64>(0)).is_empty()
    }

    fn hidden_tags(&self) -> &[String] {
        &self.hidden_tags
    }

    fn sites_in_file(&self, file: &str) -> Vec<OverlaySite> {
        self.rows("SELECT off, record FROM sites WHERE file = ?1 ORDER BY id", &[&file], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .into_iter()
        .filter_map(|(offset, record)| Some(OverlaySite { offset: offset as u64, site: parse_site(&record)? }))
        .collect()
    }

    fn sites_targeting(&self, file: &str, first_line: u32, last_line: u32) -> Vec<(OverlaySite, usize)> {
        self.rows(
            "SELECT s.off, s.record, t.idx FROM targets t JOIN sites s ON s.id = t.site_id
             WHERE t.file = ?1 AND t.line >= ?2 AND t.line <= ?3 ORDER BY t.site_id, t.idx",
            &[&file, &first_line, &last_line],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)),
        )
        .into_iter()
        .filter_map(|(offset, record, index)| Some((OverlaySite { offset: offset as u64, site: parse_site(&record)? }, index as usize)))
        .collect()
    }

    fn facts(&self, query: &FactQuery) -> Vec<FactRecord> {
        let kind = query.kind.as_str();
        let records: Vec<String> = match &query.lookup {
            FactLookup::Label { key, prefix: false } => {
                self.rows("SELECT record FROM facts WHERE kind = ?1 AND label = ?2 ORDER BY id", &[&kind, key], |row| row.get(0))
            }
            FactLookup::Label { key, prefix: true } => {
                // A range over the label index; the exact `starts_with` check follows below.
                let upper = format!("{key}\u{10FFFF}");
                let candidates: Vec<(String, String)> = self.rows(
                    "SELECT label, record FROM facts WHERE kind = ?1 AND label >= ?2 AND label <= ?3 ORDER BY id",
                    &[&kind, key, &upper],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                );
                candidates.into_iter().filter(|(label, _)| label.starts_with(key.as_str())).map(|(_, record)| record).collect()
            }
            FactLookup::Position { side, file, first_line, last_line } => {
                let subject = "(subj_file = ?2 AND subj_line >= ?3 AND subj_line <= ?4)";
                let object = "(obj_file = ?2 AND obj_line >= ?3 AND obj_line <= ?4)";
                let condition = match side {
                    FactSide::Subject => subject.to_string(),
                    FactSide::Object => object.to_string(),
                    FactSide::Either => format!("({subject} OR {object})"),
                };
                self.rows(
                    &format!("SELECT record FROM facts WHERE kind = ?1 AND {condition} ORDER BY id"),
                    &[&kind, file, first_line, last_line],
                    |row| row.get(0),
                )
            }
            FactLookup::Name { side, name } => {
                let condition = match side {
                    FactSide::Subject => "subj_name = ?2",
                    FactSide::Object => "obj_name = ?2",
                    FactSide::Either => "(subj_name = ?2 OR obj_name = ?2)",
                };
                self.rows(&format!("SELECT record FROM facts WHERE kind = ?1 AND {condition} ORDER BY id"), &[&kind, name], |row| row.get(0))
            }
        };
        records
            .iter()
            .filter_map(|record| {
                serde_json::from_str(record).map_err(|error| eprintln!("aft adapters: unreadable fact record in the overlay: {error}")).ok()
            })
            .collect()
    }

    fn counts(&self) -> OverlayCounts {
        self.counts
    }
}

#[cfg(test)]
mod tests {
    use super::super::contract::{parse_output, Pos, RegistryEntry};
    use super::super::fixture::FixtureOverlay;
    use super::*;

    fn manifest_dir() -> PathBuf {
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR for tests"))
    }

    fn entry(facts: &[&str]) -> RegistryEntry {
        let decls: Vec<serde_json::Value> =
            facts.iter().map(|kind| serde_json::json!({"kind": kind, "description": "d"})).collect();
        serde_json::from_value(serde_json::json!({
            "contract": 1, "language": "go", "root_markers": ["go.mod"], "command": ["x"],
            "hidden_tags": ["mock"], "facts": decls
        }))
        .unwrap()
    }

    fn key() -> Key {
        Key { head: "a".repeat(40), module_path: String::new(), build_id: "b".repeat(64), toolchain_id: "go1".into() }
    }

    /// Builds both overlays from the same output and returns them with the output.
    fn both(root: &Path, output_bytes: &[u8], entry: &RegistryEntry, dir: &Path) -> (FixtureOverlay, SqliteOverlay, AdapterOutput) {
        let output = parse_output(output_bytes, entry).unwrap();
        let fixture = FixtureOverlay::from_output(root, output.clone(), entry.hidden_tags.clone()).unwrap();
        let sites = sites_with_offsets(root, output.sites.clone()).unwrap();
        let meta = Meta::of_output(&output, &key(), 1);
        let path = dir.join("o.sqlite");
        write_overlay(&path, &meta, &output, &sites).unwrap();
        let sqlite = open_overlay(&path, entry.hidden_tags.clone()).unwrap();
        (fixture, sqlite, output)
    }

    fn assert_same(fixture: &FixtureOverlay, sqlite: &SqliteOverlay, output: &AdapterOutput) {
        assert_eq!(fixture.language(), sqlite.language());
        assert_eq!(fixture.hidden_tags(), sqlite.hidden_tags());
        assert_eq!(fixture.counts(), sqlite.counts());
        let files: std::collections::BTreeSet<&str> =
            output.sites.iter().flat_map(|s| std::iter::once(s.file.as_str()).chain(s.targets.iter().map(|t| t.file.as_str()))).collect();
        for file in files.iter().chain(["not/there.go", "a.go"].iter()) {
            assert_eq!(fixture.is_analysed(file), sqlite.is_analysed(file), "is_analysed {file}");
            assert_eq!(fixture.sites_in_file(file), sqlite.sites_in_file(file), "sites_in_file {file}");
            assert_eq!(fixture.sites_targeting(file, 1, u32::MAX), sqlite.sites_targeting(file, 1, u32::MAX), "targeting {file}");
        }
        // Narrow target windows, taken from real targets.
        for site in output.sites.iter().step_by(37).take(200) {
            for target in &site.targets {
                for (first, last) in [(target.line, target.line), (target.line.saturating_sub(2), target.line + 2), (target.line + 1, target.line + 9)] {
                    assert_eq!(
                        fixture.sites_targeting(&target.file, first, last),
                        sqlite.sites_targeting(&target.file, first, last),
                        "targeting {}:{first}-{last}",
                        target.file
                    );
                }
            }
        }
        for fact in &output.facts {
            let mut queries = vec![
                FactQuery { kind: fact.kind.clone(), lookup: FactLookup::Position { side: FactSide::Subject, file: fact.subject.file.clone(), first_line: fact.subject.line, last_line: fact.subject.line } },
                FactQuery { kind: fact.kind.clone(), lookup: FactLookup::Position { side: FactSide::Object, file: fact.object.file.clone(), first_line: fact.object.line.saturating_sub(1), last_line: fact.object.line + 1 } },
                FactQuery { kind: fact.kind.clone(), lookup: FactLookup::Position { side: FactSide::Either, file: fact.subject.file.clone(), first_line: 1, last_line: u32::MAX } },
            ];
            if let Some(name) = &fact.object_name {
                queries.push(FactQuery { kind: fact.kind.clone(), lookup: FactLookup::Name { side: FactSide::Object, name: name.clone() } });
                queries.push(FactQuery { kind: fact.kind.clone(), lookup: FactLookup::Name { side: FactSide::Either, name: name.clone() } });
            }
            if let Some(name) = &fact.subject_name {
                queries.push(FactQuery { kind: fact.kind.clone(), lookup: FactLookup::Name { side: FactSide::Subject, name: name.clone() } });
            }
            if let Some(label) = &fact.label {
                let half: String = label.chars().take(label.chars().count() / 2 + 1).collect();
                for (key, prefix) in [(label.clone(), false), (label.clone(), true), (half, true), (String::new(), true)] {
                    queries.push(FactQuery { kind: fact.kind.clone(), lookup: FactLookup::Label { key, prefix } });
                }
            }
            for query in queries {
                assert_eq!(fixture.facts(&query), sqlite.facts(&query), "{query:?}");
            }
        }
    }

    #[test]
    fn the_sqlite_overlay_answers_like_the_fixture_overlay_on_the_tiny_fixture() {
        let fixtures = manifest_dir().join("../../tests/adapters");
        let root = fixtures.join("fixtures/tiny-mixed");
        let output = std::fs::read(fixtures.join("fake/outputs/tiny-go.ndjson")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (fixture, sqlite, output) = both(&root, &output, &entry(&["implements"]), dir.path());
        assert_eq!(sqlite.sites_in_file("a.go").len(), 1);
        assert_eq!(sqlite.sites_targeting("a.go", 7, 7).len(), 1);
        assert_eq!(sqlite.counts().facts, 1);
        assert_same(&fixture, &sqlite, &output);
    }

    /// A large real-repository fixture (private: the consumer keeps the repository and its overlay).
    /// The test runs only when these environment variables are set (the acceptance scripts set them from
    /// the private case pack):
    ///   AFT_TEST_REAL_ROOT     a clean git clone of the repository (with go.mod)
    ///   AFT_TEST_REAL_OVERLAY  the adapter output for that clone, gzip-compressed NDJSON
    ///   AFT_TEST_REAL_SITES    the expected number of sites
    ///   AFT_TEST_REAL_CALLEE   `<file>:<line>` of a function that has at least 3 callers
    #[test]
    fn the_sqlite_overlay_answers_like_the_fixture_overlay_on_a_real_fixture() {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let (Some(root), Some(gz), Some(sites), Some(callee)) = (
            var("AFT_TEST_REAL_ROOT"),
            var("AFT_TEST_REAL_OVERLAY"),
            var("AFT_TEST_REAL_SITES"),
            var("AFT_TEST_REAL_CALLEE"),
        ) else {
            eprintln!("skipped: AFT_TEST_REAL_ROOT, _OVERLAY, _SITES and _CALLEE are not all set");
            return;
        };
        let root = std::path::PathBuf::from(root);
        if !root.join("go.mod").is_file() {
            eprintln!("skipped: no clone at {}", root.display());
            return;
        }
        let (callee_file, callee_line) = callee.rsplit_once(':').expect("AFT_TEST_REAL_CALLEE is <file>:<line>");
        let callee_line: u32 = callee_line.parse().expect("the callee line is a number");
        let mut text = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(std::fs::File::open(gz).unwrap()), &mut text).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (fixture, sqlite, output) = both(&root, &text, &entry(&[]), dir.path());
        assert_eq!(sqlite.counts().sites, sites.parse::<u64>().expect("the site count is a number"));
        let callers = sqlite.sites_targeting(callee_file, callee_line, callee_line);
        assert!(callers.len() >= 3, "{}", callers.len());
        assert_same(&fixture, &sqlite, &output);
    }

    #[test]
    fn a_fact_overlay_with_labels_and_names_answers_like_the_fixture() {
        // The fake tiny output has one fact; add more shapes here: prefix labels, absolute objects, no names.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.go"), "package p\n\nfunc F() { G() }\nfunc G() {}\n").unwrap();
        let facts = [
            ("/orders.Service/Create", Some("Create"), Pos { file: "a.go".into(), line: 3, col: 6 }, Pos { file: "a.go".into(), line: 4, col: 6 }),
            ("/orders.Service/Cancel", None, Pos { file: "a.go".into(), line: 3, col: 6 }, Pos { file: "/abs/dep.go".into(), line: 1, col: 1 }),
            ("/other", Some("G"), Pos { file: "a.go".into(), line: 4, col: 6 }, Pos { file: "a.go".into(), line: 3, col: 6 }),
        ];
        let mut text = String::from("{\"type\":\"header\",\"contract\":1,\"language\":\"go\",\"tool\":\"t\"}\n{\"type\":\"analysed\",\"files\":[\"a.go\"]}\n");
        text.push_str("{\"type\":\"site\",\"file\":\"a.go\",\"line\":3,\"col\":12,\"name\":\"G\",\"targets\":[{\"file\":\"a.go\",\"line\":4,\"col\":6,\"name\":\"G\",\"kind\":\"static\"}]}\n");
        for (label, name, subject, object) in &facts {
            let fact = serde_json::json!({"type":"fact","kind":"registers","subject":subject,"subject_name":"F","object":object,"object_name":name,"label":label,"tags":["mock"]});
            text.push_str(&format!("{fact}\n"));
        }
        text.push_str("{\"type\":\"end\",\"sites\":1,\"targets\":1,\"facts\":3,\"analysed_files\":1,\"unanalysed_files\":0}\n");
        let (fixture, sqlite, output) = both(&root, text.as_bytes(), &entry(&["registers"]), dir.path());
        assert_same(&fixture, &sqlite, &output);
        let prefix = FactQuery { kind: "registers".into(), lookup: FactLookup::Label { key: "/orders.".into(), prefix: true } };
        assert_eq!(sqlite.facts(&prefix).len(), 2);
    }

    #[test]
    fn a_site_outside_its_file_is_a_contract_violation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.go"), "package p\n").unwrap();
        let site = |line, col| SiteRecord { file: "a.go".into(), line, col, name: "f".into(), tags: vec![], targets: vec![], external: vec![] };
        let error = sites_with_offsets(dir.path(), vec![site(1, 99)]).unwrap_err();
        assert!(error.starts_with("contract violation: site a.go:1:99"), "{error}");
        let missing = SiteRecord { file: "gone.go".into(), ..site(1, 1) };
        assert!(sites_with_offsets(dir.path(), vec![missing]).unwrap_err().contains("gone.go"));
        assert_eq!(sites_with_offsets(dir.path(), vec![site(1, 3)]).unwrap()[0].offset, 2);
    }

    #[test]
    fn key_state_reads_meta_failure_and_the_head_of_another_key() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path(), Path::new("/some/repo"), "go");
        assert!(key_state(&layout, "k1").meta.is_none());
        let fixtures = manifest_dir().join("../../tests/adapters");
        let output = parse_output(&std::fs::read(fixtures.join("fake/outputs/tiny-go.ndjson")).unwrap(), &entry(&["implements"])).unwrap();
        let root = fixtures.join("fixtures/tiny-mixed");
        let sites = sites_with_offsets(&root, output.sites.clone()).unwrap();
        write_overlay(&layout.overlay_path("old"), &Meta::of_output(&output, &key(), 7), &output, &sites).unwrap();
        let state = key_state(&layout, "new");
        assert!(state.meta.is_none());
        assert_eq!(state.other_head.as_deref(), Some(key().head.as_str()));
        write_failure(&layout, "new", &FailureRecord { reason: "boom".into(), at_unix_s: 5, stderr_tail: vec!["x".into()] }).unwrap();
        assert_eq!(key_state(&layout, "new").failure.unwrap().reason, "boom");
        remove_other_keys(&layout, "new");
        assert!(!layout.overlay_path("old").exists());
        assert!(layout.failure_path("new").exists());
        // A damaged file is removed and treated as absent.
        std::fs::write(layout.overlay_path("bad"), b"not sqlite").unwrap();
        assert!(key_state(&layout, "bad").meta.is_none());
        assert!(!layout.overlay_path("bad").exists());
    }
}
