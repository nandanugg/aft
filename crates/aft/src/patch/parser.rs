//! Parser for the opencode `*** Begin Patch` envelope.
//!
//! This ports the pure parsing half of `packages/opencode-plugin/src/patch-parser.ts`.

use regex::Regex;
use std::sync::OnceLock;

/// Maximum patch text size in bytes to prevent memory exhaustion.
pub const MAX_PATCH_SIZE: usize = 1024 * 1024;
/// Maximum number of file operations per patch.
pub const MAX_HUNKS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hunk {
    Add {
        path: String,
        contents: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_path: Option<String>,
        chunks: Vec<UpdateFileChunk>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateFileChunk {
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    pub change_context: Option<String>,
    pub is_end_of_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchHeader {
    pub file_path: String,
    pub move_path: Option<String>,
    pub next_idx: usize,
}

/// Strip a whole-input heredoc wrapper; mirrors `patch-parser.ts:33-36`.
///
/// The TypeScript regex uses a backreference for the closing delimiter. Rust's
/// `regex` crate intentionally omits backreferences, so this uses `regex` for
/// the opening heredoc syntax and then verifies the matching closing delimiter
/// manually to preserve the same anchored wrapper behavior.
pub fn strip_heredoc(input: &str) -> String {
    static OPEN_RE: OnceLock<Regex> = OnceLock::new();
    let open_re = OPEN_RE.get_or_init(|| {
        Regex::new(r#"^(?:cat\s+)?<<['"]?([A-Za-z0-9_]+)['"]?\s*\n"#)
            .expect("heredoc opening regex should compile")
    });

    let Some(captures) = open_re.captures(input) else {
        return input.to_owned();
    };
    let Some(opening) = captures.get(0) else {
        return input.to_owned();
    };
    let delimiter = captures
        .get(1)
        .expect("heredoc regex has a delimiter capture")
        .as_str();
    let rest = &input[opening.end()..];

    for (offset, _) in rest.match_indices('\n') {
        let after_newline = &rest[offset + 1..];
        let Some(after_delimiter) = after_newline.strip_prefix(delimiter) else {
            continue;
        };
        if after_delimiter.chars().all(char::is_whitespace) {
            return rest[..offset].to_owned();
        }
    }

    input.to_owned()
}

/// Parse a file-operation header line and return its path, optional move destination, and next line index.
pub fn parse_patch_header(lines: &[&str], start_idx: usize) -> Option<PatchHeader> {
    let line = *lines.get(start_idx)?;

    if let Some(path) = line.strip_prefix("*** Add File:") {
        let file_path = path.trim();
        return (!file_path.is_empty()).then(|| PatchHeader {
            file_path: file_path.to_owned(),
            move_path: None,
            next_idx: start_idx + 1,
        });
    }

    if let Some(path) = line.strip_prefix("*** Delete File:") {
        let file_path = path.trim();
        return (!file_path.is_empty()).then(|| PatchHeader {
            file_path: file_path.to_owned(),
            move_path: None,
            next_idx: start_idx + 1,
        });
    }

    if let Some(path) = line.strip_prefix("*** Update File:") {
        let file_path = path.trim();
        if file_path.is_empty() {
            return None;
        }

        let mut move_path = None;
        let mut next_idx = start_idx + 1;
        if let Some(next_line) = lines.get(next_idx) {
            if let Some(path) = next_line.strip_prefix("*** Move to:") {
                move_path = Some(path.trim().to_owned());
                next_idx += 1;
            }
        }

        return Some(PatchHeader {
            file_path: file_path.to_owned(),
            move_path,
            next_idx,
        });
    }

    None
}

/// Parse added file content by collecting every `+` line until the next patch marker.
pub fn parse_add_file_content(lines: &[&str], start_idx: usize) -> (String, usize) {
    let mut content = String::new();
    let mut i = start_idx;

    while i < lines.len() && !lines[i].starts_with("***") {
        if let Some(line) = lines[i].strip_prefix('+') {
            content.push_str(line);
            content.push('\n');
        }
        i += 1;
    }

    if content.ends_with('\n') {
        content.pop();
    }

    (content, i)
}

/// Parse `@@` update chunks into old/new line vectors and optional context anchors.
pub fn parse_update_file_chunks(lines: &[&str], start_idx: usize) -> (Vec<UpdateFileChunk>, usize) {
    let mut chunks = Vec::new();
    let mut i = start_idx;

    while i < lines.len() && !lines[i].starts_with("***") {
        if lines[i].starts_with("@@") {
            let context_line = lines[i]["@@".len()..].trim();
            i += 1;

            let mut old_lines = Vec::new();
            let mut new_lines = Vec::new();
            let mut is_end_of_file = false;

            while i < lines.len() && !lines[i].starts_with("@@") {
                let change_line = lines[i];

                if change_line == "*** End of File" {
                    is_end_of_file = true;
                    i += 1;
                    break;
                }
                if change_line.starts_with("***") {
                    break;
                }

                if let Some(content) = change_line.strip_prefix(' ') {
                    old_lines.push(content.to_owned());
                    new_lines.push(content.to_owned());
                } else if let Some(content) = change_line.strip_prefix('-') {
                    old_lines.push(content.to_owned());
                } else if let Some(content) = change_line.strip_prefix('+') {
                    new_lines.push(content.to_owned());
                }

                i += 1;
            }

            chunks.push(UpdateFileChunk {
                old_lines,
                new_lines,
                change_context: (!context_line.is_empty()).then(|| context_line.to_owned()),
                is_end_of_file,
            });
        } else {
            i += 1;
        }
    }

    (chunks, i)
}

/// Parse an opencode apply_patch envelope; mirrors `patch-parser.ts:148-201`.
///
/// The size guard uses `patch_text.len()` bytes. TypeScript uses string length,
/// but the port intentionally guards bytes so Rust bounds actual allocation size.
pub fn parse_patch(patch_text: &str) -> Result<Vec<Hunk>, String> {
    if patch_text.len() > MAX_PATCH_SIZE {
        return Err(format!(
            "Patch too large: {} bytes exceeds limit of {} bytes",
            patch_text.len(),
            MAX_PATCH_SIZE
        ));
    }

    let trimmed = patch_text.trim();
    let cleaned = strip_heredoc(trimmed);
    let lines: Vec<&str> = cleaned.split('\n').collect();
    let mut hunks = Vec::new();

    let begin_idx = lines
        .iter()
        .position(|line| line.trim() == "*** Begin Patch");
    let end_idx = lines.iter().position(|line| line.trim() == "*** End Patch");

    let (Some(begin_idx), Some(end_idx)) = (begin_idx, end_idx) else {
        return Err(
            "Invalid patch format: missing *** Begin Patch / *** End Patch markers".to_owned(),
        );
    };
    if begin_idx >= end_idx {
        return Err(
            "Invalid patch format: missing *** Begin Patch / *** End Patch markers".to_owned(),
        );
    }

    let mut i = begin_idx + 1;
    while i < end_idx {
        let Some(header) = parse_patch_header(&lines, i) else {
            i += 1;
            continue;
        };

        if hunks.len() >= MAX_HUNKS {
            return Err(format!(
                "Patch exceeds maximum of {} file operations",
                MAX_HUNKS
            ));
        }

        if lines[i].starts_with("*** Add File:") {
            let (contents, next_idx) = parse_add_file_content(&lines, header.next_idx);
            hunks.push(Hunk::Add {
                path: header.file_path,
                contents,
            });
            i = next_idx;
        } else if lines[i].starts_with("*** Delete File:") {
            hunks.push(Hunk::Delete {
                path: header.file_path,
            });
            i = header.next_idx;
        } else if lines[i].starts_with("*** Update File:") {
            let (chunks, next_idx) = parse_update_file_chunks(&lines, header.next_idx);
            hunks.push(Hunk::Update {
                path: header.file_path,
                move_path: header.move_path,
                chunks,
            });
            i = next_idx;
        } else {
            i += 1;
        }
    }

    Ok(hunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_parse_error(patch: &str, expected: &str) {
        assert_eq!(parse_patch(patch).unwrap_err(), expected);
    }

    #[test]
    fn parse_patch_missing_markers_matches_patch_parser_test_4_9() {
        assert_parse_error(
            "*** Add File: hello.txt\n+hello",
            "Invalid patch format: missing *** Begin Patch / *** End Patch markers",
        );
    }

    #[test]
    fn parse_patch_empty_body_matches_patch_parser_test_11_13() {
        assert_eq!(
            parse_patch("*** Begin Patch\n*** End Patch").unwrap(),
            vec![]
        );
    }

    #[test]
    fn parse_patch_ignores_empty_add_header_matches_patch_parser_test_15_17() {
        assert_eq!(
            parse_patch("*** Begin Patch\n*** Add File:\n+hello\n*** End Patch").unwrap(),
            vec![]
        );
    }

    #[test]
    fn parse_patch_size_limit_matches_patch_parser_test_19_25() {
        let oversized_patch = "x".repeat(MAX_PATCH_SIZE + 1);
        assert_parse_error(
            &oversized_patch,
            "Patch too large: 1048577 bytes exceeds limit of 1048576 bytes",
        );
    }

    #[test]
    fn parse_patch_hunk_limit_matches_patch_parser_test_27_38() {
        let mut patch = vec!["*** Begin Patch".to_owned()];
        for index in 0..=MAX_HUNKS {
            patch.push(format!("*** Add File: file-{index}.txt"));
            patch.push(format!("+line {index}"));
        }
        patch.push("*** End Patch".to_owned());

        assert_parse_error(
            &patch.join("\n"),
            "Patch exceeds maximum of 500 file operations",
        );
    }

    #[test]
    fn parse_patch_invalid_heredoc_matches_patch_parser_test_40_56() {
        let wrapped_patch = [
            "<<EOF",
            "*** Begin Patch",
            "*** Add File: hello.txt",
            "+hello world",
            "*** End Patch",
            "NOT_EOF",
        ]
        .join("\n");

        let expected = vec![Hunk::Add {
            path: "hello.txt".to_owned(),
            contents: "hello world".to_owned(),
        }];
        assert_eq!(parse_patch(&wrapped_patch).unwrap(), expected);
        assert_eq!(
            parse_patch(&format!("prefix\n{wrapped_patch}")).unwrap(),
            expected
        );
    }

    #[test]
    fn strip_heredoc_accepts_whole_input_wrapper_from_patch_parser_source_33_36() {
        let wrapped_patch = [
            "cat <<'PATCH'",
            "*** Begin Patch",
            "*** Add File: hello.txt",
            "+hello world",
            "*** End Patch",
            "PATCH",
        ]
        .join("\n");

        assert_eq!(
            parse_patch(&wrapped_patch).unwrap(),
            vec![Hunk::Add {
                path: "hello.txt".to_owned(),
                contents: "hello world".to_owned(),
            }]
        );
    }

    #[test]
    fn parse_patch_round_trips_add_delete_update_move_from_parser_source_38_141() {
        let patch = [
            "*** Begin Patch",
            "*** Add File: src/new.txt",
            "+hello",
            "+world",
            "*** Delete File: src/old.txt",
            "*** Update File: src/edit.txt",
            "@@ function demo()",
            " const keep = true;",
            "-const value = 1;",
            "+const value = 2;",
            "*** Update File: src/from.txt",
            "*** Move to: src/to.txt",
            "@@",
            "-old",
            "+new",
            "*** End of File",
            "*** End Patch",
        ]
        .join("\n");

        assert_eq!(
            parse_patch(&patch).unwrap(),
            vec![
                Hunk::Add {
                    path: "src/new.txt".to_owned(),
                    contents: "hello\nworld".to_owned(),
                },
                Hunk::Delete {
                    path: "src/old.txt".to_owned(),
                },
                Hunk::Update {
                    path: "src/edit.txt".to_owned(),
                    move_path: None,
                    chunks: vec![UpdateFileChunk {
                        old_lines: vec![
                            "const keep = true;".to_owned(),
                            "const value = 1;".to_owned()
                        ],
                        new_lines: vec![
                            "const keep = true;".to_owned(),
                            "const value = 2;".to_owned()
                        ],
                        change_context: Some("function demo()".to_owned()),
                        is_end_of_file: false,
                    }],
                },
                Hunk::Update {
                    path: "src/from.txt".to_owned(),
                    move_path: Some("src/to.txt".to_owned()),
                    chunks: vec![UpdateFileChunk {
                        old_lines: vec!["old".to_owned()],
                        new_lines: vec!["new".to_owned()],
                        change_context: None,
                        is_end_of_file: true,
                    }],
                },
            ]
        );
    }

    #[test]
    fn parse_patch_supports_multiple_chunks_in_one_update_from_parser_source_91_141() {
        let patch = [
            "*** Begin Patch",
            "*** Update File: src/multi.txt",
            "@@ first",
            "-one",
            "+two",
            "@@ second",
            " three",
            "-four",
            "+five",
            "*** End Patch",
        ]
        .join("\n");

        assert_eq!(
            parse_patch(&patch).unwrap(),
            vec![Hunk::Update {
                path: "src/multi.txt".to_owned(),
                move_path: None,
                chunks: vec![
                    UpdateFileChunk {
                        old_lines: vec!["one".to_owned()],
                        new_lines: vec!["two".to_owned()],
                        change_context: Some("first".to_owned()),
                        is_end_of_file: false,
                    },
                    UpdateFileChunk {
                        old_lines: vec!["three".to_owned(), "four".to_owned()],
                        new_lines: vec!["three".to_owned(), "five".to_owned()],
                        change_context: Some("second".to_owned()),
                        is_end_of_file: false,
                    },
                ],
            }]
        );
    }
}
