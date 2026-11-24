#!/usr/bin/env bash
# accept_R2.sh — acceptance for R2 (core connection: AdapterGraph, mapping, finish_reply, guard test).
# Exit 0 only when every check holds. Run from anywhere; see tests/adapters/lib/common.sh for env.
#
# Overlays are served two ways at once, so the script works on R2's tree (C0 stub of
# overlay_for_query reads $AFT_ADAPTERS_FIXTURE_DIR) and after integration (R1 runs the fake
# adapter named in $AFT_ADAPTERS_REGISTRY).
set -u
. "$(dirname "$0")/lib/common.sh"
TOOLS="$PY $TREE/tests/adapters/lib/reply_tools.py"

# --- thin and touched files -------------------------------------------------------------
thin_rust
upstream_files_within $ALLOWED_UPSTREAM
build_aft
stop_if_failed "no aft binary to test"
[ -x "$STOCK_AFT" ] || { fail "stock v0.67.0 binary missing: $STOCK_AFT"; finish; }

# --- 1-3. real-repository cases (private case pack): pass-through, fixture overlay, not-fresh adapter ---
if with_cases "real-repository cases (pass-through gate K, fixture overlay, not-fresh reply)"; then
	P="$CLONES/$CASE_P_REPO"
	SS="$CLONES/$CASE_S_REPO"
	for r in "$P" "$SS"; do
		if [ -n "$(git -C "$r" rev-parse --verify -q HEAD)" ] && [ -z "$(git -C "$r" status --porcelain --untracked-files=no)" ]; then
			pass "clone is a clean git work tree with a commit: $r"
		else
			fail "clone not clean or without commit: $r"
		fi
	done
	stop_if_failed "the cases need clean clones of the two case repositories"

	# serve_overlay <mode>: empty | fixture | broken
	serve_overlay() {
		local mode=$1 dir="$WORK/fx-$1" fake
		mkdir -p "$dir/$CASE_P_REPO" "$dir/$CASE_S_REPO"
		case "$mode" in
		empty) fake=emit_empty.sh ;;
		fixture) fake=emit_fixture.sh ;;
		broken) fake=no_end.sh ;;
		esac
		for r in "$CASE_P_REPO" "$CASE_S_REPO"; do
			"$FAKE/$fake" --language go --root "$CLONES/$r" > "$dir/$r/go.ndjson"
		done
		FAKE_WAIT_S=20 FAKE_TIMEOUT_S=120 "$FAKE/make_registry.sh" "$fake" > "$WORK/registry-$mode.json"
		export AFT_ADAPTERS_FIXTURE_DIR="$dir" AFT_ADAPTERS_REGISTRY="$WORK/registry-$mode.json"
	}

	# run_cases <label> <binary>: the six graph commands on P and S, after one warm-up call per case
	# (a cold dev-profile build can pass upstream's 30 s wait). Replies: $WORK/<label>/<case>-<command>.json
	run_cases() {
		local label=$1 bin=$2 out="$WORK/$1"
		mkdir -p "$out"
		local pf="$P/$CASE_P_FILE" sf="$SS/$CASE_S_FILE" sd="$SS/$CASE_S_ORIGIN_FILE"
		local p_cmds=(
			"{\"command\":\"callers\",\"file\":\"$pf\",\"symbol\":\"$CASE_P_SYMBOL\"}"
			"{\"command\":\"call_tree\",\"file\":\"$pf\",\"symbol\":\"$CASE_P_SYMBOL\"}"
			"{\"command\":\"impact\",\"file\":\"$pf\",\"symbol\":\"$CASE_P_SYMBOL\"}"
			"{\"command\":\"trace_to\",\"file\":\"$pf\",\"symbol\":\"$CASE_P_SYMBOL\"}"
			"{\"command\":\"trace_to_symbol\",\"file\":\"$pf\",\"symbol\":\"$CASE_P_SYMBOL\",\"toSymbol\":\"$CASE_P_TARGET_SYMBOL\"}"
			"{\"command\":\"trace_data\",\"file\":\"$pf\",\"symbol\":\"$CASE_P_SYMBOL\",\"expression\":\"req\",\"depth\":5}"
		)
		local s_cmds=(
			"{\"command\":\"callers\",\"file\":\"$sf\",\"symbol\":\"$CASE_S_SYMBOL\"}"
			"{\"command\":\"call_tree\",\"file\":\"$sf\",\"symbol\":\"$CASE_S_SYMBOL\"}"
			"{\"command\":\"impact\",\"file\":\"$sf\",\"symbol\":\"$CASE_S_SYMBOL\"}"
			"{\"command\":\"trace_to\",\"file\":\"$sf\",\"symbol\":\"$CASE_S_SYMBOL\"}"
			"{\"command\":\"trace_to_symbol\",\"file\":\"$sd\",\"symbol\":\"$CASE_S_ORIGIN_SYMBOL\",\"toSymbol\":\"$CASE_S_SYMBOL\"}"
			"{\"command\":\"trace_data\",\"file\":\"$sf\",\"symbol\":\"$CASE_S_SYMBOL\",\"expression\":\"req\",\"depth\":5}"
		)
		local names=(callers call_tree impact trace_to trace_to_symbol trace_data)
		local case root i
		for case in P S; do
			if [ $case = P ]; then root=$P; set -- "${p_cmds[@]}"; else root=$SS; set -- "${s_cmds[@]}"; fi
			$PY "$TREE/tests/adapters/lib/oneshot.py" --bin "$bin" --root "$root" "$1" > /dev/null 2>&1
			$PY "$TREE/tests/adapters/lib/oneshot.py" --bin "$bin" --root "$root" "$@" > "$out/$case.jsonl" 2> "$out/$case.err"
			i=0
			while IFS= read -r line; do
				printf '%s\n' "$line" > "$out/$case-${names[$i]}.json"
				i=$((i + 1))
			done < "$out/$case.jsonl"
			[ "$i" = 6 ] || fail "$label $case: got $i of 6 replies (see $out/$case.err)"
		done
	}

	# --- 1. decision K: empty overlay = stock v0.67.0, all six commands, P and S -----------------
	isolate k-stock
	run_cases stock "$STOCK_AFT"
	isolate k-empty
	serve_overlay empty
	run_cases empty "$AFT_BIN"
	for case in P S; do
		for c in callers call_tree impact trace_to trace_to_symbol trace_data; do
			a="$WORK/stock/$case-$c.json"; b="$WORK/empty/$case-$c.json"
			if [ ! -s "$a" ] || [ ! -s "$b" ]; then fail "K $case $c: missing reply"; continue; fi
			if [ "$(json "$a" 'd.get("success")')" != True ]; then fail "K $case $c: stock reply is not a success: $(head -c 200 "$a")"; continue; fi
			if diff_out=$($TOOLS same "$a" "$b"); then pass "K $case $c: empty overlay reply equals stock v0.67.0"; else fail "K $case $c: differs from stock: $diff_out"; fi
		done
	done
	stop_if_failed "decision K (pass-through) must hold before anything else is judged"

	# --- 2. fixture overlay (Phase 1 spike edges for P and S) -------------------------------------
	isolate fixture
	serve_overlay fixture
	run_cases fx "$AFT_BIN"
	R="$WORK/fx"
	for case in P S; do
		for c in callers call_tree impact trace_to trace_to_symbol trace_data; do
			f="$R/$case-$c.json"
			st=$($TOOLS adapters "$f" 2>/dev/null || echo MISSING)
			if printf '%s' "$st" | grep -q '"language": "go"' && printf '%s' "$st" | grep -q '"status": "fresh"'; then
				pass "status: $case $c reply lists go = fresh"
			else
				fail "status: $case $c reply has no go=fresh adapter status (got: $(printf '%s' "$st" | head -c 160))"
			fi
			if [ "$(json "$f" 'isinstance(d.get("text"), str) and len(d["text"]) > 0')" = True ]; then pass "text: $case $c reply has text"; else fail "text: $case $c reply has no text field"; fi
			case $c in callers | call_tree | impact | trace_to)
				if $TOOLS mentions "$f" "/mocks/"; then fail "no mocks: $case $c reply shows a mocks/ path"; else pass "no mocks: $case $c"; fi ;;
			esac
		done
	done
	want_p="$CASE_P_CALLERS"
	want_s="$CASE_S_CALLERS"
	got_p=$($TOOLS caller-lines "$R/P-callers.json"); got_s=$($TOOLS caller-lines "$R/S-callers.json")
	miss=""; for w in $want_p; do printf '%s\n' "$got_p" | grep -qx "$w" || miss="$miss $w"; done
	[ -z "$miss" ] && pass "P callers 3/3" || fail "P callers: missing$miss (got: $(echo $got_p))"
	miss=""; for w in $want_s; do printf '%s\n' "$got_s" | grep -qx "$w" || miss="$miss $w"; done
	[ -z "$miss" ] && pass "S callers 4/4" || fail "S callers: missing$miss (got: $(echo $got_s))"
	for case in P S; do
		if [ "$(json "$R/$case-callers.json" 'd.get("complete", True)')" = True ]; then
			pass "coverage rule: $case callers reply is complete (every named site covered)"
		else
			fail "coverage rule: $case callers reply still says complete:false ($(json "$R/$case-callers.json" 'd.get("incomplete_reason")'))"
		fi
	done
	tree=$($TOOLS tree "$R/P-call_tree.json")
	for t in $CASE_P_TREE_FILES; do
		if printf '%s\n' "$tree" | grep -q " $t .* True"; then pass "call_tree P shows the target symbol in $t"; else fail "call_tree P: no resolved child in $t (children: $(printf '%s' "$tree" | tr '\n' ';' | head -c 300))"; fi
	done
	for f in $CASE_P_IMPACT_FILES; do
		$TOOLS mentions "$R/P-impact.json" "$f" && pass "impact P lists $f" || fail "impact P does not list $f"
	done
	$TOOLS mentions "$R/P-trace_to.json" "$CASE_P_TRACE_FILE" && pass "trace_to P reaches $CASE_P_TRACE_FILE" || fail "trace_to P does not reach $CASE_P_TRACE_FILE"
	if [ "$(json "$R/P-trace_to_symbol.json" 'bool(d.get("path"))')" = True ]; then pass "trace_to_symbol P -> $CASE_P_TARGET_SYMBOL has a path"; else fail "trace_to_symbol P -> $CASE_P_TARGET_SYMBOL: no path ($(json "$R/P-trace_to_symbol.json" 'd.get("reason")'))"; fi
	if [ "$(json "$R/P-call_tree.json" '"adapter:go" in d.get("text", "")')" = True ]; then pass "call_tree P text marks adapter-added targets (adapter:go)"; else fail "call_tree P text has no adapter:go mark"; fi

	# --- 3. adapter not fresh -> complete:false + adapter_reason in all six replies ----------------
	isolate broken
	serve_overlay broken
	run_cases nf "$AFT_BIN"
	for c in callers call_tree impact trace_to trace_to_symbol trace_data; do
		f="$WORK/nf/P-$c.json"
		if [ "$(json "$f" 'd.get("complete") is False and isinstance(d.get("adapter_reason"), str) and "go" in d["adapter_reason"]')" = True ]; then
			pass "not fresh: P $c says complete:false with adapter_reason"
		else
			fail "not fresh: P $c lacks complete:false/adapter_reason (complete=$(json "$f" 'd.get("complete")'), adapter_reason=$(json "$f" 'd.get("adapter_reason")'))"
		fi
	done
fi

stop_if_failed "fix the reply checks first (the cargo stages below take about 10 minutes)"

# --- 4. R2's own tests, including the guard test and the parent-folder test --------------------
if (cd "$TREE" && CARGO_TARGET_DIR="$TARGET" CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only \
	heavy cargo test --locked -p agent-file-tools --features test-hooks --lib lang_adapters) > "$WORK/unit.log" 2>&1; then
	pass "cargo test lang_adapters: $(grep -E '^test result' "$WORK/unit.log" | tail -n 1)"
else
	fail "cargo test lang_adapters failed: $(grep -E 'FAILED|panicked|^error' "$WORK/unit.log" | head -n 3 | tr '\n' ' ')"
fi
for t in callgraph_read_methods_are_all_reviewed parent_folder_reply_has_adapter_edges; do
	if grep -qE "^test .*::$t \.\.\. ok" "$WORK/unit.log"; then pass "named test passed: $t"; else fail "named test missing or failing: $t"; fi
done
if grep -q 'aft-parse/src/callgraph_store/mod.rs' "$TREE/crates/aft/src/commands/data_callgraph/lang_adapters/graph.rs" 2>/dev/null && grep -q 'CallGraphRead' "$TREE/crates/aft/src/commands/data_callgraph/lang_adapters/graph.rs"; then
	pass "guard test reads the CallGraphRead source file"
else
	fail "guard test in graph.rs does not read aft-parse/src/callgraph_store/mod.rs"
fi

# --- 5. upstream's own test targets (Phase 0): 0 failed, passed >= baseline -------------------
baseline() { # <id> <min passed> <cargo test args...>
	local id=$1 min=$2; shift 2
	local th="$WORK/test-home"
	mkdir -p "$th/home" "$th/data" "$th/config" "$th/state" "$th/cache"
	# heavy goes first: env cannot run a shell function.
	( cd "$TREE" && heavy env -u AFT_STORAGE_DIR -u AFT_CACHE_DIR -u XDG_RUNTIME_DIR HOME="$th/home" CARGO_HOME="$REAL_CARGO_HOME" XDG_DATA_HOME="$th/data" XDG_CONFIG_HOME="$th/config" \
		XDG_STATE_HOME="$th/state" XDG_CACHE_HOME="$th/cache" AFT_GATE_HERMETIC_HOME_ROOT="$th" AFT_RUST_TEST_GATE=1 \
		CARGO_TARGET_DIR="$TARGET" CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only \
		cargo test --locked "$@" ) > "$WORK/baseline-$id.log" 2>&1
	local p f
	p=$(grep -E '^test result:' "$WORK/baseline-$id.log" | awk '{s+=$4} END {print s+0}')
	f=$(grep -E '^test result:' "$WORK/baseline-$id.log" | awk '{s+=$6} END {print s+0}')
	if [ "$f" = 0 ] && [ "$p" -ge "$min" ]; then pass "upstream $id: $p passed (baseline $min), 0 failed"; else fail "upstream $id: $p passed (baseline $min), $f failed"; fi
}
baseline T1 120 -p aft-parse --features test-hooks --lib
baseline T2 677 -p agent-file-tools --features test-hooks --lib -- data_layer:: callgraph_store:: commands::zoom commands::data_callgraph commands::parent_query commands::callers commands::call_tree commands::impact commands::trace
baseline T3 114 -p agent-file-tools --features test-hooks --test integration -- callgraph_test callgraph_restart_reuse_test trace_to_symbol_test outline_zoom_test
baseline T4 27 -p agent-file-tools --features test-hooks --test list_envelope -- list_envelope_callgraph list_envelope_trace

# --- upstream's source-scan tests: our new code must not break them (for example, no compile-time
# CARGO_MANIFEST_DIR anywhere in crate sources: build_path_independence) ------------------------------
if (cd "$TREE" && CARGO_TARGET_DIR="$TARGET" CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only \
	heavy cargo test --locked -p agent-file-tools --features test-hooks --test rest -- build_path_independence launch_nonce_source_scan) > "$WORK/scan.log" 2>&1; then
	pass "upstream source-scan tests pass: $(grep -E '^test result' "$WORK/scan.log" | tail -n 1)"
else
	fail "upstream source-scan tests: $(grep -E 'FAILED|panicked|^read CARGO|^[a-z_/]*\.rs:[0-9]' "$WORK/scan.log" | head -n 4 | tr '\n' ' ')"
fi

finish
