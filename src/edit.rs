//! Shared edit engine: byte-offset conversion, content replacement,
//! syntax validation, and auto-backup orchestration.
//!
//! Used by `write`, `edit_symbol`, `edit_match`, and `batch` commands.

use std::path::Path;

use crate::context::AppContext;
use crate::error::AftError;
use crate::parser::FileParser;

/// Convert 0-indexed line/col to a byte offset within `source`.
///
/// Tree-sitter columns are byte-indexed within the line, so `col` is a byte
/// offset from the start of the line (not a character offset).
///
/// Returns `source.len()` if line is beyond the end of the file.
pub fn line_col_to_byte(source: &str, line: u32, col: u32) -> usize {
    let mut byte = 0;
    for (i, l) in source.lines().enumerate() {
        if i == line as usize {
            return byte + (col as usize).min(l.len());
        }
        byte += l.len() + 1; // +1 for the newline character
    }
    // If we run out of lines, clamp to source length.
    source.len()
}

/// Replace bytes in `[start..end)` with `replacement`.
///
/// Panics if `start > end` or indices are out of bounds for the source.
pub fn replace_byte_range(source: &str, start: usize, end: usize, replacement: &str) -> String {
    let mut result = String::with_capacity(source.len() - (end - start) + replacement.len());
    result.push_str(&source[..start]);
    result.push_str(replacement);
    result.push_str(&source[end..]);
    result
}

/// Validate syntax of a file using a fresh FileParser (D023).
///
/// Returns `Ok(Some(true))` if syntax is valid, `Ok(Some(false))` if there are
/// parse errors, and `Ok(None)` if the language is unsupported.
pub fn validate_syntax(path: &Path) -> Result<Option<bool>, AftError> {
    let mut parser = FileParser::new();
    match parser.parse(path) {
        Ok((tree, _lang)) => Ok(Some(!tree.root_node().has_error())),
        Err(AftError::InvalidRequest { .. }) => {
            // Unsupported language — not an error, just can't validate
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Snapshot the file into the backup store before mutation.
///
/// Returns `Ok(Some(backup_id))` if the file existed and was backed up,
/// `Ok(None)` if the file doesn't exist (new file creation).
///
/// Drops the RefCell borrow before returning (D029).
pub fn auto_backup(
    ctx: &AppContext,
    path: &Path,
    description: &str,
) -> Result<Option<String>, AftError> {
    if !path.exists() {
        return Ok(None);
    }
    let backup_id = {
        let mut store = ctx.backup().borrow_mut();
        store.snapshot(path, description)?
    }; // borrow dropped here
    Ok(Some(backup_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- line_col_to_byte ---

    #[test]
    fn line_col_to_byte_empty_string() {
        assert_eq!(line_col_to_byte("", 0, 0), 0);
    }

    #[test]
    fn line_col_to_byte_single_line() {
        let source = "hello";
        assert_eq!(line_col_to_byte(source, 0, 0), 0);
        assert_eq!(line_col_to_byte(source, 0, 3), 3);
        assert_eq!(line_col_to_byte(source, 0, 5), 5); // end of line
    }

    #[test]
    fn line_col_to_byte_multi_line() {
        let source = "abc\ndef\nghi\n";
        // line 0: "abc" at bytes 0..3, newline at 3
        assert_eq!(line_col_to_byte(source, 0, 0), 0);
        assert_eq!(line_col_to_byte(source, 0, 2), 2);
        // line 1: "def" at bytes 4..7, newline at 7
        assert_eq!(line_col_to_byte(source, 1, 0), 4);
        assert_eq!(line_col_to_byte(source, 1, 3), 7);
        // line 2: "ghi" at bytes 8..11, newline at 11
        assert_eq!(line_col_to_byte(source, 2, 0), 8);
        assert_eq!(line_col_to_byte(source, 2, 2), 10);
    }

    #[test]
    fn line_col_to_byte_last_line_no_trailing_newline() {
        let source = "abc\ndef";
        // line 1: "def" at bytes 4..7, no trailing newline
        assert_eq!(line_col_to_byte(source, 1, 0), 4);
        assert_eq!(line_col_to_byte(source, 1, 3), 7); // end
    }

    #[test]
    fn line_col_to_byte_multi_byte_utf8() {
        // "é" is 2 bytes in UTF-8
        let source = "café\nbar";
        // line 0: "café" is 5 bytes (c=1, a=1, f=1, é=2)
        assert_eq!(line_col_to_byte(source, 0, 0), 0);
        assert_eq!(line_col_to_byte(source, 0, 5), 5); // end of "café"
        // line 1: "bar" starts at byte 6
        assert_eq!(line_col_to_byte(source, 1, 0), 6);
        assert_eq!(line_col_to_byte(source, 1, 2), 8);
    }

    #[test]
    fn line_col_to_byte_beyond_end() {
        let source = "abc";
        // Line beyond file returns source.len()
        assert_eq!(line_col_to_byte(source, 5, 0), source.len());
    }

    #[test]
    fn line_col_to_byte_col_clamped_to_line_length() {
        let source = "ab\ncd";
        // col=10 on a 2-char line should clamp to 2
        assert_eq!(line_col_to_byte(source, 0, 10), 2);
    }

    // --- replace_byte_range ---

    #[test]
    fn replace_byte_range_basic() {
        let source = "hello world";
        let result = replace_byte_range(source, 6, 11, "rust");
        assert_eq!(result, "hello rust");
    }

    #[test]
    fn replace_byte_range_delete() {
        let source = "hello world";
        let result = replace_byte_range(source, 5, 11, "");
        assert_eq!(result, "hello");
    }

    #[test]
    fn replace_byte_range_insert_at_same_position() {
        let source = "helloworld";
        let result = replace_byte_range(source, 5, 5, " ");
        assert_eq!(result, "hello world");
    }

    #[test]
    fn replace_byte_range_replace_entire_string() {
        let source = "old content";
        let result = replace_byte_range(source, 0, source.len(), "new content");
        assert_eq!(result, "new content");
    }
}
