//! Per-file parse-result cache.
//!
//! Tree-sitter parsing + symbol extraction + import analysis is the dominant
//! cost in `build_reverse_index`: for a 500-file Go module it's ~3 seconds on
//! a warm FS. Every `aft` CLI invocation paid this cost because the process
//! dies after answering the query.
//!
//! This module caches `FileCallData` per source file, keyed on
//! `(mtime, size)`. On each call, we `stat()` the source and compare — same
//! pair → reuse the parse; different → re-parse and overwrite.
//!
//! Sibling of `go_helper::{read_cached, write_cached}` conceptually, but
//! per-file rather than per-project because a single edit shouldn't
//! invalidate the entire module's cache.

use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::callgraph::FileCallData;

/// Format version for the on-disk JSON. Bump when `FileCallData` or its
/// nested types change in an incompatible way — stale cache entries with
/// the wrong version are ignored (treated as a miss).
pub const CACHE_VERSION: u32 = 1;

/// One cached parse result.
#[derive(Debug, Serialize, Deserialize)]
pub struct CachedFile {
    pub version: u32,
    /// Source mtime in whole seconds since the Unix epoch.
    pub mtime_secs: u64,
    /// Source size in bytes.
    pub size: u64,
    /// Parser output.
    pub data: FileCallData,
}

/// Location where a given source file's cache entry lives.
pub fn cache_path(cache_dir: &Path, source: &Path) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    let hex = format!("{:016x}", h.finish());
    cache_dir.join("parse-cache").join(format!("{hex}.json"))
}

/// Read the cached parse result for `source`, verifying mtime + size.
/// Returns `None` if the cache is missing, unreadable, stale, or has the
/// wrong schema version.
pub fn read(cache_dir: &Path, source: &Path) -> Option<FileCallData> {
    let cache_file = cache_path(cache_dir, source);
    let raw = fs::read_to_string(&cache_file).ok()?;
    let entry: CachedFile = serde_json::from_str(&raw).ok()?;
    if entry.version != CACHE_VERSION {
        return None;
    }
    let meta = fs::metadata(source).ok()?;
    let current_size = meta.len();
    let current_mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())?;
    if current_size != entry.size || current_mtime != entry.mtime_secs {
        return None; // Stale — source changed.
    }
    Some(entry.data)
}

/// Write `data` to the cache for `source`, keyed on source's current
/// mtime + size. Atomic (temp file + rename) so a crashed write doesn't
/// corrupt an existing entry.
pub fn write(cache_dir: &Path, source: &Path, data: &FileCallData) -> io::Result<()> {
    let meta = fs::metadata(source)?;
    let size = meta.len();
    let mtime_secs = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
        .as_secs();

    let entry = CachedFile {
        version: CACHE_VERSION,
        mtime_secs,
        size,
        data: data.clone(),
    };
    let body =
        serde_json::to_string(&entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let cache_file = cache_path(cache_dir, source);
    if let Some(parent) = cache_file.parent() {
        fs::create_dir_all(parent)?;
    }
    // Atomic write: staging path in the same dir, then rename.
    let staging = cache_file.with_extension("json.tmp");
    fs::write(&staging, body)?;
    fs::rename(&staging, &cache_file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callgraph::{CallSite, FileCallData, SymbolMeta};
    use crate::imports::ImportBlock;
    use crate::parser::LangId;
    use crate::symbols::SymbolKind;
    use std::collections::HashMap;

    fn sample_data() -> FileCallData {
        let mut calls_by_symbol = HashMap::new();
        calls_by_symbol.insert(
            "foo".to_string(),
            vec![CallSite {
                callee_name: "bar".to_string(),
                full_callee: "bar".to_string(),
                line: 42,
                byte_start: 100,
                byte_end: 105,
            }],
        );
        let mut symbol_metadata = HashMap::new();
        symbol_metadata.insert(
            "foo".to_string(),
            SymbolMeta {
                kind: SymbolKind::Function,
                exported: true,
                signature: Some("func foo()".to_string()),
            },
        );
        FileCallData {
            calls_by_symbol,
            exported_symbols: vec!["foo".to_string()],
            symbol_metadata,
            import_block: ImportBlock::empty(),
            lang: LangId::Go,
        }
    }

    #[test]
    fn round_trip_through_cache() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.go");
        fs::write(&source, "package foo\nfunc foo() { bar() }\n").unwrap();
        let cache_dir = dir.path().join("cache");

        let data = sample_data();
        write(&cache_dir, &source, &data).unwrap();
        let back = read(&cache_dir, &source).unwrap();
        assert_eq!(back.exported_symbols, data.exported_symbols);
        assert_eq!(back.lang, data.lang);
    }

    #[test]
    fn missing_cache_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.go");
        fs::write(&source, "package foo\n").unwrap();
        assert!(read(dir.path(), &source).is_none());
    }

    #[test]
    fn stale_cache_returns_none_after_source_edit() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.go");
        fs::write(&source, "package foo\nfunc foo() {}\n").unwrap();
        let cache_dir = dir.path().join("cache");
        write(&cache_dir, &source, &sample_data()).unwrap();
        assert!(read(&cache_dir, &source).is_some());

        // Simulate an edit that changes both size and mtime.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&source, "package foo\nfunc foo() { return }\n").unwrap();
        assert!(read(&cache_dir, &source).is_none());
    }

    #[test]
    fn wrong_version_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.go");
        fs::write(&source, "package foo\n").unwrap();
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(cache_dir.join("parse-cache")).unwrap();
        let path = cache_path(&cache_dir, &source);
        fs::write(
            path,
            format!(
                r#"{{"version": {}, "mtime_secs": 0, "size": 0, "data": {{}}}}"#,
                CACHE_VERSION + 99
            ),
        )
        .unwrap();
        assert!(read(&cache_dir, &source).is_none());
    }
}
