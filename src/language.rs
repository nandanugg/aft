use std::path::Path;

use crate::error::AftError;

/// Location range within a source file (line/column, 0-indexed).
#[derive(Debug, Clone)]
pub struct Range {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// A symbol discovered in a source file.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: String, // e.g. "function", "class", "variable"
    pub range: Range,
}

/// A resolved symbol match — a `Symbol` plus the file it was found in.
#[derive(Debug, Clone)]
pub struct SymbolMatch {
    pub symbol: Symbol,
    pub file: String,
}

/// Trait for language-specific symbol resolution.
///
/// S02 implements this with tree-sitter parsing. S01 provides only the
/// `StubProvider` placeholder.
pub trait LanguageProvider {
    /// Resolve a symbol by name within a file. Returns all matches.
    fn resolve_symbol(&self, file: &Path, name: &str) -> Result<Vec<SymbolMatch>, AftError>;

    /// List all top-level symbols in a file.
    fn list_symbols(&self, file: &Path) -> Result<Vec<Symbol>, AftError>;
}

/// Placeholder provider that rejects all calls.
///
/// Used until a real language backend (tree-sitter) is wired in during S02.
pub struct StubProvider;

impl LanguageProvider for StubProvider {
    fn resolve_symbol(&self, _file: &Path, _name: &str) -> Result<Vec<SymbolMatch>, AftError> {
        Err(AftError::InvalidRequest {
            message: "no language provider configured".to_string(),
        })
    }

    fn list_symbols(&self, _file: &Path) -> Result<Vec<Symbol>, AftError> {
        Err(AftError::InvalidRequest {
            message: "no language provider configured".to_string(),
        })
    }
}
