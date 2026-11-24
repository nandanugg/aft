//! Registry loading (owner: R1).
//!
//! The registry names the adapter programs aft may run, so it is trusted input.
//! It is read from exactly two places (CONTRACT.md section 2):
//! 1. `$AFT_ADAPTERS_REGISTRY`, when it is set and not empty; else
//! 2. `adapters-registry.json` in the directory of the running aft executable
//!    (symlinks resolved).
//!
//! It is never read from a project root: a cloned repository must not be able to
//! name a program for aft to run. A missing file means no adapters; it is not an
//! error. A file that exists but is broken is an error the caller must show.

use std::path::{Path, PathBuf};

use super::contract::{Registry, RegistryEntry};

pub(crate) const REGISTRY_ENV: &str = "AFT_ADAPTERS_REGISTRY";
pub(crate) const REGISTRY_FILE: &str = "adapters-registry.json";

/// The registry as loaded, with the place it was looked for.
#[derive(Debug, Clone)]
pub(crate) struct Loaded {
    /// The file that was consulted (it may not exist). `None` when aft cannot
    /// find its own executable and no env path is set.
    pub path: Option<PathBuf>,
    pub registry: Registry,
}

/// Where the registry is read from, by the two rules above.
pub(crate) fn location() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(REGISTRY_ENV).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    Some(exe.parent()?.join(REGISTRY_FILE))
}

/// Loads the registry from its location. `Err` carries a message that names the file.
pub(crate) fn load() -> Result<Loaded, String> {
    let path = location();
    let registry = match &path {
        Some(path) => load_from(path)?,
        None => empty(),
    };
    Ok(Loaded { path, registry })
}

/// Loads one registry file. A missing file is an empty registry.
pub(crate) fn load_from(path: &Path) -> Result<Registry, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(empty()),
        Err(error) => return Err(format!("cannot read the adapter registry {}: {error}", path.display())),
    };
    let registry: Registry = serde_json::from_slice(&bytes)
        .map_err(|error| format!("the adapter registry {} is not valid: {error}", path.display()))?;
    for entry in &registry.adapters {
        check_entry(entry).map_err(|problem| format!("the adapter registry {}: entry {:?}: {problem}", path.display(), entry.language))?;
    }
    Ok(registry)
}

fn empty() -> Registry {
    Registry { adapters: Vec::new() }
}

fn check_entry(entry: &RegistryEntry) -> Result<(), String> {
    if entry.language.is_empty() {
        return Err("language is empty".into());
    }
    if entry.command.is_empty() || entry.command[0].is_empty() {
        return Err("command is empty".into());
    }
    Ok(())
}

/// The entries whose `root_markers` exist at the top of `root`, in registry order.
pub(crate) fn entries_for_root<'a>(registry: &'a Registry, root: &Path) -> Vec<&'a RegistryEntry> {
    registry
        .adapters
        .iter()
        .filter(|entry| entry.root_markers.iter().any(|marker| root.join(marker).exists()))
        .collect()
}

/// The entry for `language` at `root`: the first one of that language whose
/// marker is present. `Err` says why there is none.
pub(crate) fn entry_for_root<'a>(registry: &'a Registry, root: &Path, language: &str) -> Result<&'a RegistryEntry, String> {
    let mut of_language = registry.adapters.iter().filter(|entry| entry.language == language).peekable();
    if of_language.peek().is_none() {
        return Err(format!("no registry entry for language {language}"));
    }
    let mut markers: Vec<&str> = Vec::new();
    for entry in of_language {
        if entry.root_markers.iter().any(|marker| root.join(marker).exists()) {
            return Ok(entry);
        }
        markers.extend(entry.root_markers.iter().map(String::as_str));
    }
    Err(format!("root marker absent (needs one of: {})", markers.join(", ")))
}

/// The languages with an entry for `root`, each once, in registry order.
pub(crate) fn languages_at_root(registry: &Registry, root: &Path) -> Vec<String> {
    let mut languages: Vec<String> = Vec::new();
    for entry in entries_for_root(registry, root) {
        if !languages.contains(&entry.language) {
            languages.push(entry.language.clone());
        }
    }
    languages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    const GO: &str = r#"{"adapters":[{"contract":1,"language":"go","root_markers":["go.mod"],"command":["/bin/echo","{root}"]}]}"#;

    #[test]
    fn a_missing_file_is_an_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let registry = load_from(&dir.path().join("none.json")).unwrap();
        assert!(registry.adapters.is_empty());
    }

    #[test]
    fn an_unknown_field_is_an_error_that_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let bad = GO.replace("\"command\"", "\"comand\"");
        let path = write(dir.path(), "r.json", &bad);
        let error = load_from(&path).unwrap_err();
        assert!(error.contains("r.json") && error.contains("comand"), "{error}");
        let path = write(dir.path(), "r2.json", &GO.replace("\"go.mod\"]", "\"go.mod\"],\"colour\":1"));
        assert!(load_from(&path).is_err());
    }

    #[test]
    fn an_empty_command_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "r.json", &GO.replace(r#"["/bin/echo","{root}"]"#, "[]"));
        assert!(load_from(&path).unwrap_err().contains("command is empty"));
    }

    #[test]
    fn entries_follow_the_root_markers() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "r.json", GO);
        let registry = load_from(&path).unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        assert!(entries_for_root(&registry, &root).is_empty());
        assert!(entry_for_root(&registry, &root, "go").unwrap_err().contains("go.mod"));
        assert!(entry_for_root(&registry, &root, "rust").unwrap_err().contains("no registry entry"));
        std::fs::write(root.join("go.mod"), "module x\n").unwrap();
        assert_eq!(languages_at_root(&registry, &root), vec!["go".to_string()]);
        assert_eq!(entry_for_root(&registry, &root, "go").unwrap().language, "go");
    }
}
