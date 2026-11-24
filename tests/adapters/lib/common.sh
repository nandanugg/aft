# Shared by tests/adapters/accept_*.sh (sourced). Bash + POSIX tools + git; python3 only for JSON.
# Every acceptance script: runs all its checks, prints one line per check (PASS/FAIL), and exits 0
# only when every check passed. A check that later checks depend on can stop the script early.
#
# Environment. The first five are optional. The last four are PRIVATE inputs (real repositories,
# a stock build, a case pack): no default; a check that needs one fails and names it.
#   TREE              the source tree under test (default: the git top level of this file)
#   CARGO_TARGET_DIR  cargo target dir for the tree (default: $TREE/target)
#   AFT_ACCEPT_WORK   where the work directories go (default: ${TMPDIR:-/tmp}/aft-adapters-accept)
#   AFT_ACCEPT_HEAVY  a command that runs one heavy job (cargo, an adapter run on a large repository)
#                     when the machine has room, for example a slot wrapper (default: run it directly)
#   PYTHON            python3 for the JSON checks (default: python3 on PATH)
#   AFT_ACCEPT_CASES  the consumer's private case pack: a directory with `cases.sh` (shell variables CASE_*
#                     with the real-repository values) and `fixtures/` (real-repository fixtures). Without
#                     it, each check that needs real repositories prints SKIP and is counted as skipped.
#   STOCK_AFT         stock upstream release binary built at BASE_COMMIT (R2: the pass-through gate)
#   ACCEPT_CLONES     a folder with clean clones of the repositories that the case pack names (R1, R2, G1)
#   CASE_REPOS_ROOT       a folder with the repositories of the case pack under repos/ (W1, G2)

LIB_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
TREE=${TREE:-$(git -C "$LIB_DIR" rev-parse --show-toplevel)}
TARGET=${CARGO_TARGET_DIR:-$TREE/target}
WORK_ROOT=${AFT_ACCEPT_WORK:-${TMPDIR:-/tmp}/aft-adapters-accept}
STOCK_AFT=${STOCK_AFT:-}
CLONES=${ACCEPT_CLONES:-}
CASE_REPOS_ROOT=${CASE_REPOS_ROOT:-}
# The case pack: its shell variables (CASE_*) hold every real-repository name, path and count.
CASES=${AFT_ACCEPT_CASES:-}
if [ -n "$CASES" ]; then
	CASES=$(cd "$CASES" && pwd) || { echo "AFT_ACCEPT_CASES is not a directory: $AFT_ACCEPT_CASES" >&2; exit 2; }
	[ -f "$CASES/cases.sh" ] || { echo "AFT_ACCEPT_CASES has no cases.sh: $CASES" >&2; exit 2; }
	export AFT_ACCEPT_CASES="$CASES"
	. "$CASES/cases.sh"
fi
# The user's Go and cargo stores, read now: isolate() later moves HOME, and Go with an empty GOPATH
# would try the network.
REAL_GOPATH=$(go env GOPATH 2> /dev/null)
REAL_GOMODCACHE=$(go env GOMODCACHE 2> /dev/null)
REAL_GOCACHE=$(go env GOCACHE 2> /dev/null)
REAL_GOENV=$(go env GOENV 2> /dev/null)
REAL_CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
# rustup reads its toolchains from RUSTUP_HOME (default $HOME/.rustup). Both stores are exported now, so cargo still
# finds its toolchain and crates after isolate() moves HOME (with rustup, a moved HOME means "no default toolchain").
export CARGO_HOME="$REAL_CARGO_HOME" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
# heavy <command...>: one heavy job, through AFT_ACCEPT_HEAVY when it is set.
heavy() { if [ -n "${AFT_ACCEPT_HEAVY:-}" ]; then "$AFT_ACCEPT_HEAVY" "$@"; else "$@"; fi; }
BASE_COMMIT=f5ecf51b5
PY="${PYTHON:-python3} -I"
FAKE="$TREE/tests/adapters/fake"
FIXTURES="$TREE/tests/adapters/fixtures"
mkdir -p "$WORK_ROOT"
WORK=$(mktemp -d "$WORK_ROOT/$(basename "$0" .sh).XXXXXX")
PASSES=0
FAILS=0
SKIPS=0
FAILED_CHECKS=()

pass() { PASSES=$((PASSES + 1)); echo "PASS $*"; }
fail() { FAILS=$((FAILS + 1)); FAILED_CHECKS+=("$*"); echo "FAIL $*"; }
# with_cases "<check name>": succeeds when the case pack is loaded. Without it: prints SKIP, counts it, and
# returns 1, so the caller skips the check:  if with_cases "name"; then ...; fi
with_cases() {
	[ -n "$CASES" ] && return 0
	SKIPS=$((SKIPS + 1))
	echo "SKIP $1: needs the private case pack (AFT_ACCEPT_CASES)"
	return 1
}
check() { # check "<name>" <command...>: PASS when the command exits 0
	local name=$1; shift
	if "$@" > "$WORK/check.out" 2>&1; then pass "$name"; else fail "$name: $(tail -n 3 "$WORK/check.out" | tr '\n' ' ')"; fi
}
finish() {
	echo "---"
	echo "$(basename "$0"): $PASSES passed, $FAILS failed, $SKIPS skipped (work dir $WORK)"
	if [ "$FAILS" -gt 0 ]; then
		echo "first failure: ${FAILED_CHECKS[0]}"
		exit 1
	fi
	exit 0
}
stop_if_failed() { # stop_if_failed "<why later checks cannot run>"
	if [ "$FAILS" -gt 0 ]; then echo "STOP $*"; finish; fi
}

# isolate <name>: aft storage, cache, config and HOME under the work dir; no registry, no fixture dir.
isolate() {
	local h="$WORK/home-$1"
	mkdir -p "$h/home" "$h/data" "$h/config" "$h/state" "$h/cache" "$h/runtime" "$h/storage" "$h/aft-cache"
	chmod 700 "$h/runtime"
	export HOME="$h/home" XDG_DATA_HOME="$h/data" XDG_CONFIG_HOME="$h/config" XDG_STATE_HOME="$h/state"
	export XDG_CACHE_HOME="$h/cache" XDG_RUNTIME_DIR="$h/runtime" AFT_STORAGE_DIR="$h/storage" AFT_CACHE_DIR="$h/aft-cache"
	export GIT_CONFIG_GLOBAL="$WORK/gitconfig" GIT_CONFIG_NOSYSTEM=1
	printf '[user]\n\tname = accept\n\temail = accept@example.invalid\n[init]\n\tdefaultBranch = main\n' > "$WORK/gitconfig"
	unset AFT_ADAPTERS_REGISTRY AFT_ADAPTERS_FIXTURE_DIR AFT_FAKE_RECORD_DIR AFT_FAKE_RUN_LOG
}

# Go environment for adapter runs: the user's stores, no network (decision 8).
go_offline_env() {
	export GOPATH="$REAL_GOPATH" GOMODCACHE="$REAL_GOMODCACHE" GOCACHE="$REAL_GOCACHE" GOENV="$REAL_GOENV"
	export GOPROXY=off GONOPROXY=none GOSUMDB=off GOTOOLCHAIN=local GOFLAGS=-mod=readonly
}

# build_aft: dev build of the tree's aft binary (heavy job: with_slot). Sets AFT_BIN.
build_aft() {
	if (cd "$TREE" && CARGO_TARGET_DIR="$TARGET" CARGO_PROFILE_DEV_DEBUG=line-tables-only \
		heavy cargo build --locked -p agent-file-tools --bin aft) > "$WORK/build.log" 2>&1; then
		AFT_BIN="$TARGET/debug/aft"
		pass "build aft (dev) in $TREE"
	else
		fail "build aft: $(grep -E '^error' "$WORK/build.log" | head -n 3 | tr '\n' ' ')"
	fi
}

# scratch_repo <source dir> <dest dir>: a git work tree with one commit of the source files.
scratch_repo() {
	rm -rf "$2"; mkdir -p "$2"
	cp -R "$1"/. "$2"/
	git -C "$2" init -q && git -C "$2" add -A && git -C "$2" commit -q -m fixture
}

# Rust "thin" rule: no new crate, no change to any Cargo manifest or the lock file.
thin_rust() {
	if git -C "$TREE" diff --quiet "$BASE_COMMIT" -- Cargo.lock Cargo.toml crates/aft/Cargo.toml crates/aft-base/Cargo.toml \
		crates/aft-db/Cargo.toml crates/aft-parse/Cargo.toml crates/aft-store/Cargo.toml crates/aft-tokenizer/Cargo.toml; then
		pass "thin: Cargo.toml / Cargo.lock unchanged (no new crate)"
	else
		fail "thin: Cargo manifests or Cargo.lock changed: $(git -C "$TREE" diff --name-only "$BASE_COMMIT" -- Cargo.lock '*Cargo.toml' | tr '\n' ' ')"
	fi
}

# upstream_files_within <allowed path>...: every changed upstream file (outside our own directories)
# is in the allowed list.
OUR_DIRS='^(crates/aft/src/commands/data_callgraph/lang_adapters/|docs/adapters/|tests/adapters/|adapters/|scripts/adapters/|\.agents/skills/update-upstream/|\.github/workflows/adapters\.yml$)'
upstream_files_within() {
	local changed extra=""
	changed=$( { git -C "$TREE" diff --name-only "$BASE_COMMIT"; git -C "$TREE" ls-files --others --exclude-standard; } | sort -u | grep -Ev "$OUR_DIRS" || true)
	for f in $changed; do
		local ok=0
		for a in "$@"; do [ "$f" = "$a" ] && ok=1; done
		[ $ok = 1 ] || extra="$extra $f"
	done
	if [ -z "$extra" ]; then pass "upstream files touched are within the allowed list ($(echo $changed | wc -w | tr -d ' ') files)"; else fail "upstream files outside the allowed list:$extra"; fi
}

# The upstream files C0 and R2 may touch together (decision 12 + the C0 visibility word).
ALLOWED_UPSTREAM="crates/aft/src/main.rs crates/aft/src/commands/mod.rs crates/aft/src/commands/data_callgraph.rs crates/aft/src/commands/parent_query/mod.rs crates/aft/src/commands/callers.rs crates/aft/src/commands/call_tree.rs crates/aft/src/commands/impact.rs crates/aft/src/commands/trace_to.rs crates/aft/src/commands/trace_to_symbol.rs crates/aft/src/commands/trace_data.rs"

# json <file> <python expression over `d`>: prints the value (for small checks).
json() { $PY -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$1" "$2"; }
