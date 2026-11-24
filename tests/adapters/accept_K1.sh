#!/usr/bin/env bash
# accept_K1.sh — acceptance for K1 (the update-upstream skill and its rebase-check script). No cargo.
# Exit 0 only when every check holds. The replay by a FRESH agent (that reads only the skill) is a
# separate step of the coordinator; this script checks the skill's parts and the script's facts.
#
# Interface K1 provides (fixed in the K1 brief):
#   .agents/skills/update-upstream/SKILL.md
#   .agents/skills/update-upstream/rebase-check.sh --range <a>..<b> [--repo <dir>]
#       prints the facts a maintainer classifies: seam files touched (with line counts), the
#       CallGraphRead method list before/after, crate moves of seam files, and ONE line
#       "suggested: none|minor|major" (exactly this form; the maintainer decides).
set -u
. "$(dirname "$0")/lib/common.sh"
SK="$TREE/.agents/skills/update-upstream"
MD="$SK/SKILL.md"
RC="$SK/rebase-check.sh"

if [ ! -f "$MD" ]; then fail "SKILL.md missing at .agents/skills/update-upstream/"; finish; fi
# --- 1. the skill's sections -------------------------------------------------------------------
for w in What How When Where Who; do
	grep -qE "^#+ .*\b$w\b" "$MD" && pass "SKILL.md has a '$w' section" || fail "SKILL.md has no '$w' heading"
done
grep -qiE '^#+ .*(procedure|upgrade)' "$MD" && pass "SKILL.md has the upgrade procedure" || fail "no procedure section"
for s in none minor major; do grep -qiE "\|[[:space:]]*\**$s\**[[:space:]]*\|" "$MD" && pass "severity table row: $s" || fail "severity table has no '$s' row"; done
grep -qi 'adapter layer' "$MD" && pass "major is defined by a change in how the adapter layer talks to the core" || fail "no 'adapter layer' in the severity definition"
grep -q '795838760' "$MD" && grep -qiE '795838760.*major|major.*795838760' "$MD" && pass "replay case 795838760 -> major" || fail "replay case 795838760 -> major missing"
grep -qE '29 *(->|→|to) *31' "$MD" && grep -qiE 'minor' "$MD" && grep -q 'aft-parse' "$MD" && pass "replay case CallGraphRead 29->31 + crate move -> minor" || fail "replay case CallGraphRead 29->31 / crate move -> minor missing"
grep -qiE 'unrelated|no seam' "$MD" && grep -qiE '\bnone\b' "$MD" && pass "replay case unrelated range -> none" || fail "replay case 'unrelated range -> none' missing"
grep -qiE 'planned' "$MD" && grep -qiE 'consumer' "$MD" && pass "consumer install/update flow marked planned" || fail "consumer install/update flow not marked planned"
for who in "consumer project" "on-call" "maintainer" "adapter authors"; do grep -qi "$who" "$MD" && pass "Who: mentions $who" || fail "Who: no mention of $who"; done

# --- 2. the rebase-check script --------------------------------------------------------------------
if [ ! -x "$RC" ]; then fail "rebase-check.sh missing or not executable"; finish; fi
if grep -nE '(^|[^a-zA-Z_-])(jq|python3?|node|perl|ruby)([^a-zA-Z_-]|$)' "$RC" | grep -v '^[0-9]*:[[:space:]]*#' > "$WORK/deps.out"; then
	fail "thin: rebase-check.sh uses $(head -n 1 "$WORK/deps.out")"
else
	pass "thin: rebase-check.sh uses bash, POSIX tools and git only"
fi
"$RC" --help > /dev/null 2>&1 && pass "rebase-check.sh --help" || fail "rebase-check.sh --help fails"
before=$(git -C "$TREE" status --porcelain | LC_ALL=C sort)
run_rc() { "$RC" --repo "$TREE" --range "$1" > "$WORK/rc-$2.out" 2>&1; }
run_rc v0.66.0..v0.67.0 minor; rc=$?
[ $rc = 0 ] && grep -q CallGraphRead "$WORK/rc-minor.out" && grep -q aft-parse "$WORK/rc-minor.out" && grep -qx 'suggested: minor' "$WORK/rc-minor.out" \
	&& pass "v0.66.0..v0.67.0: reports CallGraphRead and the aft-parse crate move; suggested: minor" || fail "v0.66.0..v0.67.0: rc=$rc $(head -c 300 "$WORK/rc-minor.out")"
run_rc 795838760^..795838760 major; rc=$?
[ $rc = 0 ] && grep -q callgraph_store "$WORK/rc-major.out" && grep -qx 'suggested: major' "$WORK/rc-major.out" \
	&& pass "795838760: reports callgraph_store and suggests major" || fail "795838760: rc=$rc $(head -c 300 "$WORK/rc-major.out")"
docs_commit=$(git -C "$TREE" log --format=%H -n 300 v0.67.0 -- docs/ | while read -r c; do
	if [ -z "$(git -C "$TREE" show --name-only --format= "$c" | grep -v '^docs/')" ]; then echo "$c"; break; fi
done)
if [ -n "$docs_commit" ]; then
	run_rc "$docs_commit^..$docs_commit" none; rc=$?
	[ $rc = 0 ] && grep -qx 'suggested: none' "$WORK/rc-none.out" && pass "docs-only commit ${docs_commit:0:9}: suggests none" || fail "docs-only commit: rc=$rc $(head -c 300 "$WORK/rc-none.out")"
else
	fail "no docs-only commit found to test the 'none' class"
fi
after=$(git -C "$TREE" status --porcelain | LC_ALL=C sort)
[ "$before" = "$after" ] && pass "rebase-check.sh changes nothing in the tree" || fail "rebase-check.sh changed the tree"

finish
