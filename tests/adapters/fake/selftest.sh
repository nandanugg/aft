#!/usr/bin/env bash
# Checks every fake against the contract checker: the valid ones pass, each broken one fails for its reason.
set -u
dir=$(cd "$(dirname "$0")" && pwd)
check="${PYTHON:-python3} -I $dir/../lib/contract_check.py"
root="$dir/../fixtures/tiny-mixed"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
"$dir/make_registry.sh" ok.sh ok.sh > "$tmp/registry.json"
fail=0
expect() { # <fake> <language> <expected substring of checker output, or "ok"> [expected exit code of the fake]
	local fake=$1 lang=$2 want=$3 want_rc=${4:-0}
	"$dir/$fake" --language "$lang" --root "$root" > "$tmp/out" 2> "$tmp/err"; rc=$?
	got=$($check "$tmp/out" --registry "$tmp/registry.json" --language "$lang" --root "$root" 2>&1)
	if [ "$rc" != "$want_rc" ] || ! printf '%s' "$got" | grep -q -- "$want"; then
		echo "FAIL $fake ($lang): rc=$rc (want $want_rc) checker: $got"; fail=1
	else
		echo "ok   $fake ($lang): rc=$rc, $(printf '%s' "$got" | head -c 110)"
	fi
}
expect ok.sh go '^ok '
expect ok.sh javascript '^ok '
expect recorder.sh go '^ok '
expect partial.sh go '"partial": true'
expect no_end.sh go 'no end record'
expect partial_line.sh go 'last line is incomplete'
expect wrong_contract.sh go 'header contract 2'
expect undeclared_fact.sh go 'not declared'
expect count_mismatch.sh go 'do not match'
expect exit_nonzero.sh go 'no end record' 3
expect crash.sh go 'no end record' 134
FAKE_SLEEP_S=0 expect slow_ok.sh go '^ok '
exit $fail
