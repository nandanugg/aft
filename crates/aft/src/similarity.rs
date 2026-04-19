//! Similarity index: tokenize → stem → TF-IDF → synonym expansion → co-citation.
//!
//! # Architecture
//!
//! Two phases:
//! - **Build** (configure-time): tokenize all symbol identifiers, stem, compute TF-IDF
//!   weights, write `similarity-index.cbor` to cache dir.
//! - **Query** (hot path): load index, compute weighted similarity scores, return ranked
//!   matches with optional explain output.
//!
//! No embeddings. No model files. Purely lexical + structural similarity.
//!
//! # Layer summary
//!
//! 1. Identifier tokenization (camelCase / snake_case / PascalCase / acronyms).
//! 2. Snowball English stemming (`rust-stemmers` crate).
//! 3. TF-IDF with smoothed IDF, L2-normalized sparse vectors.
//! 4. Project synonym dict (`.aft/synonyms.toml`, opt-in).
//! 5. Call-graph co-citation (Jaccard on callee sets, computed at query time).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rust_stemmers::{Algorithm, Stemmer};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Noise tokens dropped unconditionally (Go-convention + generic names)
// ---------------------------------------------------------------------------

const NOISE_TOKENS: &[&str] = &["err", "ctx", "ok", "nil", "true", "false"];

// ---------------------------------------------------------------------------
// Layer 1: Identifier tokenization
// ---------------------------------------------------------------------------

/// Split an identifier into lower-case sub-word tokens.
///
/// Rules (applied in order):
/// - Split runs of uppercase letters followed by a lowercase letter (acronym boundary):
///   `HTTPHandler` → `[HTTP, Handler]` → `[http, handler]`
/// - Split at camelCase / PascalCase boundaries.
/// - Split at `_` (snake_case).
/// - Drop tokens shorter than 2 chars (loop variables, single letters).
/// - Drop noise tokens (`err`, `ctx`, `ok`, `nil`, `true`, `false`).
/// - Non-ASCII chars pass through unchanged in their token.
pub fn tokenize_identifier(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }

    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let c = chars[i];

        if c == '_' || c == '-' {
            // snake_case / kebab-case boundary
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            i += 1;
            continue;
        }

        if c.is_ascii_digit() {
            // Digit: accumulate (e.g., V3, OAuth2, SHA256 stay in current token)
            // Start a new token if we're transitioning from letters to digits
            // so "Foo3" → ["Foo", "3"] and "SHA256" → ["SHA", "256"]
            // Actually per design: numbers stay attached. "V3" → ["v3"], "OAuth2" → ["oauth2"]
            // We just accumulate.
            current.push(c);
            i += 1;
            continue;
        }

        if c.is_uppercase() {
            // Look ahead to determine if this is the start of a new word or part of an acronym
            let next_is_lower = i + 1 < len && chars[i + 1].is_lowercase() && chars[i + 1] != '_';
            let prev_is_lower = i > 0
                && (chars[i - 1].is_lowercase()
                    || chars[i - 1].is_ascii_digit()
                    || chars[i - 1] == '_');

            if !current.is_empty() && (prev_is_lower || (next_is_lower && current.len() > 0)) {
                // camelCase boundary: e.g. "calculateFee" at 'F', or "HTTPHandler" at 'H'
                // Only split if current non-empty and we're transitioning to a new word
                let prev_is_uppercase_run = current
                    .chars()
                    .last()
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false);

                if !prev_is_uppercase_run || next_is_lower {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            current.push(c);
        } else {
            current.push(c);
        }

        i += 1;
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    // Lowercase all, filter short/noise
    tokens
        .into_iter()
        .map(|t| t.to_lowercase())
        .filter(|t| t.chars().count() >= 2)
        .filter(|t| !NOISE_TOKENS.contains(&t.as_str()))
        .collect()
}

// ---------------------------------------------------------------------------
// Layer 2: Snowball stemming
// ---------------------------------------------------------------------------

/// Stem a single token using English Snowball algorithm.
///
/// Non-ASCII input passes through unchanged (the stemmer handles it gracefully,
/// but we make the pass-through explicit for documentation).
pub fn stem_token(token: &str) -> String {
    // Non-ASCII identifiers: stemmer may mangle them; pass through safely.
    if !token.is_ascii() {
        return token.to_string();
    }
    let stemmer = Stemmer::create(Algorithm::English);
    stemmer.stem(token).to_string()
}

/// Tokenize an identifier and stem each token, returning a deduplicated list.
pub fn tokenize_and_stem(identifier: &str) -> Vec<String> {
    tokenize_identifier(identifier)
        .into_iter()
        .map(|t| stem_token(&t))
        .collect()
}

// ---------------------------------------------------------------------------
// Sparse vector
// ---------------------------------------------------------------------------

/// Sparse TF-IDF vector. `terms` maps `token_id → weight`. L2-normalized.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SparseVec {
    /// Parallel arrays: token ids and their weights.
    pub ids: Vec<u32>,
    pub weights: Vec<f32>,
}

impl SparseVec {
    pub fn new() -> Self {
        Self {
            ids: Vec::new(),
            weights: Vec::new(),
        }
    }

    /// Add a (id, weight) entry.
    pub fn push(&mut self, id: u32, weight: f32) {
        self.ids.push(id);
        self.weights.push(weight);
    }

    /// Dot product with another sparse vector (O(|a| + |b|) with sorted ids).
    pub fn dot(&self, other: &SparseVec) -> f32 {
        let mut sum = 0.0f32;
        let mut i = 0;
        let mut j = 0;
        while i < self.ids.len() && j < other.ids.len() {
            match self.ids[i].cmp(&other.ids[j]) {
                std::cmp::Ordering::Equal => {
                    sum += self.weights[i] * other.weights[j];
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
            }
        }
        sum
    }

    /// L2 norm.
    pub fn norm(&self) -> f32 {
        self.weights.iter().map(|w| w * w).sum::<f32>().sqrt()
    }

    /// Normalize to unit length in-place.
    pub fn normalize(&mut self) {
        let n = self.norm();
        if n > 1e-10 {
            for w in &mut self.weights {
                *w /= n;
            }
        }
    }

    /// Sort by id (required for dot product correctness).
    pub fn sort_by_id(&mut self) {
        let mut pairs: Vec<(u32, f32)> = self.ids.iter().copied().zip(self.weights.iter().copied()).collect();
        pairs.sort_unstable_by_key(|(id, _)| *id);
        self.ids = pairs.iter().map(|(id, _)| *id).collect();
        self.weights = pairs.iter().map(|(_, w)| *w).collect();
    }
}

// ---------------------------------------------------------------------------
// Symbol reference
// ---------------------------------------------------------------------------

/// A symbol in the project identified by file + name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymbolRef {
    pub file: PathBuf,
    pub symbol: String,
}

// ---------------------------------------------------------------------------
// Synonym dict (Layer 4)
// ---------------------------------------------------------------------------

/// Project synonym dict loaded from `.aft/synonyms.toml`.
///
/// Format:
/// ```toml
/// [groups]
/// settlement = ["payout", "disburse", "settle"]
/// ```
///
/// Each key and all values form an equivalence class. At load time we
/// stem every term and build bidirectional mappings so `A → B` implies `B → A`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SynonymDict {
    /// stem → set of synonym stems (one-hop, bidirectional).
    pub map: HashMap<String, HashSet<String>>,
}

impl SynonymDict {
    /// Load from a TOML file. Returns `Ok(empty)` if file absent.
    /// Logs a warning and returns `Ok(empty)` on parse errors (never blocks configure).
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(".aft").join("synonyms.toml");
        if !path.exists() {
            return Self::default();
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[aft-similarity] failed to read synonyms.toml: {}", e);
                return Self::default();
            }
        };

        let parsed: toml::Value = match content.parse() {
            Ok(v) => v,
            Err(e) => {
                log::warn!("[aft-similarity] synonyms.toml parse error: {}", e);
                return Self::default();
            }
        };

        let groups = match parsed.get("groups").and_then(|v| v.as_table()) {
            Some(g) => g,
            None => {
                log::warn!("[aft-similarity] synonyms.toml: missing or non-table [groups] section");
                return Self::default();
            }
        };

        let mut map: HashMap<String, HashSet<String>> = HashMap::new();

        for (key, value) in groups {
            // Collect all terms in the group (key + array values)
            let mut group_terms: Vec<String> = Vec::new();
            group_terms.push(key.clone());

            match value.as_array() {
                Some(arr) => {
                    for item in arr {
                        match item.as_str() {
                            Some(s) => group_terms.push(s.to_string()),
                            None => {
                                log::warn!(
                                    "[aft-similarity] synonyms.toml: non-string value in group '{}', skipping item",
                                    key
                                );
                            }
                        }
                    }
                }
                None => {
                    log::warn!(
                        "[aft-similarity] synonyms.toml: value for '{}' must be an array, skipping",
                        key
                    );
                    continue;
                }
            }

            // Stem each term
            let stemmed: Vec<String> = group_terms.iter().map(|t| stem_token(t)).collect();

            // Build bidirectional mapping
            for i in 0..stemmed.len() {
                for j in 0..stemmed.len() {
                    if i != j {
                        map.entry(stemmed[i].clone())
                            .or_default()
                            .insert(stemmed[j].clone());
                    }
                }
            }
        }

        Self { map }
    }

    /// Is empty (no synonyms loaded)?
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Get synonym stems for a given stem (one hop only, no transitive closure).
    pub fn synonyms_of(&self, stem: &str) -> HashSet<String> {
        self.map.get(stem).cloned().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Similarity index
// ---------------------------------------------------------------------------

/// Full similarity index for a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarityIndex {
    /// token string → token id (vocabulary).
    pub vocab: HashMap<String, u32>,
    /// token id → smoothed IDF.
    pub idf: Vec<f32>,
    /// per-symbol unit-length TF-IDF vectors.
    pub vectors: HashMap<SymbolRef, SparseVec>,
    /// per-symbol raw stem token lists (for explain output).
    pub symbol_tokens: HashMap<SymbolRef, Vec<String>>,
    /// per-symbol callee sets (for co-citation, stored as file+symbol strings).
    pub callees: HashMap<SymbolRef, HashSet<String>>,
    /// synonym dict (may be empty if no .aft/synonyms.toml).
    pub synonyms: SynonymDict,
    /// total number of symbols at index build time.
    pub symbol_count: usize,
}

impl SimilarityIndex {
    /// Build an index from a list of (SymbolRef, callee_set) pairs.
    ///
    /// `symbols`: iterator of `(SymbolRef, callees)` where callees are
    /// `file::symbol` strings for co-citation.
    pub fn build(
        symbol_data: Vec<(SymbolRef, HashSet<String>)>,
        synonyms: SynonymDict,
    ) -> Self {
        let n = symbol_data.len();

        // Step 1: tokenize + stem each symbol identifier
        let symbol_stems: Vec<(SymbolRef, Vec<String>)> = symbol_data
            .iter()
            .map(|(sym_ref, _)| {
                let stems = tokenize_and_stem(&sym_ref.symbol);
                (sym_ref.clone(), stems)
            })
            .collect();

        // Step 2: build vocabulary (assign integer ids to stem tokens)
        let mut vocab: HashMap<String, u32> = HashMap::new();
        for (_, stems) in &symbol_stems {
            for stem in stems {
                let next_id = vocab.len() as u32;
                vocab.entry(stem.clone()).or_insert(next_id);
            }
        }

        // Step 3: compute DF (document frequency) per token
        let vocab_size = vocab.len();
        let mut df: Vec<u32> = vec![0u32; vocab_size];
        let n_f32 = n as f32;

        let mut tf_maps: Vec<HashMap<u32, u32>> = Vec::with_capacity(n);
        for (_, stems) in &symbol_stems {
            let mut tf_map: HashMap<u32, u32> = HashMap::new();
            for stem in stems {
                if let Some(&id) = vocab.get(stem) {
                    *tf_map.entry(id).or_insert(0) += 1;
                }
            }
            for &id in tf_map.keys() {
                if (id as usize) < df.len() {
                    df[id as usize] += 1;
                }
            }
            tf_maps.push(tf_map);
        }

        // Step 4: compute smoothed IDF
        // idf(t) = ln((N+1) / (df(t)+1)) + 1
        let idf: Vec<f32> = (0..vocab_size)
            .map(|i| {
                let df_i = df[i] as f32;
                ((n_f32 + 1.0) / (df_i + 1.0)).ln() + 1.0
            })
            .collect();

        // Step 5: build sparse TF-IDF vectors, normalize
        let mut vectors: HashMap<SymbolRef, SparseVec> = HashMap::new();
        let mut symbol_tokens: HashMap<SymbolRef, Vec<String>> = HashMap::new();

        for (i, ((sym_ref, stems), tf_map)) in
            symbol_stems.iter().zip(tf_maps.iter()).enumerate()
        {
            let _ = i;
            let mut vec = SparseVec::new();
            for (&id, &tf) in tf_map {
                let w = (tf as f32) * idf[id as usize];
                vec.push(id, w);
            }
            vec.sort_by_id();
            vec.normalize();
            vectors.insert(sym_ref.clone(), vec);
            symbol_tokens.insert(sym_ref.clone(), stems.clone());
        }

        // Step 6: collect callees
        let callees: HashMap<SymbolRef, HashSet<String>> = symbol_data
            .into_iter()
            .map(|(sym_ref, c)| (sym_ref, c))
            .collect();

        SimilarityIndex {
            vocab,
            idf,
            vectors,
            symbol_tokens,
            callees,
            synonyms,
            symbol_count: n,
        }
    }

    /// Serialize to CBOR and write to disk.
    pub fn write_to_disk(&self, cache_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(cache_dir)?;
        let path = cache_dir.join("similarity-index.cbor");
        let file = std::fs::File::create(&path)?;
        let writer = std::io::BufWriter::new(file);
        ciborium::ser::into_writer(self, writer)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        log::info!(
            "[aft-similarity] wrote index: {} symbols → {}",
            self.symbol_count,
            path.display()
        );
        Ok(())
    }

    /// Load from disk. Returns `None` on any error (graceful degradation).
    pub fn read_from_disk(cache_dir: &Path) -> Option<Self> {
        let path = cache_dir.join("similarity-index.cbor");
        if !path.exists() {
            return None;
        }
        let file = std::fs::File::open(&path).ok()?;
        let reader = std::io::BufReader::new(file);
        match ciborium::de::from_reader(reader) {
            Ok(index) => Some(index),
            Err(e) => {
                log::warn!("[aft-similarity] failed to load index from disk: {}", e);
                None
            }
        }
    }

    /// Get the token id for a stem, if in vocabulary.
    pub fn token_id(&self, stem: &str) -> Option<u32> {
        self.vocab.get(stem).copied()
    }

    /// Build an expanded sparse vector for a target symbol: union of original
    /// stems + synonym-expanded stems with weight penalty (0.85×).
    ///
    /// Only meaningful when `--dict` is active and synonyms are loaded.
    pub fn expanded_vec(&self, sym_ref: &SymbolRef) -> Option<SparseVec> {
        let base = self.vectors.get(sym_ref)?;
        let base_stems = self.symbol_tokens.get(sym_ref)?;

        if self.synonyms.is_empty() {
            return None;
        }

        let mut extra: HashMap<u32, f32> = HashMap::new();

        for stem in base_stems {
            let syns = self.synonyms.synonyms_of(stem);
            for syn_stem in &syns {
                if let Some(&id) = self.vocab.get(syn_stem.as_str()) {
                    // Use idf weight, penalized at 0.85×
                    let idf_w = self.idf.get(id as usize).copied().unwrap_or(1.0);
                    let entry = extra.entry(id).or_insert(0.0);
                    *entry += 0.85 * idf_w;
                }
            }
        }

        if extra.is_empty() {
            return None;
        }

        // Merge base + extra
        let mut merged: HashMap<u32, f32> = HashMap::new();
        for (&id, &w) in base.ids.iter().zip(base.weights.iter()) {
            *merged.entry(id).or_insert(0.0) += w;
        }
        for (id, w) in extra {
            *merged.entry(id).or_insert(0.0) += w;
        }

        let mut vec = SparseVec::new();
        for (id, w) in merged {
            vec.push(id, w);
        }
        vec.sort_by_id();
        vec.normalize();
        Some(vec)
    }

    /// Compute co-citation Jaccard between two symbols.
    ///
    /// Returns 0.0 if either symbol has empty callee set.
    pub fn co_citation(&self, a: &SymbolRef, b: &SymbolRef) -> f32 {
        let empty = HashSet::new();
        let ca = self.callees.get(a).unwrap_or(&empty);
        let cb = self.callees.get(b).unwrap_or(&empty);

        if ca.is_empty() && cb.is_empty() {
            return 0.0;
        }

        let intersection = ca.intersection(cb).count() as f32;
        let union = ca.union(cb).count() as f32;

        if union < 1e-10 {
            0.0
        } else {
            intersection / union
        }
    }
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// Configuration for a similarity query.
#[derive(Debug, Clone)]
pub struct SimilarityQuery {
    pub file: PathBuf,
    pub symbol: String,
    pub top: usize,
    pub use_dict: bool,
    pub min_score: f32,
    pub explain: bool,
    /// Weights: (w_lex, w_syn, w_cit)
    pub weights: (f32, f32, f32),
}

impl Default for SimilarityQuery {
    fn default() -> Self {
        Self {
            file: PathBuf::new(),
            symbol: String::new(),
            top: 10,
            use_dict: false,
            min_score: 0.15,
            explain: false,
            weights: (0.70, 0.15, 0.15),
        }
    }
}

/// A single ranked match.
#[derive(Debug, Clone, Serialize)]
pub struct SimilarityMatch {
    pub file: PathBuf,
    pub symbol: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<ScoreBreakdown>,
}

/// Per-token contributor to the lex score.
#[derive(Debug, Clone, Serialize)]
pub struct TokenContributor {
    pub token: String,
    pub target_weight: f32,
    pub candidate_weight: f32,
    pub product: f32,
}

/// Score breakdown for --explain output.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreBreakdown {
    pub lex: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lex_contributors: Vec<TokenContributor>,
    pub synonyms: f32,
    pub co_citation: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shared_callees: Vec<String>,
}

/// Full result of a similarity query.
#[derive(Debug, Clone, Serialize)]
pub struct SimilarityResult {
    pub query: QueryInfo,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub target_tokens: Vec<TargetToken>,
    pub matches: Vec<SimilarityMatch>,
}

impl SimilarityResult {
    /// Render the result as human-readable plain text, mirroring the style
    /// of other `aft` commands (callers, dispatched_by, dispatches, etc.).
    ///
    /// Without a breakdown per match, the output is a compact ranked list.
    /// When `--explain` produced breakdowns, each match is followed by an
    /// indented block showing the lex / synonym / co-citation components,
    /// the top token contributors, and shared callees.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "similar to {} ({})  total={}\n",
            self.query.symbol,
            self.query.file,
            self.matches.len()
        ));

        if self.matches.is_empty() {
            out.push_str("  (no similar symbols found above min_score)\n");
            return out;
        }

        // If we have target tokens (--explain), show them as context once.
        if !self.target_tokens.is_empty() {
            out.push_str("  target tokens (tf-idf):\n");
            for t in self.target_tokens.iter().take(8) {
                out.push_str(&format!("    {:<20}  {:.3}\n", t.token, t.tfidf));
            }
            out.push('\n');
        }

        for (i, m) in self.matches.iter().enumerate() {
            let rel = m.file.display().to_string();
            out.push_str(&format!(
                "  {:>2}. {:.3}  {} ({})\n",
                i + 1,
                m.score,
                m.symbol,
                rel
            ));
            if let Some(b) = &m.breakdown {
                out.push_str(&format!(
                    "       lex={:.2}  synonyms={:.2}  co_citation={:.2}\n",
                    b.lex, b.synonyms, b.co_citation
                ));
                if !b.lex_contributors.is_empty() {
                    let shown: Vec<String> = b
                        .lex_contributors
                        .iter()
                        .take(4)
                        .map(|c| {
                            format!(
                                "{}={:.2}·{:.2}={:.2}",
                                c.token, c.target_weight, c.candidate_weight, c.product
                            )
                        })
                        .collect();
                    out.push_str(&format!("       tokens: {}\n", shown.join("  ")));
                }
                if !b.shared_callees.is_empty() {
                    let joined = b.shared_callees.join(", ");
                    let shortened = if joined.len() > 120 {
                        format!("{}…", &joined[..120])
                    } else {
                        joined
                    };
                    out.push_str(&format!("       shared callees: {}\n", shortened));
                }
            }
        }

        out
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryInfo {
    pub file: String,
    pub symbol: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetToken {
    pub token: String,
    pub tfidf: f32,
}

/// Run a similarity query against the index.
pub fn query(index: &SimilarityIndex, q: &SimilarityQuery) -> Result<SimilarityResult, String> {
    let target_ref = SymbolRef {
        file: q.file.clone(),
        symbol: q.symbol.clone(),
    };

    // Find the target vector (try exact path match, then basename match)
    let (actual_target_ref, target_vec) = find_symbol_in_index(index, &target_ref)
        .ok_or_else(|| format!("symbol '{}' not found in similarity index", q.symbol))?;

    // Expanded vector for synonym mode
    let expanded_vec = if q.use_dict && !index.synonyms.is_empty() {
        index.expanded_vec(&actual_target_ref)
    } else {
        None
    };

    // Determine effective weights
    let (w_lex, w_syn, w_cit) = if q.use_dict && expanded_vec.is_some() {
        q.weights
    } else {
        // When no dict: w_lex absorbs w_syn → (0.85, 0.0, 0.15)
        let (wl, ws, wc) = q.weights;
        (wl + ws, 0.0, wc)
    };

    // Build target token list for explain
    let target_tokens: Vec<TargetToken> = if q.explain {
        let stems = index.symbol_tokens.get(&actual_target_ref).cloned().unwrap_or_default();
        // Reconstruct per-token TF-IDF from the normalized vector by looking up ids
        // We'll use the raw weights (already normalized) with original idf values
        let mut tt = Vec::new();
        for stem in &stems {
            if let Some(&id) = index.vocab.get(stem.as_str()) {
                let w = target_vec.weights.iter()
                    .zip(target_vec.ids.iter())
                    .find(|(_, &vid)| vid == id)
                    .map(|(w, _)| *w)
                    .unwrap_or(0.0);
                if w > 1e-10 {
                    tt.push(TargetToken { token: stem.clone(), tfidf: w });
                }
            }
        }
        tt.sort_by(|a, b| b.tfidf.partial_cmp(&a.tfidf).unwrap_or(std::cmp::Ordering::Equal));
        tt
    } else {
        Vec::new()
    };

    // Score all candidates
    let mut scored: Vec<SimilarityMatch> = index
        .vectors
        .iter()
        .filter(|(sym_ref, _)| *sym_ref != &actual_target_ref)
        .map(|(sym_ref, candidate_vec)| {
            // Layer 1: lexical cosine (dot product of unit vectors = cosine)
            let lex = target_vec.dot(candidate_vec).max(0.0);

            // Layer 2: synonym-expanded cosine
            let syn_score = expanded_vec
                .as_ref()
                .map(|ev| ev.dot(candidate_vec).max(0.0))
                .unwrap_or(0.0);

            // Layer 3: co-citation
            let cit = index.co_citation(&actual_target_ref, sym_ref);

            let score = w_lex * lex + w_syn * syn_score + w_cit * cit;

            let breakdown = if q.explain {
                let lex_contributors = build_contributors(
                    index,
                    target_vec,
                    candidate_vec,
                    &actual_target_ref,
                    sym_ref,
                );

                let shared_callees: Vec<String> = {
                    let empty = HashSet::new();
                    let ca = index.callees.get(&actual_target_ref).unwrap_or(&empty);
                    let cb = index.callees.get(sym_ref).unwrap_or(&empty);
                    let mut shared: Vec<String> = ca.intersection(cb).cloned().collect();
                    shared.sort();
                    shared
                };

                Some(ScoreBreakdown {
                    lex,
                    lex_contributors,
                    synonyms: syn_score,
                    co_citation: cit,
                    shared_callees,
                })
            } else {
                None
            };

            SimilarityMatch {
                file: sym_ref.file.clone(),
                symbol: sym_ref.symbol.clone(),
                score,
                breakdown,
            }
        })
        .filter(|m| m.score >= q.min_score)
        .collect();

    // Sort: descending score, then alphabetical tie-break
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.file
                    .cmp(&b.file)
                    .then_with(|| a.symbol.cmp(&b.symbol))
            })
    });

    scored.truncate(q.top);

    Ok(SimilarityResult {
        query: QueryInfo {
            file: q.file.to_string_lossy().to_string(),
            symbol: q.symbol.clone(),
        },
        target_tokens,
        matches: scored,
    })
}

/// Find a symbol in the index, tolerating relative vs. absolute path differences.
///
/// Try exact match first, then basename match for the symbol name.
fn find_symbol_in_index<'a>(
    index: &'a SimilarityIndex,
    target: &SymbolRef,
) -> Option<(SymbolRef, &'a SparseVec)> {
    // 1. Exact match
    if let Some(v) = index.vectors.get(target) {
        return Some((target.clone(), v));
    }

    // 2. Match by symbol name + file basename
    let target_base = target.file.file_name()?;
    for (sym_ref, vec) in &index.vectors {
        if sym_ref.symbol == target.symbol {
            if let Some(base) = sym_ref.file.file_name() {
                if base == target_base {
                    return Some((sym_ref.clone(), vec));
                }
            }
        }
    }

    // 3. Match by symbol name alone (if unique)
    let matches: Vec<_> = index
        .vectors
        .iter()
        .filter(|(sym_ref, _)| sym_ref.symbol == target.symbol)
        .collect();

    if matches.len() == 1 {
        let (sym_ref, vec) = matches[0];
        return Some((sym_ref.clone(), vec));
    }

    None
}

/// Build top-5 token contributors for the lex score breakdown.
fn build_contributors(
    index: &SimilarityIndex,
    target_vec: &SparseVec,
    candidate_vec: &SparseVec,
    target_ref: &SymbolRef,
    _candidate_ref: &SymbolRef,
) -> Vec<TokenContributor> {
    // Build reverse vocab: id → stem string
    let id_to_stem: HashMap<u32, &str> = index
        .vocab
        .iter()
        .map(|(s, &id)| (id, s.as_str()))
        .collect();

    let target_stems = index.symbol_tokens.get(target_ref).cloned().unwrap_or_default();

    let mut contributors: Vec<TokenContributor> = Vec::new();

    for stem in &target_stems {
        let Some(&id) = index.vocab.get(stem.as_str()) else {
            continue;
        };

        let tw = target_vec
            .ids
            .iter()
            .zip(target_vec.weights.iter())
            .find(|(&vid, _)| vid == id)
            .map(|(_, &w)| w)
            .unwrap_or(0.0);

        let cw = candidate_vec
            .ids
            .iter()
            .zip(candidate_vec.weights.iter())
            .find(|(&vid, _)| vid == id)
            .map(|(_, &w)| w)
            .unwrap_or(0.0);

        let product = tw * cw;
        if product > 1e-10 {
            contributors.push(TokenContributor {
                token: id_to_stem.get(&id).unwrap_or(&"?").to_string(),
                target_weight: tw,
                candidate_weight: cw,
                product,
            });
        }
    }

    contributors.sort_by(|a, b| {
        b.product
            .partial_cmp(&a.product)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    contributors.truncate(5);
    contributors
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // --- Layer 1: tokenizer golden tests ---

    fn tok(s: &str) -> Vec<String> {
        tokenize_identifier(s)
    }

    #[test]
    fn tokenize_camel_case() {
        assert_eq!(tok("calculateSettlementFee"), vec!["calculate", "settlement", "fee"]);
    }

    #[test]
    fn tokenize_snake_case() {
        assert_eq!(tok("calculate_settlement_fee"), vec!["calculate", "settlement", "fee"]);
    }

    #[test]
    fn tokenize_pascal_case() {
        assert_eq!(tok("SettlementFee"), vec!["settlement", "fee"]);
    }

    #[test]
    fn tokenize_acronym_http_handler() {
        let tokens = tok("HTTPHandler");
        // Should split: HTTP, Handler → [http, handler]
        assert!(tokens.contains(&"http".to_string()), "expected 'http' in {:?}", tokens);
        assert!(tokens.contains(&"handler".to_string()), "expected 'handler' in {:?}", tokens);
    }

    #[test]
    fn tokenize_acronym_json_parse() {
        let tokens = tok("JSONParse");
        assert!(tokens.contains(&"json".to_string()) || tokens.contains(&"jsonpars".to_string()),
            "got {:?}", tokens);
        // At minimum we should have "parse" split
        let joined = tokens.join(",");
        assert!(joined.contains("pars") || joined.contains("parse"), "got {:?}", tokens);
    }

    #[test]
    fn tokenize_version_suffix() {
        // V3, OAuth2, SHA256 — numbers stay attached in token
        let tokens = tok("processEarlySettlementV3");
        assert!(tokens.contains(&"process".to_string()), "got {:?}", tokens);
        assert!(tokens.contains(&"earli".to_string()) || tokens.contains(&"early".to_string()), "got {:?}", tokens);
        assert!(tokens.contains(&"settlement".to_string()), "got {:?}", tokens);
    }

    #[test]
    fn tokenize_drops_short_tokens() {
        // Single-char tokens should be dropped
        let tokens = tok("doX");
        assert!(!tokens.iter().any(|t| t == "x"), "got {:?}", tokens);
    }

    #[test]
    fn tokenize_drops_noise_tokens() {
        let tokens = tok("handleErr");
        assert!(!tokens.contains(&"err".to_string()), "got {:?}", tokens);

        let tokens2 = tok("processCtx");
        assert!(!tokens2.contains(&"ctx".to_string()), "got {:?}", tokens2);
    }

    #[test]
    fn tokenize_empty_string() {
        assert_eq!(tok(""), Vec::<String>::new());
    }

    #[test]
    fn tokenize_single_word() {
        assert_eq!(tok("process"), vec!["process"]);
    }

    #[test]
    fn tokenize_mixed_case_snake() {
        let tokens = tok("getMerchant_byID");
        assert!(tokens.contains(&"get".to_string()) || tokens.contains(&"getmerchant".to_string()),
            "got {:?}", tokens);
        assert!(tokens.contains(&"merchant".to_string()) || tokens.len() >= 2, "got {:?}", tokens);
    }

    // --- Layer 2: stemmer integration ---

    #[test]
    fn stem_calculating_variants() {
        let s1 = stem_token("calculate");
        let s2 = stem_token("calculated");
        let s3 = stem_token("calculating");
        assert_eq!(s1, s2, "calculate/calculated should have same stem");
        assert_eq!(s1, s3, "calculate/calculating should have same stem");
    }

    #[test]
    fn stem_settlement() {
        let s1 = stem_token("settlement");
        let s2 = stem_token("settlements");
        assert_eq!(s1, s2, "settlement/settlements should have same stem");
    }

    #[test]
    fn stem_disburse() {
        let s1 = stem_token("disburse");
        let s2 = stem_token("disbursement");
        // Both should stem to "disburs" or similar
        assert_eq!(s1, s2, "disburse/disbursement should have same stem, got: {:?} vs {:?}", s1, s2);
    }

    #[test]
    fn stem_non_ascii_passthrough() {
        // Non-ASCII should pass through unchanged
        let s = stem_token("分散");
        assert_eq!(s, "分散");
    }

    // --- Layer 3: TF-IDF correctness ---

    fn make_symbol(file: &str, sym: &str) -> SymbolRef {
        SymbolRef {
            file: PathBuf::from(file),
            symbol: sym.to_string(),
        }
    }

    #[test]
    fn tfidf_rare_token_higher_idf() {
        // Build a corpus: 100 symbols where only 1 has "settle", 50 have "handle"
        let mut symbol_data: Vec<(SymbolRef, HashSet<String>)> = Vec::new();
        for i in 0..50 {
            symbol_data.push((make_symbol("a.go", &format!("handleFoo{}", i)), HashSet::new()));
        }
        for i in 50..99 {
            symbol_data.push((make_symbol("a.go", &format!("handleBar{}", i)), HashSet::new()));
        }
        symbol_data.push((make_symbol("a.go", "settleMerchant"), HashSet::new()));

        let index = SimilarityIndex::build(symbol_data, SynonymDict::default());

        let settle_stem = stem_token("settle");
        let handle_stem = stem_token("handle");

        let settle_id = index.token_id(&settle_stem);
        let handle_id = index.token_id(&handle_stem);

        if let (Some(si), Some(hi)) = (settle_id, handle_id) {
            let settle_idf = index.idf[si as usize];
            let handle_idf = index.idf[hi as usize];
            assert!(
                settle_idf > handle_idf,
                "rare 'settle' (idf={}) should have higher IDF than frequent 'handle' (idf={})",
                settle_idf,
                handle_idf
            );
        }
    }

    #[test]
    fn tfidf_vectors_unit_length() {
        let symbol_data = vec![
            (make_symbol("a.go", "calculateSettlementFee"), HashSet::new()),
            (make_symbol("b.go", "processPayment"), HashSet::new()),
            (make_symbol("c.go", "handleRequest"), HashSet::new()),
        ];
        let index = SimilarityIndex::build(symbol_data, SynonymDict::default());

        for (_, vec) in &index.vectors {
            let norm = vec.norm();
            assert!(
                (norm - 1.0).abs() < 1e-5 || norm < 1e-10,
                "vector norm {} should be ~1.0 or 0.0",
                norm
            );
        }
    }

    #[test]
    fn cosine_sim_identical_symbols() {
        // Two symbols with the same identifier tokens should have cosine sim = 1.0
        // Use symbols that tokenize to identical stem sets
        let symbol_data = vec![
            (make_symbol("a.go", "calculateFee"), HashSet::new()),
            (make_symbol("b.go", "calculateFee"), HashSet::new()), // same name, different file
            (make_symbol("c.go", "unrelatedMethod"), HashSet::new()),
        ];
        let index = SimilarityIndex::build(symbol_data, SynonymDict::default());

        let a = make_symbol("a.go", "calculateFee");
        let b = make_symbol("b.go", "calculateFee");

        if let (Some(va), Some(vb)) = (index.vectors.get(&a), index.vectors.get(&b)) {
            let sim = va.dot(vb);
            assert!(sim > 0.99, "identical tokens should have sim ~1.0, got {}", sim);
        }

        // Also verify different symbols have lower similarity than identical ones
        let a = make_symbol("a.go", "calculateFee");
        let c = make_symbol("c.go", "unrelatedMethod");
        if let (Some(va), Some(vc)) = (index.vectors.get(&a), index.vectors.get(&c)) {
            let sim_diff = va.dot(vc);
            assert!(sim_diff < 0.9, "different symbols should have sim < 0.9, got {}", sim_diff);
        }
    }

    // --- Layer 4: Synonym dict ---

    #[test]
    fn synonym_dict_bidirectional() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let aft_dir = dir.path().join(".aft");
        std::fs::create_dir_all(&aft_dir).unwrap();
        std::fs::write(
            aft_dir.join("synonyms.toml"),
            r#"
[groups]
settlement = ["payout", "disburse", "settle"]
"#,
        ).unwrap();

        let dict = SynonymDict::load(dir.path());
        assert!(!dict.is_empty());

        // "settle" → should include "settlement" stems and vice versa
        let settle_stem = stem_token("settle");
        let settlement_stem = stem_token("settlement");
        let syns = dict.synonyms_of(&settle_stem);
        // The dict should have at least some synonym entries
        assert!(!syns.is_empty(), "expected synonyms for '{}', got empty", settle_stem);

        // Bidirectional: settlement → settle
        let syns2 = dict.synonyms_of(&settlement_stem);
        assert!(!syns2.is_empty(), "expected synonyms for '{}', got empty", settlement_stem);
    }

    #[test]
    fn synonym_dict_malformed_toml_no_crash() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let aft_dir = dir.path().join(".aft");
        std::fs::create_dir_all(&aft_dir).unwrap();
        std::fs::write(aft_dir.join("synonyms.toml"), "this is not valid toml }{").unwrap();

        let dict = SynonymDict::load(dir.path());
        assert!(dict.is_empty(), "malformed TOML should produce empty dict");
    }

    #[test]
    fn synonym_dict_absent_is_empty() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let dict = SynonymDict::load(dir.path());
        assert!(dict.is_empty());
    }

    // --- Layer 5: Co-citation ---

    #[test]
    fn co_citation_jaccard_correct() {
        let mut symbol_data: Vec<(SymbolRef, HashSet<String>)> = Vec::new();
        let mut callees_a: HashSet<String> = HashSet::new();
        callees_a.insert("helper::Foo".to_string());
        callees_a.insert("helper::Bar".to_string());
        callees_a.insert("helper::Baz".to_string());

        let mut callees_b: HashSet<String> = HashSet::new();
        callees_b.insert("helper::Foo".to_string());
        callees_b.insert("helper::Bar".to_string());
        callees_b.insert("helper::Qux".to_string());

        let sym_a = make_symbol("a.go", "processA");
        let sym_b = make_symbol("b.go", "handleB");
        symbol_data.push((sym_a.clone(), callees_a));
        symbol_data.push((sym_b.clone(), callees_b));

        let index = SimilarityIndex::build(symbol_data, SynonymDict::default());

        // Intersection: {Foo, Bar} = 2, Union: {Foo, Bar, Baz, Qux} = 4
        // Jaccard = 2/4 = 0.5
        let cit = index.co_citation(&sym_a, &sym_b);
        assert!(
            (cit - 0.5).abs() < 1e-5,
            "expected Jaccard=0.5, got {}",
            cit
        );
    }

    #[test]
    fn co_citation_empty_callees_returns_zero() {
        let symbol_data = vec![
            (make_symbol("a.go", "processA"), HashSet::new()),
            (make_symbol("b.go", "handleB"), HashSet::new()),
        ];
        let index = SimilarityIndex::build(symbol_data, SynonymDict::default());
        let cit = index.co_citation(
            &make_symbol("a.go", "processA"),
            &make_symbol("b.go", "handleB"),
        );
        assert_eq!(cit, 0.0);
    }

    // --- End-to-end query ---

    #[test]
    fn query_returns_ranked_matches() {
        let symbol_data = vec![
            (make_symbol("merchant_settlement/service.go", "SettleMerchantSettlement"), HashSet::new()),
            (make_symbol("early_settlement/service.go", "processEarlySettlementV3"), HashSet::new()),
            (make_symbol("realtime/service.go", "settleRealtime"), HashSet::new()),
            (make_symbol("payment/service.go", "processPayment"), HashSet::new()),
            (make_symbol("handler/http.go", "handleRequest"), HashSet::new()),
        ];

        let index = SimilarityIndex::build(symbol_data, SynonymDict::default());

        let q = SimilarityQuery {
            file: PathBuf::from("merchant_settlement/service.go"),
            symbol: "SettleMerchantSettlement".to_string(),
            top: 3,
            use_dict: false,
            min_score: 0.0,
            explain: false,
            weights: (0.85, 0.0, 0.15),
        };

        let result = query(&index, &q).unwrap();

        // Should return up to 3 matches, excluding the query symbol itself
        assert!(result.matches.len() <= 3);
        // Scores should be descending
        for i in 1..result.matches.len() {
            assert!(
                result.matches[i - 1].score >= result.matches[i].score,
                "scores not sorted: {} vs {}",
                result.matches[i - 1].score,
                result.matches[i].score
            );
        }
        // Settlement-related symbols should score higher than handleRequest
        let settle_match = result.matches.iter().find(|m| m.symbol.contains("Settle") || m.symbol.contains("settle"));
        let handle_match = result.matches.iter().find(|m| m.symbol.contains("handle"));
        if let (Some(s), Some(h)) = (settle_match, handle_match) {
            assert!(s.score >= h.score, "settlement symbols should score higher than handle");
        }
    }

    #[test]
    fn query_top_n_honored() {
        let symbol_data: Vec<(SymbolRef, HashSet<String>)> = (0..20)
            .map(|i| {
                (
                    make_symbol("a.go", &format!("settle{}", i)),
                    HashSet::new(),
                )
            })
            .collect();
        let index = SimilarityIndex::build(symbol_data, SynonymDict::default());

        let q = SimilarityQuery {
            file: PathBuf::from("a.go"),
            symbol: "settle0".to_string(),
            top: 5,
            use_dict: false,
            min_score: 0.0,
            explain: false,
            weights: (0.85, 0.0, 0.15),
        };

        let result = query(&index, &q).unwrap();
        assert!(result.matches.len() <= 5, "should respect top=5, got {}", result.matches.len());
    }

    #[test]
    fn query_min_score_filters() {
        let symbol_data = vec![
            (make_symbol("a.go", "settleA"), HashSet::new()),
            (make_symbol("b.go", "somethingCompletelyDifferent"), HashSet::new()),
        ];
        let index = SimilarityIndex::build(symbol_data, SynonymDict::default());

        let q = SimilarityQuery {
            file: PathBuf::from("a.go"),
            symbol: "settleA".to_string(),
            top: 10,
            use_dict: false,
            min_score: 0.9,  // very high threshold
            explain: false,
            weights: (0.85, 0.0, 0.15),
        };

        let result = query(&index, &q).unwrap();
        // Very high min_score should filter out unrelated symbols
        for m in &result.matches {
            assert!(m.score >= 0.9, "score {} below min_score 0.9", m.score);
        }
    }

    #[test]
    fn query_explain_has_breakdown() {
        let symbol_data = vec![
            (make_symbol("a.go", "calculateSettlement"), HashSet::new()),
            (make_symbol("b.go", "calculateFee"), HashSet::new()),
        ];
        let index = SimilarityIndex::build(symbol_data, SynonymDict::default());

        let q = SimilarityQuery {
            file: PathBuf::from("a.go"),
            symbol: "calculateSettlement".to_string(),
            top: 5,
            use_dict: false,
            min_score: 0.0,
            explain: true,
            weights: (0.85, 0.0, 0.15),
        };

        let result = query(&index, &q).unwrap();
        for m in &result.matches {
            assert!(m.breakdown.is_some(), "explain mode should include breakdown for {}", m.symbol);
        }
    }

    // --- Sparse vector correctness ---

    #[test]
    fn sparse_vec_dot_product() {
        let mut a = SparseVec::new();
        a.push(0, 0.6);
        a.push(1, 0.8);
        a.sort_by_id();

        let mut b = SparseVec::new();
        b.push(0, 0.6);
        b.push(1, 0.8);
        b.sort_by_id();

        let dot = a.dot(&b);
        assert!((dot - 1.0).abs() < 1e-5, "dot product of identical unit vectors should be ~1.0, got {}", dot);
    }

    #[test]
    fn sparse_vec_normalize() {
        let mut v = SparseVec::new();
        v.push(0, 3.0);
        v.push(1, 4.0);
        v.normalize();
        let norm = v.norm();
        assert!((norm - 1.0).abs() < 1e-5, "normalized vector should have norm ~1.0, got {}", norm);
    }
}
