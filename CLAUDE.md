# AFT - Agent File Tools

Tree-sitter powered code analysis for massive context savings (60-90% token reduction).

## Start With Outline, Escalate From There

**Outline is the default entry point.** Before reading full files, run `aft outline` to get structure — ~10% the tokens of a full read. This applies to code, markdown, config, and docs.

**Escalate to semantic commands only when the task needs them:**
- `aft zoom <file> <symbol>` — when you need to read a specific function body.
- `aft call_tree` / `aft callers` — when you need cross-file call relationships (grep can't infer these).
- `aft impact` — before a refactor, to see what breaks.
- `aft trace_to` — when debugging how execution reaches a point.
- `aft trace_data` — when tracking where a value came from or where it flows next.

**Don't use semantic commands reflexively.** For verification tasks — "does this symbol still exist?", "is this doc accurate?" — outline alone is usually enough. Reaching for zoom/call_tree on every task inflates work without improving answers.

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

Match the command to the task type. Outline is universal; the semantic graph tools (zoom/call_tree/callers/impact/trace_to) pay off for *comprehension* tasks, not for *verification* tasks.

**Verification tasks** — "does X still exist?", "is this doc still accurate?", "what files are in this dir?":
1. **ALWAYS start with outline** - `aft outline` to confirm structure and anchor symbols.
2. **Outline is usually enough.** Don't reach for zoom/call_tree/callers unless you need to see actual behavior, not just presence.
3. **ALWAYS outline before delegating** - When briefing a subagent to explore a repo or directory, run `aft outline <path>` yourself first and include the output in the subagent prompt. Never leave outline as a mid-step instruction — subagents don't follow ordering guarantees.

**Comprehension tasks** — "how does this flow work?", "what breaks if I change X?", "where is this called?":
4. **Use zoom** to read a specific function body without reading the whole file.
5. **Use call_tree / callers** to map cross-file relationships that grep cannot see.
6. **Use impact before a refactor** to understand blast radius before editing.

**When grep is fine.** `aft grep` for a bare identifier is correct when you just need to know "does this string appear, and where." Reach for semantic commands when you need to understand *behavior* behind the name, not every time a name shows up.

**Context protection still applies.** See the Context Protection section — even when a task is verification-only, don't read full files; outline first and selectively read what you need.

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
