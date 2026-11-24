# aft: how to use it

aft reads code and answers questions about structure and calls. Use it before you open files by hand.

## How to call it

Run `aft <command> …`. Paths are relative to the current directory.

    aft outline <repo>/orders/service.go
    aft callers <repo>/orders/service.go CreateOrder
    aft call_tree <repo>/orders/service.go CreateOrder

- `aft` finds the root from the path you give: the nearest directory above it with a language marker
  (`go.mod`, `package.json`, `Cargo.toml`, `pyproject.toml`, or a root marker of a registered adapter) or
  `.git`. A Go module in a subdirectory of a repository is its own root. A folder that only holds repositories is not
  a root: pass a path inside one repository.
- Use a bare name (`CreateOrder`). If the name is also an interface method in the same
  file, `aft` picks the real method and prints one line: `note: <name> → <chosen> (interface member … skipped)`.
  If more than one candidate is left, it lists them. Then repeat the call with a qualified name
  (`service::CreateOrder`).
- Output is short text. Add `--json` only when a script reads it.
- Exit codes: 0 ok, 1 the command failed, 2 wrong use, 3 the repository is not searchable (below).

## Commands

| Question | Command |
|---|---|
| What exists in this file or folder? | `aft outline <file\|dir>` |
| Show one symbol | `aft zoom <file> <symbol>` |
| Read lines | `aft read <file> [start_line] [limit]` |
| Does this text appear? | `aft grep <pattern> [path]`, `aft glob <pattern> [path]` |
| Who calls this? | `aft callers <file> <symbol>` |
| What does this call? | `aft call_tree <file> <symbol>` (depth 2; add `--depth N` for more) |
| What breaks if I change this? | `aft impact <file> <symbol>` |
| How does execution reach this? | `aft trace_to <file> <symbol>` |
| Does A reach B? | `aft trace_to_symbol <file> <symbol> <toSymbol> [toFile]` |
| Where does a value go? | `aft trace_data <file> <symbol> <expression> [depth]` |
| Where is code about X? | `aft search "<words>" <repo>` (upstream semantic search) |
| What code is like this symbol? | `aft similar <file> <symbol>` (upstream semantic search on its name) |
| Facts a language adapter found | `aft facts …` or an alias (below) |
| Adapter state of a repository | `aft adapters status --root <repo>` |

Ask "what exists" with `outline`. Ask "how does it run" with `callers`, `call_tree`, `trace_to` and
`impact`. Use `grep` only to check that a string appears.

`callers`, `call_tree`, `impact`, `trace_to` and `trace_to_symbol` take `--depth N` and `--include-tests`.
Callers in test files are hidden by default; the reply says how many. In a repository that is all tests
(for example a test-automation project), always add `--include-tests`. A call through a Go
interface lists every implementation, so a deep `call_tree` gets large: start at the default and go deeper
only on the branch you need (call `call_tree` again on that child). `callers` on an interface method lists
the calls made through the interface.

## Which repositories are searchable

A repository is searchable only when it is a git work tree with at least one commit (a HEAD commit).
Otherwise `aft` stops with exit 3 and says so.

If tracked files have local changes (a dirty tree), upstream answers still work, but the adapter data is
not served. The status line says why.

## Adapter status and `complete:false`

`aft` always builds its own call graph from syntax. For some languages an adapter (a program that
type-checks the code) adds call edges that syntax cannot see, for example a call through a Go interface.
An adapter only adds. It never removes an edge.

Every graph reply names the status of each language adapter for that repository:

| Status | Meaning |
|---|---|
| `fresh` | The adapter data for this commit is used. |
| `partial` | Used, but some files or modules were not analysed. They are named. |
| `building` | A run is going on. This answer has no adapter edges. Ask again later. |
| `stale` | The stored data is for another commit. A new run starts. |
| `failed` | The last run failed. The reason and the retry time are shown. |
| `skipped` | No adapter data for this repository. The reason is shown (no commit, local changes, …). |

`complete:false` means the answer may miss callers. Two causes:
1. Some call sites with that name could not be resolved, and no adapter data covers them.
2. A language adapter is not `fresh` or `partial`.

Read the `adapter_reason` line to see which. When the status is `building` or `stale`, ask again after a
short time. To run the adapter now: `aft adapters build --root <repo>`.

## Facts and aliases

A fact is one relation an adapter found, for example "type T implements interface I". Each fact has a
kind, a subject, an object and sometimes a label.

- `aft facts` lists the kinds that are registered.
- `aft facts <kind> --label <key> [--prefix]` finds facts by label (for example a route or a topic).
- `aft facts <kind> <file> <symbol> [--by subject|object|either]` finds facts about one symbol.
- A `registers` fact also shows `via=<API>`: the registration call that was made, for example
  `via=(*asynq.ServeMux).HandleFunc` or `via=pb.RegisterOrderServiceServer` (`--json`: field `via`).
- `--depth N` follows a chain of facts of one kind. `--include-tag <tag>` shows facts that are hidden by
  default (for example `mock`, or `init` for writes).

An alias is a short name for one such question, so you do not need to remember the kind. The list below
this guide shows every alias and how to call it. An alias takes a key (`<key> [--prefix]`) or a file and
a symbol (`<file> <symbol>`), as shown there. A key has no file, so when the current
directory is not inside the repository, add `--root <repo>` (for example
`aft dispatches /proto.OrderService/CreateOrder --root <repo>`).

## Known limits

- `zoom` shows calls inside the same file only, from syntax. Use `callers` and `call_tree` for calls
  across files.
- Dead-code inspection does not see adapter edges. A method that is called only through an interface can
  look unused.
- `trace_data` uses the syntax resolver and follows variables. For a field path (`req.Amount`) with no hops,
  `aft cli` asks again with the root variable (`req`) and prints a `note:` line; the field is followed
  approximately, as part of its variable. For Go data flow across functions and interfaces use the `flows`
  facts.
- Calls into dependencies (module cache, standard library) are counted, not shown.
- `search` and `similar` rank by meaning only when upstream's embedding model is installed; otherwise they
  rank by words. `similar` is a search on the symbol's name.
- While upstream builds its semantic index, `search` and `similar` wait (progress lines on stderr, up to
  `AFT_CLI_SEMANTIC_WAIT_S`, default 300 s) and then print the lexical fallback with a `note:` line.
  The index grows only while `aft` runs. When a reply ends with `semantic: N file(s) not yet embedded`, the
  ranking is partly by words: run `aft warmup --root <abs root> --only semantic` once per repository
  (about 1.5 min and 3 GB of memory for 1000 files).
- Go adapter: a call through a method value (`f := o.M; f()`) stays unresolved. Files with cgo are not
  analysed. Only files under a `mocks/` directory are tagged `mock`; a hand-written fake elsewhere is shown.
  An interface call is skipped when a generic type in the root declares all of the interface's methods.
- JavaScript adapter (CommonJS): `this.method()`, factory exports (`module.exports = makeX()`) and
  conditional exports are not resolved; for these, the reply is upstream's own answer.
