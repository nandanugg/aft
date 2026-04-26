use std::collections::HashSet;
use std::path::Path;

use crate::callgraph::{CallTreeNode, CallersResult, ImpactResult, TraceToResult};
use crate::context::AppContext;
use crate::error::AftError;
use crate::symbols::Symbol;

pub fn resolve_symbol_query(
    ctx: &AppContext,
    file: &Path,
    requested: &str,
) -> Result<String, AftError> {
    let symbols = ctx.provider().list_symbols(file)?;
    let query = strip_outline_range_suffix(requested.trim());

    if let Some(sym) = symbols.iter().find(|sym| sym.name == query) {
        return Ok(sym.name.clone());
    }
    if let Some(sym) = symbols
        .iter()
        .find(|sym| sym.signature.as_deref() == Some(query))
    {
        return Ok(sym.name.clone());
    }

    let candidates = candidate_queries(query);
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for sym in &symbols {
        if candidates
            .iter()
            .any(|candidate| symbol_matches_query(sym, candidate))
            && seen.insert(sym.name.clone())
        {
            names.push(sym.name.clone());
        }
    }

    match names.len() {
        1 => Ok(names.remove(0)),
        0 => Err(AftError::SymbolNotFound {
            name: requested.to_string(),
            file: file.display().to_string(),
        }),
        _ => Err(AftError::AmbiguousSymbol {
            name: requested.to_string(),
            candidates: names,
        }),
    }
}

pub fn filter_callers_result(result: &mut CallersResult, exclude_tests: bool) {
    if !exclude_tests {
        return;
    }
    result.callers.retain_mut(|group| {
        if path_is_test_like(&group.file) {
            return false;
        }
        true
    });
    result.total_callers = result.callers.iter().map(|group| group.callers.len()).sum();
}

pub fn filter_impact_result(result: &mut ImpactResult, exclude_tests: bool) {
    if !exclude_tests {
        return;
    }
    result
        .callers
        .retain(|caller| !path_is_test_like(&caller.caller_file));
    result.total_affected = result.callers.len();
    result.affected_files = result
        .callers
        .iter()
        .map(|caller| caller.caller_file.clone())
        .collect::<HashSet<_>>()
        .len();
}

pub fn filter_trace_to_result(result: &mut TraceToResult, exclude_tests: bool) {
    if !exclude_tests {
        return;
    }
    result
        .paths
        .retain(|path| !path.hops.iter().any(|hop| path_is_test_like(&hop.file)));
    result.total_paths = result.paths.len();
    result.entry_points_found = result
        .paths
        .iter()
        .filter_map(|path| path.hops.first())
        .filter(|hop| hop.is_entry_point)
        .map(|hop| (hop.file.clone(), hop.symbol.clone()))
        .collect::<HashSet<_>>()
        .len();
}

pub fn filter_call_tree(tree: &mut CallTreeNode, exclude_tests: bool) {
    if !exclude_tests {
        return;
    }
    for child in &mut tree.children {
        filter_call_tree(child, true);
    }
    tree.children
        .retain(|child| !path_is_test_like(&child.file));
}

pub fn path_is_test_like(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let mut parts = normalized.split('/');
    let file = parts.next_back().unwrap_or(normalized.as_str());

    if normalized
        .split('/')
        .any(|part| matches!(part, "test" | "tests" | "__tests__"))
    {
        return true;
    }

    file.ends_with("_test.go")
        || file.ends_with("_test.rs")
        || file.ends_with("_test.py")
        || file.starts_with("test_")
        || file.contains(".test.")
        || file.contains(".spec.")
}

fn candidate_queries(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    push_unique(&mut out, query);
    if let Some(name) = extract_signature_symbol(query) {
        push_unique(&mut out, &name);
    }
    let normalized = normalize_qualified_tail(query);
    push_unique(&mut out, &normalized);
    out
}

fn symbol_matches_query(sym: &Symbol, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    if sym.name == query {
        return true;
    }
    if sym.signature.as_deref() == Some(query) {
        return true;
    }

    symbol_forms(sym).iter().any(|form| form == query)
}

fn symbol_forms(sym: &Symbol) -> Vec<String> {
    let mut out = vec![sym.name.clone()];
    if let Some(sig) = &sym.signature {
        push_unique(&mut out, sig);
    }
    if !sym.scope_chain.is_empty() {
        let joined_dot = format!("{}.{}", sym.scope_chain.join("."), sym.name);
        let joined_scope = format!("{}::{}", sym.scope_chain.join("::"), sym.name);
        push_unique(&mut out, &joined_dot);
        push_unique(&mut out, &joined_scope);
        if let Some(parent) = sym.scope_chain.last() {
            push_unique(&mut out, &format!("{}.{}", parent, sym.name));
            push_unique(&mut out, &format!("({}).{}", parent, sym.name));
            push_unique(&mut out, &format!("(*{}).{}", parent, sym.name));
        }
    }
    out
}

fn extract_signature_symbol(query: &str) -> Option<String> {
    for marker in ["func ", "fn ", "def "] {
        if let Some(idx) = query.find(marker) {
            return extract_name_after_keyword(&query[idx + marker.len()..]);
        }
    }
    None
}

fn extract_name_after_keyword(rest: &str) -> Option<String> {
    let mut s = rest.trim();
    if s.starts_with('(') {
        let mut depth = 0usize;
        let mut end = None;
        for (idx, ch) in s.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(idx + ch.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(idx) = end {
            s = s[idx..].trim_start();
        }
    }
    let token = s
        .split(|ch: char| ch == '(' || ch.is_whitespace())
        .next()
        .unwrap_or_default();
    let normalized = normalize_qualified_tail(token);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn normalize_qualified_tail(query: &str) -> String {
    let mut tail = strip_outline_range_suffix(query.trim());
    if let Some(idx) = tail.rfind("::") {
        tail = &tail[idx + 2..];
    }
    if let Some(idx) = tail.rfind(").") {
        tail = &tail[idx + 2..];
    } else if let Some(idx) = tail.rfind('.') {
        tail = &tail[idx + 1..];
    }
    if let Some(idx) = tail.find('(') {
        tail = &tail[..idx];
    }
    let token = tail.split_whitespace().last().unwrap_or_default();
    token
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .to_string()
}

fn strip_outline_range_suffix(query: &str) -> &str {
    let trimmed = query.trim();
    let Some((head, tail)) = trimmed.rsplit_once(' ') else {
        return trimmed;
    };
    if looks_like_line_range(tail) {
        head.trim_end()
    } else {
        trimmed
    }
}

fn looks_like_line_range(token: &str) -> bool {
    let Some((start, end)) = token.split_once(':') else {
        return false;
    };
    !start.is_empty()
        && !end.is_empty()
        && start.chars().all(|ch| ch.is_ascii_digit())
        && end.chars().all(|ch| ch.is_ascii_digit())
}

fn push_unique(out: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !out.iter().any(|existing| existing == value) {
        out.push(value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use crate::symbols::{Range, Symbol, SymbolKind};

    use super::{
        extract_signature_symbol, normalize_qualified_tail, path_is_test_like, symbol_matches_query,
    };

    fn method_symbol() -> Symbol {
        Symbol {
            name: "ProcessMerchantSettlementV3".to_string(),
            kind: SymbolKind::Method,
            range: Range {
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 0,
            },
            signature: Some(
                "func (s *SettlementService) ProcessMerchantSettlementV3(ctx context.Context) error"
                    .to_string(),
            ),
            scope_chain: vec!["SettlementService".to_string()],
            exported: true,
            parent: Some("SettlementService".to_string()),
        }
    }

    #[test]
    fn extracts_symbol_name_from_go_signature() {
        assert_eq!(
            extract_signature_symbol(
                "E fn   func (s *SettlementService) ProcessMerchantSettlementV3(ctx context.Context) error 42:57"
            ),
            Some("ProcessMerchantSettlementV3".to_string())
        );
    }

    #[test]
    fn normalizes_receiver_qualified_queries() {
        assert_eq!(
            normalize_qualified_tail("(*SettlementService).ProcessMerchantSettlementV3"),
            "ProcessMerchantSettlementV3"
        );
        assert_eq!(
            normalize_qualified_tail("settlement.Service::ProcessMerchantSettlementV3"),
            "ProcessMerchantSettlementV3"
        );
    }

    #[test]
    fn symbol_query_matches_outline_style_method_inputs() {
        let symbol = method_symbol();
        assert!(symbol_matches_query(
            &symbol,
            "func (s *SettlementService) ProcessMerchantSettlementV3(ctx context.Context) error"
        ));
        assert!(symbol_matches_query(
            &symbol,
            "SettlementService.ProcessMerchantSettlementV3"
        ));
        assert!(symbol_matches_query(
            &symbol,
            "(*SettlementService).ProcessMerchantSettlementV3"
        ));
    }

    #[test]
    fn test_path_detection_catches_common_patterns() {
        assert!(path_is_test_like("foo/bar/service_test.go"));
        assert!(path_is_test_like("foo/tests/service.ts"));
        assert!(path_is_test_like("foo/test_helpers.ts"));
        assert!(!path_is_test_like("foo/service.go"));
    }
}
