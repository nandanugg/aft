# AFT - Agent File Tools

Tree-sitter powered code analysis for massive context savings (60-90% token reduction).

## Two Kinds of Questions, Two Kinds of Tools

Every code question is either **what exists** (structure) or **what runs** (behavior). AFT has separate tools for each — pick by the question, not by caution.

**"What exists here?"** → `aft outline`. Use when you need to know which files live in a directory, which symbols a file defines, what types are declared. Outline is fast and cheap; reach for it freely when surveying a codebase.

**"How does this work / flow / connect?"** → `aft trace_to`, `aft call_tree`, `aft callers`. These are the right tool for *every* behavior or flow question, not a last resort:
- `aft call_tree <file> <symbol>` — what this function calls (forward graph).
- `aft callers <file> <symbol>` — who calls this function (reverse graph).
- `aft trace_to <file> <symbol>` — how execution reaches this point (entry-point paths).
- `aft trace_data <file> <symbol> <expr>` — how a value flows through assignments and calls.
- `aft impact <file> <symbol>` — what breaks if this changes.
- `aft zoom <file> <symbol>` — read a specific function body with call-graph annotations.

**Default to trace tools for behavior questions.** "What's the normal flow for X?", "what handles Y?", "who sends this event?", "how does the happy path work?" — all of these are trace / call_tree / callers questions. Outline-diving through directories to piece together a flow is slower and less accurate than following the call graph directly from a known entry point (HTTP handler, Kafka consumer, CLI main, etc.).

**Grep is fine for "does this string appear?"** but reach for semantic tools when the answer requires understanding the behavior behind the name.

### Performance note

First `aft` call in a project can take 10-30 seconds on a large codebase (Go helper run + parsing hundreds of files). Progress lines go to stderr. Subsequent calls reuse a disk cache and are near-instant unless files changed. Cold-start slowness is not a hang — watch stderr for progress.

## AFT CLI Commands

Use `aft` commands via Bash for code navigation. These provide structured output optimized for LLM consumption.

### Semantic Commands

```bash
# Get structure without content (~10% of full read tokens)
aft outline <file|directory>

# Inspect symbol with call-graph annotations
aft zoom <file> <symbol>

# Forward call graph - what does this function call?
aft call_tree <file> <symbol>

# Reverse call graph - who calls this function?
aft callers <file> <symbol>

# Impact analysis - what breaks if this changes?
aft impact <file> <symbol>

# Control flow - how does execution reach this function?
aft trace_to <file> <symbol>

# Data flow - how does a value flow through assignments and across calls?
aft trace_data <file> <symbol> <expression> [depth]
```

## Tracing: control flow vs. data flow

Two different questions, two commands:
- **"How does execution reach this function?"** → `aft trace_to` (control flow).
  Example: `aft trace_to api/handler.go ChargePayment` — shows the call chain that lands on ChargePayment.
- **"Where did this value come from / where does it go next?"** → `aft trace_data` (data flow through assignments and parameter passing).
  Example: `aft trace_data api/handler.go ChargePayment merchantID` — traces how `merchantID` propagates within and across function boundaries.

For a bug like "this field got the wrong value," `trace_data` is usually the right starting point; for "why did this handler run," `trace_to` is.

### Patterns trace_data handles

`trace_data` follows values across these constructs — use it confidently on idiomatic code instead of manually reading every caller:

- **Direct args**: `f(x)` → hop into `f`'s matching parameter.
- **Reference args**: `f(&x)` → hop into `f`'s pointer parameter.
- **Field-access args**: `f(x.Field)` → approximate hop into `f`'s matching parameter (propagation continues).
- **Struct-literal wraps**: `w := Wrapper{Field: x}` → approximate assignment hop to `w`, then tracking continues on `w`.
- **Pointer-write intrinsics** (`json.Unmarshal`, `yaml.Unmarshal`, `xml.Unmarshal`, `toml.Unmarshal`, `proto.Unmarshal`, `bson.Unmarshal`, `msgpack.Unmarshal`): `json.Unmarshal(raw, &out)` binds `raw`'s flow into `out`, and further uses of `out` are tracked.
- **Method receivers**: `x.Method(...)` → hop into the receiver parameter name (Go `func (u *T) Method(...)`, Rust `&self`).
- **Destructuring assigns**: `a, b := f()` and `{a, b} = f()` → tracking splits onto the new bindings.

Hops marked `"approximate": true` are lossy (field access, struct wraps, writer intrinsics) — the flow exists but the exact subfield is not resolved.

### Basic Commands

```bash
aft read <file> [start_line] [limit]   # Read with line numbers
aft grep <pattern> [path]              # Trigram-indexed search
aft glob <pattern> [path]              # File pattern matching
```

## Decision Tree

```
Need to understand files?
    |
    +-- Don't know the file structure?
    |       -> aft outline <dir>
    |
    +-- Checking what files contain (docs, config, etc.)?
    |       -> aft outline <dir>, then selective reads
    |
    +-- Know the file, need specific symbol?
    |       -> aft zoom <file> <symbol>
    |
    +-- Need to understand what calls what?
    |       -> aft call_tree <file> <symbol>
    |
    +-- Need to find all usages?
    |       -> aft callers <file> <symbol>
    |
    +-- Planning a change?
    |       -> aft impact <file> <symbol>
    |
    +-- Debugging how execution reaches a point?
    |       -> aft trace_to <file> <symbol>
    |
    +-- Tracking where a value came from or where it flows?
            -> aft trace_data <file> <symbol> <expression>
```

## When to Use What

| Task | Command | Token Savings |
|------|---------|---------------|
| Understanding file structure | `aft outline` | ~90% vs full read |
| Checking what docs/configs contain | `aft outline` + selective read | ~80% vs read all |
| Finding function definition | `aft zoom file symbol` | Exact code only |
| Understanding dependencies | `aft call_tree` | Structured graph |
| Finding usage sites | `aft callers` | All call sites |
| Planning refactors | `aft impact` | Change propagation |
| Debugging control flow | `aft trace_to` | Execution paths |
| Debugging data flow | `aft trace_data` | Value propagation |

## Rules

Match the command to the question, not to caution.

**Structural / "what exists" questions** — "does X still exist?", "what files are in this dir?", "what symbols does this file declare?":
1. `aft outline` is the right tool. Fast and cheap.
2. For directory reads, always outline first to anchor; confirm which specific files are actually needed before expanding with zoom / selective reads.
3. When briefing a subagent to explore a repo, run `aft outline <path>` yourself first and include the output in the subagent prompt — subagents don't follow ordering guarantees, so leaving outline as a "step 1" instruction is unreliable.

**Behavioral / "what runs" questions** — "how does X work?", "what's the flow for Y?", "who calls Z?", "what happens if I change W?":
4. Reach for `aft trace_to` / `aft call_tree` / `aft callers` / `aft trace_data` / `aft impact` **first**, not as a last resort. These tools answer the question directly; outline-diving through directories to piece together a flow is slower and less accurate.
5. Start from a known entry point (HTTP handler, Kafka consumer, main, test entry) and follow the call graph out. Use `aft zoom` when you need to read the body of a specific function you've identified.
6. A zero result from `callers` or `trace_to` is itself information — but cross-check with `aft grep` on the symbol name if the result looks surprisingly sparse; some dispatch patterns (reflection, DI frameworks, callback registration) can't be resolved statically.

**`aft grep` is fine for "does this string appear?"** Reach for semantic tools when the answer requires understanding the behavior behind the name.

**Context protection still applies.** See the Context Protection section — don't read full files to piece together a flow; outline + trace + zoom gives the same answer for a fraction of the tokens.

## Context Protection

**Context is finite.** Even when a user explicitly requests "contents" or "read all files":

1. **Directory reads: outline first** - For directories with 5+ files, ALWAYS run `aft outline` and confirm which specific files are needed before reading
2. **All file types benefit** - AFT applies to markdown, config, docs, and data files — not just code. Documentation directories especially benefit from outline-first
3. **Batch limit** - Never read more than 3-5 files in a single action without confirming user intent. Context exhaustion breaks the conversation.
4. **User requests don't override physics** - "Read all files" is a request, not a command to fill context. Propose `aft outline` + selective reads instead.

## Supported Languages

TypeScript, JavaScript, Python, Rust, Go, C/C++, Java, Ruby, Markdown

## Go Interface Dispatch

For Go projects, AFT automatically runs `aft-go-helper` at configure time (if available) to resolve interface method calls and concrete receiver calls that tree-sitter alone cannot type-check. The helper uses the standard Go toolchain's SSA + class-hierarchy analysis.

**No action required**: if `go` is on PATH and the project has a `go.mod`, the helper runs in the background after `configure`. `callers`, `call_tree`, and related commands automatically use the results once they're ready.

**Requirements**: Go 1.22+ and `aft-go-helper` on PATH (or `$AFT_GO_HELPER_PATH`). The install script builds and symlinks it automatically. Falls back silently to tree-sitter if unavailable.

## Hook Integration

Grep and Glob tools are automatically routed through AFT via hooks for indexed performance.

**Reading files**: Use `aft read` via Bash for indexed reads with token savings.

**Warning**: When you need to Edit a file, use the native Read tool (not `aft read`) because Edit requires a prior Read tool call for validation.
