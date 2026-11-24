//! Position mapping (owner: R2): adapter positions -> upstream nodes and call references.
//!
//! An adapter names places, not symbols (`docs/adapters/CONTRACT.md` section 5.1):
//! - A site position is inside one call and outside every call nested in it. The
//!   call is the innermost upstream `call` or `construction` reference whose byte
//!   span holds the position. Its `caller()` is the caller node (upstream's own
//!   attribution, so the adapter needs no naming rule).
//! - A target position is the declared name of the callee. The callee is the
//!   upstream `function` or `method` node with the greatest `(start_line, start_col)`
//!   that is not after the position, if its `end_line` reaches the position's line.
//!
//! Everything here reads one upstream `FileSlice` and changes nothing.

use std::collections::HashSet;

use crate::callgraph_store::{lang_from_label, EntryFacts, StoreNode};
use crate::data_layer::query::SliceSource;
use crate::data_layer::slice::{EdgeView, FileSlice, NodeView, RefView};
use crate::parser::LangId;

/// The upstream node for a slice node, built exactly as upstream's own reader builds it
/// (`data_layer/query.rs` `store_node`), so an adapter edge and an upstream edge to the
/// same symbol carry equal nodes.
pub(crate) fn store_node(slice: &FileSlice, node: NodeView<'_>) -> StoreNode {
    StoreNode::from_parts(
        node.id().to_string(),
        slice.path().to_string(),
        node.scoped_name().to_string(),
        node.name().to_string(),
        node.kind().to_string(),
        node.start_line().saturating_add(1),
        node.end_line().saturating_add(1),
        node.signature(),
        node.exported(),
        node.entry_point(),
        lang_from_label(slice.language()).unwrap_or(LangId::TypeScript),
    )
    .with_entry(EntryFacts {
        is_test: node.is_test(),
        program_start: node.program_start(),
        entry_attribute: node.entry_attribute().map(str::to_string),
        default_export: node.is_default_export(),
    })
}

/// One upstream call reference that an adapter site maps to.
#[derive(Debug, Clone)]
pub(crate) struct MappedCall {
    /// Position of the reference in its slice.
    pub index: u32,
    pub line: u32,
    pub byte_start: usize,
    pub byte_end: usize,
    /// The caller node; `None` when the reference has no caller node.
    pub caller: Option<StoreNode>,
    pub caller_is_test: Option<bool>,
}

/// The upstream calls at one adapter site: the references that share the
/// innermost span holding the site's offset.
#[derive(Debug, Clone)]
pub(crate) struct MappedSite {
    pub span: (u64, u64),
    pub calls: Vec<MappedCall>,
}

/// Maps a byte offset to the innermost `call` or `construction` references
/// holding it. Several references can share one span (a call inside a nested
/// function is credited to the functions around it); all of them are returned.
pub(crate) fn map_site(slice: &FileSlice, offset: u64) -> Option<MappedSite> {
    let mut span: Option<(u64, u64)> = None;
    let mut indexes: Vec<u32> = Vec::new();
    for reference in slice.references() {
        if !matches!(reference.kind(), "call" | "construction") {
            continue;
        }
        let (start, end) = (reference.byte_start(), reference.byte_end());
        if offset < start || offset >= end {
            continue;
        }
        match span {
            Some(best) if end - start > best.1 - best.0 => {}
            Some(best) if (start, end) == best => indexes.push(reference.index()),
            _ => {
                span = Some((start, end));
                indexes.clear();
                indexes.push(reference.index());
            }
        }
    }
    let span = span?;
    let calls = indexes
        .into_iter()
        .map(|index| {
            let reference = slice.reference(index as usize);
            let caller = reference.caller().and_then(|handle| slice.node(handle));
            MappedCall {
                index,
                line: reference.line(),
                byte_start: usize::try_from(reference.byte_start()).unwrap_or(usize::MAX),
                byte_end: usize::try_from(reference.byte_end()).unwrap_or(usize::MAX),
                caller_is_test: caller.and_then(|node| node.is_test()),
                caller: caller.map(|node| store_node(slice, node)),
            }
        })
        .collect();
    Some(MappedSite { span, calls })
}

/// Maps a declared-name position (1-based line, 1-based byte column) to the
/// upstream `function` or `method` node that holds it.
pub(crate) fn map_target(source: &impl SliceSource, file: &str, line: u32, col: u32) -> Option<StoreNode> {
    let slice = source.slice(file)?;
    map_target_in(&slice, line, col)
}

/// `map_target` on a slice already in hand.
pub(crate) fn map_target_in(slice: &FileSlice, line: u32, col: u32) -> Option<StoreNode> {
    let wanted = (line.checked_sub(1)?, col.checked_sub(1)?);
    let node = slice
        .nodes()
        .filter(|node| matches!(node.kind(), "function" | "method"))
        .filter(|node| (node.start_line(), node.start_col()) <= wanted && node.end_line() >= wanted.0)
        .max_by_key(|node| (node.start_line(), node.start_col()))?;
    Some(store_node(slice, node))
}

// ---------------------------------------------------------------------------
// The unresolved-site predicate (repeats upstream's rule, `data_layer/query.rs`
// `visit_unresolved_spans`; a known-answer test guards it).
// ---------------------------------------------------------------------------

/// Whether upstream treats `edge` as a call edge (`query.rs` `edge_kind`): a
/// construction counts as a call when its target is a function or method.
fn edge_is_call(source: &impl SliceSource, edge: EdgeView<'_>) -> bool {
    if edge.kind() != "construction" {
        return edge.kind() == "call";
    }
    source.slice(edge.target_file()).is_some_and(|target| {
        target
            .target_node(edge.target_symbol(), edge.lookup())
            .and_then(|handle| target.node_kind(handle))
            .is_some_and(|kind| matches!(kind, "function" | "method"))
    })
}

/// The references of `slice` that already have a call edge.
pub(crate) fn references_with_call_edge(source: &impl SliceSource, slice: &FileSlice) -> HashSet<u32> {
    slice
        .edges()
        .filter_map(|edge| edge_is_call(source, edge).then(|| edge.reference_index()).flatten())
        .collect()
}

/// The references of the span of `site` that upstream counts as an unresolved
/// call: `call` or `construction`, not resolved, with no call edge, and, when
/// `name` is given, with that short name. Empty when upstream does not count
/// the span.
pub(crate) fn unresolved_refs<'s>(
    slice: &'s FileSlice,
    with_call_edge: &HashSet<u32>,
    site: &MappedSite,
    name: Option<&str>,
) -> Vec<RefView<'s>> {
    site.calls
        .iter()
        .map(|call| slice.reference(call.index as usize))
        .filter(|reference| !reference.resolved() && !with_call_edge.contains(&reference.index()))
        .filter(|reference| name.is_none_or(|name| reference.short_name() == Some(name)))
        .collect()
}
