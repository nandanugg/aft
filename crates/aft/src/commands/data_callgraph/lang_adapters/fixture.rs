//! An in-memory overlay built from validated adapter output (owner: C0).
//!
//! The C0 stub of `overlay_for_query` serves it from fixture files, and R2's
//! tests build it directly, so R2 needs no runner and no store. R1's SQLite
//! overlay must answer every `OverlayRead` method the same way for the same
//! output (R1 can test that against this type).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use super::contract::{
    line_col_to_offset, parse_output, AdapterOutput, FactLookup, FactQuery, FactRecord, FactSide,
    OverlayCounts, OverlayRead, OverlaySite, Pos, RegistryEntry,
};

pub(crate) struct FixtureOverlay {
    language: String,
    hidden_tags: Vec<String>,
    analysed: BTreeSet<String>,
    sites: Vec<OverlaySite>,
    facts: Vec<FactRecord>,
    by_caller_file: HashMap<String, Vec<usize>>,
    by_target_file: HashMap<String, Vec<(usize, usize)>>,
    counts: OverlayCounts,
}

impl FixtureOverlay {
    /// Builds the overlay from `output`, reading each caller file under `root`
    /// once to turn (line, col) into a byte offset. A site whose position is not
    /// inside its file is an error, as it is for the real store.
    pub(crate) fn from_output(root: &Path, output: AdapterOutput, hidden_tags: Vec<String>) -> Result<Self, String> {
        let mut texts: HashMap<String, String> = HashMap::new();
        let mut sites = Vec::with_capacity(output.sites.len());
        for site in output.sites {
            if !texts.contains_key(&site.file) {
                let text = std::fs::read_to_string(root.join(&site.file))
                    .map_err(|error| format!("cannot read {}: {error}", site.file))?;
                texts.insert(site.file.clone(), text);
            }
            let offset = line_col_to_offset(&texts[&site.file], site.line, site.col)
                .ok_or_else(|| format!("site {}:{}:{} is not inside the file", site.file, site.line, site.col))?;
            sites.push(OverlaySite { offset, site });
        }
        sites.sort_by(|a, b| (&a.site.file, a.offset).cmp(&(&b.site.file, b.offset)));
        let mut by_caller_file: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_target_file: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        for (index, entry) in sites.iter().enumerate() {
            by_caller_file.entry(entry.site.file.clone()).or_default().push(index);
            for (target, record) in entry.site.targets.iter().enumerate() {
                by_target_file.entry(record.file.clone()).or_default().push((index, target));
            }
        }
        let counts = OverlayCounts {
            sites: output.end.sites,
            targets: output.end.targets,
            facts: output.end.facts,
            analysed_files: output.end.analysed_files,
        };
        Ok(Self {
            language: output.header.language,
            hidden_tags,
            analysed: output.analysed,
            sites,
            facts: output.facts,
            by_caller_file,
            by_target_file,
            counts,
        })
    }

    /// Reads an adapter-output file (contract records, one per line), validates it
    /// against `entry`, and builds the overlay for `root`.
    pub(crate) fn from_file(root: &Path, path: &Path, entry: &RegistryEntry) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let output = parse_output(&bytes, entry).map_err(|violation| violation.to_string())?;
        Self::from_output(root, output, entry.hidden_tags.clone())
    }
}

fn side_matches(side: FactSide, fact: &FactRecord, test: impl Fn(&Pos, Option<&str>) -> bool) -> bool {
    let subject = || test(&fact.subject, fact.subject_name.as_deref());
    let object = || test(&fact.object, fact.object_name.as_deref());
    match side {
        FactSide::Subject => subject(),
        FactSide::Object => object(),
        FactSide::Either => subject() || object(),
    }
}

impl OverlayRead for FixtureOverlay {
    fn language(&self) -> &str {
        &self.language
    }

    fn is_analysed(&self, file: &str) -> bool {
        self.analysed.contains(file)
    }

    fn hidden_tags(&self) -> &[String] {
        &self.hidden_tags
    }

    fn sites_in_file(&self, file: &str) -> Vec<OverlaySite> {
        self.by_caller_file
            .get(file)
            .map(|indexes| indexes.iter().map(|index| self.sites[*index].clone()).collect())
            .unwrap_or_default()
    }

    fn sites_targeting(&self, file: &str, first_line: u32, last_line: u32) -> Vec<(OverlaySite, usize)> {
        self.by_target_file
            .get(file)
            .map(|pairs| {
                pairs
                    .iter()
                    .filter(|(site, target)| {
                        let line = self.sites[*site].site.targets[*target].line;
                        first_line <= line && line <= last_line
                    })
                    .map(|(site, target)| (self.sites[*site].clone(), *target))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn facts(&self, query: &FactQuery) -> Vec<FactRecord> {
        self.facts
            .iter()
            .filter(|fact| fact.kind == query.kind)
            .filter(|fact| match &query.lookup {
                FactLookup::Label { key, prefix } => fact
                    .label
                    .as_deref()
                    .is_some_and(|label| if *prefix { label.starts_with(key.as_str()) } else { label == key }),
                FactLookup::Position { side, file, first_line, last_line } => side_matches(*side, fact, |pos, _| {
                    &pos.file == file && *first_line <= pos.line && pos.line <= *last_line
                }),
                FactLookup::Name { side, name } => side_matches(*side, fact, |_, fact_name| fact_name == Some(name.as_str())),
            })
            .cloned()
            .collect()
    }

    fn counts(&self) -> OverlayCounts {
        self.counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixture_overlay_answers_both_directions() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.go"), "package p\n\nfunc F(x I) { x.M() }\n").unwrap();
        std::fs::write(root.path().join("b.go"), "package p\n\ntype A struct{}\n\nfunc (A) M() {}\n").unwrap();
        let entry: RegistryEntry = serde_json::from_value(serde_json::json!({
            "contract": 1, "language": "go", "root_markers": ["go.mod"], "command": ["x"],
            "hidden_tags": ["mock"],
            "facts": [{"kind": "implements", "description": "type -> interface"}]
        }))
        .unwrap();
        let output = concat!(
            r#"{"type":"header","contract":1,"language":"go","tool":"fake"}"#, "\n",
            r#"{"type":"analysed","files":["a.go","b.go"]}"#, "\n",
            r#"{"type":"site","file":"a.go","line":3,"col":18,"name":"M","targets":[{"file":"b.go","line":5,"col":10,"name":"M","kind":"interface"}]}"#, "\n",
            r#"{"type":"fact","kind":"implements","subject":{"file":"b.go","line":3,"col":6},"subject_name":"A","object":{"file":"a.go","line":1,"col":1},"object_name":"I","label":"I"}"#, "\n",
            r#"{"type":"end","sites":1,"targets":1,"facts":1,"analysed_files":2,"unanalysed_files":0}"#, "\n",
        );
        let path = root.path().join("go.ndjson");
        std::fs::write(&path, output).unwrap();
        let overlay = FixtureOverlay::from_file(root.path(), &path, &entry).unwrap();
        let caller = overlay.sites_in_file("a.go");
        assert_eq!(caller.len(), 1);
        assert_eq!(caller[0].offset, 28); // the '(' of x.M()
        assert_eq!(overlay.sites_targeting("b.go", 5, 5).len(), 1);
        assert!(overlay.sites_targeting("b.go", 1, 4).is_empty());
        assert!(overlay.is_analysed("b.go") && !overlay.is_analysed("c.go"));
        assert_eq!(overlay.hidden_tags(), ["mock".to_string()]);
        let by_name = FactQuery {
            kind: "implements".into(),
            lookup: FactLookup::Name { side: FactSide::Object, name: "I".into() },
        };
        assert_eq!(overlay.facts(&by_name).len(), 1);
        let by_label = FactQuery {
            kind: "implements".into(),
            lookup: FactLookup::Label { key: "J".into(), prefix: true },
        };
        assert!(overlay.facts(&by_label).is_empty());
    }
}
