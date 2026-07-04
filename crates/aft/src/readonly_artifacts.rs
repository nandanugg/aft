use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

// These openers borrow cache artifacts that may be owned by a different AFT
// session. They therefore only read and verify the opened snapshot; any repair,
// migration, deletion, or rebuild must be left to the session that owns writes
// for that project.
use crate::cache_freshness::{FileFreshness, FreshnessVerdict};
use crate::search_index::{
    artifact_cache_key, build_path_filters, resolve_cache_dir, walk_project_files,
    walk_project_files_bounded_matching, SearchIndex,
};
use crate::semantic_index::{is_semantic_indexed_extension, SemanticIndex};

#[derive(Debug)]
pub(crate) enum ReadOnlyArtifact<T> {
    Fresh(T),
    Stale(ReadOnlyStale),
    Absent,
}

#[derive(Debug, Clone)]
pub(crate) struct ReadOnlyStale {
    pub drift_count: usize,
    pub reason: String,
}

pub(crate) fn resolve_git_root_from_user_path(
    project_root: &Path,
    raw_path: &str,
) -> Result<PathBuf, String> {
    let expanded = expand_tilde(raw_path);
    let requested = if expanded.is_absolute() {
        expanded
    } else {
        project_root.join(expanded)
    };
    let existing = nearest_existing_parent(&requested).ok_or_else(|| {
        format!(
            "path has no existing parent from which to resolve a git root: {}",
            requested.display()
        )
    })?;
    let git_base = if existing.is_file() {
        existing.parent().unwrap_or(&existing).to_path_buf()
    } else {
        existing
    };
    git_toplevel(&git_base)
}

pub(crate) fn open_search_index_read_only(
    project_root: &Path,
    storage_dir: Option<&Path>,
) -> ReadOnlyArtifact<SearchIndex> {
    let cache_dir = resolve_cache_dir(project_root, storage_dir);
    if !cache_dir.join("cache.bin").is_file() {
        return ReadOnlyArtifact::Absent;
    }

    let Some(mut index) = SearchIndex::read_from_disk_strict_read_only(&cache_dir, project_root)
    else {
        return ReadOnlyArtifact::Absent;
    };
    match search_drift_count(&index, project_root) {
        0 => {
            index.set_ready(true);
            ReadOnlyArtifact::Fresh(index)
        }
        drift_count => ReadOnlyArtifact::Stale(stale(drift_count)),
    }
}

pub(crate) fn open_semantic_index_read_only(
    project_root: &Path,
    storage_dir: Option<&Path>,
) -> ReadOnlyArtifact<SemanticIndex> {
    let Some(storage_dir) = storage_dir else {
        return ReadOnlyArtifact::Absent;
    };
    let project_key = artifact_cache_key(project_root);
    let data_path = storage_dir
        .join("semantic")
        .join(&project_key)
        .join("semantic.bin");
    if !data_path.is_file() {
        return ReadOnlyArtifact::Absent;
    }

    let Some(index) =
        SemanticIndex::read_from_disk(storage_dir, &project_key, project_root, true, None)
    else {
        return ReadOnlyArtifact::Absent;
    };

    match semantic_drift_count(&index, project_root) {
        0 => ReadOnlyArtifact::Fresh(index),
        drift_count => ReadOnlyArtifact::Stale(stale(drift_count)),
    }
}

fn stale(drift_count: usize) -> ReadOnlyStale {
    ReadOnlyStale {
        drift_count,
        reason: format!("files changed since index build ({drift_count} drifted file(s))"),
    }
}

fn search_drift_count(index: &SearchIndex, project_root: &Path) -> usize {
    let filters = crate::search_index::PathFilters::default();
    let current_files = walk_project_files(project_root, &filters);
    let current_file_set: HashSet<PathBuf> = current_files.iter().cloned().collect();
    let mut drift_count = 0usize;

    for entry in index.files.iter() {
        if entry.path.as_os_str().is_empty() {
            continue;
        }
        if !current_file_set.contains(&entry.path) {
            drift_count += 1;
            continue;
        }
        let cached = FileFreshness {
            mtime: entry.modified,
            size: entry.size,
            content_hash: entry.content_hash,
        };
        if crate::cache_freshness::verify_file_strict(&entry.path, &cached)
            != FreshnessVerdict::HotFresh
        {
            drift_count += 1;
        }
    }

    drift_count
        + current_files
            .into_iter()
            .filter(|path| !index.path_to_id.contains_key(path))
            .count()
}

fn semantic_drift_count(index: &SemanticIndex, project_root: &Path) -> usize {
    let filters = build_path_filters(&[], &[]).unwrap_or_default();
    let current_files = walk_project_files_bounded_matching(
        project_root,
        &filters,
        usize::MAX,
        is_semantic_indexed_extension,
    )
    .unwrap_or_default();
    let current_file_set: HashSet<PathBuf> = current_files.iter().cloned().collect();
    let indexed_file_set = index.indexed_file_paths();
    let mut drift_count = 0usize;

    for (path, mtime, size, content_hash) in index.indexed_file_metadata() {
        if !current_file_set.contains(&path) {
            drift_count += 1;
            continue;
        }
        let cached = FileFreshness {
            mtime,
            size,
            content_hash,
        };
        if crate::cache_freshness::verify_file_strict(&path, &cached) != FreshnessVerdict::HotFresh
        {
            drift_count += 1;
        }
    }

    drift_count
        + current_files
            .into_iter()
            .filter(|path| !indexed_file_set.contains(path))
            .count()
}

fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn nearest_existing_parent(path: &Path) -> Option<PathBuf> {
    let mut current = path.to_path_buf();
    loop {
        if current.exists() {
            return std::fs::canonicalize(&current).ok().or(Some(current));
        }
        if !current.pop() {
            return None;
        }
    }
}

fn git_toplevel(base_dir: &Path) -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(base_dir)
        .output()
        .map_err(|error| format!("failed to run git: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not a git repository") {
            return Err("not_a_git_root".to_string());
        }
        return Err(format!("git rev-parse failed: {}", stderr.trim()));
    }

    let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if toplevel.is_empty() {
        return Err("git rev-parse returned an empty toplevel".to_string());
    }
    let toplevel = PathBuf::from(toplevel);
    Ok(std::fs::canonicalize(&toplevel).unwrap_or(toplevel))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::fs;
    use std::time::SystemTime;

    use tempfile::TempDir;

    use crate::semantic_index::{SemanticIndex, SemanticIndexFingerprint};

    #[derive(Debug, PartialEq, Eq)]
    struct FileSnapshot {
        modified: SystemTime,
        hash: blake3::Hash,
    }

    fn init_git(root: &Path) {
        let status = Command::new("git")
            .args(["init"])
            .current_dir(root)
            .status()
            .expect("run git init");
        assert!(status.success(), "git init failed");
        let status = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(root)
            .status()
            .expect("configure git email");
        assert!(status.success(), "git config email failed");
        let status = Command::new("git")
            .args(["config", "user.name", "AFT Test"])
            .current_dir(root)
            .status()
            .expect("configure git name");
        assert!(status.success(), "git config name failed");
    }

    fn commit_all(root: &Path) {
        let status = Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .expect("git add");
        assert!(status.success(), "git add failed");
        let status = Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(root)
            .status()
            .expect("git commit");
        assert!(status.success(), "git commit failed");
    }

    fn fixture_project() -> (TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("create project");
        init_git(temp.path());
        let source = temp.path().join("src/lib.rs");
        fs::create_dir_all(source.parent().expect("source parent")).expect("create src");
        fs::write(&source, "pub fn readonly_needle() -> bool { true }\n").expect("write source");
        commit_all(temp.path());
        let root = fs::canonicalize(temp.path()).expect("canonical project root");
        (temp, root)
    }

    fn snapshot_dir(root: &Path) -> BTreeMap<PathBuf, FileSnapshot> {
        fn visit(dir: &Path, out: &mut BTreeMap<PathBuf, FileSnapshot>, base: &Path) {
            for entry in fs::read_dir(dir).expect("read dir") {
                let entry = entry.expect("dir entry");
                let path = entry.path();
                let meta = entry.metadata().expect("entry metadata");
                if meta.is_dir() {
                    visit(&path, out, base);
                } else if meta.is_file() {
                    let bytes = fs::read(&path).expect("read snapshot file");
                    out.insert(
                        path.strip_prefix(base)
                            .expect("relative snapshot path")
                            .to_path_buf(),
                        FileSnapshot {
                            modified: meta.modified().expect("snapshot mtime"),
                            hash: blake3::hash(&bytes),
                        },
                    );
                }
            }
        }

        let mut out = BTreeMap::new();
        if root.exists() {
            visit(root, &mut out, root);
        }
        out
    }

    fn build_search_artifact(root: &Path, storage: &Path) -> PathBuf {
        let cache_dir = resolve_cache_dir(root, Some(storage));
        let mut index = SearchIndex::build(root);
        index.write_to_disk(
            &cache_dir,
            crate::search_index::current_git_head(root).as_deref(),
        );
        cache_dir
    }

    fn build_semantic_artifact(root: &Path, storage: &Path) {
        let source = root.join("src/lib.rs");
        let fingerprint = SemanticIndexFingerprint {
            backend: "openai_compatible".to_string(),
            model: "readonly-test".to_string(),
            base_url: "http://127.0.0.1".to_string(),
            dimension: 3,
            chunking_version: 1,
        };
        let mut embed =
            |texts: Vec<String>| Ok::<_, String>(vec![vec![0.1, 0.2, 0.3]; texts.len()]);
        let mut index =
            SemanticIndex::build(root, &[source], &mut embed, 8).expect("build semantic index");
        index.set_fingerprint(fingerprint);
        index.write_to_disk(storage, &artifact_cache_key(root));
    }

    #[test]
    fn search_opener_reports_fresh_stale_absent() {
        let (_project, root) = fixture_project();
        let storage = tempfile::tempdir().expect("storage");

        assert!(matches!(
            open_search_index_read_only(&root, Some(storage.path())),
            ReadOnlyArtifact::Absent
        ));

        build_search_artifact(&root, storage.path());
        assert!(matches!(
            open_search_index_read_only(&root, Some(storage.path())),
            ReadOnlyArtifact::Fresh(_)
        ));

        fs::write(
            root.join("src/lib.rs"),
            "pub fn readonly_needle() -> bool { false }\n",
        )
        .expect("mutate fixture");
        match open_search_index_read_only(&root, Some(storage.path())) {
            ReadOnlyArtifact::Stale(stale) => {
                assert!(stale.drift_count >= 1);
                assert!(stale.reason.contains("files changed since index build"));
            }
            other => panic!("expected stale artifact, got {other:?}"),
        }
    }

    #[test]
    fn read_only_openers_never_modify_artifact_directory() {
        let (_project, root) = fixture_project();
        let storage = tempfile::tempdir().expect("storage");
        let search_cache_dir = build_search_artifact(&root, storage.path());
        build_semantic_artifact(&root, storage.path());
        let semantic_cache_dir = storage
            .path()
            .join("semantic")
            .join(artifact_cache_key(&root));

        let before = snapshot_dir(storage.path());
        assert!(matches!(
            open_search_index_read_only(&root, Some(storage.path())),
            ReadOnlyArtifact::Fresh(_)
        ));
        assert!(matches!(
            open_semantic_index_read_only(&root, Some(storage.path())),
            ReadOnlyArtifact::Fresh(_)
        ));
        assert_eq!(snapshot_dir(storage.path()), before);

        fs::write(
            root.join("src/lib.rs"),
            "pub fn readonly_needle() -> bool { false }\n",
        )
        .expect("mutate fixture");
        let stale_before = snapshot_dir(storage.path());
        assert!(matches!(
            open_search_index_read_only(&root, Some(storage.path())),
            ReadOnlyArtifact::Stale(_)
        ));
        assert!(matches!(
            open_semantic_index_read_only(&root, Some(storage.path())),
            ReadOnlyArtifact::Stale(_)
        ));
        assert_eq!(snapshot_dir(storage.path()), stale_before);
        assert!(search_cache_dir.join("cache.bin").is_file());
        assert!(semantic_cache_dir.join("semantic.bin").is_file());
    }
}
