//! The per-file mapping memo for `AdapterGraph`.
//!
//! Mapping a site offset to an upstream call (`map_site`) scans every reference of the file, and
//! mapping a target (`map_target`) scans every node of the target's file. Done for each site on
//! each query, that was about 40 % of a warm `call_tree`. The answer cannot change while its
//! inputs are the same, so it is computed once per (overlay, file) and kept for the process.
//!
//! VALIDITY RULE. A `FileSites` is served only while ALL of these hold:
//! - the overlay is the same object (the overlay is read-only; a new key, a rebuilt file or other
//!   `hidden_tags` makes `store.rs` hand out a new `Arc`);
//! - the caller file's upstream slice is the same object (a slice is immutable: a changed or
//!   re-decoded file is a new `Arc`, so a stale mapping is never served);
//! - every target file's upstream slice is the same object (they are checked one by one).
//! "The same object" is the address of the `Arc`. The memo holds a `Weak` to each one, which keeps
//! the allocation (not the value) alive, so an address cannot be reused while the memo has it.
//! The memo never holds a slice alive, so it does not add to the slice cache's memory.
//!
//! The memo is bounded: `MAX_OVERLAYS` overlays, `MAX_FILES` files per overlay (it is cleared
//! when full). A miss only costs the work that was done on every query before.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::callgraph_store::StoreNode;
use crate::data_layer::query::SliceSource;
use crate::data_layer::slice::FileSlice;

use super::super::contract::{Overlay, OverlayRead, OverlaySite};
use super::super::mapping::{map_site, map_target_in, MappedSite};

const MAX_OVERLAYS: usize = 4;
const MAX_FILES: usize = 512;

/// One adapter site of a file, with everything the graph needs from upstream for it.
pub(super) struct SiteEntry {
    pub site: OverlaySite,
    /// The upstream calls at the site (`None`: the site matches no upstream call).
    pub mapped: Option<MappedSite>,
    /// The upstream node of each target of the site, in the order of `site.site.targets`.
    pub targets: Vec<Option<StoreNode>>,
    /// Every target is an upstream function or method (true too when the site has no target).
    /// Only such a site stands in for an unresolved upstream call.
    pub all_targets_mapped: bool,
}

/// The sites of one file in one overlay, mapped. See the module doc for when it is valid.
pub(super) struct FileSites {
    /// The caller file's slice. `None` when the overlay has no site in the file (then nothing
    /// depends on upstream, and the entry never goes stale).
    slice: Option<Weak<FileSlice>>,
    /// The slice of every target file, `None` where the file has no slice.
    targets_slices: Vec<(String, Option<Weak<FileSlice>>)>,
    /// The sites, in the overlay's order.
    pub entries: Vec<SiteEntry>,
    /// Indexes into `entries`, by the node id of the caller of the site's call.
    by_caller: HashMap<String, Vec<usize>>,
    /// The spans of the sites that stand in for an unresolved upstream call.
    pub covered_spans: HashSet<(u64, u64)>,
}

impl FileSites {
    /// The entries whose call upstream attributes to the node `node_id`, in the overlay's order.
    pub(super) fn entries_of_caller(&self, node_id: &str) -> impl Iterator<Item = &SiteEntry> {
        self.by_caller.get(node_id).into_iter().flatten().map(|index| &self.entries[*index])
    }

    fn is_valid(&self, source: &impl SliceSource, file: &str) -> bool {
        let Some(slice) = &self.slice else { return true };
        let same = |kept: &Weak<FileSlice>, now: Option<Arc<FileSlice>>| {
            now.is_some_and(|now| std::ptr::eq(Weak::as_ptr(kept), Arc::as_ptr(&now)))
        };
        same(slice, source.slice(file))
            && self.targets_slices.iter().all(|(target_file, kept)| match (kept, source.slice(target_file)) {
                (Some(kept), now) => same(kept, now),
                (None, now) => now.is_none(),
            })
    }

    fn build(overlay: &Overlay, source: &impl SliceSource, file: &str) -> Option<Self> {
        let sites = overlay.sites_in_file(file);
        if sites.is_empty() {
            return Some(Self {
                slice: None,
                targets_slices: Vec::new(),
                entries: Vec::new(),
                by_caller: HashMap::new(),
                covered_spans: HashSet::new(),
            });
        }
        let slice = source.slice(file)?;
        let mut target_slices: Vec<(String, Option<Arc<FileSlice>>)> = Vec::new();
        let mut entries = Vec::with_capacity(sites.len());
        for site in sites {
            let mapped = map_site(&slice, site.offset);
            let mut targets = Vec::with_capacity(site.site.targets.len());
            for target in &site.site.targets {
                let position = match target_slices.iter().position(|(name, _)| *name == target.file) {
                    Some(position) => position,
                    None => {
                        target_slices.push((target.file.clone(), source.slice(&target.file)));
                        target_slices.len() - 1
                    }
                };
                let target_slice = target_slices[position].1.as_ref();
                targets.push(target_slice.and_then(|target_slice| map_target_in(target_slice, target.line, target.col)));
            }
            let all_targets_mapped = targets.iter().all(Option::is_some);
            entries.push(SiteEntry { site, mapped, targets, all_targets_mapped });
        }
        let mut by_caller: HashMap<String, Vec<usize>> = HashMap::new();
        let mut covered_spans = HashSet::new();
        for (index, entry) in entries.iter().enumerate() {
            let Some(mapped) = &entry.mapped else { continue };
            for call in &mapped.calls {
                if let Some(caller) = &call.caller {
                    let indexes = by_caller.entry(caller.node_id().to_string()).or_default();
                    if indexes.last() != Some(&index) {
                        indexes.push(index);
                    }
                }
            }
            if entry.all_targets_mapped {
                covered_spans.insert(mapped.span);
            }
        }
        Some(Self {
            slice: Some(Arc::downgrade(&slice)),
            targets_slices: target_slices
                .into_iter()
                .map(|(name, slice)| (name, slice.as_ref().map(Arc::downgrade)))
                .collect(),
            entries,
            by_caller,
            covered_spans,
        })
    }
}

struct OverlayMemo {
    overlay: Weak<dyn OverlayRead>,
    files: HashMap<String, Arc<FileSites>>,
}

static MEMO: Mutex<Vec<OverlayMemo>> = Mutex::new(Vec::new());

fn memo() -> MutexGuard<'static, Vec<OverlayMemo>> {
    MEMO.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn same_overlay(kept: &Weak<dyn OverlayRead>, overlay: &Overlay) -> bool {
    std::ptr::eq(Weak::as_ptr(kept) as *const (), Arc::as_ptr(overlay) as *const ())
}

/// The mapped sites of `file` in `overlay`, from the memo while valid, else built and kept.
/// `None` when the file has sites but upstream has no slice for it.
pub(super) fn file_sites(overlay: &Overlay, source: &impl SliceSource, file: &str) -> Option<Arc<FileSites>> {
    let kept = memo()
        .iter()
        .find(|slot| same_overlay(&slot.overlay, overlay))
        .and_then(|slot| slot.files.get(file).cloned());
    if let Some(kept) = kept.filter(|kept| kept.is_valid(source, file)) {
        return Some(kept);
    }
    // Built outside the lock: a concurrent query may build the same entry, which is harmless.
    let built = Arc::new(FileSites::build(overlay, source, file)?);
    let mut slots = memo();
    let position = match slots.iter().position(|slot| same_overlay(&slot.overlay, overlay)) {
        Some(position) => position,
        None => {
            if slots.len() >= MAX_OVERLAYS {
                slots.remove(0);
            }
            slots.push(OverlayMemo { overlay: Arc::downgrade(overlay), files: HashMap::new() });
            slots.len() - 1
        }
    };
    let files = &mut slots[position].files;
    if files.len() >= MAX_FILES {
        files.clear();
    }
    files.insert(file.to_string(), Arc::clone(&built));
    Some(built)
}
