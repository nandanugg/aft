# Language Helper Contract

AFT's tree-sitter layer handles syntax-level call-graph resolution for every supported language. Language helpers are optional companion binaries that use each language's real type-checker to resolve edges tree-sitter cannot see on its own.

The canonical implementation today is `go-helper/` (Go SSA + class-hierarchy analysis). A Python helper would use `mypy`/`pyright`, a TypeScript helper `tsc`, a Rust helper `rustc`'s analysis outputs, etc. They all emit the same JSON format, and Rust consumes them interchangeably.

## Filter-at-source rule

A helper must emit **only** edges the generic tree-sitter layer cannot resolve. Every edge that crosses the stdout pipe is data Rust has to load, parse, and keep in memory. Don't ask Rust to post-filter a larger dataset.

### What tree-sitter already resolves

- **Bare package-level calls** — `foo()` where `foo` is in the same file or the same directory (Go same-package, Python module-level, JS in-file).
- **Same-file methods** — `self.method()` / `s.method()` where the method is defined in the same file.
- **Same-directory-same-package methods** (Go only, via sibling scan).
- **Import-driven cross-file calls** — when the call expression matches a named or aliased import in the caller's file.

### What helpers should emit

- **Dynamic dispatch** — interface methods, virtual calls, trait objects. Tree-sitter cannot infer receiver types, so every concrete implementation of a dispatched method is valuable.
- **Cross-package concrete method calls** — `pkgA.service{}.Method()` called from pkgB. Tree-sitter doesn't follow the receiver's type across package boundaries.
- **Any language-specific resolution the helper uniquely knows** — e.g. Python descriptors, Ruby metaprogramming, decorators that swap the target.

### What helpers must drop

- Edges where both caller and callee are in the caller's own file.
- Edges where both caller and callee are same-package same-directory (unless the helper adds receiver-level disambiguation that the tree-sitter layer cannot).
- Edges where caller or callee lives outside the project root (stdlib, vendored deps).
- Edges originating from synthetic / compiler-generated functions.

## JSON schema (v1)

```json
{
  "version": 1,
  "root": "/abs/path/to/project",
  "edges": [
    {
      "caller": {
        "file": "relative/path.ext",
        "line": 42,
        "symbol": "enclosingFunctionName"
      },
      "callee": {
        "file": "relative/path.ext",
        "symbol": "targetSymbolName",
        "receiver": "*pkg.Type",
        "pkg": "pkg.path"
      },
      "kind": "interface"
    }
  ],
  "skipped": ["pkg.that.failed.to.load"]
}
```

### Field semantics

- `version` — bump for any breaking change in format or filter rules. Rust rejects unknown versions.
- `root` — absolute path the helper was invoked against. Rust uses this to validate cached outputs.
- `caller.file` / `callee.file` — relative to `root`. Rust canonicalizes both.
- `caller.symbol` — enclosing top-level function / method name. Closures collapse to their outer named function.
- `callee.symbol` — bare function / method name, no receiver.
- `callee.receiver` — receiver type as the language renders it (e.g. `"*example.com/pkg.T"`). Optional; present when the helper can determine it.
- `callee.pkg` — full package import path. Optional but strongly preferred.
- `kind` — one of `"interface" | "concrete" | "static"`. Helpers must not emit `"static"` (dropped by contract above). Rust treats unknown kinds conservatively (emit, don't filter).
- `skipped` — packages the helper couldn't load (build errors, missing deps). Rust falls back to tree-sitter for these.

## CLI interface

Helpers are invoked as:

```
<helper-binary> -root <absolute-project-root>
```

They must:
- Write the JSON document to **stdout**.
- Write diagnostics to **stderr** (Rust captures but doesn't parse).
- **Exit 0** on success, **non-zero** on failure. A non-zero exit tells Rust "no helper data; fall back to tree-sitter."
- Finish within the timeout Rust gives them (default 60s). Beyond that, Rust kills the process.
- **Not** block on stdout — stream or buffer, but don't deadlock. Rust drains stdout concurrently, but helpers should still be considerate for non-AFT callers.

## Adding a new language helper

1. Implement the helper in the target language using its best type-checking tooling.
2. Produce JSON matching the schema above.
3. Apply the filter rules: emit only what tree-sitter misses.
4. Install the binary somewhere on `PATH` as `aft-<lang>-helper` (e.g. `aft-python-helper`) or set `AFT_<LANG>_HELPER_PATH`.

The Rust side currently wires Go explicitly because it's the only helper. Adding a second helper will motivate extracting a generic discovery/invocation layer — at that point, `find_helper_binary` becomes a lookup keyed on `LangId`, and `configure`'s single Go thread becomes a set of parallel threads per detected language.
