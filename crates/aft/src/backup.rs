use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::AftError;

const MAX_UNDO_DEPTH: usize = 20;

/// A single backup entry for a file.
#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub backup_id: String,
    pub content: String,
    pub timestamp: u64,
    pub description: String,
}

/// Per-file undo store with optional disk persistence.
///
/// When `storage_dir` is set, backups persist to disk so undo history
/// survives bridge and OpenCode restarts. Disk layout:
///   `<storage_dir>/backups/<path_hash>/meta.json` — file path + count
///   `<storage_dir>/backups/<path_hash>/0.bak` ... `19.bak` — content snapshots
#[derive(Debug)]
pub struct BackupStore {
    entries: HashMap<PathBuf, Vec<BackupEntry>>,
    counter: AtomicU64,
    storage_dir: Option<PathBuf>,
    disk_index: HashMap<PathBuf, DiskMeta>,
}

#[derive(Debug, Clone)]
struct DiskMeta {
    dir: PathBuf,
    count: usize,
}

impl BackupStore {
    pub fn new() -> Self {
        BackupStore {
            entries: HashMap::new(),
            counter: AtomicU64::new(0),
            storage_dir: None,
            disk_index: HashMap::new(),
        }
    }

    /// Set storage directory for disk persistence (called during configure).
    pub fn set_storage_dir(&mut self, dir: PathBuf) {
        self.storage_dir = Some(dir);
        self.load_disk_index();
    }

    /// Snapshot the current contents of `path` with a description.
    pub fn snapshot(&mut self, path: &Path, description: &str) -> Result<String, AftError> {
        let content = std::fs::read_to_string(path).map_err(|_| AftError::FileNotFound {
            path: path.display().to_string(),
        })?;

        let key = canonicalize_key(path);
        let id = self.next_id();
        let entry = BackupEntry {
            backup_id: id.clone(),
            content,
            timestamp: current_timestamp(),
            description: description.to_string(),
        };

        let stack = self.entries.entry(key.clone()).or_default();
        if stack.len() >= MAX_UNDO_DEPTH {
            stack.remove(0);
        }
        stack.push(entry);

        // Persist to disk
        let stack_clone = stack.clone();
        self.write_snapshot_to_disk(&key, &stack_clone);

        Ok(id)
    }

    /// Pop the most recent backup for `path` and restore the file contents.
    /// Returns `(entry, optional_warning)`.
    pub fn restore_latest(
        &mut self,
        path: &Path,
    ) -> Result<(BackupEntry, Option<String>), AftError> {
        let key = canonicalize_key(path);

        // Try memory first
        if self.entries.get(&key).map_or(false, |s| !s.is_empty()) {
            return self.do_restore(&key, path);
        }

        // Try disk fallback
        if self.load_from_disk_if_needed(&key) {
            // Check for external modification
            let warning = self.check_external_modification(&key, path);
            let (entry, _) = self.do_restore(&key, path)?;
            return Ok((entry, warning));
        }

        Err(AftError::NoUndoHistory {
            path: path.display().to_string(),
        })
    }

    /// Return the backup history for a file (oldest first).
    pub fn history(&self, path: &Path) -> Vec<BackupEntry> {
        let key = canonicalize_key(path);
        self.entries.get(&key).cloned().unwrap_or_default()
    }

    /// Return the number of on-disk backup entries for a file.
    pub fn disk_history_count(&self, path: &Path) -> usize {
        let key = canonicalize_key(path);
        self.disk_index.get(&key).map(|m| m.count).unwrap_or(0)
    }

    /// Return all files that have at least one backup entry (memory + disk).
    pub fn tracked_files(&self) -> Vec<PathBuf> {
        let mut files: std::collections::HashSet<PathBuf> = self.entries.keys().cloned().collect();
        for key in self.disk_index.keys() {
            files.insert(key.clone());
        }
        files.into_iter().collect()
    }

    fn next_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("backup-{}", n)
    }

    // ---- Internal helpers ----

    fn do_restore(
        &mut self,
        key: &Path,
        path: &Path,
    ) -> Result<(BackupEntry, Option<String>), AftError> {
        let stack = self
            .entries
            .get_mut(key)
            .ok_or_else(|| AftError::NoUndoHistory {
                path: path.display().to_string(),
            })?;

        let entry = stack
            .last()
            .cloned()
            .ok_or_else(|| AftError::NoUndoHistory {
                path: path.display().to_string(),
            })?;

        std::fs::write(path, &entry.content).map_err(|e| AftError::IoError {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;

        stack.pop();
        if stack.is_empty() {
            self.entries.remove(key);
            self.remove_disk_backups(key);
        } else {
            let stack_clone = self.entries.get(key).cloned().unwrap_or_default();
            self.write_snapshot_to_disk(key, &stack_clone);
        }

        Ok((entry, None))
    }

    fn check_external_modification(&self, key: &Path, path: &Path) -> Option<String> {
        if let (Some(stack), Ok(current)) = (self.entries.get(key), std::fs::read_to_string(path)) {
            if let Some(latest) = stack.last() {
                if latest.content != current {
                    return Some("file was modified externally since last backup".to_string());
                }
            }
        }
        None
    }

    // ---- Disk persistence ----

    fn backups_dir(&self) -> Option<PathBuf> {
        self.storage_dir.as_ref().map(|d| d.join("backups"))
    }

    fn path_hash(key: &Path) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    fn load_disk_index(&mut self) {
        let backups_dir = match self.backups_dir() {
            Some(d) if d.exists() => d,
            _ => return,
        };
        let entries = match std::fs::read_dir(&backups_dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let meta_path = entry.path().join("meta.json");
            if let Ok(content) = std::fs::read_to_string(&meta_path) {
                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let (Some(path_str), Some(count)) = (
                        meta.get("path").and_then(|v| v.as_str()),
                        meta.get("count").and_then(|v| v.as_u64()),
                    ) {
                        self.disk_index.insert(
                            PathBuf::from(path_str),
                            DiskMeta {
                                dir: entry.path(),
                                count: count as usize,
                            },
                        );
                    }
                }
            }
        }
        if !self.disk_index.is_empty() {
            log::info!(
                "[aft] loaded {} backup entries from disk",
                self.disk_index.len()
            );
        }
    }

    fn load_from_disk_if_needed(&mut self, key: &Path) -> bool {
        let meta = match self.disk_index.get(key) {
            Some(m) if m.count > 0 => m.clone(),
            _ => return false,
        };

        let mut entries = Vec::new();
        for i in 0..meta.count {
            let bak_path = meta.dir.join(format!("{}.bak", i));
            if let Ok(content) = std::fs::read_to_string(&bak_path) {
                entries.push(BackupEntry {
                    backup_id: format!("disk-{}", i),
                    content,
                    timestamp: 0,
                    description: "restored from disk".to_string(),
                });
            }
        }

        if entries.is_empty() {
            return false;
        }

        self.entries.insert(key.to_path_buf(), entries);
        true
    }

    fn write_snapshot_to_disk(&self, key: &Path, stack: &[BackupEntry]) {
        let backups_dir = match self.backups_dir() {
            Some(d) => d,
            None => return,
        };

        let hash = Self::path_hash(key);
        let dir = backups_dir.join(&hash);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("[aft] failed to create backup dir: {}", e);
            return;
        }

        for (i, entry) in stack.iter().enumerate() {
            let bak_path = dir.join(format!("{}.bak", i));
            let tmp_path = dir.join(format!("{}.bak.tmp", i));
            if std::fs::write(&tmp_path, &entry.content).is_ok() {
                let _ = std::fs::rename(&tmp_path, &bak_path);
            }
        }

        // Clean up extra .bak files if stack shrank
        for i in stack.len()..MAX_UNDO_DEPTH {
            let old = dir.join(format!("{}.bak", i));
            if old.exists() {
                let _ = std::fs::remove_file(&old);
            }
        }

        let meta = serde_json::json!({
            "path": key.display().to_string(),
            "count": stack.len(),
        });
        let meta_path = dir.join("meta.json");
        let meta_tmp = dir.join("meta.json.tmp");
        if let Ok(content) = serde_json::to_string_pretty(&meta) {
            if std::fs::write(&meta_tmp, &content).is_ok() {
                let _ = std::fs::rename(&meta_tmp, &meta_path);
            }
        }
    }

    fn remove_disk_backups(&mut self, key: &Path) {
        if let Some(meta) = self.disk_index.remove(key) {
            let _ = std::fs::remove_dir_all(&meta.dir);
        } else if let Some(backups_dir) = self.backups_dir() {
            let hash = Self::path_hash(key);
            let dir = backups_dir.join(&hash);
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
}

fn canonicalize_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("aft_backup_tests");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn snapshot_and_restore_round_trip() {
        let path = temp_file("round_trip.txt", "original");
        let mut store = BackupStore::new();

        let id = store.snapshot(&path, "before edit").unwrap();
        assert!(id.starts_with("backup-"));

        fs::write(&path, "modified").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "modified");

        let (entry, _) = store.restore_latest(&path).unwrap();
        assert_eq!(entry.content, "original");
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    }

    #[test]
    fn multiple_snapshots_preserve_order() {
        let path = temp_file("order.txt", "v1");
        let mut store = BackupStore::new();

        store.snapshot(&path, "first").unwrap();
        fs::write(&path, "v2").unwrap();
        store.snapshot(&path, "second").unwrap();
        fs::write(&path, "v3").unwrap();
        store.snapshot(&path, "third").unwrap();

        let history = store.history(&path);
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].content, "v1");
        assert_eq!(history[1].content, "v2");
        assert_eq!(history[2].content, "v3");
    }

    #[test]
    fn restore_pops_from_stack() {
        let path = temp_file("pop.txt", "v1");
        let mut store = BackupStore::new();

        store.snapshot(&path, "first").unwrap();
        fs::write(&path, "v2").unwrap();
        store.snapshot(&path, "second").unwrap();

        let (entry, _) = store.restore_latest(&path).unwrap();
        assert_eq!(entry.description, "second");
        assert_eq!(entry.content, "v2");

        let history = store.history(&path);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn empty_history_returns_empty_vec() {
        let store = BackupStore::new();
        let path = Path::new("/tmp/aft_backup_tests/nonexistent_history.txt");
        assert!(store.history(path).is_empty());
    }

    #[test]
    fn snapshot_nonexistent_file_returns_error() {
        let mut store = BackupStore::new();
        let path = Path::new("/tmp/aft_backup_tests/absolutely_does_not_exist.txt");
        assert!(store.snapshot(path, "test").is_err());
    }

    #[test]
    fn tracked_files_lists_snapshotted_paths() {
        let path1 = temp_file("tracked1.txt", "a");
        let path2 = temp_file("tracked2.txt", "b");
        let mut store = BackupStore::new();

        store.snapshot(&path1, "snap1").unwrap();
        store.snapshot(&path2, "snap2").unwrap();
        assert_eq!(store.tracked_files().len(), 2);
    }

    #[test]
    fn disk_persistence_survives_reload() {
        let dir = std::env::temp_dir().join("aft_backup_disk_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let file_path = temp_file("disk_persist.txt", "original");

        // Create store with storage, snapshot, then drop
        {
            let mut store = BackupStore::new();
            store.set_storage_dir(dir.clone());
            store.snapshot(&file_path, "before edit").unwrap();
        }

        // Modify the file externally
        fs::write(&file_path, "externally modified").unwrap();

        // Create new store, load from disk, restore
        let mut store2 = BackupStore::new();
        store2.set_storage_dir(dir.clone());

        let (entry, warning) = store2.restore_latest(&file_path).unwrap();
        assert_eq!(entry.content, "original");
        assert!(warning.is_some()); // file was modified externally
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "original");

        let _ = fs::remove_dir_all(&dir);
    }
}
