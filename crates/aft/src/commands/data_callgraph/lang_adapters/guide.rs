//! `aft guide` (owner: W1): prints `docs/adapters/GUIDE.md` (embedded with
//! `include_str!`), then the consumer addendum (`guide.local.md` in the directory of the registry file,
//! when it exists; `install.sh --guide-local` puts it there), then the registered languages and fact
//! kinds from the registry.

use std::path::{Path, PathBuf};

use super::contract::AliasBy;
use super::registry;

/// The consumer addendum: a file next to the registry file.
const LOCAL_ADDENDUM_FILE: &str = "guide.local.md";

/// The guide text, embedded at build time.
const GUIDE: &str = include_str!("../../../../../../docs/adapters/GUIDE.md");

/// Entry point for `aft guide`; returns the exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("usage: aft guide   (or: aft cli guide)");
        return 2;
    }
    print!("{}", render());
    0
}

/// GUIDE.md followed by what the registry declares right now.
pub(crate) fn render() -> String {
    let mut out = String::from(GUIDE);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if let Some(addendum) = registry::location().as_deref().and_then(local_addendum) {
        out.push('\n');
        out.push_str(&addendum);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str("\n## Registered languages and fact kinds (from the registry now)\n\n");
    match registry::location() {
        Some(path) => out.push_str(&format!("Registry file: {}\n\n", path.display())),
        None => out.push_str("Registry file: (cannot locate the aft executable)\n\n"),
    }
    let registry = match registry::load() {
        Ok(loaded) => loaded.registry,
        Err(message) => {
            out.push_str(&format!("{message}\n"));
            return out;
        }
    };
    if registry.adapters.is_empty() {
        out.push_str("No adapter is registered. Only the built-in tree-sitter graph answers.\n");
        return out;
    }
    for entry in &registry.adapters {
        out.push_str(&format!(
            "- language {} (contract {}, root markers: {})\n",
            entry.language,
            entry.contract,
            entry.root_markers.join(", ")
        ));
        if entry.facts.is_empty() {
            out.push_str("  - no fact kinds\n");
        }
        for fact in &entry.facts {
            out.push_str(&format!("  - fact kind {}: {}\n", fact.kind, fact.description));
            for alias in &fact.aliases {
                out.push_str(&format!("    - alias {}: {}\n", alias.name, alias_usage(&fact.kind, &alias.name, alias.by)));
            }
        }
    }
    out
}

/// Path of the addendum for a registry file: `guide.local.md` in the same directory.
fn local_addendum_path(registry_file: &Path) -> Option<PathBuf> {
    Some(registry_file.parent()?.join(LOCAL_ADDENDUM_FILE))
}

/// The text of the addendum, or `None` when the file does not exist or cannot be read as text.
fn local_addendum(registry_file: &Path) -> Option<String> {
    std::fs::read_to_string(local_addendum_path(registry_file)?).ok()
}

/// How to call one alias, in words an agent can copy.
pub(crate) fn alias_usage(kind: &str, name: &str, by: AliasBy) -> String {
    match by {
        AliasBy::Label => format!(
            "aft {name} <key> [--prefix] --root <repo>   (= aft facts {kind} --label <key>; without --root, the root of the current directory)"
        ),
        AliasBy::Subject => format!("aft {name} <file> <symbol>   (= aft facts {kind} <file> <symbol> --by subject)"),
        AliasBy::Object => format!("aft {name} <file> <symbol>   (= aft facts {kind} <file> <symbol> --by object)"),
        AliasBy::Either => format!("aft {name} <file> <symbol>   (= aft facts {kind} <file> <symbol> --by either)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addendum_is_read_from_the_directory_of_the_registry_file() {
        let dir = tempfile::tempdir().unwrap();
        let registry_file = dir.path().join("adapters-registry.json");
        assert_eq!(local_addendum(&registry_file), None, "no file, no addendum");
        std::fs::write(dir.path().join(LOCAL_ADDENDUM_FILE), "## Consumer notes\n\nRun from the project root.\n").unwrap();
        assert_eq!(local_addendum(&registry_file).as_deref(), Some("## Consumer notes\n\nRun from the project root.\n"));
    }

    #[test]
    fn addendum_path_sits_next_to_the_registry_file() {
        let path = local_addendum_path(Path::new("/opt/aft/adapters-registry.json")).unwrap();
        assert_eq!(path, Path::new("/opt/aft/guide.local.md"));
    }
}
