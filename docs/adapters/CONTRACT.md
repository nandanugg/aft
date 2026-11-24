# Language adapter contract, version 1

This document is for two readers: people who write an adapter for a language, and people who maintain
the adapter system in aft. The Rust form of the same rules is
`crates/aft/src/commands/data_callgraph/lang_adapters/contract.rs`. Change both together.

## 1. What an adapter is

aft builds a call graph with tree-sitter for every language. Tree-sitter sees syntax, not types. So it
cannot tell which method an interface call reaches, or which module a `require()` loads.

An adapter is a separate program for one language. It uses that language's own tools (for Go: the type
checker) to find what tree-sitter cannot. It prints records on stdout. aft stores the records per
repository, language and commit, and lays them over its own graph when a query runs.

- aft's own graph is always the base. An adapter only adds. It never removes an upstream edge.
- An adapter knows nothing about aft's internals. It gives positions in files, not aft symbol names.
- A query never waits for an adapter for long and never fails because of one. The reply says what the
  adapter added, or why it added nothing.

## 2. Registry

The registry is one JSON file. aft reads it only from:
1. `$AFT_ADAPTERS_REGISTRY` (an absolute path), when it is set; else
2. the install location: the file `adapters-registry.json` in the directory of the running aft
   executable, after symlinks are resolved (`install.sh` writes it there).

aft never reads a registry from a project root: a cloned repository must not be able to name a program
for aft to run. No registry file means no adapters; that is not an error.

An example with a Go and a JavaScript entry is in `docs/adapters/registry.example.json`.

```json
{ "adapters": [ { "contract": 1, "language": "go", "root_markers": ["go.mod"],
                  "command": ["/opt/aft/adapters/aft-go-adapter", "--root", "{root}"] } ] }
```

### 2.1 Required fields

| Field | Meaning |
|---|---|
| `contract` | The contract version the adapter speaks. This build reads `1`. The version is per entry, so adapters on different versions can live side by side. |
| `language` | aft's language label of the files the adapter covers: `go`, `javascript`, `typescript`, `python`, `rust`, … (upstream `lang_label`). |
| `root_markers` | File names. The adapter runs for a repository root that has one of them at its top (for Go: `go.mod`; for CommonJS: `package.json`). |
| `command` | The program and its arguments, as a list. `{root}` is the only placeholder. aft runs it directly, with no shell. |

### 2.2 Optional fields

| Field | Default | Meaning |
|---|---|---|
| `toolchain_id_command` | none | A command whose trimmed stdout names the toolchain, for example `["go", "env", "GOVERSION"]`. It is part of the overlay key, so a new toolchain gives a new run. aft runs it only when the program file changes (cached by path, size, time and inode). Limit 5 s. |
| `env` | `{}` | Variables added to the adapter's environment. |
| `timeout_s` | `120` | After this time aft stops the run (section 6). |
| `query_wait_s` | `20` | A query waits at most this long for a run that is going on. It stays below upstream's own 30 s wait. |
| `failure_cooldown_s` | `600` | After a failed or partial run, aft does not start the same key again before this time (section 6). |
| `hidden_tags` | `[]` | Target tags that are not shown in replies unless the user asks (`--include-tag`). Counts still use them. Example: `["mock"]`. |
| `facts` | `[]` | The fact kinds the adapter may print (section 2.3). |

An unknown field in an entry is an error. This catches spelling mistakes.

### 2.3 Fact declarations

```json
{ "kind": "registers",
  "description": "registration site -> handler; label = route, topic or job key",
  "hidden_tags": ["mock"],
  "aliases": [ {"name": "dispatches", "by": "label"},
               {"name": "dispatched_by", "by": "object"} ] }
```

- `kind`: the name used in `fact` records and in `aft facts <kind> …`.
- `description`: one line. `aft facts` and `aft guide` show it.
- `hidden_tags`: fact tags hidden unless asked.
- `aliases`: command names that are shortcuts for `aft facts <kind> …`. `by` sets how the alias looks up
  facts:
  - `label`: `aft <alias> <key> [--prefix]` matches the fact label.
  - `subject`, `object`, `either`: `aft <alias> <file> <symbol>` matches that end of the fact.
  - The old flag `--include-mocks` is `--include-tag mock`.
- `lookup` (optional): what `<file> <symbol>` matches when the parser finds the symbol. `declaration` (default):
  a fact end on the symbol's first line — the symbol itself (a function literal in its body is not it).
  `inside`: a fact end anywhere in the symbol's lines — for kinds about values inside it (Go `flows`).

### 2.4 Limits that are not registry fields

- At most **2 adapter runs at the same time** on the machine, for all repositories and languages
  together. One Go run uses 4.4-6.0 GB of memory. A run that waits for a slot does not block queries;
  they answer with status `building`.
- `git status` check per query: 1 s. Kill grace after SIGTERM: 5 s. Stderr kept on failure: 20 lines.

## 3. Which roots get adapter data

- Only a root that is inside a **git work tree with a HEAD commit**. The root may be a subdirectory of
  the work tree (for example a Go module below the repository top). No commit: aft never runs or serves
  an adapter for that root.
- A **clean tree** (the whole work tree, not only the root's directory:
  `git status --porcelain --untracked-files=no` is empty). If tracked files have local changes, aft does
  not serve adapter data. Upstream's own answers still work. The status says why.
- A root marker of the entry is present.
- The overlay **key** is (HEAD commit, the root's path inside the work tree, build id, toolchain id).
  The build id is the SHA-256 of the file
  `command[0]`. A new commit, a new adapter build or a new toolchain is a new key, so a new run.
- Known gap: a new untracked source file does not change the key. Its calls are missing from the
  overlay until the next commit.

## 4. Input

- `command` with `{root}` replaced by the absolute path of the repository root.
- Working directory: the root. Stdin: empty (`/dev/null`). Environment: aft's environment plus `env`.
- The adapter runs in its own process group. It may start child processes (Go starts `go list`).
- The adapter reads the tree. It must not write into it.
- The adapter must not use the network. For Go, the registry sets `GOPROXY=off`, `GONOPROXY=none`,
  `GOSUMDB=off`, `GOTOOLCHAIN=local`, `GOFLAGS=-mod=readonly`. `GOPROXY=off` alone is not enough:
  modules that match `GOPRIVATE` go straight to git even then.

## 5. Output

Stdout has one JSON object per line (UTF-8). Every line ends with a newline. The order is: one `header`
first, then `analysed`, `site`, `fact` and `unanalysed` records in any order, then one `end` record last.
Stderr is free text for people; aft keeps its last 20 lines when a run fails.

### 5.1 Positions

A position is `{"file", "line", "col"}`.
- `file`: relative to the root, with `/` separators, no `..`. Only a fact `object` may be an absolute
  path, when it is outside the root.
- `line`: 1-based. `col`: 1-based, counted in **bytes of UTF-8** (not characters, not UTF-16 units).

**Identity is the position.** aft maps positions to its own nodes and call sites. Names in records are
labels for checks and display only. An adapter never needs aft's naming rules (`Type::method`,
generics, closures).

- A **site position** must be inside the call and outside every call nested in it. For Go, use the
  call's open parenthesis `(`, also in `go` and `defer` statements. Do not use the start of the whole
  call expression: in `a.F().G()`, that position is inside `F()` too.
- A **target position** is the position of the callee's declared name.

### 5.2 Records

`header` (first, exactly once)
```json
{"type":"header","contract":1,"language":"go","tool":"aft-go-adapter","tool_version":"1.0.0"}
```
`contract` and `language` must equal the registry entry.

`analysed` (one or more; the file lists are united)
```json
{"type":"analysed","files":["orders/service.go","store/order_store.go"]}
```
Every file whose calls the adapter analysed and whose functions and methods it considered as call
targets. This is the adapter's **coverage** (section 7). For Go: all compiled non-test files of the
packages that type-checked.

`site` (one per analysed call)
```json
{"type":"site","file":"orders/service.go","line":3397,"col":42,"name":"CreateTx","tags":["in_error_branch"],
 "targets":[{"file":"store/order_store.go","line":344,"col":25,"name":"CreateTx","kind":"interface"},
            {"file":"store/mocks/OrderStorer.go","line":424,"col":25,"name":"CreateTx","kind":"interface","tags":["mock"]}],
 "external":[{"name":"Now","package":"time"}]}
```
- Meaning: "this call was analysed, and **every** target of it among the analysed files is in
  `targets`". So do not leave out a target to hide it (mocks): tag it and let `hidden_tags` hide it.
- `name`: the called name as written at the site (for `rf(x)` it is `rf`, whatever `rf` holds).
- `tags` (optional): facts about the call site, for example `in_loop`, `in_error_branch`, `in_defer`,
  `in_goroutine`.
- `targets`: may be empty, when every callee is outside the root. Each target has a position, a `name`
  (declared bare name), a `kind` (free label, shown as `adapter:go (interface)`), and optional `tags`.
  For a call through an interface, list the interface method itself too (the Go adapter uses kind
  `interface method`): then `callers` on the interface method finds the call.
- `external` (optional): labels of callees outside the root: `name`, `package`, optional
  `module` (`path@version`). Version 1 does not show them; aft only counts the call as analysed.
- A call that the adapter could not analyse has no record. aft then keeps upstream's answer for it.
- Do not emit "every function with this signature" for a call through a function value. That is not an
  answer. Leave the call out.

`fact` (zero or more)
```json
{"type":"fact","kind":"registers","subject":{"file":"server/routes.go","line":40,"col":9},"subject_name":"registerRoutes",
 "object":{"file":"handler/handler.go","line":2824,"col":24},"object_name":"CreateOrder",
 "label":"/orders.OrderService/CreateOrder","tags":[]}
```
- `kind` must be declared in the entry's `facts`.
- `subject` and `object` are positions. `subject_name` and `object_name` (optional) let users find
  fields and variables by name, since aft has no nodes for them. Names are labels: aft matches the last
  name segments (`CallbackMetadata` finds `OrderDetails.CallbackMetadata`), only in the file the user gives.
- A position may be an anonymous function, for example a job handler written as a function literal
  (the Go adapter: object at the `func` keyword, `object_name` `func literal`).
- `label` (optional): a key to search, such as a route, topic or job name, or a condition.
- `via` (optional, a string): how the fact came about, for example the registration API of a `registers`
  fact: `(*asynq.ServeMux).HandleFunc`, `kafka.InitConsumer`, `pb.RegisterOrderServiceServer`. The adapter
  chooses the text; `aft facts` shows it as `via=…`. It is a minor addition inside contract 1: a reader that
  does not know `via` ignores it (the rule on unknown fields at the end of this section).
- `tags` (optional): for example `mock`, `init`.
- Facts do not change the call graph. `aft facts` answers them (section 9).

`unanalysed` (zero or more)
```json
{"type":"unanalysed","files":["pkg/broken/a.go"],"reason":"package did not type-check: missing module x","unit":"example.com/pkg/broken"}
```
Files left out because of an error. Any such record makes the run **partial**.

`end` (last, exactly once)
```json
{"type":"end","sites":25841,"targets":44702,"facts":13506,"analysed_files":690,"unanalysed_files":1}
```
The counts must equal the records printed: `sites` = site records, `targets` = all targets of all sites,
`facts` = fact records, `analysed_files` = distinct analysed files, `unanalysed_files` = all files in
`unanalysed` records. The `end` record proves the output is whole.

Unknown fields in a known record are ignored, so a later minor version can add fields. An unknown record
`type` is an error.

## 6. Failure

aft writes the output to a temporary file and makes it visible only after all checks pass. A broken run
is never half-visible.

| What happens | Result |
|---|---|
| Exit code is not 0, or the adapter crashed | `failed`: exit code and the last 20 stderr lines |
| Exit code 0, but no `end` record | `failed`: "contract violation: no end record" |
| The last line has no newline (cut off) | `failed`: "contract violation at line N: the last line is incomplete" |
| A line is not JSON, or has an unknown `type` | `failed`: "contract violation at line N: …" |
| `header` has another `contract` or `language` | `failed` |
| A fact kind that the entry does not declare | `failed` |
| `end` counts do not match the records | `failed` |
| A position is not root-relative, or not 1-based | `failed` |
| The run takes longer than `timeout_s` | aft sends SIGTERM to the whole process group, waits 5 s, then SIGKILL; `failed`: "timeout after N s". No child process stays alive. |
| `unanalysed` records | `partial`: the edges are served, the status names the unanalysed packages or modules |

After a `failed` **or** `partial` run, aft keeps a record for the key (reason, time, stderr tail) and does
not start the same key again before `failure_cooldown_s`. A partial run gets the retry because the key
does not change when the missing module appears later. `aft adapters build --root <repo> --force` runs
it at once. A new commit is a new key, so it runs at once too.

## 7. Coverage rule (when aft may say "complete")

Upstream marks a reply `complete:false` when some call sites with the same name could not be resolved.
With an adapter, aft may lower that count, but only when all of these hold:

1. The status is `fresh` or `partial`.
2. **Every** upstream node with that name, in that language, is in an analysed file. (A test file is not
   analysed by the Go adapter, so a `_test.go` stub with the same name keeps the count.)
3. Upstream's count is below its cap of 1,000 (at the cap it is only a lower bound).

The count for the whole language is never lowered. In all other cases aft passes upstream's count
through, and `complete` stays false.

## 8. Status in every reply

Every graph reply (`callers`, `call_tree`, `impact`, `trace_to`, `trace_to_symbol`, `trace_data`) lists
the status of each language that has a registry entry for the root:

| Status | Meaning | Adapter edges |
|---|---|---|
| `fresh` | The overlay for the current key is served. | yes |
| `partial` | Served; some files were not analysed (named). Retried after the cooldown. | yes |
| `building` | A run is going on or waits for a slot; the query waited and answers without it. | no |
| `stale` | The stored overlay is for another commit; a new run starts (or waits for its cooldown). | no |
| `failed` | The last run for this key failed (reason, time, retry time). | no |
| `skipped` | No adapter data for this root: no commit, local changes, root marker absent, tree check timed out, … | no |

When a language with an entry is not `fresh` or `partial`, the reply has `complete:false` and an
`adapter_reason` line, also for the commands that have no `complete` flag upstream.

Adapter-added targets are marked `adapter:<language> (<kind>)`. Targets with a hidden tag are left out of
the display, with a count of what was hidden.

## 9. Facts query

- `aft facts` lists the declared kinds with their descriptions and aliases.
- `aft facts <kind> <file> <symbol>` matches facts whose subject or object is in that symbol (by
  position), or whose `subject_name`/`object_name` is that symbol.
- `aft facts <kind> --label <key> [--prefix]` matches the label.
- `--depth N` follows object → subject chains of one kind (for multi-hop data flow).
- `--include-tag <tag>` shows facts and targets with a hidden tag.
- Each alias is `aft facts <kind>` with its fixed `by` mode (section 2.3).

## 10. How to extend

| Level | What changes | Who changes it |
|---|---|---|
| 1. No system change | A new language, a new adapter version, more sites, new tags, new labels, better detection | The adapter author, plus a registry entry |
| 2. Contract step | A new record type or a new meaning for the core graph (for example a new edge kind the graph must follow) | A new `contract` number and one mapping in `lang_adapters` |
| 3. New fact kind + alias | A new question to answer | A `facts` declaration (and alias) in the registry entry; the adapter prints the facts; no Rust change |

## 11. Known limits (version 1)

- `zoom` shows same-file calls from the syntax tree only; use `callers` and `call_tree`.
- `inspect` dead-code does not see adapter edges: a method called only through an interface can look dead.
- `trace_data` keeps upstream's resolver; for Go data flow, use the `flows` facts.
- Calls into dependencies are counted (they no longer make a reply incomplete), not shown.
