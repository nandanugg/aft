# Decisions Register

<!-- Append-only. Never edit or remove existing rows.
     To reverse a decision, add a new row that supersedes it.
     Read this file at the start of any planning or research phase. -->

| # | When | Scope | Decision | Choice | Rationale | Revisable? |
|---|------|-------|----------|--------|-----------|------------|
| D001 | M001 | arch | Binary process model | Persistent process (spawned once, stays alive, JSON over stdin/stdout) | Avoids re-parsing tree-sitter grammars per command, enables in-memory caching of parse trees, call graphs, checkpoints, and edit history | No |
| D002 | M001 | arch | LSP integration architecture | Plugin mediates LSP — binary never connects to language servers directly | Keeps binary's external surface clean (JSON in, JSON out). Plugin enriches commands with LSP data via optional `lsp_hints` fields. | No |
| D003 | M001 | arch | LSP readiness | Provider interface from M001 onward — tree-sitter default, LSP-ready | Command JSON schema has optional `lsp_hints` fields. Architecture supports LSP upgrade path without refactoring. | No |
| D004 | M001 | convention | Language priority | Web-first: TS/JS/TSX → Python → Rust → Go | Most AI agents work in TS/JS. TS/JS/TSX share ~80% of tree-sitter query patterns. Ship highest-value languages first. | Yes — if usage data shows different priority |
| D005 | M001 | arch | Safety system scope | Both per-file undo AND workspace checkpoints from M001 | Per-file undo is the immediate safety net. Workspace checkpoints are "save game" for risky multi-file changes. Both needed for agent confidence. | No |
| D006 | M001 | arch | Binary distribution timing | Local-first — build distribution pipeline as late M001 slice after tools are proven | No point building a distribution pipeline for a binary that doesn't do anything useful yet. | No |
| D007 | M001 | arch | Validation scope | Tree-sitter syntax (default, ~1ms) + opt-in full type-checker invocation (synchronous, 1-10s) | Default is fast. Full mode is synchronous because it's opt-in — agent explicitly asked for it, occasional wait is acceptable. | Yes — if full validation proves too slow in practice |
| D008 | M003 | arch | Call graph construction strategy | Lazy/incremental with file watcher for invalidation | Eager full scan too slow for large codebases. Lazy gives fast first results. File watcher keeps graph current. Worktree-aware scoping. | No |
| D009 | M001 | convention | JSON protocol format | Newline-delimited JSON (one JSON object per line) | Simple, well-understood protocol. Each request/response is self-contained. Easy to parse and debug. | No |
| D010 | M001/S01 | arch | Request parsing strategy | Two-stage: deserialize JSON envelope (id + command + flattened params), then dispatch on command string | Separates transport concerns (JSON parsing, ID tracking, error envelope) from command logic. Each command handler receives pre-validated params map. Malformed JSON is caught at stage 1 without touching command logic. | No |
| D011 | M001/S01 | convention | Integration test architecture | Persistent AftProcess struct with BufReader held over child stdout lifetime | Per-call BufReader silently loses buffered data during sequential reads. AftProcess pattern is mandatory for any test sending multiple commands to the binary. | No |
