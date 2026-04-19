# ADR-0005: Similarity stack and `aft similar`

## Status

Accepted — shipped in commit 90edba9.

## Context

CBM computes `SIMILAR_TO` edges via embeddings over function bodies / identifiers. An agent asks "what's similar to `calculateSettlementFee`" and gets semantically-related code even when identifiers differ.

AFT has no equivalent. An agent asking "what else handles money?" has to grep for words and miss anything phrased differently.

Embeddings are the brute-force solution: powerful, but require a local model (~200MB), GPU-friendly runtime, and opaque output (the cosine says so — can't explain why). For code specifically, a lighter dict-based stack captures 75–85% of the value at a small fraction of the infrastructure cost, and it's **explainable** (the output can show exactly why two symbols were judged similar).

Binding design principles:

1. **No embeddings by default.** Keep install small. Deterministic, explainable output.
2. **Explainability is a feature.** `--explain` shows the ranking logic — matched tokens, TF-IDF weights, synonym expansions, co-citation overlap. This is something embeddings fundamentally can't do.
3. **Project-local customization via dict.** CBM's embeddings don't know your team's jargon. AFT's synonym dict does.
4. **Stack, not one algorithm.** Five independent layers; each adds distinct signal. Each can be toggled.
5. **Index at configure-time, query sub-100ms.** No query-time parsing of identifiers across the whole project.

## Decision

### Architecture

Similarity has two phases: **index build** (runs once per configure, or incrementally with the persistent cache from ADR-0004-persistent-graph.md) and **query** (hot path, must be fast).

#### Index build

Input: list of all symbols in the project (from tree-sitter's outline data).

For each symbol:
1. **Tokenize** its identifier → list of sub-words.
2. **Stem** each token → list of stem tokens.
3. **Compute TF-IDF weights** over the project's stem corpus.

Project-wide products:
- `vocabulary: HashMap<StemToken, TokenId>` — token → id mapping.
- `idf: HashMap<TokenId, f32>` — inverse document frequency per token (document = one symbol's identifier).
- `symbol_vectors: HashMap<SymbolRef, SparseVector>` — per-symbol (token_id → weight) sparse vectors.

Optional:
- `synonyms: HashMap<StemToken, HashSet<StemToken>>` loaded from `.aft/synonyms.toml` if present.

All stored in `similarity-index.cbor` under the project's cache dir (same cache infrastructure as ADR-0004-persistent-graph.md).

Build-time budget: under 500ms for 10k symbols. Tokenize+stem is microseconds per identifier.

#### Query

`aft similar <file> <symbol> [--dict] [--explain] [--top=N] [--min-score=F]`

1. Look up `symbol_vectors[target]` → target's sparse vector.
2. For each candidate symbol: compute cosine similarity with target vector.
3. With `--dict`: expand target's tokens through synonym dict before comparing (union of original stems + synonym stems, weighted slightly lower to reflect approximation).
4. Combine with call-graph co-citation score (fraction of shared callees) — weighted into a final score.
5. Rank, return top N.

Query-time budget: under 50ms for top-10 across 10k symbols. TF-IDF sparse cosine is ~microseconds per pairwise comparison; pruning by vocabulary overlap keeps the comparison set small.

### Layer 1: Identifier tokenization

Rules (stateless, Unicode-aware):
- Split at camelCase boundaries: `calculateSettlementFee` → `[calculate, Settlement, Fee]`.
- Split at snake_case: `calculate_settlement_fee` → `[calculate, settlement, fee]`.
- Split at PascalCase: `HTTPHandler` → `[HTTP, Handler]`. (Runs of uppercase + following capitalized word: `HTTPHandler` → `[HTTP, Handler]`, not `[HTTPHandler]`.)
- Lowercase all tokens after splitting.
- Drop tokens shorter than 2 chars (`x`, `i`) — almost always loop variables.
- Drop Go-convention noise tokens: `{err, ctx, ok, nil, true, false}`.

Implementation: one small `tokenize_identifier(s: &str) -> Vec<String>` function. Pure, no external deps.

### Layer 2: Snowball stemming

Dependency: [`rust-stemmers`](https://crates.io/crates/rust-stemmers) (Snowball, Apache-2.0, zero-dep).

Stem each token post-tokenization. Algorithm: English (Porter2/Snowball-English).

Examples:
- `calculating`, `calculated`, `calculate` → `calcul`
- `settlements`, `settlement` → `settlement` (both stem identically)
- `disburse`, `disbursement` → `disburs`

Why English only: Go identifiers are overwhelmingly English even in non-English teams. Other languages rare enough that failing safely (no stem, original token used) is acceptable.

Implementation: wrap rust-stemmers in a `stem(tok) -> String` helper. Cache stem results per-session (the vocabulary is finite per-project; the stemmer gets called ~10k times per configure, worth caching to a HashMap).

### Layer 3: TF-IDF weighting

Built once per configure. For each stem token `t`:

```
df(t) = count of symbols containing t at least once
idf(t) = ln((N + 1) / (df(t) + 1)) + 1   // smoothed IDF
```

Where `N` = total symbol count.

For each symbol:
```
tf(t, sym) = count of t in sym.tokens
tfidf(t, sym) = tf(t, sym) * idf(t)
```

Store sparse vectors. Normalize each vector to unit length (so cosine similarity reduces to dot product).

**Effect:** tokens like `handler`, `service`, `get`, `new` appear in hundreds of symbols → very low IDF → near-zero contribution to similarity. Tokens like `settle`, `merchant`, `kafka` appear in a handful of symbols → high IDF → dominate similarity scoring. This is what makes the stack work without domain knowledge.

### Layer 4: Project synonym dict (the differentiator)

`.aft/synonyms.toml` at project root. Opt-in — absent by default.

```toml
# AFT Similarity Synonym Dictionary — optional
# Format: each line defines a group of terms considered synonyms.

[groups]
settlement = ["payout", "disburse", "settle"]
merchant = ["seller", "vendor"]
provider = ["gateway", "psp"]
initiate = ["start", "begin", "kickoff"]
recover = ["rollback", "undo", "revert"]
```

Semantic: the listed tokens form equivalence classes. A query for a symbol tokenized with `settlement` is matched against symbols containing `payout`, `disburse`, or `settle`, at a slight weight penalty (recommended: 0.85× the original token's weight — configurable).

Loading: read at configure-time, stem each term, build a `HashMap<StemToken, HashSet<StemToken>>` of bidirectional equivalences. Stored in the index.

Schema errors (non-string values, malformed TOML): log a warning, proceed without the dict. Never block configure on a bad dict.

**This is the one thing CBM embeddings genuinely can't match** — they're trained on general code, not your codebase's vocabulary.

### Layer 5: Call-graph co-citation

Two functions are more similar if they share a large fraction of their callees. This is pure graph structure, no lexical input.

```
co_citation(a, b) = |callees(a) ∩ callees(b)| / |callees(a) ∪ callees(b)|
```

Jaccard index over callee sets. Computed on-demand at query time (not pre-indexed — callee sets change per-query).

**When to use:** as a tie-breaker / fallback when TF-IDF similarity is low. Two functions named `processA` and `handleB` might have no lexical overlap but call the same set of helper functions — a signal worth surfacing.

Configurable weight in final score.

### Final score

```
score(target, candidate) =
    w_lex * cosine_sim(tfidf_vec(target), tfidf_vec(candidate))
  + w_syn * cosine_sim(tfidf_vec(target, synonym_expanded), tfidf_vec(candidate))
  + w_cit * co_citation(target, candidate)
```

Default weights (configurable in `[similarity]` section of aft settings):

```
w_lex = 0.70     # TF-IDF identifier similarity (primary signal)
w_syn = 0.15     # only non-zero when --dict is set; adds ≤0.15 from synonym expansion
w_cit = 0.15     # call-graph structure
```

Weights sum to 1.0 for readability; final score is in [0, 1].

`w_syn` is 0 when `--dict` is off (no synonym dict loaded, or flag off); in that case `w_lex` absorbs it for a total of 0.85 lex + 0.15 cit.

### `aft similar` command

#### Signature

```
aft similar <file> <symbol> [--top=N] [--dict] [--explain] [--min-score=F]
```

Flags:
- `--top=N` — return top N candidates. Default 10.
- `--dict` — apply synonym dict. Default off (opt-in; otherwise silent no-op if no dict file exists).
- `--explain` — include per-candidate scoring breakdown in output.
- `--min-score=F` — drop candidates with score below F. Default 0.15 (empirical noise floor).

#### Output (without `--explain`)

```json
{
  "query": {"file": "merchant_settlement/service.go", "symbol": "SettleMerchantSettlement"},
  "matches": [
    {"file": "early_settlement/service.go", "symbol": "processEarlySettlementV3", "score": 0.72},
    {"file": "merchant_settlement/service.go", "symbol": "OnHoldMerchantSettlement", "score": 0.68},
    {"file": "realtime_settlement/service.go", "symbol": "settleRealtime", "score": 0.64}
  ]
}
```

#### Output (with `--explain`)

```json
{
  "query": {"file": "...", "symbol": "SettleMerchantSettlement"},
  "target_tokens": [
    {"token": "settl", "tfidf": 0.42},
    {"token": "merchant", "tfidf": 0.38}
  ],
  "matches": [
    {
      "file": "early_settlement/service.go",
      "symbol": "processEarlySettlementV3",
      "score": 0.72,
      "breakdown": {
        "lex": 0.65,
        "lex_contributors": [
          {"token": "settl", "target_weight": 0.42, "candidate_weight": 0.38, "product": 0.16},
          {"token": "process", "target_weight": 0.12, "candidate_weight": 0.25, "product": 0.03}
        ],
        "synonyms": 0.0,
        "co_citation": 0.81,
        "shared_callees": ["FindOrCreateProcessingMerchantSettlement", "GetMerchantByID"]
      }
    }
  ]
}
```

The `--explain` output is verbose but bounded: top 10 matches × top 5 contributors per match × short lists = few hundred KB max.

### Rollout / feature flag

- Rust: `[similarity] enabled = true`, `[similarity] auto_build_index = true`.
- CLI: `aft similar` is the only user-facing surface; no-op if index missing and `--no-auto-build` set.
- Dict is opt-in via file presence; no flag needed to disable.

## Consequences

### Positive consequences

- `aft similar` provides semantically similar symbol lookup without a model file, without GPU, without a network call. Install size is unchanged.
- The `--explain` output is the primary differentiator vs embedding-based approaches: agents and developers can see exactly which tokens and shared callees drove the similarity score.
- The synonym dict lets teams encode domain vocabulary (settlement ↔ payout ↔ disburse) that no pre-trained model knows.
- Index build < 500ms for 10k symbols; query < 50ms for top-10; index < 5MB on disk.
- The similarity index stores under the same cache dir as ADR-0004-persistent-graph.md — one cache dir per project.

### Trade-offs

- Estimated coverage is 75–85% of what embedding-based approaches achieve. Identifiers with no lexical overlap and no shared callees score zero even when semantically related.
- English-only stemming: non-English identifiers pass through unchanged (no false-negative, just no stem normalization).
- `co_citation` is computed on-demand at query time (not pre-indexed). For large projects with high fan-out, this could approach the 10ms budget ceiling.
- The synonym dict requires manual curation. An absent or stale dict gives lower synonym coverage than a well-maintained one.

### Open follow-ups

1. **Stemming language detection:** Go projects are usually English, but if the project has 20%+ non-ASCII identifiers, a future iteration could skip stemming entirely. Currently: always English-stem; non-ASCII tokens pass through unchanged.

2. **Function body content:** a future tier could use body text similarity, not just identifier similarity. Currently out of scope — identifier-only is cheap and sufficient for 80% of cases; body similarity is expensive and noisy (error-handling boilerplate dominates).

3. **Multi-term synonym expansion:** if a target token has synonyms, those synonyms' synonyms are not expanded (one hop only). This prevents runaway expansion. Users write dict groups; groups are already clusters.

4. **Ranking tie-breaker:** at identical scores, ordering is alphabetical by qualified name. Deterministic; no surprise ranking shuffles across runs.

5. **Embeddings opt-in:** explicitly excluded per the no-embeddings principle. May be added later as `--embeddings` opt-in.

6. **Cross-project similarity:** per-project only. Cross-project (org-wide codebase) similarity is out of scope.

## Alternatives considered

**Embeddings as the primary approach** was explicitly rejected. The infrastructure cost (local model, ~200MB download, GPU-friendly runtime) and opaque output (cosine says so — can't explain why) make it a poor fit for the "lightweight, deterministic, explainable" design goal. The dict-based stack captures 75–85% of the value at a fraction of the cost, and the synonym dict gives domain customization that pre-trained models cannot.

**Body/docstring similarity** was considered and deferred. Identifier-only is sufficient for 80% of cases; body-level similarity is expensive to compute and noisy (error-handling boilerplate, parameter validation patterns dominate over domain signal).
