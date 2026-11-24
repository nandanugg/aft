#!/usr/bin/env bash
# accept_W1.sh — acceptance for W1 (`aft cli`, `aft guide`, GUIDE.md, shell shim, install.sh, uninstall.sh).
# Exit 0 only when every check holds. Run it on the integrated tree: the alias and facts checks need
# R1's `aft facts` (on a tree without R1 they fail with that reason).
#
# Interfaces W1 provides (fixed in the W1 brief):
#   aft cli <command> <args...> [--json]   commands: outline zoom read grep glob callers call_tree impact
#       trace_to trace_to_symbol trace_data facts adapters guide + every fact alias of the registry.
#       Exit 0 ok; 1 the command failed (reply success:false); 2 usage; 3 root refused (not a git work
#       tree with a HEAD commit). Prints text, not JSON (unless --json).
#   aft guide
#   scripts/adapters/aft                    shell shim: exec <installed binary> cli "$@" (warmup: the binary itself)
#   scripts/adapters/install.sh --prefix <dir> (--release-url <url> | --binary <file>) [--without-semantic]   (platform detected; tests/adapters/accept_P1.sh covers shells and Linux)
#       By default it also installs ONNX Runtime and the embedding model under <prefix>/libexec/aft-adapters
#       (pinned https downloads; this test downloads them once) and the shim points aft to them.
#       release: <url>/aft-<platform> + <url>/checksums.sha256 (curl; file:// works); a checksum
#       mismatch stops it before anything is installed. Installs <prefix>/libexec/aft-adapters/aft,
#       <prefix>/libexec/aft-adapters/adapters-registry.json, the adapters that exist in adapters/,
#       <prefix>/bin/aft (shim), and <prefix>/share/aft-adapters/install-manifest.txt.
#   scripts/adapters/uninstall.sh --prefix <dir>   removes exactly the files in the manifest.
set -u
. "$(dirname "$0")/lib/common.sh"
SCRIPTS="$TREE/scripts/adapters"

# --- 0. thin: shell parts use bash + POSIX tools + curl + git + shasum (+ go to build the Go adapter) --
for f in aft install.sh uninstall.sh; do
	if [ ! -f "$SCRIPTS/$f" ]; then fail "missing scripts/adapters/$f"; continue; fi
	if grep -nE '(^|[^a-zA-Z_-])(jq|python3?|node|perl|ruby)([^a-zA-Z_-]|$)' "$SCRIPTS/$f" | grep -v '^[0-9]*:[[:space:]]*#' | grep -v 'thin-ok' > "$WORK/deps.out"; then
		fail "thin: scripts/adapters/$f uses a tool outside bash/POSIX/curl/git/sha256 (lines marked thin-ok are the last fallbacks): $(head -n 2 "$WORK/deps.out" | tr '\n' ' ')"
	else
		pass "thin: scripts/adapters/$f uses only bash/POSIX/curl/wget/git/sha256 tools"
	fi
done
thin_rust
upstream_files_within $ALLOWED_UPSTREAM
[ -s "$TREE/docs/adapters/GUIDE.md" ] && pass "docs/adapters/GUIDE.md exists" || fail "docs/adapters/GUIDE.md missing"
build_aft
stop_if_failed "no aft binary or no shell parts to test"
A="$AFT_BIN"
isolate main
echo '{"adapters": []}' > "$WORK/empty-registry.json"
export AFT_ADAPTERS_REGISTRY="$WORK/empty-registry.json"

# --- 1. every command one-shot from a folder with repos/<repo>/ paths; bare names (case pack) -------
if with_cases "aft cli on real repositories (every command, bare names, outline parse)"; then
cli() { # cli <label> <args...>: runs `aft cli` from the folder that holds repos/; text output required
	local label=$1; shift
	( cd "$CASE_REPOS_ROOT" && "$A" cli "$@" ) > "$WORK/cli-$label.out" 2> "$WORK/cli-$label.err"
	local rc=$?
	local first; first=$(grep -m 1 -v '^[[:space:]]*$' "$WORK/cli-$label.out" | sed 's/^[[:space:]]*//' | cut -c1)
	if [ $rc = 0 ] && [ -s "$WORK/cli-$label.out" ] && [ "$first" != "{" ] && ! grep -qi ambiguous "$WORK/cli-$label.out" "$WORK/cli-$label.err"; then
		pass "cli $label: exit 0, text output"
	else
		fail "cli $label: rc=$rc first='$first' $(head -c 160 "$WORK/cli-$label.err") $(head -c 160 "$WORK/cli-$label.out")"
	fi
}
PF=repos/$CASE_P_REPO/$CASE_P_FILE
SYM=$CASE_P_SYMBOL_BARE
cli outline outline "$PF"
cli zoom zoom "$PF" "$SYM"
cli read read "$PF" "$CASE_P_READ_LINE" 20
cli grep grep "$SYM" "repos/$CASE_P_REPO/$CASE_P_GREP_DIR"
cli glob glob "$CASE_P_GLOB_PATTERN" "repos/$CASE_P_REPO/$CASE_P_GLOB_DIR"
cli callers callers "$PF" "$SYM"
cli call_tree call_tree "$PF" "$SYM"
cli impact impact "$PF" "$SYM"
cli trace_to trace_to "$PF" "$SYM"
cli trace_to_symbol trace_to_symbol "$PF" "$SYM" "$CASE_P_TARGET_SYMBOL"
cli trace_data trace_data "$PF" "$SYM" req
cli callers_S callers "repos/$CASE_S_REPO/$CASE_S_FILE" "$SYM"
grep -q "$CASE_P_SYMBOL" "$WORK/cli-callers.out" "$WORK/cli-callers.err" && pass "bare name in P resolved to $CASE_P_SYMBOL (shown)" || fail "bare name P: no resolution note"
grep -q "$CASE_S_SYMBOL" "$WORK/cli-callers_S.out" "$WORK/cli-callers_S.err" && pass "bare name in S resolved to $CASE_S_SYMBOL (shown)" || fail "bare name S: no resolution note"
# The outline text that cli.rs parses (a runtime seam): a call-site name in trace_data's candidate list,
# and a bare toSymbol where the file declares the interface and the implementation.
cli trace_data_handler trace_data "repos/$CASE_P_REPO/$CASE_P_HANDLER_FILE" "$SYM" req
grep -q "$CASE_P_HANDLER_NOTE" "$WORK/cli-trace_data_handler.out" && pass "trace_data: the call-site candidate is skipped (outline parse)" || fail "trace_data handler: no resolution note"
cli trace_to_symbol_bare trace_to_symbol "$PF" "$SYM" "$CASE_P_BARE_TO_SYMBOL" "repos/$CASE_P_REPO/$CASE_P_BARE_TO_FILE"
grep -q "$CASE_P_BARE_TO_NOTE" "$WORK/cli-trace_to_symbol_bare.out" && pass "trace_to_symbol: a bare toSymbol goes to the implementation (outline parse)" || fail "trace_to_symbol bare toSymbol: no note for the implementation"
fi
stop_if_failed "aft cli does not answer yet"

# --- 2. git-commit only -----------------------------------------------------------------------------
rm -rf "$WORK/nogit"; mkdir -p "$WORK/nogit"; cp -R "$FIXTURES/tiny-mixed"/. "$WORK/nogit"/
"$A" cli callers "$WORK/nogit/a.go" Use > "$WORK/nogit.out" 2>&1; rc=$?
[ $rc = 3 ] && grep -qi commit "$WORK/nogit.out" && pass "refuses a root without git (exit 3, names the commit rule)" || fail "no-git root: rc=$rc $(head -c 160 "$WORK/nogit.out")"
rm -rf "$WORK/nocommit"; mkdir -p "$WORK/nocommit"; cp -R "$FIXTURES/tiny-mixed"/. "$WORK/nocommit"/; git -C "$WORK/nocommit" init -q
"$A" cli callers "$WORK/nocommit/a.go" Use > "$WORK/nocommit.out" 2>&1; rc=$?
[ $rc = 3 ] && pass "refuses a git work tree without a commit (exit 3)" || fail "no-commit root: rc=$rc"

# --- 3. guide, facts and fact aliases (through a test registry with the fake Go adapter) -----------
"$FAKE/make_registry.sh" ok.sh > "$WORK/registry.json"; export AFT_ADAPTERS_REGISTRY="$WORK/registry.json"
scratch_repo "$FIXTURES/tiny-mixed" "$WORK/tiny"
"$A" guide > "$WORK/guide.out" 2>&1; rc=$?
head1=$(grep -m 1 -v '^[[:space:]]*$' "$TREE/docs/adapters/GUIDE.md")
[ $rc = 0 ] && grep -qF "$head1" "$WORK/guide.out" && grep -q go "$WORK/guide.out" && grep -q implements "$WORK/guide.out" \
	&& pass "aft guide prints GUIDE.md plus the registered language and fact kinds" || fail "aft guide: rc=$rc $(head -c 200 "$WORK/guide.out")"
( cd "$WORK/tiny" && "$A" cli adapters build --root . ) > /dev/null 2>&1
( cd "$WORK/tiny" && "$A" cli implementations a.go Doer ) > "$WORK/alias.out" 2>&1; rc=$?
[ $rc = 0 ] && grep -q impl "$WORK/alias.out" && pass "alias: implementations a.go Doer lists impl" || fail "alias implementations: rc=$rc $(head -c 200 "$WORK/alias.out")"
( cd "$WORK/tiny" && "$A" cli facts implements --label Doer ) > "$WORK/facts.out" 2>&1; rc=$?
[ $rc = 0 ] && grep -q impl "$WORK/facts.out" && pass "cli facts implements --label Doer lists impl" || fail "cli facts: rc=$rc $(head -c 200 "$WORK/facts.out")"

# --- 4. install from a file:// release; checksum mismatch stops it; uninstall removes exactly it ---
# The release asset name of this machine, as install.sh computes it.
case "$(uname -s)-$(uname -m)" in
	Darwin-arm64) PLATFORM=darwin-arm64 ;;
	Darwin-x86_64) PLATFORM=darwin-x64 ;;
	Linux-x86_64 | Linux-amd64) PLATFORM=linux-x64 ;;
	Linux-aarch64 | Linux-arm64) PLATFORM=linux-arm64 ;;
	*) PLATFORM=unknown ;;
esac
REL="$WORK/release"; mkdir -p "$REL"; cp "$A" "$REL/aft-$PLATFORM"
( cd "$REL" && shasum -a 256 "aft-$PLATFORM" > checksums.sha256 )
PREFIX="$WORK/prefix"; mkdir -p "$PREFIX"; touch "$PREFIX/.keep-before"
snap() { (cd "$1" && find . -print | LC_ALL=C sort); }
# HOME without Go's own telemetry counters: `go build` (the Go adapter build) writes them under
# os.UserConfigDir()/go/telemetry, the same class of Go toolchain state as GOCACHE. install.sh writes nothing there.
snap_home() { snap "$1" | grep -v -E '^\./Library(/Application Support(/go(/telemetry(/.*)?)?)?)?$'; }
snap "$PREFIX" > "$WORK/prefix.before"; snap_home "$HOME" > "$WORK/home.before"
unset AFT_ADAPTERS_REGISTRY
go_offline_env   # install.sh builds the Go adapter: the user's module cache, no network
if "$SCRIPTS/install.sh" --prefix "$PREFIX" --release-url "file://$REL" > "$WORK/install.out" 2>&1; then
	pass "install from file:// release"
else
	fail "install: $(tail -n 3 "$WORK/install.out" | tr '\n' ' ')"
fi
[ -x "$PREFIX/bin/aft" ] && "$PREFIX/bin/aft" guide > "$WORK/guide-installed.out" 2>&1 && pass "installed shim runs (aft guide)" || fail "installed shim does not run"
! grep -q 'LOCAL-ADDENDUM-MARKER' "$WORK/guide-installed.out" && [ ! -e "$PREFIX/libexec/aft-adapters/guide.local.md" ] \
	&& pass "install without --guide-local: no addendum file, none in aft guide" || fail "an addendum appeared without --guide-local"
# Semantic search is installed by default, inside the prefix, and the shim points aft to it.
ORT_LIB=$(ls "$PREFIX"/libexec/aft-adapters/onnxruntime/*/libonnxruntime.dylib "$PREFIX"/libexec/aft-adapters/onnxruntime/*/libonnxruntime.so 2> /dev/null | head -n 1)
MODEL=$(ls "$PREFIX"/libexec/aft-adapters/models/models--Qdrant--all-MiniLM-L6-v2-onnx/snapshots/*/model.onnx 2> /dev/null | head -n 1)
[ -n "$ORT_LIB" ] && [ -n "$MODEL" ] && pass "install put ONNX Runtime and the embedding model inside the prefix" || fail "install did not put ONNX Runtime and the model in the prefix"
grep -qF "$ORT_LIB" "$PREFIX/share/aft-adapters/install-manifest.txt" && grep -qF "$MODEL" "$PREFIX/share/aft-adapters/install-manifest.txt" \
	&& pass "the manifest lists the ONNX Runtime library and the model (uninstall removes them)" || fail "the manifest does not list the ONNX files"
"$PREFIX/bin/aft" search "call a function" "$WORK/tiny" > "$WORK/search-semantic.out" 2>&1; rc=$?
[ $rc = 0 ] && ! grep -q "ONNX Runtime not found" "$WORK/search-semantic.out" && pass "aft search from the install: no 'ONNX Runtime not found'" \
	|| fail "aft search from the install: rc=$rc $(head -c 200 "$WORK/search-semantic.out")"
# `aft warmup` is upstream's own subcommand (GUIDE: finish the semantic index); the shim passes it to the binary.
"$PREFIX/bin/aft" warmup --root "$WORK/tiny" --only semantic --timeout 120000 > "$WORK/warmup.out" 2>&1; rc=$?
[ $rc = 0 ] && ! grep -qF 'adapters list|status|build' "$WORK/warmup.out" && pass "aft warmup from the install reaches upstream's warmup" \
	|| fail "aft warmup from the install: rc=$rc $(head -c 200 "$WORK/warmup.out")"
reg="$PREFIX/libexec/aft-adapters/adapters-registry.json"
if [ -f "$reg" ] && $PY -c 'import json,os,sys
d=json.load(open(sys.argv[1]))
for e in d["adapters"]:
    p=e["command"][0] if os.path.isabs(e["command"][0]) else e["command"][1]
    assert os.path.exists(p), p' "$reg"; then pass "installed registry is next to the binary; every adapter path exists"; else fail "installed registry missing or names a missing program"; fi
# Every installed adapter must RUN from the install (all its files copied): a tiny committed root per
# language, the registry command and env, and an `end` record as the last line (CONTRACT.md 5.2).
if [ -f "$reg" ] && $PY - "$reg" "$WORK/adapter-run" <<'RUNPY'
import json, os, subprocess, sys
reg, base = sys.argv[1], sys.argv[2]
sources = {"go.mod": ("go.mod", "module tiny\n\ngo 1.21\n", "a.go", "package tiny\n\nfunc A() { B() }\n\nfunc B() {}\n"),
           "package.json": ("package.json", "{}\n", "a.js", "const { b } = require('./b.js');\nb();\n")}
extra = {"package.json": ("b.js", "function b() {}\nmodule.exports = { b };\n")}
for entry in json.load(open(reg))["adapters"]:
    marker = entry["root_markers"][0]
    root = os.path.join(base, entry["language"])
    os.makedirs(root, exist_ok=True)
    name, text, src, body = sources[marker]
    open(os.path.join(root, name), "w").write(text)
    open(os.path.join(root, src), "w").write(body)
    if marker in extra:
        open(os.path.join(root, extra[marker][0]), "w").write(extra[marker][1])
    for args in (["init", "-q"], ["add", "-A"], ["commit", "-q", "-m", "tiny"]):
        subprocess.run(["git", "-C", root, *args], check=True)
    command = [part.replace("{root}", root) for part in entry["command"]]
    env = dict(os.environ, **entry.get("env", {}))
    out = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True, timeout=120)
    last = out.stdout.strip().splitlines()[-1] if out.stdout.strip() else ""
    assert out.returncode == 0 and json.loads(last or "{}").get("type") == "end", (entry["language"], out.returncode, out.stderr[-300:])
RUNPY
then pass "every installed adapter runs from the install on a tiny committed root (end record)"; else fail "an installed adapter does not run from the install (missing files?)"; fi
snap_home "$HOME" > "$WORK/home.after"
cmp -s "$WORK/home.before" "$WORK/home.after" && pass "install wrote nothing in HOME" || fail "install changed HOME: $(diff "$WORK/home.before" "$WORK/home.after" | head -n 3 | tr '\n' ' ')"
"$SCRIPTS/uninstall.sh" --prefix "$PREFIX" > "$WORK/uninstall.out" 2>&1
snap "$PREFIX" > "$WORK/prefix.after"
cmp -s "$WORK/prefix.before" "$WORK/prefix.after" && pass "uninstall removed exactly what install added" || fail "uninstall left or removed files: $(diff "$WORK/prefix.before" "$WORK/prefix.after" | head -n 4 | tr '\n' ' ')"
# --without-semantic: no ONNX files, no download, and aft says that the runtime is missing.
PREFIX3="$WORK/prefix3"; mkdir -p "$PREFIX3"
snap "$PREFIX3" > "$WORK/prefix3.before"
if "$SCRIPTS/install.sh" --prefix "$PREFIX3" --release-url "file://$REL" --without-semantic > "$WORK/install-nosem.out" 2>&1; then
	[ ! -e "$PREFIX3/libexec/aft-adapters/onnxruntime" ] && [ ! -e "$PREFIX3/libexec/aft-adapters/models" ] && pass "--without-semantic installs no ONNX Runtime and no model" \
		|| fail "--without-semantic left ONNX files: $(ls "$PREFIX3/libexec/aft-adapters" | tr '\n' ' ')"
	"$PREFIX3/bin/aft" search "call a function" "$WORK/tiny" > "$WORK/search-nosem.out" 2>&1
	grep -q "ONNX Runtime not found" "$WORK/search-nosem.out" && pass "--without-semantic: aft search reports the missing runtime and falls back to words" \
		|| fail "--without-semantic: aft search did not report the missing runtime: $(head -c 200 "$WORK/search-nosem.out")"
	"$SCRIPTS/uninstall.sh" --prefix "$PREFIX3" > /dev/null 2>&1
	snap "$PREFIX3" > "$WORK/prefix3.after"
	cmp -s "$WORK/prefix3.before" "$WORK/prefix3.after" && pass "uninstall after --without-semantic removed exactly what install added" || fail "uninstall left files in prefix3"
else
	fail "install --without-semantic: $(tail -n 3 "$WORK/install-nosem.out" | tr '\n' ' ')"
fi
# --guide-local: the file is copied next to the registry, listed in the manifest, appended by `aft guide`,
# and removed by uninstall.
printf '## Consumer notes\n\nLOCAL-ADDENDUM-MARKER run from the project root.\n' > "$WORK/guide.local.md"
PREFIX4="$WORK/prefix4"; mkdir -p "$PREFIX4"
snap "$PREFIX4" > "$WORK/prefix4.before"
if "$SCRIPTS/install.sh" --prefix "$PREFIX4" --release-url "file://$REL" --without-semantic --guide-local "$WORK/guide.local.md" > "$WORK/install-guide.out" 2>&1; then
	"$PREFIX4/bin/aft" guide > "$WORK/guide-local.out" 2>&1
	grep -q 'LOCAL-ADDENDUM-MARKER' "$WORK/guide-local.out" && grep -qF "$(grep -m 1 -v '^[[:space:]]*$' "$TREE/docs/adapters/GUIDE.md")" "$WORK/guide-local.out" \
		&& pass "--guide-local: aft guide shows the generic guide and the addendum" || fail "--guide-local: aft guide has no addendum: $(head -c 200 "$WORK/guide-local.out")"
	grep -qF "$PREFIX4/libexec/aft-adapters/guide.local.md" "$PREFIX4/share/aft-adapters/install-manifest.txt" \
		&& pass "--guide-local: the manifest lists guide.local.md" || fail "--guide-local: guide.local.md is not in the manifest"
	"$SCRIPTS/uninstall.sh" --prefix "$PREFIX4" > /dev/null 2>&1
	snap "$PREFIX4" > "$WORK/prefix4.after"
	cmp -s "$WORK/prefix4.before" "$WORK/prefix4.after" && pass "uninstall after --guide-local removed exactly what install added" || fail "uninstall left files in prefix4"
else
	fail "install --guide-local: $(tail -n 3 "$WORK/install-guide.out" | tr '\n' ' ')"
fi
BAD="$WORK/release-bad"; mkdir -p "$BAD"; cp "$A" "$BAD/aft-$PLATFORM"
echo "0000000000000000000000000000000000000000000000000000000000000000  aft-$PLATFORM" > "$BAD/checksums.sha256"
PREFIX2="$WORK/prefix2"; mkdir -p "$PREFIX2"
if "$SCRIPTS/install.sh" --prefix "$PREFIX2" --release-url "file://$BAD" > "$WORK/install-bad.out" 2>&1; then
	fail "install accepted a checksum mismatch"
else
	[ -z "$(ls -A "$PREFIX2")" ] && pass "checksum mismatch stops install; nothing installed" || fail "checksum mismatch left files: $(ls -A "$PREFIX2" | tr '\n' ' ')"
fi

# --- 5. shell lint ------------------------------------------------------------------------------
if command -v shellcheck > /dev/null; then
	check "shellcheck clean" shellcheck "$SCRIPTS/aft" "$SCRIPTS/install.sh" "$SCRIPTS/uninstall.sh"
else
	check "bash -n (shellcheck is not installed on this machine: SKIPPED shellcheck)" sh -c 'for f; do bash -n "$f" || exit 1; done' _ "$SCRIPTS/aft" "$SCRIPTS/install.sh" "$SCRIPTS/uninstall.sh"
fi

finish
