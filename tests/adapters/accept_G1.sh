#!/usr/bin/env bash
# accept_G1.sh — acceptance for G1 (the Go adapter in adapters/go, contract v1). No cargo.
# Exit 0 only when every check holds. Go runs use the user's module cache with no network
# (GOPROXY=off GONOPROXY=none ...) and go through heavy (AFT_ACCEPT_HEAVY, see lib/common.sh).
#
# Interface G1 provides (fixed in the G1 brief):
#   adapters/go: `aft-go-adapter --root <repo> [--cpuprofile <file>] [--memprofile <file>]`
#   prints contract-v1 records on stdout; exit 0 also for a partial run.
#   adapters/go/PROFILE.md: old fork helper profile first, then the new adapter (decision O).
set -u
. "$(dirname "$0")/lib/common.sh"
GO_DIR="$TREE/adapters/go"
CHECK="$PY $TREE/tests/adapters/lib/contract_check.py"
REG="$TREE/docs/adapters/registry.example.json"
go_offline_env

# --- 0. thin: std + golang.org/x/* only -----------------------------------------------------
if [ ! -f "$GO_DIR/go.mod" ]; then fail "adapters/go/go.mod missing (no Go adapter yet)"; finish; fi
# The go.mod require lines (direct and indirect), not `go list -m all`: that also lists x/tools' own
# test dependencies (github.com/google/go-cmp, github.com/yuin/goldmark), which never reach the binary.
awk '/^require \(/{inblock=1; next} inblock && /^\)/{inblock=0; next} inblock && NF {print $1} /^require [^(]/{print $2}' "$GO_DIR/go.mod" > "$WORK/requires.txt"
others=$(grep -v '^golang\.org/x/' "$WORK/requires.txt" || true)
if [ -z "$others" ]; then pass "thin: go.mod requires only golang.org/x/* ($(tr '\n' ' ' < "$WORK/requires.txt"))"; else fail "thin: go.mod requires other modules: $(echo $others)"; fi

# --- 1. build and the adapter's own tests -------------------------------------------------------
if ( cd "$GO_DIR" && heavy go build -o "$WORK/aft-go-adapter" . ) > "$WORK/build.log" 2>&1; then pass "go build"; else fail "go build: $(tail -n 3 "$WORK/build.log" | tr '\n' ' ')"; fi
stop_if_failed "no adapter binary"
AD="$WORK/aft-go-adapter"
if ( cd "$GO_DIR" && heavy go test ./... ) > "$WORK/gotest.log" 2>&1; then pass "go test ./... passes"; else fail "go test: $(grep -E 'FAIL|panic' "$WORK/gotest.log" | head -n 3 | tr '\n' ' ')"; fi
grep -lq testdata "$GO_DIR"/*_test.go 2>/dev/null && pass "tests use restored testdata" || fail "no test file uses testdata/"

if with_cases "real repositories (contract, budget, profiles, edges, tags, facts)"; then
	# --- 2. real repositories (case pack): contract, budget, profiles -----------------------------------------------------
	run_repo() { # run_repo <label> <repo dir>
		local label=$1 repo=$2
		heavy /usr/bin/time -l "$AD" --root "$repo" --cpuprofile "$WORK/$label.cpu.prof" --memprofile "$WORK/$label.mem.prof" \
			> "$WORK/$label.ndjson" 2> "$WORK/$label.err"
		local rc=$?
		[ $rc = 0 ] && pass "$label: adapter exit 0" || fail "$label: adapter exit $rc: $(grep -v '^ ' "$WORK/$label.err" | tail -n 2 | tr '\n' ' ')"
		if $CHECK "$WORK/$label.ndjson" --registry "$REG" --language go --root "$repo" --summary "$WORK/$label.summary.json" > "$WORK/$label.check" 2>&1; then
			pass "$label: output is valid contract v1 (positions inside the files)"
		else
			fail "$label: $(cat "$WORK/$label.check")"
		fi
		local wall rss
		wall=$(awk '/ real /{print $1}' "$WORK/$label.err" | tail -n 1)
		rss=$(awk '/maximum resident set size/{print $1}' "$WORK/$label.err" | tail -n 1)
		# Budget = the old fork helper (Phase 1): CPU (user + sys) <= 23 s and RSS <= 6.0 GB. CPU, not wall
		# time, because other agents share the machine. Wall time is printed for the report.
		local cpu
		cpu=$(awk '/ real /{print $3 + $5}' "$WORK/$label.err" | tail -n 1)
		if [ -n "$cpu" ] && [ -n "$rss" ] && $PY -c "import sys; sys.exit(0 if float('$cpu') <= 23.0 and int('$rss') <= 6442450944 else 1)"; then
			pass "$label: budget ok (cpu $cpu s <= 23, RSS $((rss / 1048576)) MB <= 6144; wall $wall s)"
		else
			fail "$label: over budget (cpu ${cpu:-?} s, RSS ${rss:-?} bytes, wall ${wall:-?} s)"
		fi
		[ -s "$WORK/$label.cpu.prof" ] && [ -s "$WORK/$label.mem.prof" ] && go tool pprof -top -nodecount=5 "$AD" "$WORK/$label.cpu.prof" > "$WORK/$label.top" 2>&1 \
			&& pass "$label: cpu and mem profiles written and readable" || fail "$label: profiles missing or unreadable"
	}
	run_repo P "$CLONES/$CASE_P_REPO"
	run_repo S "$CLONES/$CASE_S_REPO"
	run_repo C "$CLONES/$CASE_C_REPO"

	# --- 3. edges, tags, mocks, fan-out, external labels, facts on P and S (case pack; the expected edges are in
	# CASE_G1_EDGES_JSON: p_callers, s_callers, p_target, s_target, p_calls) ------------------------------
	CASE_G1_EDGES_JSON="$CASE_G1_EDGES_JSON" $PY - "$WORK/P.ndjson" "$WORK/S.ndjson" > "$WORK/edges.out" 2>&1 <<'EOF'
import json, os, sys
def load(p):
    recs = [json.loads(l) for l in open(p) if l.strip()]
    return [r for r in recs if r["type"] == "site"], [r for r in recs if r["type"] == "fact"]
ps, pf = load(sys.argv[1]); ss, sf = load(sys.argv[2])
def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else ": " + detail))
def targets_at(sites, f, l):
    return [t for s in sites if s["file"] == f and s["line"] == l for t in s["targets"]]
E = json.loads(os.environ["CASE_G1_EDGES_JSON"])
for f, l in E["p_callers"]:
    t = targets_at(ps, f, l)
    check(f"P caller {f}:{l} -> {E['p_target']['file']}:{E['p_target']['line']}", any(x["file"] == E["p_target"]["file"] and x["line"] == E["p_target"]["line"] for x in t), str(t)[:200])
for f, l in E["s_callers"]:
    t = targets_at(ss, f, l)
    check(f"S caller {f}:{l} -> {E['s_target']['file']}:{E['s_target']['line']}", any(x["file"] == E["s_target"]["file"] and x["line"] == E["s_target"]["line"] for x in t), str(t)[:200])
for c in E["p_calls"]:
    t = targets_at(ps, c["file"], c["line"])
    check(f"P call at {c['file']}:{c['line']} -> {c['target_file']}:{c['target_line']}", any(x["file"] == c["target_file"] and x["line"] == c["target_line"] for x in t), str(t)[:200])
    check(f"P call at {c['file']}:{c['line']}: mock target present and tagged mock", any("/mocks/" in x["file"] and "mock" in x.get("tags", []) for x in t), str(t)[:200])
allt = [t for s in ps + ss for t in s["targets"]]
bad = [t for t in allt if ("/mocks/" in t["file"] or t["file"].startswith("mocks/")) != ("mock" in t.get("tags", []))]
check("mock tag on exactly the targets in mocks/", not bad, str(bad[:2]))
fan = [s for s in ps + ss if len(s["targets"]) > 1 and not all("interface" in t["kind"] for t in s["targets"])]
check("no signature-match fan-out (a site with >1 targets is an interface call)", not fan, str(fan[:1])[:300])
empty = [s for s in ps if not s["targets"]]
withext = [s for s in empty if s.get("external")]
check("external labels on >= 90% of sites with only external callees", empty and len(withext) >= 0.9 * len(empty), f"{len(withext)}/{len(empty)}")
check("external labels name a dependency with module@version", any(E["external_module_prefix"] in (e.get("module") or "") for s in withext for e in s["external"]), "none")
for tag in ["in_loop", "in_error_branch", "in_defer", "in_goroutine"]:
    check(f"site tag {tag} present on P", any(tag in s.get("tags", []) for s in ps), "0 sites")
for kind in ["registers", "implements", "writes", "returns", "flows"]:
    n = sum(1 for f in pf if f["kind"] == kind)
    check(f"P facts of kind {kind}: {n}", n > 0, "none")
check("facts carry subject_name", all(f.get("subject_name") for f in pf), "some facts lack subject_name")
check("mock implementations tagged mock", all(("mocks/" in f["subject"]["file"]) == ("mock" in f.get("tags", [])) for f in pf if f["kind"] == "implements"), "untagged mock")
EOF
	while IFS= read -r line; do case "$line" in PASS*) pass "${line#PASS }" ;; FAIL*) fail "${line#FAIL }" ;; *) fail "edge check error: $line" ;; esac; done < "$WORK/edges.out"
fi

# --- 4. restored fork expectations (decision L) ------------------------------------------------------
GF="$FIXTURES/go-facts"
for m in controlflow dispatch impls writes data_flow; do
	src="$GF/testdata/$m"; dst="$WORK/gf-$m"
	[ -d "$src" ] || { fail "go-facts module $m missing"; continue; }
	rm -rf "$dst"; cp -R "$src" "$dst"
	[ -f "$dst/go.mod" ] || printf 'module example.com/%s\n\ngo 1.22\n' "$m" > "$dst/go.mod"
	heavy "$AD" --root "$dst" > "$WORK/gf-$m.ndjson" 2> "$WORK/gf-$m.err"
	$CHECK "$WORK/gf-$m.ndjson" --registry "$REG" --language go --root "$dst" > "$WORK/gf-$m.check" 2>&1 || { fail "go-facts $m: $(cat "$WORK/gf-$m.check")"; continue; }
	if $PY "$TREE/tests/adapters/lib/facts_compare.py" "$GF/expected.json" --module "$m" --output "$WORK/gf-$m.ndjson" > "$WORK/gf-$m.cmp" 2>&1; then
		pass "go-facts $m: $(tail -n 1 "$WORK/gf-$m.cmp") ($(grep -c '^WARN' "$WORK/gf-$m.cmp") warn)"
	else
		fail "go-facts $m: $(grep '^FAIL' "$WORK/gf-$m.cmp" | head -n 3 | tr '\n' ' ')"
	fi
done

# --- 5. a missing module -> partial with the package named, exit 0 -------------------------------
MM="$WORK/missing-mod"; rm -rf "$MM"; mkdir -p "$MM/ok" "$MM/broken"
printf 'module example.com/mm\n\ngo 1.22\n\nrequire example.com/doesnotexist v1.0.0\n' > "$MM/go.mod"
printf 'package ok\n\nfunc A() int { return B() }\n\nfunc B() int { return 1 }\n' > "$MM/ok/ok.go"
printf 'package broken\n\nimport "example.com/doesnotexist/x"\n\nfunc C() { x.D() }\n' > "$MM/broken/broken.go"
heavy "$AD" --root "$MM" > "$WORK/mm.ndjson" 2> "$WORK/mm.err"; rc=$?
if [ $rc = 0 ] && $CHECK "$WORK/mm.ndjson" --registry "$REG" --language go --root "$MM" | grep -q '"partial": true' \
	&& grep '"type": *"unanalysed"' "$WORK/mm.ndjson" | grep -q 'broken'; then
	pass "missing module: exit 0, partial, the package is named"
else
	fail "missing module: rc=$rc $(grep unanalysed "$WORK/mm.ndjson" | head -c 200) $(tail -n 2 "$WORK/mm.err" | tr '\n' ' ')"
fi

# --- 6. profiling report (decision O) ----------------------------------------------------------------
PR="$GO_DIR/PROFILE.md"
if [ -f "$PR" ] && grep -qi 'old fork helper' "$PR" && grep -qi 'new adapter' "$PR" && grep -q 'repository P' "$PR" && grep -q 'repository C' "$PR" && grep -qiE 'top|flat' "$PR"; then
	pass "PROFILE.md: old helper and new adapter, both repository kinds, top functions"
else
	fail "PROFILE.md missing or incomplete (needs old fork helper + new adapter, repository P + repository C, top functions)"
fi

finish
