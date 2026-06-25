# shellcheck shell=bash
# Shown on each interactive shell in the setup sandbox. Sourced from .bashrc.
cat <<'BANNER'

  AFT: setup/doctor sandbox  (published @latest)
  ===============================================
  Project:  /test/project   (git repo, has src/index.ts)

  OpenCode setup:   aft setup --harness opencode
  Pi setup:         aft setup --harness pi
  Interactive:      aft setup            (harness picker)
  Doctor:           aft doctor --harness opencode
                    aft doctor --harness pi
  Auto-fix:         aft doctor --fix     (downloads the native binary)
  Non-interactive:  aft doctor --harness opencode --force

  Verify the CortexKit config location after setup (v0.40 consolidation):
    cat ~/.config/cortexkit/aft.jsonc            # user config (NOT ~/.config/opencode)
    cat /test/project/.cortexkit/aft.jsonc        # project config
    ls -la ~/.local/share/cortexkit/aft/          # data + indexes
    ls -la ~/.cache/aft/bin/                       # native binary (after doctor --fix)
    cat ~/.config/opencode/opencode.json          # opencode plugin registration
    cat ~/.pi/agent/settings.json                  # pi extension registration

  Confirm doctor reads the RIGHT config (the v0.40.2 fix):
    aft doctor --harness opencode | grep -i 'aft config'
    # must point at ~/.config/cortexkit/aft.jsonc, never "(not set)"

  Versions:  aft --version ; opencode --version ; pi --version

BANNER
