# DESIGN — Similarity stack + `aft similar` (Tier 3)

Status: design (not implemented)
Scope: Rust-side only. Five-layer similarity computation with optional project synonym dict. New `aft similar` command.
No helper changes. No new `EdgeKind` values.

## Motivation

CBM computes `SIMILAR_TO` edges via embeddings over function bodies / identifiers. An agent asks "what's similar to `calculateSettlementFee`" and gets semantically-related code even when identifiers differ.

AFT has no equivalent. An agent asking "what else handles money?" has to grep for words and miss anything phrased differently.

Embeddings are the brute-force solution: powerful, but require a local model (~200MB), GPU-friendly runtime, and opaque output (the cosine says so — can't explain why). For code specifically, a lighter dict-based stack captures 75–85% of the value at a small fraction of the infrastructure cost, and it's **explainable** (the output can show exactly why two symbols were judged similar).

## Design principles (binding)

1. **No embeddings by default.** Keep install small. Deterministic, explainable output.
2. **Explainability is a feature.** `--explain` shows the ranking logic — matched tokens, TF-IDF weights, synonym expansions, co-citation overlap. This is something embeddings fundamentally can't do.
3. **Project-local customization via dict.** CBM's embeddings don't know your team's jargon. AFT's synonym dict does.
4. **Stack, not one algorithm.** Five independent layers; each adds distinct signal. Each can be toggled.
5. **Index at configure-time, query sub-100ms.** No query-time parsing of identifiers across the whole project.

## Architecture

Similarity has two phases: **index build** (runs once per configure, or incrementally with the persistent cache from `DESIGN-persistent-graph.md`) and **query** (hot path, must be fast).

### Index build

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

All stored in `similarity-index.cbor` under the project's cache dir (same cache infra as `DESIGN-persistent-graph.md`).

Build-time budget: under 500ms for 10k symbols. Tokenize+stem is microseconds per identifier.

### Query

`aft similar <file> <symbol> [--dict] [--explain] [--top=N] [--min-score=F]`

1. Look up `symbol_vectors[target]` → target's sparse vector.
2. For each candidate symbol: compute cosine similarity with target vector.
3. With `--dict`: expand target's tokens through synonym dict before comparing (union of original stems + synonym stems, weighted slightly lower to reflect approximation).
4. Combine with call-graph co-citation score (fraction of shared callees) — weighted into a final score.
5. Rank, return top N.

Query-time budget: under 50ms for top-10 across 10k symbols. TF-IDF sparse cosine is ~microseconds per pairwise comparison; pruning by vocabulary overlap keeps the comparison set small.

## Layer 1: Identifier tokenization

Rules (stateless, Unicode-aware):
- Split at camelCase boundaries: `calculateSettlementFee` → `[calculate, Settlement, Fee]`.
- Split at snake_case: `calculate_settlement_fee` → `[calculate, settlement, fee]`.
- Split at PascalCase: `HTTPHandler` → `[HTTP, Handler]`. (Runs of uppercase + following capitalized word: `HTTPHandler` → `[HTTP, Handler]`, not `[HTTPHandler]`.)
- Lowercase all tokens after splitting.
- Drop tokens shorter than 2 chars (`x`, `i`) — almost always loop variables.
- Drop Go-convention noise tokens: `{err, ctx, ok, nil, true, false}`.

Implementation: one small `tokenize_identifier(s: &str) -> Vec<String>` function. Pure, no external deps.

Golden tests required for the full grammar of edge cases.

## Layer 2: Snowball stemming

Dependency: [`rust-stemmers`](https://crates.io/crates/rust-stemmers) (Snowball, Apache-2.0, zero-dep).

Stem each token post-tokenization. Algorithm: English (Porter2/Snowball-English).

Examples:
- `calculating`, `calculated`, `calculate` → `calcul`
- `settlements`, `settlement` → `settlement` (both stem identically)
- `disburse`, `disbursement` → `disburs`

Why English only: Go identifiers are overwhelmingly English even in non-English teams. Other languages rare enough that failing safely (no stem, original token used) is acceptable.

Implementation: wrap rust-stemmers in a `stem(tok) -> String` helper. Cache stem results per-session (the vocabulary is finite per-project; the stemmer gets called ~10k times per configure, worth caching to a HashMap).

## Layer 3: TF-IDF weighting

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

## Layer 4: Project synonym dict (the differentiator)

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

## Layer 5: Call-graph co-citation

Two functions are more similar if they share a large fraction of their callees. This is pure graph structure, no lexical input.

```
co_citation(a, b) = |callees(a) ∩ callees(b)| / |callees(a) ∪ callees(b)|
```

Jaccard index over callee sets. Computed on-demand at query time (not pre-indexed — callee sets change per-query).

**When to use:** as a tie-breaker / fallback when TF-IDF similarity is low. Two functions named `processA` and `handleB` might have no lexical overlap but call the same set of helper functions — a signal worth surfacing.

Configurable weight in final score.

## Final score

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

## `aft similar` command

### Signature

```
aft similar <file> <symbol> [--top=N] [--dict] [--explain] [--min-score=F]
```

Flags:
- `--top=N` — return top N candidates. Default 10.
- `--dict` — apply synonym dict. Default off (opt-in; otherwise silent no-op if no dict file exists).
- `--explain` — include per-candidate scoring breakdown in output.
- `--min-score=F` — drop candidates with score below F. Default 0.15 (empirical noise floor).

### Output (without `--explain`)

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

### Output (with `--explain`)

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

The `--explain` output is verbose but bounded: top 10 matches × top 5 contributors per match × short lists = few hundred KB max. It's also the output that makes AFT useful as a developer tool, not just an agent tool — an engineer debugging "why did AFT say these are similar" gets a real answer.

## Performance budget

| Metric | Target | Notes |
|---|---|---|
| Index build (10k symbols) | < 500ms | Tokenize + stem + TF-IDF. Happens once per configure. |
| Index disk footprint | < 5MB | Sparse vectors compress well. CBOR-encoded. |
| Index memory | < 20MB | Sparse vectors stay in memory after load. |
| Query latency (top-10) | < 50ms | Pairwise cosine with vocabulary overlap pruning. |
| Co-citation computation | < 10ms per query | Set intersection on callee lists; small sets. |
| `--explain` overhead | < 20ms extra | String building; bounded by top-N. |

## Rollout / feature flag

- Rust: `[similarity] enabled = true`, `[similarity] auto_build_index = true`.
- CLI: `aft similar` is the only user-facing surface; no-op if index missing and `--no-auto-build` set.
- Dict is opt-in via file presence; no flag needed to disable.

## Tests

1. **Tokenizer golden tests**
   - camelCase, snake_case, PascalCase, mixed.
   - Acronyms: `HTTPHandler`, `JSONParse`, `URLMatcher`.
   - Numbers: `V3`, `OAuth2`, `SHA256`.
   - Non-ASCII: Unicode identifiers (rare but valid in Go).
   - Noise-token drop: `err`, `ctx`, `i`, `ok`.

2. **Stemmer integration**
   - `calculate`/`calculated`/`calculating` → same stem.
   - Known false positives (e.g., `banner` ≠ `ban`) — document, don't try to fix.

3. **TF-IDF correctness**
   - Known-weight case: in a 100-symbol corpus, a token in 1 symbol gets much higher IDF than a token in 50 symbols.
   - Normalization: unit-length vectors, cosine bounded [-1, 1] (with non-negative weights, [0, 1]).

4. **Synonym dict**
   - Load, validate, apply.
   - Malformed TOML → warning, no crash.
   - Chain synonyms: `A ↔ B ↔ C` — A's query finds C-containing symbols.

5. **Co-citation**
   - Synthetic call graph where two functions share callees but have unrelated names.
   - Jaccard computed correctly.

6. **End-to-end `aft similar`**
   - Against a curated fixture project with known similarity relationships.
   - `--explain` produces coherent breakdowns.
   - `--top=N` and `--min-score=F` honored.

7. **Benchmarks**
   - 10k-symbol synthetic project: index build < 500ms, query < 50ms, top-10 correctness.

## Open questions for the implementer

1. **Stemming language detection:** Go projects are usually English, but if the project has 20%+ non-ASCII identifiers, should we skip stemming entirely? *Default: always English-stem. Non-ASCII tokens pass through unchanged via the stemmer's built-in behavior.*

2. **Function body content:** should similarity use body text, not just identifier? *Default: no. Identifier-only is cheap and sufficient for 80% of cases. Body similarity is a separate future tier; it's expensive and noisy (error-handling boilerplate dominates).*

3. **Multi-term synonym expansion:** if a target token has synonyms, do we expand each synonym's synonyms too (transitive closure)? *Default: no, one hop. Prevents runaway expansion. Users write dict groups; groups are already clusters.*

4. **Ranking tie-breaker:** at identical scores, how to order? *Default: alphabetical by qualified name. Deterministic; no surprise ranking shuffles across runs.*

## Out of scope

- Embeddings (explicitly excluded per design principle 1). May be added later as `--embeddings` opt-in.
- Cross-project similarity (e.g., "what in org-wide codebase is similar"). Per-project only.
- Body / docstring similarity. Identifier-only for this tier.
- Runtime similarity (e.g., "functions with similar execution profiles"). Out of AFT's static-analysis scope.

## Summary

Five-layer dict-based similarity: tokenize, Snowball stem, TF-IDF weighting, optional project synonym dict, call-graph co-citation. No embeddings, no model files, < 5MB index, < 50ms queries. `aft similar` with `--dict` and `--explain`. Synonym dict is AFT's differentiator vs CBM — project-specific jargon beats generic embeddings.
