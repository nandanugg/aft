#!/usr/bin/env bash
# accept_R1.sh — acceptance for R1 (registry, key, runner, store, overlay_for_query, `aft adapters`, `aft facts`).
# Exit 0 only when every check holds. Uses the fake adapters in tests/adapters/fake and the tiny
# repo tests/adapters/fixtures/tiny-mixed (Go + JavaScript in one root).
#
# CLI shapes R1 provides (fixed in the R1 brief):
#   aft adapters list
#   aft adapters status --root <repo> --json   -> {"root": ..., "adapters": [AdapterStatus, ...]}
#   aft adapters build  --root <repo> [--language <l>] [--force] --json   (same shape, after the runs)
#   aft facts [--root <repo>] --json           -> {"kinds": [{"language","kind","description","aliases"}]}
#   aft facts <kind> --label <key> [--prefix] [--root <repo>] --json
#   aft facts <kind> <file> <symbol> [--depth N] [--include-tag <t>] --json
#                                               -> {"facts": [FactRecord...], "adapters": [AdapterStatus...]}
#   detached runner process: aft adapters run --root <repo> --language <l>
set -u
. "$(dirname "$0")/lib/common.sh"

thin_rust
upstream_files_within $ALLOWED_UPSTREAM
build_aft
stop_if_failed "no aft binary to test"
A="$AFT_BIN"
TINY="$FIXTURES/tiny-mixed"
REC="$WORK/records"; mkdir -p "$REC"
RUNLOG="$WORK/runs.log"; : > "$RUNLOG"

registry() { "$FAKE/make_registry.sh" "$@" > "$WORK/registry.json"; export AFT_ADAPTERS_REGISTRY="$WORK/registry.json"; }
status_of() { # status_of <json file> <language> -> status word, or "absent"
	$PY -c 'import json,sys
d=json.load(open(sys.argv[1])); s=[a for a in d.get("adapters",[]) if a.get("language")==sys.argv[2]]
print(s[0].get("status") if s else "absent")' "$1" "$2" 2>/dev/null || echo unreadable
}
field_of() { # field_of <json file> <language> <field>
	$PY -c 'import json,sys
d=json.load(open(sys.argv[1])); s=[a for a in d.get("adapters",[]) if a.get("language")==sys.argv[2]]
print(json.dumps(s[0].get(sys.argv[3])) if s else "")' "$1" "$2" "$3" 2>/dev/null
}
runs() { grep -c "^$1 " "$RUNLOG" 2>/dev/null || true; }
build() { # build <repo> [args...] -> $WORK/last.json
	"$A" adapters build --root "$1" "${@:2}" --json > "$WORK/last.json" 2> "$WORK/last.err"
}

isolate main
export AFT_FAKE_RUN_LOG="$RUNLOG"

# --- 1. list ---------------------------------------------------------------------------------
registry ok.sh ok.sh
if "$A" adapters list > "$WORK/list.out" 2>&1 && grep -q go "$WORK/list.out" && grep -q ok.sh "$WORK/list.out" && grep -q go.mod "$WORK/list.out"; then
	pass "aft adapters list shows language, command and markers"
else
	fail "aft adapters list: $(head -c 200 "$WORK/list.out")"
fi
stop_if_failed "the adapter CLI is not there yet"

# --- 2. registry only from the env path / install location ----------------------------------
scratch_repo "$TINY" "$WORK/r-evil"
for f in registry.json adapters.json .aft/adapters.json aft-adapters.json; do
	mkdir -p "$(dirname "$WORK/r-evil/$f")"; "$FAKE/make_registry.sh" recorder.sh recorder.sh > "$WORK/r-evil/$f"
done
git -C "$WORK/r-evil" add -A && git -C "$WORK/r-evil" commit -q -m "registry files in the project root"
( unset AFT_ADAPTERS_REGISTRY; export AFT_FAKE_RECORD_DIR="$REC"; "$A" adapters build --root "$WORK/r-evil" --json > "$WORK/evil.json" 2>&1 )
if [ -z "$(ls "$REC")" ]; then pass "a registry file in the project root is ignored (no adapter ran)"; else fail "an adapter ran from a project-root registry: $(ls "$REC")"; fi

# --- 3. binding: right command, root, language, env, process group; two languages routed apart --
registry recorder.sh recorder.sh
scratch_repo "$TINY" "$WORK/r-bind"
ROOT_REAL=$(cd "$WORK/r-bind" && pwd -P)
AFT_FAKE_RECORD_DIR="$REC" build "$WORK/r-bind"
for lang in go javascript; do
	n=$(ls "$REC"/record-$lang-*.txt 2>/dev/null | wc -l | tr -d ' ')
	if [ "$n" = 1 ]; then pass "binding: $lang adapter ran once"; else fail "binding: $lang adapter ran $n times"; continue; fi
	r=$(ls "$REC"/record-$lang-*.txt)
	grep -qx "language=$lang" "$r" && grep -qE "^root=($ROOT_REAL|$WORK/r-bind)$" "$r" && grep -qE "^pwd=($ROOT_REAL|$WORK/r-bind)$" "$r" \
		&& pass "binding: $lang got --language $lang, --root <repo>, cwd <repo>" || fail "binding: $lang argv/cwd wrong: $(grep -E '^(argv|pwd|root|language)' "$r" | tr '\n' ' ')"
	grep -qx "env:AFT_FAKE_ENTRY=$lang" "$r" && pass "binding: $lang got its own entry env" || fail "binding: $lang entry env missing"
	grep -qx "stdin=file-or-null" "$r" && pass "binding: $lang stdin is not a terminal or pipe" || fail "binding: $lang stdin: $(grep stdin= "$r")"
	[ "$(sed -n 's/^pgid=//p' "$r")" = "$(sed -n 's/^pid=//p' "$r")" ] && pass "binding: $lang runs in its own process group" || fail "binding: $lang not a process group leader"
done
"$A" adapters status --root "$WORK/r-bind" --json > "$WORK/bind-status.json" 2>&1
[ "$(status_of "$WORK/bind-status.json" go)" = fresh ] && [ "$(field_of "$WORK/bind-status.json" go sites)" = 2 ] && [ "$(field_of "$WORK/bind-status.json" go facts)" = 1 ] \
	&& pass "status: go fresh, 2 sites, 1 fact" || fail "status go: $(head -c 300 "$WORK/bind-status.json")"
[ "$(status_of "$WORK/bind-status.json" javascript)" = fresh ] && [ "$(field_of "$WORK/bind-status.json" javascript sites)" = 1 ] \
	&& pass "status: javascript fresh, 1 site (separate overlay)" || fail "status javascript: $(head -c 300 "$WORK/bind-status.json")"
"$A" facts implements --label Doer --root "$WORK/r-bind" --json > "$WORK/facts.json" 2>&1
[ "$(json "$WORK/facts.json" 'len(d.get("facts", []))')" = 1 ] && pass "facts: implements --label Doer -> 1 fact (go overlay)" || fail "facts by label: $(head -c 300 "$WORK/facts.json")"
"$A" facts implements "$WORK/r-bind/a.go" Doer --json > "$WORK/facts2.json" 2>&1
[ "$(json "$WORK/facts2.json" 'len(d.get("facts", []))')" = 1 ] && pass "facts: implements a.go Doer -> 1 fact (by position/name)" || fail "facts by symbol: $(head -c 300 "$WORK/facts2.json")"
"$A" facts --root "$WORK/r-bind" --json > "$WORK/kinds.json" 2>&1
[ "$(json "$WORK/kinds.json" '[k["kind"] for k in d.get("kinds", [])]')" = "['implements']" ] && grep -q implementations "$WORK/kinds.json" \
	&& pass "facts: kinds listed with description and alias" || fail "facts kinds: $(head -c 300 "$WORK/kinds.json")"

# --- 4. no run when markers do not match --------------------------------------------------------
rm -f "$REC"/*
mkdir -p "$WORK/src-nojs"; cp "$TINY"/*.go "$TINY"/go.mod "$WORK/src-nojs/"
scratch_repo "$WORK/src-nojs" "$WORK/r-nojs"
AFT_FAKE_RECORD_DIR="$REC" build "$WORK/r-nojs"
[ -z "$(ls "$REC"/record-javascript-* 2>/dev/null)" ] && [ -n "$(ls "$REC"/record-go-* 2>/dev/null)" ] \
	&& pass "markers: no javascript run without package.json; go ran" || fail "markers: records $(ls "$REC")"

# --- 5. git-commit only; dirty tree ----------------------------------------------------------------
registry ok.sh
rm -rf "$WORK/r-nogit"; mkdir -p "$WORK/r-nogit"; cp -R "$TINY"/. "$WORK/r-nogit"/
before=$(runs ok.sh); build "$WORK/r-nogit"
[ "$(runs ok.sh)" = "$before" ] && [ "$(status_of "$WORK/last.json" go)" = skipped ] && grep -qi commit "$WORK/last.json" \
	&& pass "no git: no run, status skipped naming the commit rule" || fail "no git: runs=$(runs ok.sh) status=$(status_of "$WORK/last.json" go)"
rm -rf "$WORK/r-nocommit"; mkdir -p "$WORK/r-nocommit"; cp -R "$TINY"/. "$WORK/r-nocommit"/; git -C "$WORK/r-nocommit" init -q
before=$(runs ok.sh); build "$WORK/r-nocommit"
[ "$(runs ok.sh)" = "$before" ] && [ "$(status_of "$WORK/last.json" go)" = skipped ] \
	&& pass "git without commit: no run, skipped" || fail "git without commit: runs changed or status=$(status_of "$WORK/last.json" go)"
scratch_repo "$TINY" "$WORK/r-dirty"; echo "// local change" >> "$WORK/r-dirty/a.go"
before=$(runs ok.sh); build "$WORK/r-dirty"
[ "$(runs ok.sh)" = "$before" ] && [ "$(status_of "$WORK/last.json" go)" = skipped ] && grep -qiE 'local change|dirty' "$WORK/last.json" \
	&& pass "dirty tree: no run, skipped with the reason" || fail "dirty tree: runs=$(runs ok.sh) $(head -c 200 "$WORK/last.json")"

# --- 6. failures are failures, with reasons ----------------------------------------------------
expect_failed() { # <fake> <substring in the reason>
	registry "$1"; scratch_repo "$TINY" "$WORK/r-$1"; build "$WORK/r-$1"
	local st; st=$(status_of "$WORK/last.json" go)
	if [ "$st" = failed ] && grep -q -- "$2" "$WORK/last.json"; then pass "failure: $1 -> failed ($2)"; else fail "failure: $1 -> $st, reason $(field_of "$WORK/last.json" go reason)"; fi
}
expect_failed crash.sh failed
expect_failed exit_nonzero.sh "fake stderr line 30"
expect_failed no_end.sh "no end record"
expect_failed partial_line.sh "incomplete"
expect_failed wrong_contract.sh "contract 2"
expect_failed undeclared_fact.sh "not declared"
expect_failed count_mismatch.sh "do not match"
registry exit_nonzero.sh; "$A" adapters status --root "$WORK/r-exit_nonzero.sh" --json > "$WORK/nz.json" 2>&1
tail_lines=$(field_of "$WORK/nz.json" go stderr_tail)
if printf '%s' "$tail_lines" | grep -q 'line 30' && ! printf '%s' "$tail_lines" | grep -q 'line 10"'; then pass "failure record keeps the last 20 stderr lines"; else fail "stderr tail: $tail_lines"; fi

# --- 7. partial = served + cooldown + retry; --force; new commit = new key ---------------------
registry partial.sh; scratch_repo "$TINY" "$WORK/r-partial"
build "$WORK/r-partial"; n1=$(runs partial.sh)
[ "$(status_of "$WORK/last.json" go)" = partial ] && grep -qE 'b\.go|example.com/tiny/b' "$WORK/last.json" \
	&& pass "partial: status partial, names the unanalysed unit" || fail "partial: $(head -c 300 "$WORK/last.json")"
build "$WORK/r-partial"; [ "$(runs partial.sh)" = "$n1" ] && pass "partial: cooldown, no second run" || fail "partial: ran again inside the cooldown"
build "$WORK/r-partial" --force; [ "$(runs partial.sh)" = $((n1 + 1)) ] && pass "partial: --force runs again" || fail "partial: --force did not run"
echo "// new commit" >> "$WORK/r-partial/b.go"; git -C "$WORK/r-partial" commit -q -am "new commit"
build "$WORK/r-partial"; [ "$(runs partial.sh)" = $((n1 + 2)) ] && pass "partial: a new commit is a new key and runs" || fail "partial: new commit did not run"
registry crash.sh; build "$WORK/r-crash.sh"; n2=$(runs crash.sh); build "$WORK/r-crash.sh"
[ "$(runs crash.sh)" = "$n2" ] && pass "failed: cooldown, no retry storm" || fail "failed: ran again inside the cooldown"

# --- 8. timeout kills the whole process group; no orphan ----------------------------------------
FAKE_TIMEOUT_S=3 registry hang_child.sh; scratch_repo "$TINY" "$WORK/r-hang"
t0=$(date +%s); build "$WORK/r-hang"; t1=$(date +%s)
[ $((t1 - t0)) -le 15 ] && pass "timeout: build returned in $((t1 - t0)) s (timeout 3 s + grace)" || fail "timeout: build took $((t1 - t0)) s"
[ "$(status_of "$WORK/last.json" go)" = failed ] && grep -qi timeout "$WORK/last.json" && pass "timeout: status failed (timeout)" || fail "timeout: $(head -c 200 "$WORK/last.json")"
sleep 1
if pgrep -f "sleep 86398" > /dev/null; then fail "timeout: the adapter's child 'sleep 86398' is still alive"; pkill -f "sleep 86398"; else pass "timeout: no orphan child left"; fi

# --- 9. detached runner; bounded wait; one run for parallel queries ---------------------------
FAKE_WAIT_S=2 FAKE_TIMEOUT_S=60 registry slow_ok.sh; scratch_repo "$TINY" "$WORK/r-slow"
export FAKE_SLEEP_S=10
before=$(runs slow_ok.sh)
t0=$(date +%s); "$A" facts implements --label Doer --root "$WORK/r-slow" --json > "$WORK/q1.json" 2>&1; t1=$(date +%s)
"$A" facts implements --label Doer --root "$WORK/r-slow" --json > "$WORK/q2.json" 2>&1; t2=$(date +%s)
[ $((t1 - t0)) -le 6 ] && [ $((t2 - t1)) -le 6 ] && pass "wait: queries returned in $((t1 - t0)) s and $((t2 - t1)) s (wait 2 s)" || fail "wait: queries took $((t1 - t0)) s and $((t2 - t1)) s"
[ "$(status_of "$WORK/q1.json" go)" = building ] && pass "wait: status building while the run goes on" || fail "wait: status $(status_of "$WORK/q1.json" go)"
sleep 13
[ "$(runs slow_ok.sh)" = $((before + 1)) ] && pass "detached: one run for two queries, finished after the query exited" || fail "detached: $(($(runs slow_ok.sh) - before)) runs"
"$A" facts implements --label Doer --root "$WORK/r-slow" --json > "$WORK/q3.json" 2>&1
[ "$(status_of "$WORK/q3.json" go)" = fresh ] && [ "$(json "$WORK/q3.json" 'len(d.get("facts", []))')" = 1 ] && pass "detached: later query is fresh with the fact" || fail "detached: $(head -c 200 "$WORK/q3.json")"

# --- 10. SIGKILL of the runner frees the lock ---------------------------------------------------
FAKE_WAIT_S=1 FAKE_TIMEOUT_S=60 registry slow_ok.sh; scratch_repo "$TINY" "$WORK/r-kill"
export FAKE_SLEEP_S=30
"$A" facts implements --label Doer --root "$WORK/r-kill" --json > /dev/null 2>&1
sleep 1
pkill -9 -f "adapters run --root $WORK/r-kill" || true; pkill -9 -f "adapters run --root $(cd "$WORK/r-kill" && pwd -P)" || true
pkill -f "slow_ok.sh --language go --root .*r-kill" || true
export FAKE_SLEEP_S=0
t0=$(date +%s); build "$WORK/r-kill" --force; t1=$(date +%s)
[ "$(status_of "$WORK/last.json" go)" = fresh ] && [ $((t1 - t0)) -le 10 ] && pass "lock: free after SIGKILL of the runner; next run works" || fail "lock: status $(status_of "$WORK/last.json" go) after $((t1 - t0)) s"

# --- 11. global limit: at most 2 adapter runs at a time ----------------------------------------
FAKE_WAIT_S=1 FAKE_TIMEOUT_S=60 registry slow_ok.sh; export FAKE_SLEEP_S=6
for i in 1 2 3; do scratch_repo "$TINY" "$WORK/r-lim$i"; done
for i in 1 2 3; do "$A" adapters build --root "$WORK/r-lim$i" --json > "$WORK/lim$i.json" 2>&1 & done
max=0
for _ in $(seq 1 40); do
	n=$(ps -axo pid=,ppid=,command= | awk '/slow_ok.sh --language go --root .*r-lim/ && !/awk/ {pid[$1]=1; par[$1]=$2} END {n=0; for (p in pid) if (!(par[p] in pid)) n++; print n}')
	[ "$n" -gt "$max" ] && max=$n
	sleep 0.5
done
wait
all_fresh=1; for i in 1 2 3; do "$A" adapters status --root "$WORK/r-lim$i" --json > "$WORK/lims$i.json" 2>&1; [ "$(status_of "$WORK/lims$i.json" go)" = fresh ] || all_fresh=0; done
[ "$max" -le 2 ] && [ "$max" -ge 1 ] && [ $all_fresh = 1 ] && pass "global limit: at most $max runs at once; all 3 finished fresh" || fail "global limit: max $max at once, all fresh=$all_fresh"
unset FAKE_SLEEP_S

# --- 12. overlay size: a large real-repository fixture loads within budget (case pack) ----------
if with_cases "overlay size on a large repository"; then
	FAKE_TIMEOUT_S=60 registry emit_fixture.sh
	build "$CLONES/$CASE_P_REPO"
	[ "$(status_of "$WORK/last.json" go)" = fresh ] && [ "$(field_of "$WORK/last.json" go sites)" = "$CASE_P_SITES" ] && pass "store: P fixture fresh with $CASE_P_SITES sites" || fail "store: $(head -c 200 "$WORK/last.json")"
	t0=$($PY -c 'import time; print(time.time())')
	"$A" facts implements --label nothing --root "$CLONES/$CASE_P_REPO" --json > /dev/null 2>&1
	t1=$($PY -c 'import time; print(time.time())')
	ms=$($PY -c "print(int(($t1 - $t0) * 1000))")
	[ "$ms" -le 2000 ] && pass "store: a query on the P overlay took $ms ms (budget 2000)" || fail "store: query took $ms ms"
fi

# --- 13. R1's own tests ---------------------------------------------------------------------------
if (cd "$TREE" && CARGO_TARGET_DIR="$TARGET" CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only \
	heavy cargo test --locked -p agent-file-tools --features test-hooks --lib lang_adapters) > "$WORK/unit.log" 2>&1; then
	pass "cargo test lang_adapters: $(grep -E '^test result' "$WORK/unit.log" | tail -n 1)"
else
	fail "cargo test lang_adapters failed: $(grep -E 'FAILED|panicked|^error' "$WORK/unit.log" | head -n 3 | tr '\n' ' ')"
fi

# --- upstream's source-scan tests: our new code must not break them (for example, no compile-time
# CARGO_MANIFEST_DIR anywhere in crate sources: build_path_independence) ------------------------------
if (cd "$TREE" && CARGO_TARGET_DIR="$TARGET" CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only \
	heavy cargo test --locked -p agent-file-tools --features test-hooks --test rest -- build_path_independence launch_nonce_source_scan) > "$WORK/scan.log" 2>&1; then
	pass "upstream source-scan tests pass: $(grep -E '^test result' "$WORK/scan.log" | tail -n 1)"
else
	fail "upstream source-scan tests: $(grep -E 'FAILED|panicked|^read CARGO|^[a-z_/]*\.rs:[0-9]' "$WORK/scan.log" | head -n 4 | tr '\n' ' ')"
fi

finish
