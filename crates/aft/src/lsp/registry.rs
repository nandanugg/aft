use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::config::{Config, UserServerDef};

/// Unique identifier for a language server kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ServerKind {
    TypeScript,
    Python,
    Rust,
    Go,
    Bash,
    Yaml,
    Ty,
    Custom(Arc<str>),
}

impl ServerKind {
    pub fn id_str(&self) -> &str {
        match self {
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Bash => "bash",
            Self::Yaml => "yaml",
            Self::Ty => "ty",
            Self::Custom(id) => id.as_ref(),
        }
    }
}

/// Definition of a language server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDef {
    pub kind: ServerKind,
    /// Display name.
    pub name: String,
    /// File extensions this server handles.
    pub extensions: Vec<String>,
    /// Binary name to look up on PATH.
    pub binary: String,
    /// Arguments to pass when spawning.
    pub args: Vec<String>,
    /// Root marker files — presence indicates a workspace root.
    pub root_markers: Vec<String>,
    /// Extra environment variables for this server process.
    pub env: HashMap<String, String>,
    /// Optional JSON initializationOptions for the initialize request.
    pub initialization_options: Option<serde_json::Value>,
}

impl ServerDef {
    /// Check if this server handles a given file extension.
    pub fn matches_extension(&self, ext: &str) -> bool {
        self.extensions
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(ext))
    }

    /// Check if the server binary is available on PATH.
    pub fn is_available(&self) -> bool {
        which::which(&self.binary).is_ok()
    }
}

/// Built-in server definitions.
pub fn builtin_servers() -> Vec<ServerDef> {
    vec![
        builtin_server(
            ServerKind::TypeScript,
            "TypeScript Language Server",
            &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
            "typescript-language-server",
            &["--stdio"],
            &["tsconfig.json", "jsconfig.json", "package.json"],
        ),
        builtin_server(
            ServerKind::Python,
            "Pyright",
            &["py", "pyi"],
            "pyright-langserver",
            &["--stdio"],
            &[
                "pyproject.toml",
                "setup.py",
                "setup.cfg",
                "pyrightconfig.json",
                "requirements.txt",
            ],
        ),
        builtin_server(
            ServerKind::Rust,
            "rust-analyzer",
            &["rs"],
            "rust-analyzer",
            &[],
            &["Cargo.toml"],
        ),
        // gopls requires opt-in for `textDocument/diagnostic` (LSP 3.17 pull)
        // via the `pullDiagnostics` initializationOption. Without this the
        // server still publishes via push but ignores pull requests.
        // See https://github.com/golang/tools/blob/master/gopls/doc/settings.md
        builtin_server_with_init(
            ServerKind::Go,
            "gopls",
            &["go"],
            "gopls",
            &["serve"],
            &["go.mod"],
            serde_json::json!({ "pullDiagnostics": true }),
        ),
        builtin_server(
            ServerKind::Bash,
            "bash-language-server",
            &["sh", "bash", "zsh"],
            "bash-language-server",
            &["start"],
            &["package.json", ".git"],
        ),
        builtin_server(
            ServerKind::Yaml,
            "yaml-language-server",
            &["yaml", "yml"],
            "yaml-language-server",
            &["--stdio"],
            &["package.json", ".git"],
        ),
        builtin_server(
            ServerKind::Ty,
            "ty",
            &["py", "pyi"],
            "ty",
            &["server"],
            &[
                "pyproject.toml",
                "ty.toml",
                "setup.py",
                "setup.cfg",
                "requirements.txt",
                "Pipfile",
                "pyrightconfig.json",
            ],
        ),
    ]
}

/// Find all server definitions that handle a given file path.
pub fn servers_for_file(path: &Path, config: &Config) -> Vec<ServerDef> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();

    builtin_servers()
        .into_iter()
        .chain(config.lsp_servers.iter().filter_map(custom_server))
        .filter(|server| !is_disabled(server, config))
        .filter(|server| config.experimental_lsp_ty || server.kind != ServerKind::Ty)
        .filter(|server| server.matches_extension(extension))
        .collect()
}

fn builtin_server(
    kind: ServerKind,
    name: &str,
    extensions: &[&str],
    binary: &str,
    args: &[&str],
    root_markers: &[&str],
) -> ServerDef {
    ServerDef {
        kind,
        name: name.to_string(),
        extensions: strings(extensions),
        binary: binary.to_string(),
        args: strings(args),
        root_markers: strings(root_markers),
        env: HashMap::new(),
        initialization_options: None,
    }
}

/// Builder variant of [`builtin_server`] that includes a default
/// `initializationOptions` payload — used for servers that need server-specific
/// settings to enable LSP features (e.g., gopls's `pullDiagnostics`).
fn builtin_server_with_init(
    kind: ServerKind,
    name: &str,
    extensions: &[&str],
    binary: &str,
    args: &[&str],
    root_markers: &[&str],
    initialization_options: serde_json::Value,
) -> ServerDef {
    let mut def = builtin_server(kind, name, extensions, binary, args, root_markers);
    def.initialization_options = Some(initialization_options);
    def
}

fn custom_server(server: &UserServerDef) -> Option<ServerDef> {
    if server.disabled {
        return None;
    }

    Some(ServerDef {
        kind: ServerKind::Custom(Arc::from(server.id.as_str())),
        name: server.id.clone(),
        extensions: server.extensions.clone(),
        binary: server.binary.clone(),
        args: server.args.clone(),
        root_markers: server.root_markers.clone(),
        env: server.env.clone(),
        initialization_options: server.initialization_options.clone(),
    })
}

fn is_disabled(server: &ServerDef, config: &Config) -> bool {
    config
        .disabled_lsp
        .contains(&server.kind.id_str().to_ascii_lowercase())
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use crate::config::{Config, UserServerDef};

    use super::{servers_for_file, ServerKind};

    fn matching_kinds(path: &str, config: &Config) -> Vec<ServerKind> {
        servers_for_file(Path::new(path), config)
            .into_iter()
            .map(|server| server.kind)
            .collect()
    }

    #[test]
    fn test_servers_for_typescript_file() {
        assert_eq!(
            matching_kinds("/tmp/file.ts", &Config::default()),
            vec![ServerKind::TypeScript]
        );
    }

    #[test]
    fn test_servers_for_python_file() {
        assert_eq!(
            matching_kinds("/tmp/file.py", &Config::default()),
            vec![ServerKind::Python]
        );
    }

    #[test]
    fn test_servers_for_rust_file() {
        assert_eq!(
            matching_kinds("/tmp/file.rs", &Config::default()),
            vec![ServerKind::Rust]
        );
    }

    #[test]
    fn test_servers_for_go_file() {
        assert_eq!(
            matching_kinds("/tmp/file.go", &Config::default()),
            vec![ServerKind::Go]
        );
    }

    #[test]
    fn test_servers_for_unknown_file() {
        assert!(matching_kinds("/tmp/file.txt", &Config::default()).is_empty());
    }

    #[test]
    fn test_tsx_matches_typescript() {
        assert_eq!(
            matching_kinds("/tmp/file.tsx", &Config::default()),
            vec![ServerKind::TypeScript]
        );
    }

    #[test]
    fn test_case_insensitive_extension() {
        assert_eq!(
            matching_kinds("/tmp/file.TS", &Config::default()),
            vec![ServerKind::TypeScript]
        );
    }

    #[test]
    fn test_bash_and_yaml_builtins() {
        assert_eq!(
            matching_kinds("/tmp/file.sh", &Config::default()),
            vec![ServerKind::Bash]
        );
        assert_eq!(
            matching_kinds("/tmp/file.yaml", &Config::default()),
            vec![ServerKind::Yaml]
        );
    }

    #[test]
    fn test_ty_requires_experimental_flag() {
        assert_eq!(
            matching_kinds("/tmp/file.py", &Config::default()),
            vec![ServerKind::Python]
        );

        let config = Config {
            experimental_lsp_ty: true,
            ..Config::default()
        };
        assert_eq!(
            matching_kinds("/tmp/file.py", &config),
            vec![ServerKind::Python, ServerKind::Ty]
        );
    }

    #[test]
    fn test_custom_server_matches_extension() {
        let config = Config {
            lsp_servers: vec![UserServerDef {
                id: "tinymist".to_string(),
                extensions: vec!["typ".to_string()],
                binary: "tinymist".to_string(),
                root_markers: vec!["typst.toml".to_string()],
                ..UserServerDef::default()
            }],
            ..Config::default()
        };

        assert_eq!(
            matching_kinds("/tmp/file.typ", &config),
            vec![ServerKind::Custom(Arc::from("tinymist"))]
        );
    }
}
