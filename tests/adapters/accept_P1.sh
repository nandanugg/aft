#!/usr/bin/env bash
# accept_P1.sh — portable install test: install.sh, the aft shim and uninstall.sh run from bash, zsh and fish.
# Runs on the host or in a container. It needs bash, git, go (to build the Go adapter), cmp and diff (diffutils),
# and the shells it tests; it needs no cargo and no python. Exit 0 only when no check failed (skipped shells are
# counted, not failed).
#
# Input:   AFT_RELEASE_DIR=<dir>   a release folder: aft-<platform> and checksums.sha256
#                                  (platform: darwin-arm64, darwin-x64, linux-x64, linux-arm64)
# Options: AFT_P1_CACHE=<dir>      AFT_INSTALL_CACHE for the installs: the first install downloads ONNX Runtime and the
#                                  model, the next installs read them from the cache (default: one cache in the work dir)
#          AFT_P1_GO_OFFLINE=1     build the Go adapter with the Go module cache only (no network)
#          TREE=<dir>              the source tree (default: two folders above this file)
#
# For each shell that exists (bash, zsh, fish; on a Mac also /bin/bash 3.2 as "bash32"), the shell itself runs:
#   install.sh --release-url file://<dir>   (semantic search installed, binary checked by sha256)
#   aft guide, aft outline, aft callers on a Go repository (an interface call: the reply must show adapter:go)
#   aft callers on a CommonJS repository (the reply must show adapter:javascript)
#   aft search (no "ONNX Runtime not found"), aft warmup --only semantic
#   uninstall.sh (the prefix is as before; HOME is unchanged)
# SHELL is set to that shell, so aft's login-shell PATH probe runs that shell too.
set -u
TREE=${TREE:-$(cd "$(dirname "$0")/../.." && pwd)}
export TREE
. "$(dirname "$0")/lib/common.sh"
SCRIPTS="$TREE/scripts/adapters"
FIX="$TREE/tests/adapters/portable/fixtures"

REL=${AFT_RELEASE_DIR:-}
[ -n "$REL" ] && [ -f "$REL/checksums.sha256" ] || { echo "accept_P1: AFT_RELEASE_DIR must hold checksums.sha256 and aft-<platform>" >&2; exit 2; }
REL=$(cd "$REL" && pwd)
case "$WORK" in *[!A-Za-z0-9._/+-]*) echo "accept_P1: the work dir path must be plain: $WORK" >&2; exit 2 ;; esac

# --- platform of this machine, and whether the release has its binary ---------------------------
case "$(uname -s)-$(uname -m)" in
	Darwin-arm64) PLATFORM=darwin-arm64 ;;
	Darwin-x86_64) PLATFORM=darwin-x64 ;;
	Linux-x86_64 | Linux-amd64) PLATFORM=linux-x64 ;;
	Linux-aarch64 | Linux-arm64) PLATFORM=linux-arm64 ;;
	*) PLATFORM=unknown ;;
esac
if [ -f "$REL/aft-$PLATFORM" ]; then pass "release has aft-$PLATFORM"; else fail "release $REL has no aft-$PLATFORM"; finish; fi
SEMANTIC_ARGS=""
[ "$PLATFORM" != darwin-x64 ] || SEMANTIC_ARGS="--without-semantic" # no ONNX Runtime library for an Intel Mac

# --- isolated HOME and XDG dirs; Go keeps the user's stores (the Go adapter build needs its modules) ----
isolate p1
export GOPATH="$REAL_GOPATH" GOMODCACHE="$REAL_GOMODCACHE" GOCACHE="$REAL_GOCACHE" GOENV="$REAL_GOENV"
export GOFLAGS=-mod=readonly
if [ "${AFT_P1_GO_OFFLINE:-}" = 1 ]; then go_offline_env; fi
CACHE=${AFT_P1_CACHE:-$WORK/install-cache}
mkdir -p "$CACHE"

for fixture in go-iface cjs-app; do
	scratch_repo "$FIX/$fixture" "$WORK/repo-$fixture"
done
GO_REPO="$WORK/repo-go-iface"
JS_REPO="$WORK/repo-cjs-app"

# --- the shells to test: "<label>:<shell program>" ----------------------------------------------------
# Each shell runs its commands as `<shell> -c "<simple commands>"`: the same text is valid in bash, zsh and fish.
SHELLS=""
for name in bash zsh fish; do
	if command -v "$name" > /dev/null 2>&1; then
		SHELLS="$SHELLS $name:$(command -v "$name")"
	else
		SKIPS=$((SKIPS + 1)); echo "SKIP shell $name: not installed"
	fi
done
# macOS ships bash 3.2 as /bin/bash. When it is not the bash on PATH, test it too: a folder with a `bash`
# link to it goes first in PATH, so every `#!/usr/bin/env bash` script runs with bash 3.2.
BASH32_BIN="$WORK/bash32-bin"
if [ -x /bin/bash ] && ! [ /bin/bash -ef "$(command -v bash)" ]; then
	mkdir -p "$BASH32_BIN" && ln -s /bin/bash "$BASH32_BIN/bash"
	SHELLS="$SHELLS bash32:/bin/bash"
fi

snap() { (cd "$1" && find . -print | LC_ALL=C sort); }
# Go's own telemetry counters live under the user config dir (macOS: inside HOME). install.sh does not write them.
snap_home() { snap "$1" | grep -v -E '^\./Library(/Application Support(/go(/telemetry(/.*)?)?)?)?$'; }

test_shell() { # test_shell <label> <shell program>
	local label=$1 shell_prog=$2 n="p1-$1"
	local prefix="$WORK/prefix-$label" out="$WORK/$n"
	local path_front=""
	[ "$label" != bash32 ] || path_front="$BASH32_BIN:"
	mkdir -p "$prefix" "$out"
	# sh_run <outfile> <command words...>: the shell under test runs one command with SHELL set to itself.
	sh_run() {
		local file=$1 words="" w; shift
		for w in "$@"; do words="$words '$w'"; done # single quotes mean the same in bash, zsh and fish
		"$shell_prog" -c "env SHELL=$shell_prog PATH=${path_front}$prefix/bin:$PATH AFT_INSTALL_CACHE=$CACHE$words" > "$file" 2>&1
	}
	# Without the prefix in PATH the install prints the PATH lines; with it, the shell finds `aft` by name.
	sh_install() { "$shell_prog" -c "env SHELL=$shell_prog PATH=${path_front}$PATH AFT_INSTALL_CACHE=$CACHE $SCRIPTS/install.sh --prefix $prefix --release-url file://$REL $SEMANTIC_ARGS" > "$out/install.out" 2>&1; }

	snap "$prefix" > "$out/prefix.before"; snap_home "$HOME" > "$out/home.before"
	if sh_install; then pass "[$label] install.sh from the shell (file:// release, $PLATFORM)"; else fail "[$label] install.sh: $(tail -n 3 "$out/install.out" | tr '\n' ' ')"; return; fi
	# The PATH note: the line of the current shell is marked; fish uses fish_add_path.
	grep -q 'fish_add_path' "$out/install.out" && grep -E "^ *${label%32}:.*<- your shell" "$out/install.out" > /dev/null \
		&& pass "[$label] install prints the PATH line for each shell and marks this one" || fail "[$label] PATH lines missing or not marked: $(tail -n 5 "$out/install.out" | tr '\n' ' ')"
	snap_home "$HOME" > "$out/home.after"
	cmp -s "$out/home.before" "$out/home.after" && pass "[$label] install wrote nothing in HOME" || fail "[$label] install changed HOME: $(diff "$out/home.before" "$out/home.after" | head -n 3 | tr '\n' ' ')"

	sh_run "$out/guide.out" aft guide
	local first; first=$(grep -m 1 -v '^[[:space:]]*$' "$TREE/docs/adapters/GUIDE.md")
	grep -qF "$first" "$out/guide.out" && pass "[$label] aft guide" || fail "[$label] aft guide: $(head -c 200 "$out/guide.out")"

	sh_run "$out/outline.out" aft outline "$GO_REPO/store/store.go"
	grep -q 'MemStore' "$out/outline.out" && pass "[$label] aft outline (Go)" || fail "[$label] aft outline: $(head -c 200 "$out/outline.out")"

	sh_run "$out/callers-go.out" aft callers "$GO_REPO/store/store.go" MemStore::Save
	grep -q 'CreateOrder.*via adapter:go' "$out/callers-go.out" && pass "[$label] aft callers (Go interface call: adapter:go edge)" \
		|| fail "[$label] callers Go: $(head -c 300 "$out/callers-go.out")"

	sh_run "$out/callers-js.out" aft callers "$JS_REPO/lib/price.js" applyTax
	grep -q 'checkout.*via adapter:javascript' "$out/callers-js.out" && pass "[$label] aft callers (CommonJS: adapter:javascript edge)" \
		|| fail "[$label] callers JS: $(head -c 300 "$out/callers-js.out")"

	if [ -z "$SEMANTIC_ARGS" ]; then
		sh_run "$out/search.out" aft search "save an order" "$GO_REPO"
		local rc=$?
		[ $rc = 0 ] && ! grep -q 'ONNX Runtime not found' "$out/search.out" && pass "[$label] aft search: ONNX Runtime found" || fail "[$label] aft search: $(head -c 300 "$out/search.out")"
		if sh_run "$out/warmup.out" aft warmup --root "$GO_REPO" --only semantic --timeout 120000; then pass "[$label] aft warmup --only semantic exits 0"; else fail "[$label] aft warmup: $(head -c 300 "$out/warmup.out")"; fi
	else
		SKIPS=$((SKIPS + 1)); echo "SKIP [$label] semantic checks: no ONNX Runtime library for $PLATFORM"
	fi

	"$shell_prog" -c "env SHELL=$shell_prog PATH=${path_front}$PATH $SCRIPTS/uninstall.sh --prefix $prefix" > "$out/uninstall.out" 2>&1
	snap "$prefix" > "$out/prefix.after"
	cmp -s "$out/prefix.before" "$out/prefix.after" && pass "[$label] uninstall left the prefix as before" || fail "[$label] uninstall: $(diff "$out/prefix.before" "$out/prefix.after" | head -n 4 | tr '\n' ' ')"
}

for entry in $SHELLS; do
	test_shell "${entry%%:*}" "${entry#*:}"
done
finish
