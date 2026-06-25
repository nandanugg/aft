# Docker E2E tests

Integration tests that prove AFT actually loads and works end-to-end inside a clean Linux environment with a real OpenCode binary plus a mock LLM (aimock), and a separate **interactive sandbox** for manually exercising the published setup/doctor wizards after a release.

## Layout

```
tests/docker/
├── Dockerfile.linux-x64          # Scripted E2E: OpenCode + locally-packed AFT plugin/bridge + aimock
├── Dockerfile.build-linux        # Builds the linux-x64 aft binary fixture
├── Dockerfile.setup-sandbox      # Interactive setup/doctor sandbox (published @latest)
├── test-e2e.sh                   # The scripted multi-turn session assertions
├── run-linux-test.sh             # Local runner for the scripted E2E
├── setup-sandbox.sh              # Build + run the interactive sandbox
├── setup-sandbox-banner.sh       # Shell banner listing the manual test steps
└── fixtures/                     # aimock fixtures + packed binary/plugin
```

## Scripted E2E (`Dockerfile.linux-x64` + `test-e2e.sh`)

Builds the local binary + packs the local plugin/bridge, then runs a realistic multi-turn OpenCode session against aimock, exercising the tool surface (outline, read, grep, glob, search, edit, safety), the trigram + semantic indexes, and several ONNX Runtime failure scenarios. This is the layer CI runs as the Linux Docker E2E gate.

```bash
tests/docker/run-linux-test.sh
```

It tests the **locally-built** artifacts (what you're about to ship), not the published ones.

## Interactive setup sandbox (`Dockerfile.setup-sandbox`)

A clean, throwaway machine for **manually** exercising the **published** `setup` and `doctor` wizards interactively (via a PTY) after a release. Unlike the scripted E2E (which copies the locally-built artifacts and runs a non-interactive smoke), this image installs the **real published** `@cortexkit/aft@latest` from npm, with OpenCode and Pi present, then drops you into a shell so you can drive the wizard yourself and inspect where config and state land.

```bash
# build @latest (fresh npm fetch) and drop into an interactive shell
tests/docker/setup-sandbox.sh

# pin a specific version, or just (re)build the image
tests/docker/setup-sandbox.sh 0.40.2
tests/docker/setup-sandbox.sh --build-only
```

Rebuild after each release to pick up the newest published version (the build forces a fresh `@latest` fetch via a cache-bust arg). Use it to confirm the wizard writes to — and `doctor` reads from — the CortexKit config location on a fresh machine (the v0.40 consolidation):

- user config → `~/.config/cortexkit/aft.jsonc`
- project config → `<project>/.cortexkit/aft.jsonc`
- data + indexes → `~/.local/share/cortexkit/aft/`
- native binary → `~/.cache/aft/bin/v<version>/aft` (after `aft doctor --fix`)

The banner printed on each shell lists the exact commands and the paths to verify, including the `doctor` config-read check that guards against the legacy-path regression.

Requires Docker with `linux/amd64` support; on Apple Silicon the runner sets `--platform linux/amd64` automatically (emulated).
