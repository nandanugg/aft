# Go adapter: profile (decision O)

The user called the old Go helper "notoriously cpu expensive" and did not say which mode. So this file
profiles the old helper first, finds what is avoidable, and then profiles the new adapter the same way.

## What was measured

- The mode measured is the adapter process itself: one full run on one repository. This is what
  `aft adapters build` starts in the background. A query never runs the adapter: it reads the stored
  overlay. So build cost and query cost are separate, and this file is only about build cost.
- Not measured here: the `aft adapters build` command end to end (R1's Rust code; no cargo ran in this
  task), and query timings (P2).
- Tools: `/usr/bin/time -l` (wall, user + sys CPU, max RSS), runtime/pprof with `-cpuprofile` and
  `-memprofile`, `go tool pprof -top` (CPU, `-cum`) and `-sample_index=alloc_space` / `inuse_space`.
- Repositories: clones at the Phase 1 commits of repository P and repository C (P, S and C are three real Go repositories; the consumer keeps them and their names privately). Go 1.27, offline
  Go environment, every run through `with_slot.sh`. The machine was shared with cargo builds of other
  tasks, so CPU numbers move by about 20 % from run to run (see the GOGC sweep below).
- Raw files: `$S/g1-old-helper/out/*` (old helper), `$S/g1-work/out/*` (new adapter).

## There are two "old" helpers

| | Fork main helper | Backup-branch helper |
|---|---|---|
| Source | `aft-fork-main/go-helper/main.go` (991 lines), the one the spike measured | `backup/main-before-origin-rebase-20260705` (2171 lines) |
| Does | sites (CHA over every function), dispatch edges | the same, plus call context, return paths, implements, writes |

Both were rebuilt with `--cpuprofile` / `--memprofile` added and nothing else changed.

## Old fork helper: results

| Variant | Repo | Wall | User + sys CPU | Max RSS |
|---|---|---|---|---|
| fork main, as built | repository P | 11.15 s | 23.0 s | 4.73 GB |
| fork main, as built | repository C | 9.45 s | 23.1 s | 4.69 GB |
| fork main + `-all -external` (spike copy) | repository P | 14.56 s | 25.3 s | 5.93 GB |
| backup-branch helper (all features) | repository P | **196 s** | **211 s** | 4.80 GB |
| backup-branch helper (all features) | repository C | **251 s** | **260 s** | 4.59 GB |

### What the old profile shows

**Fork main (repository P; repository C is the same shape).** Total 19.6 s of samples in 10.7 s.
- `runtime.madvise` 25.7 % flat, GC (`gcBgMarkWorker`) 23.8 % cum, `tryDeferToSpanScan` 12 %: the cost of a 4.7 GB heap.
- `callgraph.(*Graph).DeleteSyntheticNodes` 15.1 % cum (12.3 % on repository C).
- `main.relPath` 8.7 % cum: `filepath.Abs` and `Rel` for every edge (repository C 2.6 %).
- `ssa.(*Program).Build` 4.4 % (6 %), `go/types` checkFiles 5.9 % (5 %), `loader.parseFile` (read and parse) 5.9 % (6 %).
- `alloc_space` 4.98 GB in total: `go/types.recordTypeAndValue` 807 MB (16 %), SSA instruction and block
  emit 324 MB + 154 MB. `inuse_space`: recordTypeAndValue 403 MB (19 %), SSA blocks 105 MB.

**Fork main with `-all -external` (the fan-out).** `DeleteSyntheticNodes` 17.4 % cum (3.7 s); `emitEdge`
allocates 1.6 GB (`filepath.Rel` alone 753 MB). The spike counted the cause: calls through a function
value get every function of the same signature as a target. 451 sites gave 212,902 edges (83 % of all
edges) on repository P, and 1,104 sites gave 342,442 edges (88 %) on repository C.

**Backup-branch helper.** 84 to 87 % of the CPU (154 s of 183 s of samples on repository P, 198 s of
228 s on repository C) is `annotateCallContext` -> `findFunctionContainingCall`. For every edge it scans every
SSA function, and every instruction of the matching ones, calling `FileSet.Position` on each
(`token.searchInts`, `File.unpack`, `tree.locate` are the leaves). That is edges x all instructions. This
code, not the type checker, is the "notoriously cpu expensive" part.

### What is avoidable

1. The per-edge scan for call context (the backup helper, 84-87 %): replace by one walk of the syntax tree.
2. SSA and CHA over the function bodies of every dependency, and `DeleteSyntheticNodes` (15-17 %): the
   repository's call sites need only the declared callee, which the type checker gives.
3. The signature-match fan-out (83-88 % of edges in the `-external` runs): decision 6 removes it.
4. Type information for the bodies of dependencies (`recordTypeAndValue`, 16-19 % of memory) and the SSA
   of them: a root package only needs the declarations of what it imports.
5. Per-edge `filepath.Abs/Rel` (9 %): cache the root-relative path per file.

Budget set from this (decision W): CPU (user + sys) at most the fork main helper, 23 s, and RSS at most 6.0 GB.

## New adapter: design that follows from the profile

- No SSA, no call graph, no CHA. A site comes from `types.Info` (`Uses`, `Selections`) of the repository's
  own files. For an interface call the adapter looks in an index of the methods of the repository's own
  types (`impls_index.go`), memoised per (interface, method).
- `ParseFile` hook (`load.go`): files outside the root lose their function bodies before type-checking.
  Dependencies are still read from source (no export data: that needs a build cache that matches the
  toolchain, and x/tools v0.28.0 is older than Go 1.27).
- Context tags (`in_loop`, `in_error_branch`, `in_defer`, `in_goroutine`) and return paths come from one
  walk of each syntax tree.
- A position is the real file position; the root-relative path is cached per file name.
- No fan-out: a call through a function value has no site.

## New adapter: results

| Repo | Wall | User + sys CPU | Max RSS | Sites | Targets | Facts |
|---|---|---|---|---|---|---|
| repository P | 2.28 s | 6.4 s | 0.96 GB | 25,760 | 8,503 | 29,206 |
| repository C | 2.66 s | 6.3 s | 1.08 GB | 27,546 | 8,152 | 30,075 |
| repository S | 2.37 s | 7.0 s | 1.26 GB | 26,009 | 7,761 | 28,563 |

Against the fork main helper as built: wall 11.15 -> 2.28 s (repository P) and 9.45 -> 2.66 s
(repository C); CPU 23 -> 6.4 s and 23 -> 6.3 s; RSS 4.7 -> 1.0 GB. Against the backup-branch helper
(same features, plus facts): 196 s -> 2.3 s and 251 s -> 2.7 s. The last `accept_G1.sh` run of the
final code gave CPU 6.1 s, 6.8 s and 6.6 s (repository P, repository S, repository C), RSS
1.0, 1.2 and 1.0 GB, wall 3.3 to 4.4 s on a busy machine.

### Top functions (repository P, then repository C)

CPU, flat, total 4.04 s and 4.08 s of samples. The rest of the process CPU (about 2.3 s of 6.4 s) is the
`go list` child process and kernel time that pprof does not sample.

| repository P | flat | | repository C | flat |
|---|---|---|---|---|
| `runtime.madvise` | 22.3 % | | `runtime.madvise` | 34.8 % |
| `syscall.rawsyscalln` (file reads) | 13.4 % | | `syscall.rawsyscalln` | 12.0 % |
| `runtime.memclrNoHeapPointers` | 10.2 % | | `runtime.memclrNoHeapPointers` | 8.6 % |
| `runtime.tryDeferToSpanScan` (GC) | 10.2 % | | `runtime.tryDeferToSpanScan` | 5.2 % |
| `go/scanner.(*Scanner).next` | 4.0 % | | `runtime.scanObject` (GC) | 3.4 % |

Cumulative: `loader.parseFile` (reading and parsing every dependency file) 24 % and 18 %; `os.ReadFile`
13 % and 12 %; `go/types.(*Checker).Files` 19.6 % and 20 %; `runtime.gcBgMarkWorker` 23 % and 15 %. Every
`main.*` function of the adapter is below 12 % cumulative: `main.(*project).parseFile` (the hook, mostly
`go/parser`) 11 % and 6 %; `main.analyze` (all producers together) 3-4 %.

`alloc_space` (repository P) 1.75 GB in total, down from 4.98 GB: `recordTypeAndValue` 316 MB (18 %),
`parseIdent` 92 MB, `os.readFileContents` 90 MB. Repository C 1.7 GB: `recordTypeAndValue` 338 MB (20 %).
`inuse_space` (written when all records are in memory, with the loaded packages still alive): 620 MB on
repository P and 616 MB on repository C, of which `recordTypeAndValue` is 163 MB (26 %) and 183 MB (30 %),
then `go/parser` (parseOperand, parseIdent, scanString) and `go/types` (newVar, Scope.insert, recordUse).
The adapter's own records are only 17-19 MB of it. Peak RSS is 1.0 to 1.5 GB.

### The 20 % rule: functions with more than 20 % of CPU or memory

None of the adapter's own functions is above 20 %. These runtime and library entries are:

- `runtime.madvise` (22-35 % flat). The Go runtime on macOS faults in fresh heap pages. The adapter controls
  it only through how much it allocates, and that fell from 4.98 GB to 1.75 GB. The heap that is left is the
  type-checked syntax of the dependency declarations. A GOGC sweep on one machine (CPU user + sys, RSS):
  GOGC=100 5.9 s and 0.9 GB; 200 5.2 s and 1.1 GB; 400 4.8 s and 1.1 GB; 800 4.75 s and 1.4 GB. A later
  run of the final code with GOGC=300 gave 6.1 s on a busier machine, so the difference is inside the
  noise of a shared machine. No change made; the default stays.
- `syscall.rawsyscalln` / `os.ReadFile` (12-13 % flat; `os.ReadFile` 12-20 % cumulative, most of it the open call): reading the
  source of every dependency from the module cache. A source-based type-check needs it. The alternative
  is compiled export data. It needs a warm build cache, compiles every dependency on a cold cache, and its
  reader in x/tools v0.28.0 may not match the Go 1.27 format. Not done.
- `go/types.recordTypeAndValue` (18-22 % of allocation, 26-30 % of live heap at the end). `go/packages`
  records a full `types.Info` for each package it checks from source, dependencies included (declarations
  and package-level initialisers of dependencies, since their bodies are gone); the API has no switch for
  it. A loader of our own (std only, `go list` plus `go/types` with `IgnoreFuncBodies` and no Info for
  dependencies) would skip it and save about 350 MB of allocation and 160-180 MB of live heap. Not done:
  the run is 2 to 3 s and 1 GB, far inside the budget (23 s, 6 GB), and an own loader is code to maintain.
  It is the first thing to do if the budget ever tightens.

## Checks against the Phase 1 spike overlays

The spike overlays (kept in the consumer's private case pack) were compared with the new output:

- repository P: 25,908 spike sites against 25,760 new ones. All 282 spike sites that the new adapter
  does not emit are calls through a function value (written name differs from every target name:
  `returnFunc`, `rf`, `run` in `mocks/`): decision 6. 0 missing sites have a name equal to a target name.
- repository S: 457 missing, all of the same kind.
- At sites that both emit, the target sets are equal, except 1 site (repository P) and 3 sites
  (repository S). Two are new targets that the spike missed (a value-receiver method that implements
  the interface). Two are spike-only `Close` targets of a queue client type at calls of `resp.Body.Close()`:
  that type has `Read(ctx)`, so it does not implement `io.ReadCloser`; the spike target looks wrong.
- 78 new sites with targets on repository P are calls in package-level variable initialisers, which the
  old helper dropped (synthetic init functions).
