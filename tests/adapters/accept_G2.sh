#!/usr/bin/env bash
# accept_G2.sh — acceptance for G2 (the CommonJS adapter in adapters/js, contract v1). No cargo.
# Exit 0 only when every check holds. The real proof of the registry design: a second language
# plugs in by a registry entry only, with no Rust change.
#
# Interface G2 provides (fixed in the G2 brief):
#   node adapters/js/aft-js-adapter.js --root <repo>   -> contract-v1 records on stdout
#   adapters/js/vendor/acorn.js + adapters/js/vendor/LICENSE (acorn 8.16.0, MIT), no npm install
#   adapters/js/test/*.test.js for `node --test`
set -u
. "$(dirname "$0")/lib/common.sh"
JS="$TREE/adapters/js"
CHECK="$PY $TREE/tests/adapters/lib/contract_check.py"
REG="$TREE/docs/adapters/registry.example.json"
# The real repository and its known answers come from the case pack (CASE_JS_REPO, fixtures/js-known-answers.json).
REPO=${JS_REPO:-$CASE_REPOS_ROOT/repos/${CASE_JS_REPO:-}}
KA="${CASES:+$CASES/fixtures/js-known-answers.json}"

# --- 0. thin: Node built-ins + one vendored zero-dependency parser -------------------------------
if [ ! -f "$JS/aft-js-adapter.js" ]; then fail "adapters/js/aft-js-adapter.js missing (no JS adapter yet)"; finish; fi
[ -f "$JS/vendor/acorn.js" ] && [ -f "$JS/vendor/LICENSE" ] && grep -q 'MIT' "$JS/vendor/LICENSE" \
	&& pass "thin: vendored acorn.js with its MIT license" || fail "thin: vendor/acorn.js or vendor/LICENSE (MIT) missing"
[ ! -d "$JS/node_modules" ] && pass "thin: no node_modules" || fail "thin: node_modules present"
if [ -f "$JS/package.json" ] && grep -qE '"(dependencies|devDependencies|peerDependencies|optionalDependencies)"' "$JS/package.json"; then
	fail "thin: package.json declares dependencies"
else
	pass "thin: no declared npm dependencies"
fi
$PY - "$JS" > "$WORK/requires.out" <<'EOF'
import os, re, sys
root = sys.argv[1]
builtin = set("assert buffer child_process crypto events fs os path process readline stream string_decoder timers url util worker_threads zlib perf_hooks module".split())
bad = []
for d, _, files in os.walk(root):
    if "/vendor" in d:
        continue
    for f in files:
        if not f.endswith((".js", ".cjs", ".mjs")):
            continue
        text = open(os.path.join(d, f), encoding="utf-8").read()
        for m in re.finditer(r"""require\(\s*['"]([^'"]+)['"]\s*\)|from\s+['"]([^'"]+)['"]""", text):
            spec = m.group(1) or m.group(2)
            if spec.startswith("node:") or spec in builtin or spec.split("/")[0] in builtin or spec.startswith("./") or spec.startswith("../"):
                continue
            bad.append(f"{os.path.relpath(os.path.join(d, f), root)}: {spec}")
print("\n".join(bad), end="")
EOF
[ ! -s "$WORK/requires.out" ] && pass "thin: adapter code requires only Node built-ins and its own files" || fail "thin: other requires: $(head -n 3 "$WORK/requires.out" | tr '\n' ' ')"
if grep -rqE 'aft-js-adapter|"javascript".*adapter' "$TREE/crates" 2>/dev/null; then fail "a Rust file special-cases the JS adapter"; else pass "no Rust change for the JS language (registry entry only)"; fi
command -v node > /dev/null && pass "node available: $(node --version)" || { fail "node not found"; finish; }

# --- 1. the adapter's own tests -------------------------------------------------------------------
if ls "$JS"/test/*.test.js > /dev/null 2>&1 && node --test "$JS"/test/*.test.js > "$WORK/nodetest.log" 2>&1; then
	pass "node --test adapters/js/test passes"
else
	fail "node --test: $(tail -n 3 "$WORK/nodetest.log" 2>/dev/null | tr '\n' ' ')"
fi

if with_cases "real JavaScript repository (contract, time, known answers)"; then
	# --- 2. real repository (case pack): contract and time ------------------------------------------------------------------
	t0=$($PY -c 'import time; print(time.time())')
	node "$JS/aft-js-adapter.js" --root "$REPO" > "$WORK/js.ndjson" 2> "$WORK/js.err"; rc=$?
	t1=$($PY -c 'import time; print(time.time())')
	secs=$($PY -c "print(round($t1 - $t0, 1))")
	[ $rc = 0 ] && pass "adapter exit 0 on the real repository in $secs s" || fail "adapter exit $rc: $(tail -n 2 "$WORK/js.err" | tr '\n' ' ')"
	$PY -c "import sys; sys.exit(0 if $secs <= 30 else 1)" && pass "time budget (<= 30 s)" || fail "took $secs s (> 30)"
	if $CHECK "$WORK/js.ndjson" --registry "$REG" --language javascript --root "$REPO" > "$WORK/js.check" 2>&1; then pass "output is valid contract v1: $(cut -c1-120 "$WORK/js.check")"; else fail "contract: $(cat "$WORK/js.check")"; fi
	[ -z "$(git -C "$REPO" status --porcelain)" ] && pass "the repo is unchanged after the run" || fail "the adapter changed files in the repo"

	# --- 3. known answers (decision N + verified CommonJS cases) ---------------------------------------
	$PY - "$KA" "$WORK/js.ndjson" > "$WORK/ka.out" <<'EOF'
import json, sys
ka = json.load(open(sys.argv[1]))
sites = [json.loads(l) for l in open(sys.argv[2]) if l.strip()]
sites = [s for s in sites if s.get("type") == "site"]
for case in ka["cases"]:
    d = case["definition"]
    def hits(f, l):
        return [s for s in sites if s["file"] == f and s["line"] == l and any(t["file"] == d["file"] and t["line"] == d["line"] for t in s["targets"])]
    missing = [f"{c['file']}:{c['line']}" for c in case["callers"] if not hits(c["file"], c["line"])]
    wrong = [f"{c['file']}:{c['line']}" for c in case.get("not_callers", []) if hits(c["file"], c["line"])]
    ok = not missing and not wrong
    n = len(case["callers"])
    print(("PASS " if ok else "FAIL ") + f"known answer {case['id']}: {n - len(missing)}/{n} callers" + ("" if not wrong else f", {len(wrong)} wrong") + ("" if ok else f" missing {missing[:3]} wrong {wrong[:3]}"))
EOF
	while IFS= read -r line; do case "$line" in PASS*) pass "${line#PASS }" ;; FAIL*) fail "${line#FAIL }" ;; esac; done < "$WORK/ka.out"
fi

finish
