# AFT - Agent File Tools for Codex

Tree-sitter powered code navigation for lower-context, higher-signal exploration.

## Pick by question

Every code question is either **structure** or **behavior**. Pick the AFT command that answers that question directly.

**Structure / "what exists?"**
- Use `aft outline <file|dir>` to see files, symbols, types, headings, and shape without pulling full contents into context.

**Behavior / "what runs?"**
- Use `aft trace_to <file> <symbol>` for execution paths into a function.
- Use `aft call_tree <file> <symbol>` for the forward call graph.
- Use `aft callers <file> <symbol>` for reverse call sites.
- Use `aft impact <file> <symbol>` when planning changes or refactors.
- Use `aft trace_data <file> <symbol> <expr>` when the problem is about where a value came from or where it flows next.

**Targeted code body**
- Use `aft zoom <file> <symbol>` when you already know the symbol you want to inspect.

**Targeted file inspection**
- Use `aft read <file> [start] [limit]` for narrow file reads with line numbers.

`aft grep` is fine for "does this exact string appear?" questions. Reach for the semantic commands when the answer depends on behavior, not spelling.

## Commands

```bash
aft outline <file|directory>
aft zoom <file> <symbol>
aft call_tree <file> <symbol>
aft callers <file> <symbol>
aft impact <file> <symbol>
aft trace_to <file> <symbol>
aft trace_data <file> <symbol> <expression> [depth]
aft read <file> [start_line] [limit]
aft grep <pattern> [path]
aft glob <pattern> [path]
```

## Tracing: control flow vs. data flow

- Use `aft trace_to` for "how does execution reach this function?"
- Use `aft trace_data` for "where did this value come from?" or "where does this value go next?"

For "why did this handler run?", `trace_to` is usually right.
For "why is this field wrong?", `trace_data` is usually right.

## Working rules

**Structural questions**
1. Default to `aft outline` first.
2. When a directory has several files, outline first and narrow the scope before reading file contents.
3. When you need one function or type, `aft zoom` is usually better than reading the full file.

**Behavioral questions**
1. Default to `aft trace_to` / `aft call_tree` / `aft callers` / `aft impact` / `aft trace_data` before reconstructing the flow from raw file reads.
2. Start from a concrete entry point when possible: HTTP handler, CLI main, job runner, test, Kafka consumer.
3. A sparse or zero result is still information, but cross-check with `aft grep` if the dispatch looks dynamic.

## Context protection

Context is still finite.

- For directories or many files, use `aft outline` first.
- Do not read whole trees into context when the question is only about shape or entry points.
- If a user asks for "all files" or "the whole repo", outline the relevant directories first and expand selectively.
- Prefer `aft read` for targeted inspection when only a small slice of a file is needed.

## Performance note

The first `aft` call in a large repo can take 10-30 seconds while caches warm and the optional Go helper resolves interface-dispatch edges. That is expected. Later calls are much faster unless the project changed significantly.

## Codex note

Codex hooks currently inject guidance and Bash reminders; they do **not** transparently replace Codex's non-Bash file tools. When AFT is the better tool, call `aft ...` explicitly through shell.

## Go interface dispatch

If `go` is installed and the project has a `go.mod`, AFT can use `aft-go-helper` to resolve Go interface method calls more accurately. The install script builds it when possible and symlinks it if the target bin directory is writable.
