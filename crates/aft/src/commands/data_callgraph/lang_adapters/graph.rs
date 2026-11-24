//! The call-graph wrapper (owner: R2): `AdapterGraph`.
//!
//! `AdapterGraph` is a `CallGraphRead` that owns upstream's concrete `DataGraph` and, for
//! each language with an adapter, the overlay `overlay_for_query` returned for it. Upstream's
//! own graph is always the base. The overlay only adds:
//!
//! - Edges. An adapter site maps to an upstream call reference, and each of its targets to an
//!   upstream node (`mapping.rs`). The edge is added to the callers and callees upstream
//!   returns. Upstream edges are never removed. A target that upstream already has at the
//!   same call site is not added twice. Targets with a hidden tag (for example `mock`) are
//!   not added; the wrapper counts them for the reply text.
//! - Unresolved sites (decisions 3 and 4). A site the adapter analysed leaves
//!   `unresolved_calls_of` (option a: a call whose callees are all outside the root is
//!   dropped and counted). A NAMED unresolved count is lowered only under the coverage rule
//!   (CONTRACT.md section 7). The count for a whole language is never lowered.
//!
//! - Tags. A site's tags (`in_loop`, `in_defer`, ...) are kept with each edge the wrapper notes for the
//!   reply text, for an adapter edge and for an upstream edge at the same call (`ServedEdge`).
//!
//! Speed. Mapping a site to upstream's call, and a target to upstream's node, is done once per file
//! and kept in `memo.rs` (its validity rule is there). Without it the mapping was most of a warm
//! `call_tree`.
//!
//! The trait has 31 methods. Every one is written out below, as a delegation to the inner
//! reader or as an overlay. A method left to its trait default would bypass the inner reader
//! too (upstream answers would be lost), so the guard test
//! `callgraph_read_methods_are_all_reviewed` compares this impl with the trait source.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::callgraph::{CallTreeNode, TraceToResult, TraceToSymbolCandidate, TraceToSymbolResult};
use crate::callgraph_store::{
    lang_label, CallGraphRead, Result, StoreCallSite, StoreCallersResult, StoreImpactResult,
    StoreNode, StoreRefSite, StoreUnresolvedCall, StoredEdge,
};
use crate::commands::data_callgraph::DataGraph;
use crate::data_layer::query::SliceSource;
use crate::parser::LangId;

use super::contract::{
    AdapterStatus, Overlay, OverlaySite, TargetRecord, DEFAULT_QUERY_WAIT_S, UNRESOLVED_COUNT_CAP,
};
use super::mapping::{map_site, map_target, references_with_call_edge, unresolved_refs};
use super::query::{languages_for_root, overlay_for_query};

/// One language's overlay. Only a fresh or partial overlay becomes a layer.
struct Layer {
    language: String,
    overlay: Overlay,
}

/// An edge of this reply that the overlay says something about. `reply.rs` marks it in the text.
///
/// Two cases, told apart by `added`:
/// - `added`: the adapter added the edge (upstream had none at that call). The mark names the
///   adapter and the kind, and then the site's tags.
/// - not `added`: upstream resolved the call itself, and the adapter's site at the same call
///   carries tags. Only the tags are marked.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ServedEdge {
    pub language: String,
    pub added: bool,
    pub kind: String,
    /// The tags of the adapter's site (`in_loop`, `in_error_branch`, ...), in the adapter's order.
    pub tags: Vec<String>,
    pub caller_file: String,
    pub caller_symbol: String,
    pub call_line: u32,
    pub target_file: String,
    pub target_symbol: String,
    pub target_line: u32,
}

impl ServedEdge {
    /// How an added edge is named: `adapter:go (interface)`.
    pub(crate) fn label(&self) -> String {
        format!("adapter:{} ({})", self.language, self.kind)
    }

    /// The text mark: `adapter:go (interface) [in_loop, in_error_branch]`, or `[in_loop]` for an
    /// edge upstream resolved. `None` when there is nothing to say.
    pub(crate) fn mark(&self) -> Option<String> {
        let tags = (!self.tags.is_empty()).then(|| format!("[{}]", self.tags.join(", ")));
        match (self.added, tags) {
            (true, Some(tags)) => Some(format!("{} {tags}", self.label())),
            (true, None) => Some(self.label()),
            (false, tags) => tags,
        }
    }
}

#[derive(Default)]
struct Served {
    edges: BTreeSet<ServedEdge>,
    /// Hidden targets met while answering: (language, site file, site line, target file, target line).
    hidden: BTreeSet<(String, String, u32, String, u32)>,
}

#[cfg(test)]
mod known_answers;
mod memo;

#[cfg(test)]
thread_local! {
    /// Test hook: overlays that `AdapterGraph::new` uses on this thread instead of asking the
    /// seam. The parent-folder test sets it, because the wrap in `parent_query` builds the graph
    /// itself and the test has no other way to hand it a `FixtureOverlay`.
    static TEST_ANSWERS: std::cell::RefCell<Option<Vec<(Option<Overlay>, AdapterStatus)>>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) struct AdapterGraph {
    inner: DataGraph,
    layers: Vec<Layer>,
    statuses: Vec<AdapterStatus>,
    served: Mutex<Served>,
}

impl AdapterGraph {
    /// Wraps `inner` with the overlays of the languages that have an adapter for its root.
    /// ONE deadline is set for the whole reply, so two languages never wait 2 x the limit.
    pub(crate) fn new(inner: DataGraph) -> Self {
        #[cfg(test)]
        if let Some(answers) = TEST_ANSWERS.with(|cell| cell.borrow().clone()) {
            return Self::with_overlays(inner, answers);
        }
        let root = inner.project_root().to_path_buf();
        let deadline = Instant::now() + Duration::from_secs(DEFAULT_QUERY_WAIT_S);
        let answers = languages_for_root(&root)
            .into_iter()
            .map(|language| {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let (overlay, status) = overlay_for_query(&root, &language, remaining);
                (overlay, status)
            })
            .collect();
        Self::with_overlays(inner, answers)
    }

    /// Wraps `inner` with overlays already read (for tests, and for `new`).
    pub(crate) fn with_overlays(inner: DataGraph, answers: Vec<(Option<Overlay>, AdapterStatus)>) -> Self {
        let mut layers = Vec::new();
        let mut statuses = Vec::new();
        for (overlay, status) in answers {
            if let (Some(overlay), true) = (overlay, status.serves_edges()) {
                layers.push(Layer { language: status.language.clone(), overlay });
            }
            statuses.push(status);
        }
        Self { inner, layers, statuses, served: Mutex::default() }
    }

    /// The status of every language that has an adapter for this root.
    pub(crate) fn statuses(&self) -> &[AdapterStatus] {
        &self.statuses
    }

    /// The edges the adapter added to the answers given so far, in a stable order.
    pub(crate) fn served_edges(&self) -> Vec<ServedEdge> {
        self.served().edges.iter().cloned().collect()
    }

    /// How many hidden targets (for example mocks) the answers met, per language.
    pub(crate) fn hidden_counts(&self) -> Vec<(String, usize)> {
        let served = self.served();
        let mut counts: Vec<(String, usize)> = Vec::new();
        for (language, ..) in &served.hidden {
            match counts.last_mut() {
                Some((last, count)) if last == language => *count += 1,
                _ => counts.push((language.clone(), 1)),
            }
        }
        counts
    }

    /// The tags the registry hides for `language`.
    pub(crate) fn hidden_tags(&self, language: &str) -> Vec<String> {
        self.layer(language).map(|layer| layer.overlay.hidden_tags().to_vec()).unwrap_or_default()
    }

    fn served(&self) -> std::sync::MutexGuard<'_, Served> {
        self.served.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn layer(&self, language: &str) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.language == language)
    }

    fn layer_of(&self, node: &StoreNode) -> Option<&Layer> {
        self.layer(lang_label(node.lang))
    }

    /// `mapped`, the upstream node of `target`, when its tags are not hidden. `None` when the target is
    /// not an upstream function or method, or when its tags are hidden (then the target is counted
    /// for the reply text).
    fn visible_target(
        &self,
        layer: &Layer,
        site: &OverlaySite,
        target: &TargetRecord,
        mapped: Option<&StoreNode>,
    ) -> Option<StoreNode> {
        let node = mapped?;
        let hidden = layer.overlay.hidden_tags();
        if target.tags.iter().any(|tag| hidden.contains(tag)) {
            self.served().hidden.insert((
                layer.language.clone(),
                site.site.file.clone(),
                site.site.line,
                target.file.clone(),
                target.line,
            ));
            return None;
        }
        Some(node.clone())
    }

    /// Notes an edge of this reply for the text. `added`: the adapter added it. Otherwise upstream
    /// had it, and it is noted only when the site has tags to show.
    fn record_edge(
        &self,
        layer: &Layer,
        added: bool,
        site: &OverlaySite,
        kind: &str,
        caller: &StoreNode,
        call_line: u32,
        callee: &StoreNode,
    ) {
        if !added && site.site.tags.is_empty() {
            return;
        }
        self.served().edges.insert(ServedEdge {
            language: layer.language.clone(),
            added,
            kind: kind.to_string(),
            tags: site.site.tags.clone(),
            caller_file: caller.file.clone(),
            caller_symbol: caller.symbol.clone(),
            call_line,
            target_file: callee.file.clone(),
            target_symbol: callee.symbol.clone(),
            target_line: callee.line,
        });
    }

    /// Adapter edges that leave `node`: one per visible target of each site whose call
    /// upstream attributes to `node`, except those in `known` (upstream's edges, as
    /// (call byte start, target file, target symbol)). Only an edge that is returned is recorded.
    /// An upstream edge at a site with tags is recorded too, for its tags.
    fn adapter_calls_from(&self, node: &StoreNode, known: &HashSet<(usize, String, String)>) -> Vec<StoreCallSite> {
        let Some(layer) = self.layer_of(node) else { return Vec::new() };
        let Some(file_sites) = memo::file_sites(&layer.overlay, self.inner.source(), &node.file) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut calls = Vec::new();
        for entry in file_sites.entries_of_caller(node.node_id()) {
            let Some(call) = entry
                .mapped
                .as_ref()
                .and_then(|mapped| mapped.calls.iter().find(|call| call.caller.as_ref().is_some_and(|caller| caller.node_id() == node.node_id())))
            else {
                continue;
            };
            for (target, mapped_target) in entry.site.site.targets.iter().zip(&entry.targets) {
                let Some(callee) = self.visible_target(layer, &entry.site, target, mapped_target.as_ref()) else { continue };
                let key = (call.byte_start, callee.file.clone(), callee.symbol.clone());
                if known.contains(&key) {
                    self.record_edge(layer, false, &entry.site, &target.kind, node, call.line, &callee);
                    continue;
                }
                if !seen.insert(key) {
                    continue;
                }
                self.record_edge(layer, true, &entry.site, &target.kind, node, call.line, &callee);
                calls.push(StoreCallSite {
                    kind: "call".to_string(),
                    caller_is_test: None,
                    caller: node.clone(),
                    target_file: callee.file.clone(),
                    target_symbol: callee.symbol.clone(),
                    target: Some(callee),
                    line: call.line,
                    byte_start: call.byte_start,
                    byte_end: call.byte_end,
                    resolved: true,
                    provenance: format!("adapter:{} ({})", layer.language, target.kind),
                });
            }
        }
        calls
    }

    /// `upstream` plus the adapter edges that leave `node`, except those upstream already has at
    /// the same call (same byte start, same target). Unchanged when the adapter adds nothing.
    fn merge_calls_from(&self, node: &StoreNode, mut upstream: Vec<StoreCallSite>) -> Vec<StoreCallSite> {
        let known: HashSet<(usize, String, String)> = upstream
            .iter()
            .map(|call| (call.byte_start, call.target_file.clone(), call.target_symbol.clone()))
            .collect();
        let added = self.adapter_calls_from(node, &known);
        if added.is_empty() {
            return upstream;
        }
        upstream.extend(added);
        upstream.sort_by_key(|call| (call.byte_start, call.line));
        upstream
    }

    /// Adapter edges that reach the symbol `symbol` of `file`, except those whose caller and call
    /// byte start are in `known` already.
    fn adapter_callers_of(&self, file: &str, symbol: &str, known: &[StoreCallSite]) -> Vec<StoreCallSite> {
        if self.layers.is_empty() {
            return Vec::new();
        }
        let Ok(nodes) = self.inner.nodes_for_stored(file, symbol) else { return Vec::new() };
        let known: HashSet<(String, usize)> = known
            .iter()
            .map(|site| (site.caller.node_id().to_string(), site.byte_start))
            .collect();
        let mut seen = HashSet::new();
        let mut callers = Vec::new();
        for node in nodes.iter().filter(|node| node.symbol == symbol) {
            let Some(layer) = self.layer_of(node) else { continue };
            for (site, target_index) in layer.overlay.sites_targeting(file, node.line, node.end_line) {
                let target = &site.site.targets[target_index];
                let Some(callee) = map_target(self.inner.source(), &target.file, target.line, target.col) else {
                    continue;
                };
                if callee.node_id() != node.node_id() {
                    continue;
                }
                if self.visible_target(layer, &site, target, Some(&callee)).is_none() {
                    continue;
                }
                let Some(slice) = self.inner.source().slice(&site.site.file) else { continue };
                let Some(mapped) = map_site(&slice, site.offset) else { continue };
                for call in mapped.calls {
                    let Some(caller) = call.caller else { continue };
                    let key = (caller.node_id().to_string(), call.byte_start);
                    if known.contains(&key) {
                        self.record_edge(layer, false, &site, &target.kind, &caller, call.line, node);
                        continue;
                    }
                    if !seen.insert(key) {
                        continue;
                    }
                    self.record_edge(layer, true, &site, &target.kind, &caller, call.line, node);
                    callers.push(StoreCallSite {
                        kind: "call".to_string(),
                        caller_is_test: call.caller_is_test,
                        caller,
                        target_file: file.to_string(),
                        target_symbol: symbol.to_string(),
                        target: Some(node.clone()),
                        line: call.line,
                        byte_start: call.byte_start,
                        byte_end: call.byte_end,
                        resolved: true,
                        provenance: format!("adapter:{} ({})", layer.language, target.kind),
                    });
                }
            }
        }
        callers
    }

    /// `upstream` callers of (`file`, `symbol`) plus the adapter's.
    fn merge_callers(&self, file: &str, symbol: &str, mut upstream: Vec<StoreCallSite>) -> Vec<StoreCallSite> {
        let added = self.adapter_callers_of(file, symbol, &upstream);
        if added.is_empty() {
            return upstream;
        }
        upstream.extend(added);
        upstream.sort_by_key(|site| (site.caller.node_id().to_string(), site.byte_start));
        upstream
    }

    /// Lowers the exact list of unresolved sites named `name` (coverage rule, CONTRACT.md section
    /// 7, rules 1 and 2; rule 3 is the caller's). `all` is upstream's complete list. The list is
    /// returned unchanged when some upstream node of that name lies outside the analysed files.
    fn lower_named(
        &self,
        layer: &Layer,
        name: &str,
        language: LangId,
        mut all: Vec<StoreRefSite>,
    ) -> Result<Vec<StoreRefSite>> {
        let nodes = self.inner.nodes_matching(name)?;
        let outside = nodes
            .iter()
            .any(|node| node.name == name && node.lang == language && !layer.overlay.is_analysed(&node.file));
        if outside {
            return Ok(all);
        }
        let source = self.inner.source();
        let label = lang_label(language);
        let files: BTreeSet<String> = source
            .files_with_ref_name(name)
            .intersection(&source.files_of_language(label))
            .cloned()
            .collect();
        for file in files {
            let Some(file_sites) = memo::file_sites(&layer.overlay, source, &file) else { continue };
            if file_sites.entries.is_empty() {
                continue;
            }
            let Some(slice) = source.slice(&file) else { continue };
            let with_call_edge = references_with_call_edge(source, &slice);
            let mut spans = HashSet::new();
            for entry in file_sites.entries.iter().filter(|entry| entry.all_targets_mapped) {
                let Some(mapped) = &entry.mapped else { continue };
                if !spans.insert(mapped.span) {
                    continue;
                }
                let refs = unresolved_refs(&slice, &with_call_edge, mapped, Some(name));
                // Upstream's own list says whether it counted the span (its test filter
                // decides): drop one listed site that matches one of the span's references.
                let listed = all.iter().position(|listed| {
                    listed.file == file
                        && refs.iter().any(|reference| {
                            let caller = reference.caller().and_then(|handle| slice.node(handle));
                            listed.line == reference.line()
                                && listed.caller_symbol.as_deref() == caller.map(|node| node.scoped_name())
                        })
                });
                if let Some(position) = listed {
                    all.remove(position);
                }
            }
        }
        Ok(all)
    }
}

impl CallGraphRead for AdapterGraph {
    // ---- identity and plain lookups: delegate ----

    fn project_root(&self) -> &Path {
        self.inner.project_root()
    }

    fn project_key(&self) -> &str {
        self.inner.project_key()
    }

    fn sqlite_path(&self) -> &Path {
        self.inner.sqlite_path()
    }

    fn is_current(&self) -> bool {
        self.inner.is_current()
    }

    /// Delegate. The gates compare upstream's own graphs; an overlay must not leak into them.
    fn edge_snapshot(&self) -> Result<BTreeSet<StoredEdge>> {
        self.inner.edge_snapshot()
    }

    fn indexed_file_count(&self) -> Result<usize> {
        self.inner.indexed_file_count()
    }

    fn node_for(&self, file_rel: &Path, symbol: &str) -> Result<StoreNode> {
        self.inner.node_for(file_rel, symbol)
    }

    fn nodes_for(&self, file_rel: &Path, symbol: &str) -> Result<Vec<StoreNode>> {
        self.inner.nodes_for(file_rel, symbol)
    }

    fn nodes_for_stored(&self, file: &str, symbol: &str) -> Result<Vec<StoreNode>> {
        self.inner.nodes_for_stored(file, symbol)
    }

    fn stored_content_hash(&self, file: &str) -> Result<Option<String>> {
        self.inner.stored_content_hash(file)
    }

    fn nodes_matching(&self, symbol: &str) -> Result<Vec<StoreNode>> {
        self.inner.nodes_matching(symbol)
    }

    // ---- callers: overlay ----

    fn direct_callers_of(&self, file_rel: &Path, symbol: &str) -> Result<Vec<StoreCallSite>> {
        let upstream = self.inner.direct_callers_of(file_rel, symbol)?;
        if self.layers.is_empty() {
            return Ok(upstream);
        }
        // `file_rel` may be absolute: the nodes carry the store-relative file.
        let Some(file) = self.inner.nodes_for(file_rel, symbol)?.first().map(|node| node.file.clone()) else {
            return Ok(upstream);
        };
        Ok(self.merge_callers(&file, symbol, upstream))
    }

    fn direct_callers_for_symbols(
        &self,
        targets: &[(String, String)],
    ) -> Result<HashMap<(String, String), Vec<StoreCallSite>>> {
        let mut by_target = self.inner.direct_callers_for_symbols(targets)?;
        if self.layers.is_empty() {
            return Ok(by_target);
        }
        for target in targets {
            let upstream = by_target.remove(target).unwrap_or_default();
            by_target.insert(target.clone(), self.merge_callers(&target.0, &target.1, upstream));
        }
        Ok(by_target)
    }

    fn direct_caller_counts_of(&self, targets: &[(String, String)]) -> Result<HashMap<(String, String), usize>> {
        if self.layers.is_empty() {
            return self.inner.direct_caller_counts_of(targets);
        }
        // The same grouping upstream counts with: one per (caller file, line, kind).
        let sites = self.direct_callers_for_symbols(targets)?;
        Ok(sites
            .into_iter()
            .map(|(target, sites)| {
                let groups: BTreeSet<_> =
                    sites.into_iter().map(|site| (site.caller.file, site.line, site.kind)).collect();
                (target, groups.len())
            })
            .collect())
    }

    fn direct_callers_bounded(
        &self,
        targets: &[(String, String)],
        limit: usize,
    ) -> Result<(HashMap<(String, String), Vec<StoreCallSite>>, bool, usize)> {
        let (mut by_target, exhausted, mut examined) = self.inner.direct_callers_bounded(targets, limit)?;
        if self.layers.is_empty() {
            return Ok((by_target, exhausted, examined));
        }
        for target in targets {
            let upstream = by_target.remove(target).unwrap_or_default();
            let before = upstream.len();
            let merged = self.merge_callers(&target.0, &target.1, upstream);
            examined += merged.len() - before;
            by_target.insert(target.clone(), merged);
        }
        Ok((by_target, exhausted, examined))
    }

    // ---- callees: overlay ----

    fn outgoing_calls_for_symbols(
        &self,
        sources: &[(String, String)],
    ) -> Result<HashMap<(String, String), Vec<StoreCallSite>>> {
        let mut by_source = self.inner.outgoing_calls_for_symbols(sources)?;
        if self.layers.is_empty() {
            return Ok(by_source);
        }
        for source in sources {
            let nodes = self.inner.nodes_for_stored(&source.0, &source.1)?;
            let mut calls = by_source.remove(source).unwrap_or_default();
            let known: HashSet<(usize, String, String)> = calls
                .iter()
                .map(|call| (call.byte_start, call.target_file.clone(), call.target_symbol.clone()))
                .collect();
            let mut added = Vec::new();
            for node in nodes.iter().filter(|node| node.symbol == source.1) {
                added.extend(self.adapter_calls_from(node, &known));
            }
            if !added.is_empty() {
                calls.extend(added);
                calls.sort_by_key(|call| (call.byte_start, call.line));
            }
            by_source.insert(source.clone(), calls);
        }
        Ok(by_source)
    }

    fn outgoing_calls_of(&self, node: &StoreNode) -> Result<Vec<StoreCallSite>> {
        let upstream = self.inner.outgoing_calls_of(node)?;
        if self.layers.is_empty() {
            return Ok(upstream);
        }
        Ok(self.merge_calls_from(node, upstream))
    }

    /// Delegate. Upstream's self-recursive calls without an edge; an adapter edge that
    /// recurses arrives through `outgoing_calls_of`.
    fn resolved_self_calls_of(&self, node: &StoreNode) -> Result<Vec<StoreCallSite>> {
        self.inner.resolved_self_calls_of(node)
    }

    // ---- composed by the command builders from the direct lookups: delegate ----

    fn callers_of(&self, file_rel: &Path, symbol: &str, depth: usize) -> Result<StoreCallersResult> {
        self.inner.callers_of(file_rel, symbol, depth)
    }

    fn impact_of(&self, file_rel: &Path, symbol: &str, depth: usize) -> Result<StoreImpactResult> {
        self.inner.impact_of(file_rel, symbol, depth)
    }

    fn call_tree(&self, file_rel: &Path, symbol: &str, depth: usize) -> Result<CallTreeNode> {
        self.inner.call_tree(file_rel, symbol, depth)
    }

    fn trace_to(&self, file_rel: &Path, symbol: &str, max_depth: usize) -> Result<TraceToResult> {
        self.inner.trace_to(file_rel, symbol, max_depth)
    }

    fn trace_to_symbol_candidates(&self, to_symbol: &str) -> Result<Vec<TraceToSymbolCandidate>> {
        self.inner.trace_to_symbol_candidates(to_symbol)
    }

    fn trace_to_symbol(
        &self,
        file_rel: &Path,
        symbol: &str,
        to_symbol: &str,
        to_file: Option<&Path>,
        max_depth: usize,
    ) -> Result<TraceToSymbolResult> {
        self.inner.trace_to_symbol(file_rel, symbol, to_symbol, to_file, max_depth)
    }

    // ---- unresolved sites ----

    /// Overlay. A call the adapter analysed is no longer unresolved (option a).
    fn unresolved_calls_of(&self, node: &StoreNode) -> Result<Vec<StoreUnresolvedCall>> {
        let mut calls = self.inner.unresolved_calls_of(node)?;
        let Some(layer) = self.layer_of(node) else { return Ok(calls) };
        if calls.is_empty() {
            return Ok(calls);
        }
        let Some(file_sites) = memo::file_sites(&layer.overlay, self.inner.source(), &node.file) else { return Ok(calls) };
        calls.retain(|call| !file_sites.covered_spans.contains(&(call.byte_start as u64, call.byte_end as u64)));
        Ok(calls)
    }

    fn ref_sites_named(&self, kind: &str, short_name: &str, limit: usize) -> Result<Vec<StoreRefSite>> {
        self.inner.ref_sites_named(kind, short_name, limit)
    }

    fn macro_template_calls_of(&self, node: &StoreNode) -> Result<Vec<StoreUnresolvedCall>> {
        self.inner.macro_template_calls_of(node)
    }

    /// Delegate. Only the gates read it.
    fn unresolved_method_sites_named(&self, name: &str, language: LangId) -> Result<Vec<StoreRefSite>> {
        self.inner.unresolved_method_sites_named(name, language)
    }

    /// Overlay, named count only. The list is exact, so no cap applies.
    fn unresolved_call_sites_named(
        &self,
        name: &str,
        language: LangId,
        include_tests: bool,
        limit: usize,
    ) -> Result<(usize, Vec<StoreRefSite>)> {
        let Some(layer) = self.layer(lang_label(language)) else {
            return self.inner.unresolved_call_sites_named(name, language, include_tests, limit);
        };
        let (total, all) = self.inner.unresolved_call_sites_named(name, language, include_tests, usize::MAX)?;
        let mut remaining = if total == 0 { all } else { self.lower_named(layer, name, language, all)? };
        let total = remaining.len();
        remaining.truncate(limit);
        Ok((total, remaining))
    }

    /// Delegate. The count for a whole language is never lowered (decision 3).
    fn unresolved_call_site_count(&self, language: LangId, include_tests: bool) -> Result<usize> {
        self.inner.unresolved_call_site_count(language, include_tests)
    }

    /// Overlay, under the coverage rule. Upstream stops counting at 1,000: that is a lower bound,
    /// so it passes through.
    fn unresolved_call_site_count_named(&self, name: &str, language: LangId, include_tests: bool) -> Result<usize> {
        let counted = self.inner.unresolved_call_site_count_named(name, language, include_tests)?;
        if counted == 0 || counted >= UNRESOLVED_COUNT_CAP {
            return Ok(counted);
        }
        let Some(layer) = self.layer(lang_label(language)) else { return Ok(counted) };
        let (_, all) = self.inner.unresolved_call_sites_named(name, language, include_tests, usize::MAX)?;
        Ok(self.lower_named(layer, name, language, all)?.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Delegate` returns what the inner reader returns. `Overlay` merges the overlay.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Review {
        Delegate,
        Overlay,
    }

    /// Every method of `CallGraphRead`, reviewed. A new upstream method fails the guard test
    /// until someone decides which of the two it is.
    const REVIEWED: [(&str, Review); 31] = [
        ("project_root", Review::Delegate),
        ("project_key", Review::Delegate),
        ("sqlite_path", Review::Delegate),
        ("is_current", Review::Delegate),
        ("edge_snapshot", Review::Delegate),
        ("indexed_file_count", Review::Delegate),
        ("node_for", Review::Delegate),
        ("nodes_for", Review::Delegate),
        ("nodes_for_stored", Review::Delegate),
        ("stored_content_hash", Review::Delegate),
        ("nodes_matching", Review::Delegate),
        ("direct_callers_of", Review::Overlay),
        ("direct_callers_for_symbols", Review::Overlay),
        ("direct_caller_counts_of", Review::Overlay),
        ("direct_callers_bounded", Review::Overlay),
        ("outgoing_calls_for_symbols", Review::Overlay),
        ("callers_of", Review::Delegate),
        ("impact_of", Review::Delegate),
        ("outgoing_calls_of", Review::Overlay),
        ("resolved_self_calls_of", Review::Delegate),
        ("unresolved_calls_of", Review::Overlay),
        ("ref_sites_named", Review::Delegate),
        ("macro_template_calls_of", Review::Delegate),
        ("unresolved_method_sites_named", Review::Delegate),
        ("unresolved_call_sites_named", Review::Overlay),
        ("unresolved_call_site_count", Review::Delegate),
        ("unresolved_call_site_count_named", Review::Overlay),
        ("call_tree", Review::Delegate),
        ("trace_to", Review::Delegate),
        ("trace_to_symbol_candidates", Review::Delegate),
        ("trace_to_symbol", Review::Delegate),
    ];

    /// The `fn` names declared at the top level of the block that starts at the line holding
    /// `header`. Bodies and nested items are indented deeper, so only four-space `fn` lines count.
    fn method_names(source: &str, header: &str) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        let mut inside = false;
        for line in source.lines() {
            if !inside {
                inside = line.contains(header) && line.trim_end().ends_with('{');
                continue;
            }
            if line.starts_with('}') {
                break;
            }
            if let Some(rest) = line.strip_prefix("    fn ") {
                let name: String = rest.chars().take_while(|ch| ch.is_alphanumeric() || *ch == '_').collect();
                names.insert(name);
            }
        }
        names
    }

    #[test]
    fn callgraph_read_methods_are_all_reviewed() {
        // Read at run time: upstream's `build_path_independence` test forbids the compile-time macro.
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR for tests");
        let trait_path = Path::new(&manifest).join("../aft-parse/src/callgraph_store/mod.rs");
        let trait_source = std::fs::read_to_string(&trait_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", trait_path.display()));
        let upstream = method_names(&trait_source, "pub trait CallGraphRead");
        let reviewed: BTreeSet<String> = REVIEWED.iter().map(|(name, _)| name.to_string()).collect();
        let implemented = method_names(include_str!("graph.rs"), "impl CallGraphRead for AdapterGraph");

        let added: Vec<_> = upstream.difference(&reviewed).collect();
        let removed: Vec<_> = reviewed.difference(&upstream).collect();
        assert!(
            added.is_empty() && removed.is_empty(),
            "CallGraphRead changed (added: {added:?}, removed: {removed:?}). For each new method, decide: \
             does it return edges, callers, callees or unresolved sites? If yes, AdapterGraph must merge \
             the overlay in it (and subtract covered sites from unresolved counts); if no, delegate it \
             explicitly to the inner reader. A method left to its trait default bypasses the inner reader \
             and the overlay. Then add it to REVIEWED and to the impl block."
        );
        let missing: Vec<_> = upstream.difference(&implemented).collect();
        let extra: Vec<_> = implemented.difference(&upstream).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "AdapterGraph must implement every CallGraphRead method itself (missing: {missing:?}, not in the trait: {extra:?}); \
             a method left to its trait default bypasses the inner reader"
        );
        assert_eq!(upstream.len(), 31, "the trait has 31 methods");
        assert!(REVIEWED.iter().filter(|(_, review)| *review == Review::Overlay).count() >= 9);
    }
}
