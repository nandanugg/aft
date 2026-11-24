# tests/adapters — acceptance for the language-adapter system (fork-only)

- `accept_<task>.sh`: one script per task (R1, R2, W1, G1, G2, K1, P1). Each exits 0 only when every
  acceptance check of that task holds; its header fixes the interfaces the task provides.
  Environment: see `lib/common.sh` (TREE, CARGO_TARGET_DIR, AFT_ACCEPT_WORK, AFT_ACCEPT_HEAVY, PYTHON; the private inputs STOCK_AFT, ACCEPT_CLONES, CASE_REPOS_ROOT and AFT_ACCEPT_CASES have no default; without AFT_ACCEPT_CASES, the checks on real repositories print SKIP and are counted as skipped).
- `accept_P1.sh`: the portable install test. Needs `AFT_RELEASE_DIR` (a release folder), no cargo, no python.
  It installs from bash, zsh and fish (and `/bin/bash` 3.2 on a Mac), runs aft on the Go and CommonJS projects in
  `portable/fixtures/`, and uninstalls. Summary line: `N passed, F failed, K skipped`.
- `portable/`: `run_matrix.sh` runs accept_P1 in containers (`AFT_CONTAINER_ENGINE`, default `docker`; Ubuntu,
  Debian, Arch; arm64 and x86_64), `container_setup.sh` is the script inside each container (packages, Go), and
  `fixtures/` holds the two small projects. Run each image on a host of its own CPU type. An x86_64 image on an
  arm64 host runs under CPU emulation, and there `aft warmup` (ONNX Runtime) can stop with a segmentation fault;
  the same images pass on x86_64 hardware.
- `lib/`: `common.sh` (shared checks, isolation), `contract_check.py` (contract v1 validator without
  Rust), `oneshot.py` (NDJSON driver), `reply_tools.py`, `facts_compare.py`, `build_fixture_overlay.py`.
- `fake/`: fake adapters for binding and failure tests (`selftest.sh` checks them; `make_registry.sh`
  writes a test registry).
- `fixtures/`: `tiny-mixed` (Go + JS root) and `go-facts/` (fork testdata + expected facts, decision L). The
  real-repository fixtures (overlays, JavaScript known answers) are in the consumer's private case pack.

CI: `.github/workflows/adapters.yml` runs these scripts on the supported platforms only (darwin-arm64,
linux-x64, linux-arm64), without the private case pack, so the real-repository checks print SKIP there.

The contract itself is `docs/adapters/CONTRACT.md`. The install guide is `docs/adapters/INSTALL.md`.
