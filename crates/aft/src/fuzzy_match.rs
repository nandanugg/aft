//! Fuzzy string matching for edit_match, inspired by opencode's 4-pass approach.
//!
//! When exact matching fails, progressively relaxes comparison:
//!   Pass 1: Exact match (str::find / match_indices)
//!   Pass 2: Trim trailing whitespace per line
//!   Pass 3: Trim both ends per line
//!   Pass 4: Normalize Unicode punctuation + trim
//!   Pass 5: Reflowed line wraps/joins with whitespace-normalized content

/// A match result: byte offset in source and the matched byte length.
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    pub byte_start: usize,
    pub byte_len: usize,
    /// Which pass found the match (1=exact, 2=rstrip, 3=trim, 4=unicode, 5=reflow)
    pub pass: u8,
}

/// Find all occurrences of `needle` in `haystack` using progressive fuzzy matching.
/// Returns matches in order of their byte position in the source.
pub fn find_all_fuzzy(haystack: &str, needle: &str) -> Vec<FuzzyMatch> {
    // Pass 1: exact match (fast path)
    let exact: Vec<FuzzyMatch> = haystack
        .match_indices(needle)
        .map(|(idx, _)| FuzzyMatch {
            byte_start: idx,
            byte_len: needle.len(),
            pass: 1,
        })
        .collect();

    if !exact.is_empty() {
        return exact;
    }

    // For fuzzy passes, work line-by-line
    let needle_lines: Vec<&str> = needle.lines().collect();
    if needle_lines.is_empty() {
        return vec![];
    }

    let haystack_lines: Vec<&str> = haystack.lines().collect();
    let line_byte_offsets = compute_line_offsets(haystack);

    // Pass 2: rstrip (trim trailing whitespace)
    let rstrip_matches = find_line_matches(
        &haystack_lines,
        &needle_lines,
        &line_byte_offsets,
        haystack,
        |a, b| a.trim_end() == b.trim_end(),
        2,
    );
    if !rstrip_matches.is_empty() {
        return rstrip_matches;
    }

    // Pass 3: trim (both ends)
    let trim_matches = find_line_matches(
        &haystack_lines,
        &needle_lines,
        &line_byte_offsets,
        haystack,
        |a, b| a.trim() == b.trim(),
        3,
    );
    if !trim_matches.is_empty() {
        return trim_matches;
    }

    // Pass 4: normalized Unicode + trim. Normalize each line once instead of
    // allocating inside the O(haystack_lines × needle_lines) comparison loop.
    let normalized_haystack_lines: Vec<String> = haystack_lines
        .iter()
        .map(|line| normalize_unicode(line.trim()))
        .collect();
    let normalized_needle_lines: Vec<String> = needle_lines
        .iter()
        .map(|line| normalize_unicode(line.trim()))
        .collect();
    let normalized_haystack_refs: Vec<&str> = normalized_haystack_lines
        .iter()
        .map(String::as_str)
        .collect();
    let normalized_needle_refs: Vec<&str> =
        normalized_needle_lines.iter().map(String::as_str).collect();
    let normalized_matches = find_line_matches(
        &normalized_haystack_refs,
        &normalized_needle_refs,
        &line_byte_offsets,
        haystack,
        |a, b| a == b,
        4,
    );
    if !normalized_matches.is_empty() {
        return normalized_matches;
    }

    // Pass 5: final fallback for formatter reflows. This pass deliberately
    // runs only after every line-contiguous pass fails, and each candidate
    // window must have the same non-whitespace content as the needle.
    find_reflow_matches(&haystack_lines, &needle_lines, &line_byte_offsets, haystack)
}

/// Compute byte offset of each line start in the source string.
fn compute_line_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (i, c) in source.char_indices() {
        if c == '\n' && i + 1 <= source.len() {
            offsets.push(i + 1);
        }
    }
    offsets
}

/// Find all positions where `needle_lines` matches a contiguous sequence in `haystack_lines`.
fn find_line_matches<F>(
    haystack_lines: &[&str],
    needle_lines: &[&str],
    line_offsets: &[usize],
    haystack: &str,
    compare: F,
    pass: u8,
) -> Vec<FuzzyMatch>
where
    F: Fn(&str, &str) -> bool,
{
    let mut matches = Vec::new();
    if needle_lines.len() > haystack_lines.len() {
        return matches;
    }

    'outer: for i in 0..=(haystack_lines.len() - needle_lines.len()) {
        for j in 0..needle_lines.len() {
            if !compare(haystack_lines[i + j], needle_lines[j]) {
                continue 'outer;
            }
        }
        // Found a match at line `i` spanning `needle_lines.len()` lines
        let byte_start = line_offsets[i];
        let end_line = i + needle_lines.len();
        let byte_end = if end_line < line_offsets.len() {
            // Include the newline after the last matched line
            line_offsets[end_line]
        } else {
            haystack.len()
        };
        matches.push(FuzzyMatch {
            byte_start,
            byte_len: byte_end - byte_start,
            pass,
        });
    }

    matches
}

const REFLOW_NON_WS_TOLERANCE: usize = 8;

fn find_reflow_matches(
    haystack_lines: &[&str],
    needle_lines: &[&str],
    line_offsets: &[usize],
    haystack: &str,
) -> Vec<FuzzyMatch> {
    let needle_text = needle_lines.join("\n");
    let normalized_needle = normalize_reflow_whitespace(&needle_text);
    let needle_non_whitespace = strip_reflow_whitespace(&needle_text);
    if normalized_needle.is_empty() || needle_non_whitespace.is_empty() {
        return Vec::new();
    }

    let min_non_whitespace = needle_non_whitespace
        .len()
        .saturating_sub(REFLOW_NON_WS_TOLERANCE);
    let max_non_whitespace = needle_non_whitespace.len() + REFLOW_NON_WS_TOLERANCE;
    let line_non_whitespace_lens: Vec<usize> = haystack_lines
        .iter()
        .map(|line| strip_reflow_whitespace(line).len())
        .collect();
    let mut matches = Vec::new();

    for start in 0..haystack_lines.len() {
        if !has_reflow_content(haystack_lines[start]) {
            continue;
        }

        let mut window_non_whitespace_len = 0usize;
        for end in (start + 1)..=haystack_lines.len() {
            let line = haystack_lines[end - 1];
            window_non_whitespace_len += line_non_whitespace_lens[end - 1];

            if window_non_whitespace_len > max_non_whitespace {
                break;
            }
            if window_non_whitespace_len < min_non_whitespace {
                continue;
            }
            if !has_reflow_content(line) {
                continue;
            }

            let window_text = haystack_lines[start..end].join("\n");
            let window_non_whitespace = strip_reflow_whitespace(&window_text);
            if window_non_whitespace != needle_non_whitespace {
                continue;
            }
            if normalize_reflow_whitespace(&window_text) != normalized_needle {
                continue;
            }

            let byte_start = line_offsets[start];
            let byte_end = if end < line_offsets.len() {
                line_offsets[end]
            } else {
                haystack.len()
            };
            matches.push(FuzzyMatch {
                byte_start,
                byte_len: byte_end - byte_start,
                pass: 5,
            });
        }
    }

    matches
}

fn normalize_reflow_whitespace(s: &str) -> String {
    let mut normalized = String::new();
    let mut in_whitespace = false;

    for c in s.trim().chars() {
        if c.is_whitespace() {
            in_whitespace = true;
        } else {
            if in_whitespace && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(c);
            in_whitespace = false;
        }
    }

    normalized
}

fn strip_reflow_whitespace(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn has_reflow_content(s: &str) -> bool {
    s.chars().any(|c| !c.is_whitespace())
}

/// Normalize Unicode punctuation to ASCII equivalents.
fn normalize_unicode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' => '-',
            '\u{00A0}' => ' ',
            _ => c,
        })
        .collect::<String>()
        .replace('\u{2026}', "...")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let matches = find_all_fuzzy("hello world", "world");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].byte_start, 6);
        assert_eq!(matches[0].pass, 1);
    }

    #[test]
    fn test_exact_match_multiple() {
        let matches = find_all_fuzzy("foo bar foo baz foo", "foo");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].byte_start, 0);
        assert_eq!(matches[1].byte_start, 8);
        assert_eq!(matches[2].byte_start, 16);
    }

    #[test]
    fn test_rstrip_match() {
        let source = "  hello  \n  world  \n";
        let needle = "  hello\n  world";
        let matches = find_all_fuzzy(source, needle);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pass, 2); // rstrip pass
    }

    #[test]
    fn test_trim_match() {
        let source = "    function foo() {\n      return 1;\n    }\n";
        let needle = "function foo() {\n  return 1;\n}";
        let matches = find_all_fuzzy(source, needle);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pass, 3); // trim pass
    }

    #[test]
    fn test_unicode_normalize() {
        let source = "let msg = \u{201C}hello\u{201D}\n";
        let needle = "let msg = \"hello\"";
        let matches = find_all_fuzzy(source, needle);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pass, 4); // unicode pass
    }

    #[test]
    fn test_unicode_normalize_multiline_variants() {
        let source = "alpha\n  let title = \u{201C}hello\u{201D}\u{2026}\n  let slug = foo\u{2014}bar\u{00A0}baz\nomega\n";
        let needle = "let title = \"hello\"...\nlet slug = foo-bar baz";
        let matches = find_all_fuzzy(source, needle);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pass, 4);
        assert_eq!(matches[0].byte_start, source.find("  let title").unwrap());
    }

    #[test]
    fn test_no_match() {
        let matches = find_all_fuzzy("hello world", "xyz");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_multiline_exact() {
        let source = "line1\nline2\nline3\nline4\n";
        let needle = "line2\nline3";
        let matches = find_all_fuzzy(source, needle);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].byte_start, 6);
        assert_eq!(matches[0].pass, 1);
    }

    #[test]
    fn test_reflow_one_line_needle_matches_three_line_split() {
        let source = "before\nlet total = alpha +\n    beta +\n    gamma;\nafter\n";
        let needle = "let total = alpha + beta + gamma;";
        let matches = find_all_fuzzy(source, needle);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pass, 5);
        assert_eq!(matches[0].byte_start, source.find("let total").unwrap());
        assert_eq!(
            &source[matches[0].byte_start..matches[0].byte_start + matches[0].byte_len],
            "let total = alpha +\n    beta +\n    gamma;\n"
        );
    }

    #[test]
    fn test_reflow_three_line_needle_matches_one_line_join() {
        let source = "before\nlet total = alpha + beta + gamma;\nafter\n";
        let needle = "let total = alpha +\n    beta +\n    gamma;";
        let matches = find_all_fuzzy(source, needle);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pass, 5);
        assert_eq!(matches[0].byte_start, source.find("let total").unwrap());
        assert_eq!(
            &source[matches[0].byte_start..matches[0].byte_start + matches[0].byte_len],
            "let total = alpha + beta + gamma;\n"
        );
    }

    #[test]
    fn test_reflow_reports_all_ambiguous_windows() {
        let source =
            "let total = alpha +\n  beta +\n  gamma;\n\nlet total = alpha +\n  beta +\n  gamma;\n";
        let needle = "let total = alpha + beta + gamma;";
        let matches = find_all_fuzzy(source, needle);

        assert_eq!(matches.len(), 2);
        assert!(matches.iter().all(|m| m.pass == 5));
    }

    #[test]
    fn test_reflow_near_miss_does_not_match() {
        let source = "let total = alpha +\n  beta +\n  gamma;\n";
        let needle = "let total = alpha + beta + delta;";
        let matches = find_all_fuzzy(source, needle);

        assert!(matches.is_empty());
    }

    #[test]
    fn test_reflow_does_not_preempt_exact_match() {
        let source = "let total = alpha +\n  beta +\n  gamma;\nlet total = alpha + beta + gamma;\n";
        let needle = "let total = alpha + beta + gamma;";
        let matches = find_all_fuzzy(source, needle);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pass, 1);
        assert_eq!(matches[0].byte_start, source.rfind("let total").unwrap());
    }
}
