//! Root detection and the git rule for `aft cli`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Marker files of the languages that work without an adapter. The registry adds the
/// `root_markers` of every registered adapter (see `find_root`), so a new language needs
/// only its registry entry. `.git` also marks a root (a repository without a language marker).
const BUILT_IN_LANGUAGE_MARKERS: &[&str] = &["go.mod", "package.json", "Cargo.toml", "pyproject.toml"];

/// Why no root was returned. The caller maps it to an exit code.
pub(crate) enum RootError {
    /// The path does not exist, or no project root is above it (usage error, exit 2).
    Usage(String),
    /// The root exists but is not a git work tree with a HEAD commit (exit 3).
    NotSearchable(String),
}

/// The absolute, canonical form of `path` (relative paths are relative to the cwd).
pub(crate) fn absolute(path: &str) -> Result<PathBuf, RootError> {
    let joined = std::env::current_dir()
        .map_err(|err| RootError::Usage(format!("cannot read the current directory: {err}")))?
        .join(path);
    joined
        .canonicalize()
        .map_err(|err| RootError::Usage(format!("path not found: {path} ({err})")))
}

/// The project root of `target`: the nearest directory above it (or itself) that has a
/// language marker (built-in or from `registry_markers`) or `.git`. A folder that
/// holds repositories (`.git` plus `repos/`, no language marker) is never a root.
pub(crate) fn find_root(target: &Path, registry_markers: &[String]) -> Result<PathBuf, RootError> {
    let mut language_markers: Vec<&str> = BUILT_IN_LANGUAGE_MARKERS.to_vec();
    for marker in registry_markers {
        if !language_markers.contains(&marker.as_str()) {
            language_markers.push(marker);
        }
    }
    let start = if target.is_dir() { target } else { target.parent().unwrap_or(target) };
    for dir in start.ancestors() {
        let has_language_marker = language_markers.iter().any(|marker| dir.join(marker).exists());
        if !has_language_marker && !dir.join(".git").exists() {
            continue;
        }
        if !has_language_marker && dir.join("repos").is_dir() {
            return Err(RootError::Usage(format!(
                "{} holds repositories under repos/, it is not a project. Pass a path inside repos/<repo>/ (for example repos/<repo>/<file>).",
                dir.display()
            )));
        }
        return Ok(dir.to_path_buf());
    }
    Err(RootError::Usage(format!(
        "no project root above {} (looked for {}, .git)",
        start.display(),
        language_markers.join(", ")
    )))
}

/// The user rule: a root is searchable only when it is a git work tree with a HEAD commit.
/// Checked before aft starts, so a refused root costs no index work.
pub(crate) fn require_git_commit(root: &Path) -> Result<(), RootError> {
    let git = |args: &[&str]| -> bool {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
    if !git(&["rev-parse", "--is-inside-work-tree"]) {
        return Err(RootError::NotSearchable(format!(
            "refused: {} is not a git work tree. aft searches a root only when it is a git work tree with at least one commit.",
            root.display()
        )));
    }
    if !git(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]) {
        return Err(RootError::NotSearchable(format!(
            "refused: {} has no HEAD commit. aft searches a root only when it is a git work tree with at least one commit.",
            root.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository with `.git` at the top and a module of a registered language below it.
    fn repo_with_module(marker: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("module/src")).unwrap();
        std::fs::write(dir.path().join("module").join(marker), "").unwrap();
        std::fs::write(dir.path().join("module/src/a.txt"), "").unwrap();
        dir
    }

    #[test]
    fn a_registry_marker_makes_the_module_the_root() {
        let dir = repo_with_module("pom.xml");
        let file = dir.path().join("module/src/a.txt");
        let root = find_root(&file, &["pom.xml".to_string()]).ok().unwrap();
        assert_eq!(root, dir.path().join("module"));
    }

    #[test]
    fn without_the_registry_marker_the_git_top_is_the_root() {
        let dir = repo_with_module("pom.xml");
        let file = dir.path().join("module/src/a.txt");
        let root = find_root(&file, &[]).ok().unwrap();
        assert_eq!(root, dir.path().to_path_buf());
    }

    #[test]
    fn a_built_in_marker_below_the_git_top_is_the_root() {
        let dir = repo_with_module("go.mod");
        let file = dir.path().join("module/src/a.txt");
        let root = find_root(&file, &[]).ok().unwrap();
        assert_eq!(root, dir.path().join("module"));
    }
}
