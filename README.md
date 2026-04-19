<p align="center">
  <img src="assets/banner.jpeg" alt="AFT — Agent File Toolkit" width="50%" />
</p>

<h1 align="center">AFT — Agent File Toolkit</h1>

<p align="center">
  <strong>Tree-sitter powered code analysis tools for AI coding agents.</strong><br>
  Semantic editing, call-graph navigation, and structural search — all in one toolkit.
</p>

<p align="center">
  <a href="https://crates.io/crates/agent-file-tools"><img src="https://img.shields.io/crates/v/agent-file-tools?label=crate&color=blue&style=flat-square" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/@cortexkit/aft-opencode"><img src="https://img.shields.io/npm/v/@cortexkit/aft-opencode?color=blue&style=flat-square" alt="npm"></a>
  <a href="https://github.com/cortexkit/aft/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-green?style=flat-square" alt="MIT License"></a>
</p>

<p align="center">
  <a href="#get-started">Get Started</a> ·
  <a href="#what-is-aft">What is AFT?</a> ·
  <a href="#search-benchmarks">Benchmarks</a> ·
  <a href="#features">Features</a> ·
  <a href="#tool-reference">Tool Reference</a> ·
  <a href="#configuration">Configuration</a> ·
  <a href="#architecture">Architecture</a>
</p>

---

## Get Started

Pick your agent. Each install guide is collapsed below — expand only the one you use.

<details>
<summary><strong>Claude Code</strong> — hook-based tool interception</summary>

Run the install script:

```bash
./scripts/install-claude-hooks.sh
```

This installs:
- **Tool interception hooks** — `Grep` and `Glob` route through AFT for indexed performance; a first-call discovery gate nudges Claude toward semantic tools before raw `Read`/`Search`.
- **CLI wrapper** — the `aft` command is placed on `PATH` for shell use (`aft outline src/`, `aft zoom file sym`, etc.).
- **Session reminder** — a `SessionStart` hook injects AFT's code-discovery protocol at the top of every session.
- **Instructions** — `~/.claude/AFT.md` is added to the global `CLAUDE.md` include chain.

After install, restart Claude Code. See the [Tool Reference](#tool-reference) for every command.

**Uninstall:**

```bash
./scripts/uninstall-claude-hooks.sh
```

</details>

<details>
<summary><strong>Codex</strong> — prompt-injection guidance hooks</summary>

Run the install script:

```bash
./scripts/install-codex-hooks.sh
```

This installs:
- **SessionStart hook** — injects AFT's code-discovery protocol at session start.
- **UserPromptSubmit hook** — nudges the agent toward the right semantic command based on the prompt shape.
- **CLI wrapper** — `aft` command for shell use.
- **Instructions** — `~/.codex/AFT.md` is added to the global `AGENTS.md` include chain.
- **Codex config** — `~/.codex/config.toml` gains `codex_hooks = true` and suppresses the unstable-feature warning.

Codex hooks currently do **not** replace its non-Bash file tools, so this integration teaches
Codex when to call `aft` explicitly via shell rather than transparently intercepting `Read`/`Grep`.

After install, restart Codex. See the [Tool Reference](#tool-reference) for every command.

**Uninstall:**

```bash
./scripts/uninstall-codex-hooks.sh
```

</details>

<details>
<summary><strong>OpenCode</strong> — not published for this fork</summary>

The OpenCode integration lives as an npm package. **This fork has not published an OpenCode
package**, so the plugin-based OpenCode install path is not available here.

If you want AFT inside OpenCode *without* the fork's accuracy features (dispatch edges,
implementation edges, control-flow context, similarity stack, etc.), install the upstream
[cortexkit/aft](https://github.com/cortexkit/aft) OpenCode package:

```bash
bunx @cortexkit/aft-opencode@latest setup
```

That gets you upstream AFT's OpenCode integration, with none of this fork's additions. If you
want this fork's features in OpenCode, either use Claude Code / Codex instead, or let us know
via an issue so publishing priority can go up.

</details>

---

## Accuracy-Focused Fork (nandanugg/aft)

This is a **fork** of the upstream [cortexkit/aft](https://github.com/cortexkit/aft), adding
features driven by one question: *does routing an AI coding agent's code exploration through
richer structural tools actually make its generated documentation more accurate, or just more
verbose?* The answer, after a five-iteration measurement study against two other tools in the
same space, is **yes — and by a measurable margin**.

### The study (brief)

A business-flow documentation task was given to Claude Code running inside three isolated
Docker containers, each configured with exactly one code-navigation tool: this fork, the
[codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp) server, and
[Serena](https://github.com/oraios/serena). Each tool produced five independent documentation
passes (15 runs total). Factual claims were extracted from each doc and verified against the
real codebase — a production Go service with 473 files and ~10k symbols.

**Results (lower is better):**

| tool                                 | wrong-rate | stale-oracle catches |
|--------------------------------------|-----------:|---------------------:|
| **nandanugg/aft (this fork, v3)**    | **18.0 %** |                   16 |
| codebase-memory-mcp                  |    20.2 %  |                   12 |
| cortexkit/aft (upstream, baseline)   |    22.6 %  |                    3 |
| Serena                               |    23.7 %  |                    3 |

"Stale-oracle catches" = cases where the agent's doc disagreed with a prior knowledge-base
summary AND the real code sided with the agent. Higher is better — the tool is helping the
agent trust current code over stale priors.

### What this fork adds

Each new command follows the standard `[desc] / [input] / [output]` shape. Full parameters and
every output field live in [Tool Reference](#tool-reference); the blurbs here show typical
shape. Fork-only additions are linked individually:

- [`aft dispatched_by`](#aft-dispatched_by) — reverse lookup: who registered this as a handler?
- [`aft dispatches`](#aft-dispatches) — forward lookup by dispatch key to its handler.
- [`aft implementations`](#aft-implementations) — which concrete types satisfy an interface?
- [`aft writers`](#aft-writers) — cross-package writers to a package-level variable.
- [`aft similar`](#aft-similar) — semantically-similar symbols, dict-based, no embeddings.

#### `aft dispatched_by` (preview)

**desc.** Reverse lookup on dispatch edges. Given a handler function, returns every call site
that passed it as a function-value argument, along with the FQN of the receiving call (so the
caller can tell `asynq.HandleFunc` from `redis.Set` from `logger.With`) and the dispatch key
string when one is present.

**input.**

```bash
aft dispatched_by server/asynq_handler.go HandleMerchantSettlementTask
```

**output.**

```
dispatched_by HandleMerchantSettlementTask (server/asynq_handler.go)  total=1
  - startAsyncQueueServer (server/asynq_server.go:69)
      key=merchant_settlement:merchant_id
      via (*github.com/hibiken/asynq.ServeMux).HandleFunc
```

Full parameters & JSON schema: [Tool Reference ▸ `aft dispatched_by`](#aft-dispatched_by).

#### `aft implementations` (preview)

**desc.** Which concrete types satisfy this interface. Covers same-package / same-file pairs
(upstream dropped these on a flawed tree-sitter assumption). Mock directories filtered by
default; pass `--include-mocks` to see them.

**input.**

```bash
aft implementations store/settlement_store.go SettlementStorer
```

**output.**

```
implementations of SettlementStorer (store/settlement_store.go)  total=1
  *store.settlementStore  (store):
    - Create (store/settlement_store.go:125)
    - FindOrCreate (store/settlement_store.go:501)
    - ListByMerchantID (store/settlement_store.go:251)
    ... 40 more methods
```

Full params: [Tool Reference ▸ `aft implementations`](#aft-implementations).

#### `aft similar` (preview)

**desc.** Semantically similar symbols computed from identifier tokens (camelCase split +
Snowball stem + project TF-IDF + optional synonym dict + call-graph co-citation). No embedding
model; explainable rankings.

**input.**

```bash
aft similar merchant_settlement/service.go SettleMerchantSettlement --top=3 --explain
```

**output.** (abbreviated)

```json
{
  "query": {"file": "merchant_settlement/service.go", "symbol": "SettleMerchantSettlement"},
  "matches": [
    {"file": "early_settlement/service.go",    "symbol": "processEarlySettlementV3", "score": 0.72},
    {"file": "merchant_settlement/service.go", "symbol": "OnHoldMerchantSettlement", "score": 0.68},
    {"file": "realtime_settlement/service.go", "symbol": "settleRealtime",           "score": 0.64}
  ]
}
```

With `--explain` each match includes a `breakdown` object listing the matching stem tokens with
their TF-IDF contributions and the shared callees driving co-citation. Full params & field-level
output: [Tool Reference ▸ `aft similar`](#aft-similar).

`dispatches` and `writers` follow the same two-field (`desc / input / output`) shape — see their
entries in the Tool Reference.

### New structural data the agent gets for free

Every existing command (`aft callers`, `aft call_tree`, `aft trace_to`, `aft zoom`) returns
richer output on this fork without any new command surface:

- **Dispatch / goroutine / defer edge kinds** — call-graph results now distinguish direct calls
  from `go fn()`, `defer fn()`, and function-value registrations.
- **Constant-resolved `nearby_string`** — when a dispatch key is `string(pkg.TypedConst)`, the
  resolved literal shows up in results instead of being silently dropped.
- **Dispatched-via FQN** — every registration edge carries the receiving call's qualified name.
- **Call-context flags** — every caller edge is annotated with `in_defer`, `in_goroutine`,
  `in_loop`, `in_error_branch`, and a `branch_depth`. Derived from SSA dominator analysis.
- **Per-return path conditions** — `aft zoom <file> <func>` now includes a `returns` block
  showing each return statement's path condition (the conjunction of dominating ifs) and the
  returned expression. Critical for documenting retry/error semantics without guessing.
- **Package-level var / const nodes** — show up in `aft outline` as first-class symbols;
  `aft callers` resolves cross-package writes to them.
- **Persistent merged-graph cache** — second invocation on the same tree is ~30× faster. CBOR
  mtime index; no daemon; no behavior change at the agent level, just warm-start latency.
- **Closure-to-handler resolution** — anonymous registration lambdas
  (`mux.HandleFunc("X", func(...) { return Handler(...) })`) resolve through to the inner
  named handler when there's exactly one in-project call in the body. This alone closes the
  async-dispatch accuracy gap measured in the study.

### Steering-layer changes (Claude Code)

- **SessionStart reminder** — injects an AFT code-discovery protocol into every session.
  Biases the agent toward structural tools before raw `Grep`/`Glob`/`Read`, and toward trusting
  the current code over stale prior knowledge.
- **PreToolUse discovery gate** — blocks the *first* raw `Grep|Glob|Read|Search` of a session
  with a nudge toward `aft outline` / `aft trace_to` / `aft callers`. One-shot per session;
  subsequent calls pass through unmolested.

### Design docs and reproduction

Each feature has a design doc under `docs/DESIGN-*.md` with the SSA mechanics, filter rules,
performance budget, and rollout strategy:

- [DESIGN-dispatch-edges.md](docs/DESIGN-dispatch-edges.md) — dispatch, goroutine, defer edges.
- [DESIGN-call-site-provenance.md](docs/DESIGN-call-site-provenance.md) — `dispatched_via` FQN + typed-constant resolution.
- [DESIGN-interface-edges.md](docs/DESIGN-interface-edges.md) — `aft implementations` + implements edges.
- [DESIGN-variable-nodes.md](docs/DESIGN-variable-nodes.md) — var/const outline + cross-package writes.
- [DESIGN-control-flow-context.md](docs/DESIGN-control-flow-context.md) — call-context flags + per-return path conditions.
- [DESIGN-similarity.md](docs/DESIGN-similarity.md) — tokenize / stem / TF-IDF / synonym dict / co-citation.
- [DESIGN-persistent-graph.md](docs/DESIGN-persistent-graph.md) — CBOR cache + incremental updates.

Upstream cortexkit/aft remains the source of everything structural about AFT's core
architecture (tree-sitter parser, edit primitives, Codex integration, etc.). This fork
contributes the extensions above and the accuracy-centered measurement work. Features may or
may not be accepted upstream; this fork stands regardless.

---

## What is AFT?

AFT addresses code by what it *is* — a function, a class, a call site, a symbol — rather than
by line number. It's a two-component system: a Rust binary that does parsing, analysis, edits,
and formatting on top of tree-sitter concrete syntax trees; and a set of agent integrations
(Claude Code hooks, Codex prompt hooks, OpenCode plugin) that expose those operations as tool
calls. Every operation is symbol-aware by default, which makes agent edits stable against
unrelated line shifts and cuts token usage sharply — a file outline is ~10 % of a full read,
and `zoom` on a single function skips everything else.

Details on how each operation is structured live in [**Tool Reference**](#tool-reference).

---

## How it Helps Agents

Three pain points agents hit every session:

- **Token blow-up** — reading whole files to find one function wastes context.
- **Line-number fragility** — edits made by line break the moment something above them moves.
- **Blind navigation** — "who calls this?" and "what does this break?" require grep + cross-file reads.

Each of the tools below solves one. Full parameter list + every output field is in
[Tool Reference](#tool-reference); this section picks a flagship subset and shows the `[desc] / [input] / [output]` shape that repeats across every AFT tool.

---

#### `aft_outline`

**desc.** Structural outline of a file, files, or directory. Returns every top-level symbol
(functions, classes, types, vars) with kind, visibility, signature, and line range — no bodies.
Typically 10 % of the tokens a full `read` costs on the same file.

**input.**

```json
{ "filePath": "src/auth/session.ts" }
```

**output.**

```
src/auth/session.ts
  E fn    createSession(userId: string, opts?: SessionOpts): Promise<Session> 12:38
  E fn    validateToken(token: string): boolean 40:52
  E fn    refreshSession(sessionId: string): Promise<Session> 54:71
  - fn    signPayload(data: Record<string, unknown>): string 73:80
  E type  SessionOpts 5:10
  E var   SESSION_TTL 3:3
```

`E` = exported, `-` = private. Kinds: `fn`, `class`, `type`, `var`, `const`, etc.

---

#### `aft_zoom`

**desc.** Read a single symbol with call-graph annotations. Shows the body, who it calls out to,
and who calls it in — in one request, instead of three separate `read` + `grep` sequences.

**input.**

```json
{ "filePath": "src/auth/session.ts", "symbol": "validateToken" }
```

**output.**

```
src/auth/session.ts:40-52
  calls_out: verifyJwt (src/auth/jwt.ts:8), isExpired (src/auth/utils.ts:15)
  called_by: authMiddleware (src/middleware/auth.ts:22), handleLogin (src/routes/login.ts:45)

  39: /** Validate a JWT token and check expiration. */
  40: export function validateToken(token: string): boolean {
  41:   if (!token) return false;
  42:   const decoded = verifyJwt(token);
  43:   if (!decoded) return false;
  44:   return !isExpired(decoded.exp);
  45: }
```

---

#### `edit` (symbol mode)

**desc.** Replace a named symbol in-place. AFT finds the symbol's AST node, swaps the body, runs
the language's formatter, validates the parse, and writes a backup. No line counting, no diff
that breaks when something above it shifts.

**input.**

```json
{
  "filePath": "src/auth/session.ts",
  "symbol": "validateToken",
  "content": "export function validateToken(token: string): boolean {\n  if (!token) return false;\n  return verifyJwt(token);\n}"
}
```

**output.** The file is rewritten; the response carries the diff summary:

```
edit ok: src/auth/session.ts
  symbol: validateToken  (lines 40-52 → 40-43, -8 +3)
  formatter: biome (applied)
  diagnostics: 0 errors, 0 warnings
  backup: .aft/backups/src/auth/session.ts.bak.20260419-063312
```

---

#### `aft_navigate` (callers / impact modes)

**desc.** Workspace-wide call-graph lookup. `callers` mode returns every call site that lands on
a symbol. `impact` mode walks the transitive reverse graph and lists what would need to change
if the target's signature changed.

**input.**

```json
{ "op": "callers", "filePath": "src/auth/session.ts", "symbol": "validateToken", "depth": 2 }
```

**output.**

```
callers of validateToken (src/auth/session.ts)  total=3 files=3
  src/middleware/auth.ts (1):
    - authMiddleware:22
  src/routes/login.ts (1):
    - handleLogin:45
  src/routes/api.ts (1):
    - requireAuth:17  ← (depth=2, via authMiddleware)
```

---

Same four-line structure (`desc / input / output`, plus notes) applies to every tool in
[Tool Reference](#tool-reference), including fork-only additions like `dispatched_by`,
`dispatches`, `implementations`, `writers`, and `similar`.

---

## Search Benchmarks

With `experimental_search_index: true`, AFT builds a trigram index in the background and serves
grep queries from memory. Here's how it compares to ripgrep on real codebases:

### opencode-aft (253 files)

| Query | ripgrep | AFT | Speedup |
|-------|---------|-----|---------|
| `validate_path` | 31.4ms | 1.48ms | **21x** |
| `BinaryBridge` | 31.0ms | 1.3ms | **24x** |
| `fn handle_grep` | 31.3ms | 0.2ms | **136x** |
| `experimental_search_index` | 31.5ms | 0.4ms | **71x** |

### reth (1,878 Rust files)

| Query | ripgrep | AFT | Speedup |
|-------|---------|-----|---------|
| `impl Display for` | 98.9ms | 1.10ms | **90x** |
| `BlockNumber` | 61.6ms | 2.19ms | **28x** |
| `EthApiError` | 32.7ms | 1.31ms | **25x** |
| `fn execute` | 36.6ms | 2.19ms | **17x** |

### Chromium/base (3,953 C++ files)

| Query | ripgrep | AFT | Speedup |
|-------|---------|-----|---------|
| `WebContents` | 69.5ms | 0.29ms | **236x** |
| `StringPiece` | 51.8ms | 0.78ms | **66x** |
| `NOTREACHED` | 51.6ms | 2.16ms | **24x** |
| `base::Value` | 54.4ms | 1.13ms | **48x** |

Rare queries see the biggest gains — the trigram index narrows candidates to a few files instantly.
High-match queries still benefit from `memchr` SIMD scanning and early termination.

Index builds in ~2s for most projects (under 2K files). Larger codebases like Chromium/base
(~4K files) take ~2 minutes for the initial build. Once built, the index persists to disk for
instant cold starts and stays fresh via file watcher and mtime verification.

---

## Features

- **File read** — line-numbered file content, directory listing, and image/PDF detection
- **Semantic outline** — list all symbols in a file (or several files, or a directory) with kind, name, line range, visibility
- **Symbol editing** — replace a named symbol by name with auto-format and syntax validation
- **Match editing** — find-and-replace by content with fuzzy fallback (4-pass: exact → trim trailing → trim both → normalize Unicode)
- **Batch & transaction edits** — atomic multi-edit within a file, or atomic multi-file edits with rollback
- **Glob replace** — pattern replace across all matching files in one call
- **Patch apply** — multi-file `*** Begin Patch` format for creates, updates, deletes, and moves
- **Call tree & callers** — forward call graph and reverse lookup across the workspace
- **Trace-to & impact analysis** — how does execution reach this function? what breaks if it changes?
- **Data flow tracing** — follow a value through assignments and parameters across files
- **Dispatch edges & keys** *(fork)* — function-value registrations (`asynq.HandleFunc("X", h)`) with
  receiving-call FQN and constant-resolved dispatch-key strings; queryable via `aft dispatched_by` / `aft dispatches`
- **Interface implementation edges** *(fork)* — cross-package and same-file implements relationships,
  mock-filtered by default; queryable via `aft implementations`
- **Variable/const nodes** *(fork)* — package-level declarations as first-class symbols,
  with cross-package write tracking via `aft writers`
- **Control-flow context** *(fork)* — per-edge `in_defer` / `in_goroutine` / `in_loop` / `in_error_branch`
  flags and per-return path-condition analysis surfaced in `aft zoom`
- **Semantic similarity** *(fork, no embeddings)* — `aft similar` ranks by TF-IDF on stemmed identifier
  tokens plus call-graph co-citation plus optional project synonym dict
- **Persistent merged-graph cache** *(fork)* — warm runs 10-30× faster via CBOR-encoded per-file cache
- **Auto-format & auto-backup** — every edit formats the file and saves a snapshot for undo
- **Import management** — add, remove, organize imports language-aware (TS/JS/TSX/Python/Rust/Go)
- **Structural transforms** — add class members, Rust derive macros, Python decorators, Go struct tags, wrap try/catch
- **Workspace-wide refactoring** — move symbols between files (updates all imports), extract functions, inline functions
- **Safety & recovery** — undo last edit, named checkpoints, restore to any checkpoint
- **AST pattern search & replace** — structural code search using meta-variables (`$VAR`, `$$$`), powered by ast-grep
- **Git conflict viewer** — show all merge conflicts across the repository in a single call with line-numbered regions
- **Indexed search** *(experimental)* — trigram-indexed `grep` and `glob` that hoist opencode's built-ins, with background index building, disk persistence, and compressed output mode
- **Semantic search** *(experimental)* — search code by meaning using local embeddings (fastembed + all-MiniLM-L6-v2), with cAST-style symbol chunking, cosine similarity ranking, and disk persistence
- **Inline diagnostics** — write and edit return LSP errors detected after the change
- **UI metadata** — the OpenCode desktop shows file paths and diff previews (`+N/-N`) for every edit
- **Local tool discovery** — finds biome, prettier, tsc, pyright in `node_modules/.bin` automatically

---

## Tool Reference

> **All line numbers are 1-based** (matching editor, git, and compiler conventions).
> Line 1 is the first line of the file.

### Hoisted tools

These replace opencode's built-ins. Registered under the same names by default. When
`hoist_builtin_tools: false`, they get the `aft_` prefix instead (e.g. `aft_read`).

| Tool | Replaces | Description | Key Params |
|------|----------|-------------|------------|
| `read` | opencode read | File read, directory listing, image/PDF detection | `filePath`, `startLine`, `endLine`, `offset`, `limit` |
| `write` | opencode write | Write file with auto-dirs, backup, format, inline diagnostics | `filePath`, `content` |
| `edit` | opencode edit | Find/replace, symbol replace, batch, transaction, glob | `filePath`, `oldString`, `newString`, `symbol`, `content`, `edits[]` |
| `apply_patch` | opencode apply_patch | `*** Begin Patch` multi-file patch format | `patchText` |
| `ast_grep_search` | oh-my-opencode ast_grep | AST pattern search with meta-variables | `pattern`, `lang`, `paths[]`, `globs[]` |
| `ast_grep_replace` | oh-my-opencode ast_grep | AST pattern replace (applies by default) | `pattern`, `rewrite`, `lang`, `dryRun` |
| `lsp_diagnostics` | opencode lsp_diagnostics | Errors/warnings from language server | `filePath`, `directory`, `severity`, `waitMs` |
| `grep` | opencode grep *(experimental)* | Trigram-indexed regex search with compressed output | `pattern`, `path`, `include`, `exclude` |
| `glob` | opencode glob *(experimental)* | Indexed file discovery with compressed output | `pattern`, `path` |

### AFT-only tools

Always registered with `aft_` prefix regardless of hoisting setting.

**Recommended tier** (default):

| Tool | Description | Key Params |
|------|-------------|------------|
| `aft_outline` | Structural outline of a file, files, or directory | `filePath`, `files[]`, `directory` |
| `aft_zoom` | Inspect symbols with call-graph annotations | `filePath`, `symbol`, `symbols[]` |
| `aft_import` | Language-aware import add/remove/organize | `op`, `filePath`, `module`, `names[]` |
| `aft_conflicts` | Show all git merge conflicts with line-numbered regions | *(none)* |
| `aft_search` | Semantic code search by meaning *(experimental)* | `query`, `topK` |
| `aft_safety` | Undo, history, checkpoints, restore | `op`, `filePath`, `name` |

**All tier** (set `tool_surface: "all"`):

| Tool | Description | Key Params |
|------|-------------|------------|
| `aft_delete` | Delete a file with backup | `filePath` |
| `aft_move` | Move or rename a file with backup | `filePath`, `destination` |
| `aft_navigate` | Call graph and data-flow navigation | `op`, `filePath`, `symbol`, `depth` |
| `aft_transform` | Structural code transforms (members, derives, decorators) | `op`, `filePath`, `container`, `target` |
| `aft_refactor` | Workspace-wide move, extract, inline | `op`, `filePath`, `symbol`, `destination` |

**Fork-only commands (CLI):**

| Command | Description |
|---------|-------------|
| `aft dispatched_by` | Reverse lookup: who registered this function as a dispatch handler? |
| `aft dispatches` | Forward lookup: what handler is registered under this dispatch key? |
| `aft implementations` | Which concrete types satisfy this interface? |
| `aft writers` | Cross-package write-sites for a package-level variable or constant |
| `aft similar` | Semantically similar symbols ranked by TF-IDF + call-graph co-citation |

---

### read

**desc.** Plain file reading and directory listing. Pass `filePath` to read a file or a directory
path to list its entries. Paginate large files with `startLine`/`endLine` or `offset`/`limit`.
Binary, image, and PDF files are detected automatically and return metadata rather than raw bytes.

**input.** JSON object — `filePath` required; pagination params optional:

```json
// Read full file
{ "filePath": "src/app.ts" }

// Read lines 50-100
{ "filePath": "src/app.ts", "startLine": 50, "endLine": 100 }

// Read 30 lines from line 200
{ "filePath": "src/app.ts", "offset": 200, "limit": 30 }

// List directory
{ "filePath": "src/" }
```

**output.** Line-numbered content, one line per entry:

```
   1: import { createApp } from "./app"
   2:
   3: const server = createApp()
   4: server.listen(3000)
```

Directories return sorted entries with trailing `/` for subdirectories. Output is capped at 50KB.
For symbol inspection with call-graph annotations, use `aft_zoom` instead.

---

### write

**desc.** Write the full content of a file. Creates the file and any missing parent directories
if they don't exist. Backs up any existing content before overwriting, and auto-formats using
the project's configured formatter. Returns inline LSP diagnostics when type errors are introduced.

**input.** JSON object with `filePath` and `content`:

```json
{ "filePath": "src/config.ts", "content": "export const TIMEOUT = 10000;\n" }
```

**output.** Confirmation with optional inline diagnostics:

```
wrote src/config.ts (312 bytes)
```

For partial edits (find/replace or symbol replace), use `edit` instead.

---

### edit

**desc.** The main editing tool. Supports four modes selected by the parameters you pass:
find-and-replace (fuzzy 4-pass matching), symbol replace (by name), batch edits (atomic
within a file), multi-file transaction (atomic with full rollback), and glob replace. All modes
support `dryRun: true` and return inline LSP diagnostics when type errors are introduced.

**input.** JSON object — mode is determined by which fields are present:

```json
// Find and replace (fuzzy matching, 4-pass)
{ "filePath": "src/config.ts", "oldString": "const TIMEOUT = 5000", "newString": "const TIMEOUT = 10000" }

// Symbol replace (covers decorators, doc comments, attributes)
{
  "filePath": "src/utils.ts",
  "symbol": "formatDate",
  "content": "export function formatDate(d: Date): string {\n  return d.toISOString().split('T')[0];\n}"
}

// Batch edits — atomic: all apply or none do
{
  "filePath": "src/constants.ts",
  "edits": [
    { "oldString": "VERSION = '1.0'", "newString": "VERSION = '2.0'" },
    { "startLine": 5, "endLine": 7, "content": "// updated header\n" }
  ]
}

// Multi-file transaction — rolls back all files on failure
{
  "operations": [
    { "file": "a.ts", "command": "write", "content": "..." },
    { "file": "b.ts", "command": "edit_match", "match": "x", "replacement": "y" }
  ]
}

// Glob replace
{ "filePath": "src/**/*.ts", "oldString": "oldName", "newString": "newName", "replaceAll": true }
```

**output.** Confirmation with diff summary; with `dryRun: true`, a unified diff:

```
edited src/config.ts (+1/-1)
```

If multiple matches exist for a find-and-replace, use `occurrence: N` (0-indexed) to target one
or `replaceAll: true` to replace all occurrences. Set `content: ""` in a batch edit to delete lines.

---

### apply_patch

**desc.** Apply a multi-file patch using the `*** Begin Patch` format. Creates, updates, deletes,
and renames files atomically — if any operation fails, all changes revert. Context anchors use
fuzzy matching to handle whitespace and Unicode differences.

**input.** A `patchText` string in `*** Begin Patch` format:

```
*** Begin Patch
*** Add File: path/to/new-file.ts
+line 1
+line 2
*** Update File: path/to/existing-file.ts
@@ context anchor line
-old line
+new line
*** Delete File: path/to/obsolete-file.ts
*** End Patch
```

**output.** Summary of files affected, with inline LSP diagnostics for any type errors introduced:

```
applied patch: 1 added, 1 updated, 1 deleted
```

---

### ast_grep_search

**desc.** Search for structural code patterns using ast-grep meta-variables. Patterns must be
complete AST nodes — `$VAR` matches a single node, `$$$` matches multiple nodes (variadic). Returns
matches with file, line, column, matched text, and captured variable values.

**input.** JSON object with `pattern` and `lang`; optionally `paths[]`, `globs[]`, `contextLines`:

```json
{ "pattern": "console.log($MSG)", "lang": "typescript" }
```

**output.** Matches grouped by file with captured meta-variable values:

```
src/server.ts:42:5
  console.log(req.method)
  $MSG => req.method

src/utils.ts:17:3
  console.log("starting up")
  $MSG => "starting up"

Found 2 match(es) across 2 file(s).
```

Add `contextLines: 3` to include surrounding lines in each match.

---

### ast_grep_replace

**desc.** Replace structural code patterns across files using ast-grep. Meta-variables captured
in `pattern` are available in `rewrite`. Applies changes by default (with backups); set
`dryRun: true` to preview as unified diffs without writing any files.

**input.** JSON object with `pattern`, `rewrite`, and `lang`; optional `dryRun`, `paths[]`, `globs[]`:

```json
{ "pattern": "console.log($MSG)", "rewrite": "logger.info($MSG)", "lang": "typescript" }
```

**output.** Summary of files modified, or a diff per file in dry-run mode:

```
replaced 3 match(es) across 2 file(s)
  src/server.ts (+1/-1)
  src/utils.ts (+2/-2)
```

---

### lsp_diagnostics

**desc.** Get errors, warnings, and hints from the language server for a file or directory.
Lazily spawns the appropriate server (typescript-language-server, pyright, rust-analyzer, gopls)
on first use; subsequent calls reuse the live server.

**input.** JSON object — `filePath` or `directory`, optional `severity` and `waitMs`:

```json
// Check a single file for errors only
{ "filePath": "src/api.ts", "severity": "error" }

// Check all files in a directory
{ "directory": "src/", "severity": "all" }

// Wait for fresh diagnostics after an edit
{ "filePath": "src/api.ts", "waitMs": 2000 }
```

**output.** One diagnostic per line as `{ file, line, column, severity, message, code }`:

```json
[
  { "file": "src/api.ts", "line": 42, "column": 5, "severity": "error",
    "message": "Type 'string' is not assignable to type 'number'", "code": 2322 },
  { "file": "src/api.ts", "line": 67, "column": 12, "severity": "warning",
    "message": "'result' is declared but never used", "code": 6133 }
]
```

---

### aft_outline

**desc.** Returns all top-level symbols in a file with their kind, name, line range, visibility,
and nested `members` (methods in classes, sub-headings in Markdown). Accepts a single `filePath`,
a `files` array, or a `directory` to outline all source files recursively. For Markdown files
(`.md`, `.mdx`), returns heading hierarchy with section ranges.

**input.** JSON object — one of `filePath`, `files[]`, or `directory`:

```json
// Outline two files at once
{ "files": ["src/server.ts", "src/router.ts"] }

// Outline all source files in a directory
{ "directory": "src/auth" }
```

**output.** Symbols listed with kind, name, line range, and visibility:

```
src/server.ts
  function  createApp          export  1:45
  function  handleRequest      export  47:89
  class     RequestContext             91:130
    method  constructor                93:102
    method  toJSON             export  104:110
```

---

### aft_zoom

**desc.** Inspect a named symbol with full source and call-graph annotations. Returns the symbol's
body alongside `calls_out` (what it calls) and `called_by` (who calls it). Use this when you need
to understand a specific function, class, or type in detail rather than reading an entire file.

**input.** JSON object with `filePath` and `symbol` (or `symbols[]` for multiple):

```json
// Inspect a single symbol
{ "filePath": "src/app.ts", "symbol": "handleRequest" }

// Inspect multiple symbols in one call
{ "filePath": "src/app.ts", "symbols": ["Config", "createApp"] }
```

**output.** Symbol source annotated with callers and callees:

```
── handleRequest (function, export) src/app.ts:47-89 ──

called_by:
  createApp  src/app.ts:30
  retryMiddleware  src/middleware.ts:12

calls_out:
  parseBody  src/utils.ts:8
  sendResponse  src/utils.ts:44

export async function handleRequest(req: Request): Promise<Response> {
  const body = await parseBody(req)
  ...
}
```

For Markdown files, use the heading text as the symbol name (e.g. `"symbol": "Architecture"`).

---

### aft_conflicts

**desc.** Show all git merge conflicts across the repository in a single call. Auto-discovers
conflicted files via `git ls-files --unmerged`, parses conflict markers, and returns line-numbered
regions with 3 lines of surrounding context. When a `git merge` or `git rebase` produces conflicts,
the plugin automatically appends a hint suggesting this tool.

**input.** No parameters required:

```json
{}
```

**output.** All conflict regions across all conflicted files, grouped by file:

```
9 files, 13 conflicts

── src/manager.ts [3 conflicts] ──

  15:   resolveInheritedPromptTools,
  16:   createInternalAgentTextPart,
  17: } from "../../shared"
  18: <<<<<<< HEAD
  19: import { normalizeAgentForPrompt } from "../../shared/agent-display-names"
  20: =======
  21: import { applySessionPromptParams } from "../../shared/session-prompt-params-helpers"
  22: >>>>>>> upstream/dev
  23: import { setSessionTools } from "../../shared/session-tools-store"
```

Use `edit` with the full conflict block (including markers) as `oldString` to resolve each conflict.

---

### grep *(experimental)*

**desc.** Trigram-indexed regex search that hoists opencode's built-in `grep`. The index is
built in a background thread at session start, persisted to disk for fast cold starts, and kept
fresh via file watcher. Falls back to direct ripgrep scanning for out-of-project paths or when
the index is not yet ready. Requires `experimental_search_index: true` in config.

**input.** JSON object with `pattern` required; `path`, `include`, `exclude`, `case_sensitive` optional:

```json
{ "pattern": "handleRequest", "include": "*.ts" }
```

**output.** Matches grouped by file, sorted by modification time (newest first), capped at 100:

```
src/server.ts
42: export async function handleRequest(req: Request) {
89:     return handleRequest(retryReq)

src/test/server.test.ts
15: import { handleRequest } from "../server"

Found 3 match(es) across 2 file(s). [index: ready]
```

Files with more than 5 matches show the first 5 and `... and N more matches`. Lines are truncated
at 200 characters.

---

### glob *(experimental)*

**desc.** Indexed file discovery that hoists opencode's built-in `glob`. Requires
`experimental_search_index: true`. Returns relative paths sorted by modification time, capped
at 100 files. Small result sets are listed flat; larger sets (>20 files) are grouped by directory.

**input.** JSON object with `pattern` required; optional `path` to scope to a subdirectory:

```json
{ "pattern": "**/*.test.ts" }
```

**output.** Flat list for small results, directory-grouped for larger ones:

```
3 files matching **/*.test.ts

src/server.test.ts
src/utils.test.ts
src/auth/login.test.ts
```

```
20 files matching **/*.test.ts

src/ (8 files)
  server.test.ts, utils.test.ts, config.test.ts, ...

src/auth/ (4 files)
  login.test.ts, session.test.ts, token.test.ts, permissions.test.ts

... and 8 more files in 3 directories
```

---

### aft_search *(experimental)*

**desc.** Semantic code search — find code by describing what it does in natural language.
Uses a local embedding model (all-MiniLM-L6-v2, ~22MB, downloaded on first use) with cosine
similarity ranking. No API keys needed. Requires `experimental_semantic_search: true` and
[ONNX Runtime](https://onnxruntime.ai/) installed (`brew install onnxruntime` on macOS).

**input.** JSON object with `query` required; optional `topK` (default 10):

```json
{ "query": "authentication middleware that validates JWT tokens" }
```

**output.** Ranked results with relevance scores and code snippets:

```
crates/aft/src/commands/configure.rs
  handle_configure (function, exported) 17:253 [0.42]
    pub fn handle_configure(req: &RawRequest, ctx: &AppContext) -> Response {
      let root = match req.params.get("project_root")...
      ...

packages/opencode-plugin/src/bridge.ts
  checkVersion (function) 150:175 [0.38]
    private async checkVersion(): Promise<void> {
      ...

Found 10 results [semantic index: ready]
```

The index is built in a background thread at session start, persisted to disk for fast cold
start, and uses cAST-style enrichment (file path + kind + name + signature + body snippet).
Without ONNX Runtime, all other AFT tools work normally — only `aft_search` is unavailable.

---

### aft_delete

**desc.** Delete a file with an in-memory backup that survives for the session and can be
restored via `aft_safety`. Only available in the `all` tool surface tier.

**input.** JSON object with `filePath`:

```json
{ "filePath": "src/deprecated/old-utils.ts" }
```

**output.** Confirmation with backup reference:

```json
{ "file": "src/deprecated/old-utils.ts", "deleted": true, "backup_id": "bk_1a2b3c" }
```

---

### aft_move

**desc.** Move or rename a file. Creates parent directories for the destination automatically,
falls back to copy+delete for cross-filesystem moves, and backs up the original before moving.
Only available in the `all` tool surface tier.

**input.** JSON object with `filePath` and `destination`:

```json
{ "filePath": "src/helpers.ts", "destination": "src/utils/helpers.ts" }
```

**output.** Confirmation with source, destination, and backup reference:

```json
{ "file": "src/helpers.ts", "destination": "src/utils/helpers.ts", "moved": true, "backup_id": "bk_4d5e6f" }
```

---

### aft_navigate

**desc.** Call graph and data-flow analysis across the workspace. Supports five modes: forward
call tree, reverse callers, execution trace-to, impact analysis, and data-flow tracing. Only
available in the `all` tool surface tier — in the `recommended` tier, use the CLI commands
`aft call_tree`, `aft callers`, `aft trace_to`, `aft impact`, and `aft trace_data` instead.

**input.** JSON object with `op`, `filePath`, `symbol`, and optional `depth` or `expression`:

```json
// Find everything that would break if processPayment changes
{
  "op": "impact",
  "filePath": "src/payments/processor.ts",
  "symbol": "processPayment",
  "depth": 3
}
```

**output.** Call graph tree or flat list of affected symbols depending on `op`:

```
impact: processPayment  (src/payments/processor.ts)
  chargeCard  src/payments/card.ts:22
    createInvoice  src/billing/invoice.ts:88
    sendReceipt  src/notifications/email.ts:14
  refundPayment  src/payments/refund.ts:45
```

Ops: `call_tree` (forward, default depth 5), `callers` (reverse, default depth 1),
`trace_to` (entry-point paths), `impact` (affected callers), `trace_data` (value flow, needs `expression`).

---

### aft_import

**desc.** Language-aware import management for TS, JS, TSX, Python, Rust, and Go. Supports
adding named imports with auto-grouping and deduplication, removing a single named import,
and re-sorting and deduplicating all imports by language convention.

**input.** JSON object with `op`, `filePath`, and operation-specific fields:

```json
// Add named imports with auto-grouping and deduplication
{
  "op": "add",
  "filePath": "src/api.ts",
  "module": "react",
  "names": ["useState", "useEffect"]
}

// Remove a single named import
{ "op": "remove", "filePath": "src/api.ts", "module": "react", "removeName": "useEffect" }

// Re-sort and deduplicate all imports by language convention
{ "op": "organize", "filePath": "src/api.ts" }
```

**output.** Confirmation with the resulting import line(s):

```
added to src/api.ts:
  import { useState, useEffect } from "react"
```

---

### aft_transform

**desc.** Scope-aware structural code transformations that handle indentation correctly. Supports
adding class/struct members, Rust derive macros (with deduplication), TS/JS try/catch wrapping,
Python decorators, and Go struct field tags. All ops support `dryRun` and `validate`. Only
available in the `all` tool surface tier.

**input.** JSON object with `op`, `filePath`, and op-specific fields:

```json
// Add a method to a TypeScript class
{
  "op": "add_member",
  "filePath": "src/user.ts",
  "container": "UserService",
  "code": "async deleteUser(id: string): Promise<void> {\n  await this.db.users.delete(id);\n}",
  "position": "last"
}
```

**output.** Confirmation or diff (with `dryRun: true`):

```
transformed src/user.ts: added member deleteUser to UserService
```

Ops: `add_member`, `add_derive`, `wrap_try_catch`, `add_decorator`, `add_struct_tags`.
Use `validate: "full"` to run the type checker after the transform.

---

### aft_refactor

Workspace-wide refactoring that updates imports and references across all files.

| Op | Description |
|----|-------------|
| `move` | Move a symbol to another file, updating all imports workspace-wide |
| `extract` | Extract a line range (1-based) into a new function (auto-detects parameters) |
| `inline` | Replace a call site (1-based `callSiteLine`) with the function's body |

```json
// Move a utility function to a shared module
{
  "op": "move",
  "filePath": "src/pages/home.ts",
  "symbol": "formatCurrency",
  "destination": "src/utils/format.ts"
}
```

`move` saves a checkpoint before mutating anything. Use `dryRun: true` to preview as a diff.

---

### aft_safety

Backup and recovery for risky edits.

| Op | Description |
|----|-------------|
| `undo` | Undo the last edit to a file |
| `history` | List all edit snapshots for a file |
| `checkpoint` | Save a named snapshot of tracked files |
| `restore` | Restore files to a named checkpoint |
| `list` | List all available checkpoints |

```json
// Checkpoint before a multi-file refactor
{ "op": "checkpoint", "name": "before-auth-refactor" }

// Restore if something goes wrong
{ "op": "restore", "name": "before-auth-refactor" }
```

> **Note:** Backups are held in-memory for the session lifetime (lost on restart). Per-file undo
> stack is capped at 20 entries — oldest snapshots are evicted when exceeded.

---

### aft dispatched_by

**desc.** Reverse lookup on dispatch edges. Given a handler function (a function passed as a
value somewhere in the codebase — Kafka consumers, asynq handlers, HTTP handlers, gRPC service
registrations), returns every call site that registered it, the dispatch key string if one is
present, and the fully-qualified name of the receiving call so the agent can distinguish
`asynq.HandleFunc` from `redis.Set` from `logger.With`. Fork-only; design:
[DESIGN-call-site-provenance.md](docs/DESIGN-call-site-provenance.md).

**input.** Shell form (CLI wrapper):

```bash
aft dispatched_by server/asynq_handler.go HandleMerchantSettlementTask
```

**output.** Plain-text summary followed by structured JSON when requested:

```
dispatched_by HandleMerchantSettlementTask (server/asynq_handler.go)  total=1
  - startAsyncQueueServer (server/asynq_server.go:69)
      key=merchant_settlement:merchant_id
      via (*github.com/hibiken/asynq.ServeMux).HandleFunc
```

Returns empty ("no dispatch registrations found") when the function isn't passed as a value
anywhere, or when the registration goes through a pattern the helper can't resolve (reflection,
runtime map lookup, closure with multiple in-project calls).

---

### aft dispatches

**desc.** Forward lookup by dispatch key. Given a string that appears as a dispatch-key argument
somewhere in the codebase (asynq task type, HTTP route pattern, Kafka topic constant — whatever
the library uses), returns the handler(s) registered under that key and the registrars. Use
`--prefix` to match all keys starting with a given prefix. Fork-only; design:
[DESIGN-call-site-provenance.md](docs/DESIGN-call-site-provenance.md).

**input.**

```bash
aft dispatches "merchant_settlement:merchant_id"
aft dispatches "merchant_settlement:" --prefix
```

**output.**

```
dispatches key=merchant_settlement:merchant_id  total=1
  - HandleMerchantSettlementTask (server/asynq_handler.go)
      registered by startAsyncQueueServer (server/asynq_server.go:69)
      via (*github.com/hibiken/asynq.ServeMux).HandleFunc
```

With `--prefix`, returns one block per matched key.

---

### aft implementations

**desc.** Which concrete types satisfy an interface. Works across any file boundary (same-package
and same-file pairs included — upstream filters these out, incorrectly assuming tree-sitter
resolves them; Go's implements-relation is structural and needs the type checker). Mock
directories (`**/mocks/**`) and mock-receiver types (`*Mock*`) are filtered by default;
`--include-mocks` shows them. Fork-only; design:
[DESIGN-interface-edges.md](docs/DESIGN-interface-edges.md).

**input.**

```bash
aft implementations store/settlement_store.go SettlementStorer
aft implementations store/settlement_store.go SettlementStorer --include-mocks
```

**output.**

```
implementations of SettlementStorer (store/settlement_store.go)  total=1
  *store.settlementStore  (store):
    - Create (store/settlement_store.go:125)
    - FindOrCreate (store/settlement_store.go:501)
    - ListByMerchantID (store/settlement_store.go:251)
    - BulkInsert (store/settlement_store.go:389)
    ... 39 more methods
```

With `--include-mocks`, mock-generated receiver types (e.g. `*store.mocks.SettlementStorer`)
appear alongside the real implementations.

---

### aft writers

**desc.** Who writes to a package-level variable (or const) across package boundaries.
Same-package writes are filtered by the helper (tree-sitter already sees them in a single
`aft zoom` / file read). Captures init-function writes too — SSA renders `var X = fn()` as a
write from the synthetic `init()`. Fork-only; design:
[DESIGN-variable-nodes.md](docs/DESIGN-variable-nodes.md).

**input.**

```bash
aft writers server/registry.go handlerRegistry
```

**output.**

```
writers handlerRegistry (server/registry.go)  total=2
  server/asynq_server.go (1):
    - startAsyncQueueServer:47
  server/asynq_server.go (1):
    - init:12
```

Returns `(no cross-package writers found)` when the variable is only written from within its
own package — that's the common case for well-encapsulated Go code.

---

### aft similar

**desc.** Semantically similar symbols. Computed from identifier tokens (camelCase/snake_case
split, Snowball-stemmed), weighted by project-wide TF-IDF, optionally expanded through a
project-local synonym dict (`.aft/synonyms.toml`), and combined with call-graph co-citation
(fraction of shared callees). No embedding model, no neural inference, no model download.
Explainable: `--explain` shows which tokens and shared callees drove each match's score.
Fork-only; design: [DESIGN-similarity.md](docs/DESIGN-similarity.md).

**input.**

```bash
aft similar merchant_settlement/service.go SettleMerchantSettlement --top=5
aft similar merchant_settlement/service.go SettleMerchantSettlement --dict --explain
```

**output.** Default (abbreviated):

```json
{
  "query": {"file": "merchant_settlement/service.go", "symbol": "SettleMerchantSettlement"},
  "matches": [
    {"file": "early_settlement/service.go",    "symbol": "processEarlySettlementV3", "score": 0.72},
    {"file": "merchant_settlement/service.go", "symbol": "OnHoldMerchantSettlement", "score": 0.68},
    {"file": "realtime_settlement/service.go", "symbol": "settleRealtime",           "score": 0.64}
  ]
}
```

With `--explain` each match gains a `breakdown` object listing the contributing stem tokens
with per-token TF-IDF product, the dict synonym expansions that fired (if `--dict`), and the
shared callees driving co-citation. Useful for debugging why two things ranked near each other
and for explaining rankings in docs.

Optional flags: `--top=N` (default 10), `--min-score=F` (default 0.15), `--dict` (load
`.aft/synonyms.toml` if present), `--explain` (verbose scoring breakdown per match).

---

## Configuration

AFT uses a two-level config system: user-level defaults plus project-level overrides.
Both files are JSONC (comments allowed).

**User config** — applies to all projects:
```
~/.config/opencode/aft.jsonc
```

**Project config** — overrides user config for a specific project:
```
.opencode/aft.jsonc
```

### Config Options

```jsonc
{
  // Replace opencode's built-in read/write/edit/apply_patch and
  // ast_grep_search/ast_grep_replace/lsp_diagnostics with AFT-enhanced versions.
  // Default: true. Set to false to use aft_ prefix on all tools instead.
  "hoist_builtin_tools": true,

  // Auto-format files after every edit. Default: true
  "format_on_edit": true,

  // Auto-validate after edits: "syntax" (tree-sitter, fast) or "full" (runs type checker)
  "validate_on_edit": "syntax",

  // Per-language formatter overrides (auto-detected from project config files if omitted)
  // Keys: "typescript", "python", "rust", "go"
  // Values: "biome" | "prettier" | "deno" | "ruff" | "black" | "rustfmt" | "goimports" | "gofmt" | "none"
  "formatter": {
    "typescript": "biome",
    "rust": "rustfmt"
  },

  // Per-language type checker overrides (auto-detected if omitted)
  // Keys: "typescript", "python", "rust", "go"
  // Values: "tsc" | "biome" | "pyright" | "ruff" | "cargo" | "go" | "staticcheck" | "none"
  "checker": {
    "typescript": "biome"
  },

  // Tool surface level: "minimal" | "recommended" (default) | "all"
  // minimal:     aft_outline, aft_zoom, aft_safety only (no hoisting)
  // recommended: minimal + hoisted tools + lsp_diagnostics + ast_grep + aft_import + aft_conflicts
  //              + grep/glob (when experimental_search_index is enabled)
  //              + aft_search (when experimental_semantic_search is enabled)
  // all:         recommended + aft_navigate, aft_delete, aft_move, aft_transform, aft_refactor
  "tool_surface": "recommended",

  // List of tool names to disable after surface filtering
  "disabled_tools": [],

  // --- Experimental ---

  // Enable trigram-indexed grep/glob that hoist opencode's built-ins.
  // Builds a background index on session start, persists to disk, updates via file watcher.
  // Falls back to direct scanning when the index isn't ready or for out-of-project paths.
  // Default: false
  "experimental_search_index": false,

  // Enable semantic code search (aft_search tool).
  // Requires ONNX Runtime installed (brew install onnxruntime on macOS).
  // Builds embeddings for all symbols using a local model (all-MiniLM-L6-v2, ~22MB).
  // The model is downloaded on first use. Index persists to disk for fast cold start.
  // Default: false
  "experimental_semantic_search": false,

  // Restrict all file operations to the project root directory.
  // Default: false (matches opencode's permission-based model — operations prompt via ctx.ask)
  "restrict_to_project_root": false
}
```

AFT auto-detects the formatter and checker from project config files (`biome.json` → biome,
`.prettierrc` → prettier, `Cargo.toml` → rustfmt, `pyproject.toml` → ruff/black, `go.mod` →
goimports). Local tool binaries (biome, prettier, tsc, pyright) are discovered in
`node_modules/.bin` before falling back to the system PATH. You only need per-language overrides
if auto-detection picks the wrong tool or you want to pin a specific formatter.

---

## Architecture

AFT is two components that talk over JSON-over-stdio:

```
OpenCode agent
     |
     | tool calls
     v
@cortexkit/aft-opencode (TypeScript plugin)
  - Hoists enhanced read/write/edit/apply_patch/ast_grep_*/lsp_diagnostics/grep/glob
  - Registers aft_outline/navigate/import/transform/refactor/safety/delete/move/search
  - Manages a BridgePool (one aft process per session)
  - Resolves the binary path (cache → npm → PATH → cargo → download)
     |
     | JSON-over-stdio (newline-delimited)
     v
aft binary (Rust)
  - tree-sitter parsing (14 language grammars)
  - Symbol resolution, call graph, diff generation
  - Format-on-edit (shells out to biome / rustfmt / etc.)
  - Backup/checkpoint management
  - Trigram search index (experimental: background thread, disk persistence, file watcher)
  - Semantic search with local embeddings (experimental: fastembed + all-MiniLM-L6-v2)
  - Persistent storage at ~/.local/share/opencode/storage/plugin/aft/
```

The binary speaks a simple request/response protocol: the plugin writes a JSON object to stdin,
the binary writes a JSON object to stdout. One process per session stays alive for the session
lifetime — warm parse trees, isolated undo history, no re-spawn overhead per call.

---

## Supported Languages

| Language | Outline | Edit | Imports | Refactor |
|----------|---------|------|---------|---------|
| TypeScript | ✓ | ✓ | ✓ | ✓ |
| JavaScript | ✓ | ✓ | ✓ | ✓ |
| TSX | ✓ | ✓ | ✓ | ✓ |
| Python | ✓ | ✓ | ✓ | ✓ |
| Rust | ✓ | ✓ | ✓ | partial |
| Go | ✓ | ✓ | ✓ | partial |
| C | ✓ | ✓ | — | — |
| C++ | ✓ | ✓ | — | — |
| C# | ✓ | ✓ | — | — |
| Zig | ✓ | ✓ | — | — |
| Bash | ✓ | ✓ | — | — |
| HTML | ✓ | ✓ | — | — |
| Markdown | ✓ | ✓ | — | — |

---

## Development

AFT is a monorepo: bun workspaces for TypeScript, cargo workspace for Rust.

**Requirements:** Bun ≥ 1.0, Rust stable toolchain (1.80+).

```sh
# Install JS dependencies
bun install

# Build the Rust binary
cargo build --release

# Build the TypeScript plugin
bun run build

# Run all tests
bun run test        # TypeScript tests
cargo test          # Rust tests

# Lint and format
bun run lint        # biome check
bun run lint:fix    # biome check --write
bun run format      # biome format + cargo fmt
```

**Project layout:**

```
opencode-aft/
├── crates/
│   └── aft/              # Rust binary (tree-sitter core)
│       └── src/
├── packages/
│   ├── opencode-plugin/  # TypeScript OpenCode plugin (@cortexkit/aft-opencode)
│   │   └── src/
│   │       ├── tools/    # One file per tool group
│   │       ├── config.ts # Config loading and schema
│   │       └── downloader.ts
│   └── npm/              # Platform-specific binary packages
└── scripts/
    └── version-sync.mjs  # Keeps npm and cargo versions in sync
```

---

## Roadmap

- Cursor support via hooks
- LSP integration for type-aware symbol resolution (partially implemented)
- Streaming responses for large call trees
- Watch mode for live outline updates

---

## Contributing

Bug reports and pull requests are welcome. For larger changes, open an issue first to discuss
the approach.

The binary protocol is documented in `crates/aft/src/main.rs`. Adding a new command means
implementing it in Rust and adding a corresponding tool definition (or extending an existing one)
in `packages/opencode-plugin/src/tools/`.

Run `bun run format` and `cargo fmt` before submitting. The CI will reject unformatted code.

---

## License

[MIT](LICENSE)
