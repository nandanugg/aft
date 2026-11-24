#!/usr/bin/env bash
# rebase-check.sh --range <a>..<b> [--repo <dir>]
#
# Read-only. Collects the facts a maintainer needs to classify an upstream aft range as
# none / minor / major (see SKILL.md, "Severity"). It prints facts, then ONE line
# "suggested: none|minor|major" and the reasons. The maintainer decides.
# Needs: bash, POSIX tools, git. It never writes to the repository.
set -u
GIT=/usr/bin/git
[ -x "$GIT" ] || GIT=git

usage() {
	echo "usage: rebase-check.sh --range <a>..<b> [--repo <dir>]"
	echo "  <a>..<b>  the upstream range to check: tags, branches or commits (a = what we have, b = the target)"
	echo "  --repo    the aft repository (default: the current directory)"
}

RANGE=""
REPO="."
while [ $# -gt 0 ]; do
	case "$1" in
	--range) RANGE=${2:-}; shift 2 ;;
	--repo) REPO=${2:-}; shift 2 ;;
	-h | --help) usage; exit 0 ;;
	*) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
	esac
done
case "$RANGE" in
*..*) ;;
*) echo "error: --range must look like <a>..<b>" >&2; usage >&2; exit 2 ;;
esac
A=${RANGE%%..*}
B=${RANGE##*..}
G() { "$GIT" -C "$REPO" "$@"; }
G rev-parse --git-dir > /dev/null 2>&1 || { echo "error: $REPO is not a git repository" >&2; exit 2; }
G rev-parse --verify --quiet "$A^{commit}" > /dev/null || { echo "error: '$A' is not a commit here (fetch upstream tags first)" >&2; exit 2; }
G rev-parse --verify --quiet "$B^{commit}" > /dev/null || { echo "error: '$B' is not a commit here (fetch upstream tags first)" >&2; exit 2; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/rebase-check.XXXXXX") || exit 2
trap 'rm -rf "$WORK"' EXIT

# The seams: every upstream file our adapter layer touches or reads.
# One entry per line: "<path>" or "<path> <older path of the same file>".
SEAMS='crates/aft/src/commands/data_callgraph.rs
crates/aft/src/commands/mod.rs
crates/aft/src/commands/parent_query/mod.rs
crates/aft/src/commands/callers.rs
crates/aft/src/commands/call_tree.rs
crates/aft/src/commands/impact.rs
crates/aft/src/commands/trace_to.rs
crates/aft/src/commands/trace_to_symbol.rs
crates/aft/src/commands/trace_data.rs
crates/aft/src/main.rs
crates/aft-parse/src/callgraph_store/mod.rs crates/aft/src/callgraph_store/mod.rs
crates/aft/src/data_layer/query.rs
crates/aft/src/data_layer/slice.rs
crates/aft/src/subc_format.rs
crates/aft-base/src/protocol.rs
crates/aft-base/src/error.rs'
TRAIT_NEW=crates/aft-parse/src/callgraph_store/mod.rs
TRAIT_OLD=crates/aft/src/callgraph_store/mod.rs
COMMANDS='crates/aft/src/commands/callers.rs crates/aft/src/commands/call_tree.rs crates/aft/src/commands/impact.rs crates/aft/src/commands/trace_to.rs crates/aft/src/commands/trace_to_symbol.rs crates/aft/src/commands/trace_data.rs'
# Crates our Rust code uses that must stay in upstream's Cargo.lock (thin rule: no Cargo change).
NEEDED_CRATES='rusqlite sha2 libc serde serde_json tempfile'   # the same list as the seam table in SKILL.md

REASONS_MAJOR="$WORK/major"
REASONS_MINOR="$WORK/minor"
: > "$REASONS_MAJOR"
: > "$REASONS_MINOR"
major() { echo "$*" >> "$REASONS_MAJOR"; }
minor() { echo "$*" >> "$REASONS_MINOR"; }

# show <rev> <path>: file text at a revision, empty when the file does not exist there.
show() { G show "$1:$2" 2> /dev/null; }
# count_in <rev> <path> <ERE>: number of matching lines of the file at the revision.
count_in() { show "$1" "$2" | /usr/bin/grep -cE "$3"; }
# trait_methods <rev>: sorted fn names of `trait CallGraphRead`, from whichever crate holds it.
trait_methods() {
	local p
	for p in "$TRAIT_NEW" "$TRAIT_OLD"; do
		if show "$1" "$p" | /usr/bin/grep -qE 'pub trait CallGraphRead'; then
			show "$1" "$p" | awk '/pub trait CallGraphRead/{f=1} f{print} f&&/^}/{exit}' \
				| sed -n 's/^    fn \([a-z_0-9]*\).*/\1/p' | LC_ALL=C sort
			return 0
		fi
	done
	return 0
}
trait_path() {
	local p
	for p in "$TRAIT_NEW" "$TRAIT_OLD"; do
		if show "$1" "$p" | /usr/bin/grep -qE 'pub trait CallGraphRead'; then echo "$p"; return 0; fi
	done
	echo "(not found)"
}

echo "== range: $A..$B"
echo "a = $(G log -1 --format='%h %ad %s' --date=short "$A")"
echo "b = $(G log -1 --format='%h %ad %s' --date=short "$B")"
echo "commits in range: $(G rev-list --count "$A..$B")"

# --- 1. seam files touched ---------------------------------------------------------------------
echo
echo "== 1. seam files touched (added+deleted lines; -M follows moves)"
TOUCHED=0
CHURN_TRAIT=0
while IFS= read -r line; do
	[ -n "$line" ] || continue
	# shellcheck disable=SC2086
	set -- $line
	NEWP=$1
	STAT=$(G diff -M --numstat "$A" "$B" -- "$@" | awk '{a+=$1; d+=$2} END{if (NR>0) print a+0, d+0}')
	if [ -z "$STAT" ]; then
		printf '  %-6s %s\n' "same" "$NEWP"
		continue
	fi
	ADDED=${STAT% *}
	DELETED=${STAT#* }
	if [ "$#" -gt 1 ]; then
		printf '  %-6s %s  +%s -%s  (older path: %s; give both paths to git diff)\n' "TOUCH" "$NEWP" "$ADDED" "$DELETED" "$2"
	else
		printf '  %-6s %s  +%s -%s\n' "TOUCH" "$NEWP" "$ADDED" "$DELETED"
	fi
	TOUCHED=$((TOUCHED + 1))
	if [ "$NEWP" = "$TRAIT_NEW" ]; then CHURN_TRAIT=$((ADDED + DELETED)); fi
	# a seam file that exists at a and not at b (and is not a move) is gone
	for p in "$@"; do
		if [ -n "$(show "$A" "$p")" ] && [ -z "$(show "$B" "$p")" ]; then
			if [ "$p" = "$NEWP" ] || [ -z "$(show "$B" "$NEWP")" ]; then
				major "seam file deleted: $p (not found at $B; look for where it went)"
			fi
		fi
	done
done <<SEAMS_END
$SEAMS
SEAMS_END
SEAM_COUNT=$(printf '%s\n' "$SEAMS" | /usr/bin/grep -c .)
echo "  seam files touched: $TOUCHED of $SEAM_COUNT"

# --- 2. CallGraphRead -----------------------------------------------------------------------------
echo
echo "== 2. CallGraphRead (the reader trait our AdapterGraph implements; all methods must be reviewed)"
trait_methods "$A" > "$WORK/m.a"
trait_methods "$B" > "$WORK/m.b"
COUNT_A=$(wc -l < "$WORK/m.a" | tr -d ' ')
COUNT_B=$(wc -l < "$WORK/m.b" | tr -d ' ')
echo "  at a: $COUNT_A methods in $(trait_path "$A")"
echo "  at b: $COUNT_B methods in $(trait_path "$B")"
LC_ALL=C comm -13 "$WORK/m.a" "$WORK/m.b" > "$WORK/m.added"
LC_ALL=C comm -23 "$WORK/m.a" "$WORK/m.b" > "$WORK/m.removed"
echo "  added:   $(tr '\n' ' ' < "$WORK/m.added")"
echo "  removed: $(tr '\n' ' ' < "$WORK/m.removed")"
echo "  (a method removed and one added with a similar name is a rename: check both lists)"
if [ "$COUNT_A" -gt 0 ] && [ "$COUNT_B" -eq 0 ]; then
	major "trait CallGraphRead is not found at $B (reader replaced or renamed)"
fi
REMOVED=$(wc -l < "$WORK/m.removed" | tr -d ' ')
ADDED_M=$(wc -l < "$WORK/m.added" | tr -d ' ')
if [ "$REMOVED" -ge 5 ]; then major "$REMOVED CallGraphRead methods removed"; fi
if [ "$REMOVED" -gt 0 ] && [ "$REMOVED" -lt 5 ]; then minor "CallGraphRead methods removed or renamed: $(tr '\n' ' ' < "$WORK/m.removed")"; fi
if [ "$ADDED_M" -gt 0 ]; then minor "CallGraphRead methods added ($COUNT_A -> $COUNT_B): the guard test must list them"; fi
# 1) the trait body itself: how many lines of `pub trait CallGraphRead { ... }` differ
trait_body() {
	local p
	for p in "$TRAIT_NEW" "$TRAIT_OLD"; do
		if show "$1" "$p" | /usr/bin/grep -qE 'pub trait CallGraphRead'; then
			show "$1" "$p" | awk '/pub trait CallGraphRead/{f=1} f{print} f&&/^}/{exit}'
			return 0
		fi
	done
	return 0
}
trait_body "$A" > "$WORK/body.a"
trait_body "$B" > "$WORK/body.b"
BODY_DIFF=$(diff "$WORK/body.a" "$WORK/body.b" | /usr/bin/grep -cE '^[<>]')
echo "  trait body lines that differ: $BODY_DIFF of $(wc -l < "$WORK/body.a" | tr -d ' ')"
# An added method WITHOUT a default body does not compile until AdapterGraph implements it. One WITH a
# default body compiles, and AdapterGraph then silently uses the default: decide overlay or delegate anyway.
while IFS= read -r m; do
	[ -n "$m" ] || continue
	kind=$(awk -v name="$m" '
		$0 ~ "fn " name "[(<]" { f = 1 }
		f { for (i = 1; i <= length($0); i++) { c = substr($0, i, 1)
			if (c == ";") { print "required (no default body)"; exit }
			if (c == "{") { print "HAS A DEFAULT BODY: AdapterGraph would use it silently"; exit } } }' "$WORK/body.b")
	echo "  added method $m: ${kind:-(body not found)}"
done < "$WORK/m.added"
if [ "$BODY_DIFF" -ge 40 ]; then major "the CallGraphRead trait body changed by $BODY_DIFF lines (a mechanism change, not only new methods)"; fi
# 2) the file that holds the trait, when it stays at the same path: a rewrite means the store behind the reader changed
for p in "$TRAIT_NEW" "$TRAIT_OLD"; do
	if show "$A" "$p" | /usr/bin/grep -qE 'pub trait CallGraphRead' && show "$B" "$p" | /usr/bin/grep -qE 'pub trait CallGraphRead'; then
		CH=$(G diff --numstat "$A:$p" "$B:$p" | awk '{print $1+$2}')
		echo "  $p keeps the trait; changed lines at that path: ${CH:-0}"
		if [ "${CH:-0}" -ge 3000 ]; then
			major "$p was rewritten (${CH} lines) and still holds the trait: the store behind the reader was probably replaced (compare commit 795838760)"
		fi
	fi
done
# 3) storage modules removed: files deleted (not moved) under the call-graph store or the data layer
DELETED_STORE=$(G diff -M --name-status "$A" "$B" | awk '$1=="D"{print $2}' | /usr/bin/grep -E '^crates/[^/]+/src/(callgraph_store|data_layer)/' | wc -l | tr -d ' ')
echo "  files deleted under callgraph_store/ or data_layer/: $DELETED_STORE"
if [ "$DELETED_STORE" -ge 5 ]; then major "$DELETED_STORE files deleted under callgraph_store/ or data_layer/: a storage or reader mechanism was removed"; fi
if show "$A" "$TRAIT_OLD" | /usr/bin/grep -qE 'pub trait CallGraphRead' && show "$B" "$TRAIT_NEW" | /usr/bin/grep -qE 'pub trait CallGraphRead'; then
	minor "CallGraphRead moved from crates/aft to crates/aft-parse (the trait path the guard test reads and the imports must follow)"
fi

# --- 3. moves of seam files ---------------------------------------------------------------------------
echo
echo "== 3. moves and renames of seam files (git diff -M)"
G diff -M --name-status "$A" "$B" | awk '$1 ~ /^R/ {print}' > "$WORK/renames"
: > "$WORK/seam-moves"
printf '%s\n' "$SEAMS" | tr ' ' '\n' | /usr/bin/grep -v '^$' > "$WORK/seam-paths"
while IFS= read -r line; do
	[ -n "$line" ] || continue
	# shellcheck disable=SC2086
	set -- $line
	if /usr/bin/grep -qxF -e "$2" -e "$3" "$WORK/seam-paths"; then
		echo "  $1 $2 -> $3" | tee -a "$WORK/seam-moves"
	fi
done < "$WORK/renames"
if [ -s "$WORK/seam-moves" ]; then
	minor "seam files moved (section 3): update the paths in SKILL.md, this script and the guard test"
else
	echo "  none"
fi
echo "  renames of any file in the range: $(awk 'END{print NR+0}' "$WORK/renames")"

# --- 4. the named seam symbols ---------------------------------------------------------------------------
echo
echo "== 4. seam symbols: matching lines at a and at b, and +/- lines in the range"
echo "  (a=0 means the file or the symbol did not exist at a yet, for example the help guard before v0.67.0)"
sym() { # sym <label> <ERE> <paths...>
	local label=$1 re=$2 na=0 nb=0 p c diff pl mi
	shift 2
	for p in "$@"; do
		c=$(count_in "$A" "$p" "$re"); na=$((na + c))
		c=$(count_in "$B" "$p" "$re"); nb=$((nb + c))
	done
	diff=$(G diff -M -U0 "$A" "$B" -- "$@" 2> /dev/null)
	pl=$(printf '%s\n' "$diff" | /usr/bin/grep -E '^\+[^+]' | /usr/bin/grep -cE "$re")
	mi=$(printf '%s\n' "$diff" | /usr/bin/grep -E '^-[^-]' | /usr/bin/grep -cE "$re")
	printf '  %-36s a=%-4s b=%-4s +%s -%s\n' "$label" "$na" "$nb" "$pl" "$mi"
	if [ "$na" -gt 0 ] && [ "$nb" -eq 0 ]; then
		major "$label: present at $A, gone at $B (the adapter layer talks to the core through it)"
	elif [ "$pl" -gt 0 ] || [ "$mi" -gt 0 ]; then
		minor "$label: lines changed (+$pl -$mi); read: git diff -M -U3 $A $B -- <file>"
	fi
}
# shellcheck disable=SC2086
{
	sym "OpsGraph / ops_graph / Access" '\bOpsGraph\b|\bops_graph\b|\benum Access\b' crates/aft/src/commands/data_callgraph.rs crates/aft/src/commands/parent_query/mod.rs
	sym "reply path in the 6 commands" '\bops_graph\(|\bfinish_reply\(|\bserialized_response' $COMMANDS
	sym "SliceReader/SliceSource/FileSlice" '\bSliceReader\b|\bSliceSource\b|\bFileSlice\b' crates/aft/src/data_layer/query.rs crates/aft/src/data_layer/slice.rs
	sym "NodeView/RefView" '\bNodeView\b|\bRefView\b' crates/aft/src/data_layer/slice.rs
	sym "subc_format::format_callgraph" '\bformat_callgraph\b' crates/aft/src/subc_format.rs
	sym "protocol Response/RawRequest" '\bpub struct (Response|RawRequest)\b' crates/aft-base/src/protocol.rs
	sym "main.rs help guard" 'help::handle_first_arg' crates/aft/src/main.rs
	sym "commands/mod.rs data_callgraph" '\bmod data_callgraph\b' crates/aft/src/commands/mod.rs
}
# aft cli parses upstream's ambiguous-name message text (parse_ambiguity in lang_adapters/cli.rs):
# "symbol 'X' is ambiguous, candidates: [A, B]". A text change breaks only the one-shot retry: minor.
amb_a=$(count_in "$A" crates/aft-base/src/error.rs 'is ambiguous, candidates: \[')
amb_b=$(count_in "$B" crates/aft-base/src/error.rs 'is ambiguous, candidates: \[')
if [ "$amb_a" -gt 0 ] && [ "$amb_b" -eq 0 ]; then
	minor "ambiguous_symbol message text changed in crates/aft-base/src/error.rs: update parse_ambiguity in lang_adapters/cli.rs"
fi
echo "  help guard line in main.rs: a=$(show "$A" crates/aft/src/main.rs | /usr/bin/grep -nE 'help::handle_first_arg' | head -n 1 | cut -d: -f1) b=$(show "$B" crates/aft/src/main.rs | /usr/bin/grep -nE 'help::handle_first_arg' | head -n 1 | cut -d: -f1)  (our routing must stay before it)"

# --- 4b. runtime seams --------------------------------------------------------------------------------
# Words that aft cli and finish_reply send or read at run time. A change here still compiles, but breaks
# behaviour. Our own module is left out of the count, so a fork commit as <a> does not hide a removal.
echo
echo "== 4b. runtime seams: request words, reply words and texts our layer relies on (outside our module)"
OUR_MODULE=':(exclude)crates/aft/src/commands/data_callgraph/lang_adapters'
for lit in '"configure"' '"project_root"' '"harness"' '"includeTests"' '"toSymbol"' '"toFile"' '"expression"' \
	'"start_line"' '"semantic_search"' '"ambiguous_symbol"' 'is ambiguous, candidates: [' '"text"'; do
	na=$(G grep -F -c -e "$lit" "$A" -- crates "$OUR_MODULE" 2> /dev/null | awk -F: '{s += $NF} END {print s + 0}')
	nb=$(G grep -F -c -e "$lit" "$B" -- crates "$OUR_MODULE" 2> /dev/null | awk -F: '{s += $NF} END {print s + 0}')
	printf '  %-36s a=%-4s b=%-4s\n' "$lit" "$na" "$nb"
	if [ "$na" -gt 0 ] && [ "$nb" -eq 0 ]; then
		minor "runtime seam $lit: present at $A, gone at $B (aft cli or finish_reply uses it; check lang_adapters/cli.rs)"
	fi
done
echo "  (the outline text that cli.rs parses — 'type X interface', 'func (r *T) name(' — is checked by accept_W1 on a real case)"

# Semantic search install: install.sh pins an ONNX Runtime version that upstream's ort_pin.rs must accept and the
# model upstream loads; the shim sets ORT_DYLIB_PATH and FASTEMBED_CACHE_DIR and passes `warmup` to the binary.
echo "  semantic search install (scripts/adapters/install.sh and the shim):"
for lit in 'ORT_DYLIB_PATH' 'FASTEMBED_CACHE_DIR' 'all-MiniLM-L6-v2' '"warmup"'; do
	na=$(G grep -F -c -e "$lit" "$A" -- crates 2> /dev/null | awk -F: '{s += $NF} END {print s + 0}')
	nb=$(G grep -F -c -e "$lit" "$B" -- crates 2> /dev/null | awk -F: '{s += $NF} END {print s + 0}')
	printf '  %-36s a=%-4s b=%-4s\n' "$lit" "$na" "$nb"
	if [ "$na" -gt 0 ] && [ "$nb" -eq 0 ]; then
		minor "semantic seam $lit: present at $A, gone at $B (install.sh or the shim uses it; accept_W1 shows the break)"
	fi
done
OURS_ORT=$(sed -n 's/^ort_version=//p' "$REPO/scripts/adapters/install.sh" 2> /dev/null)
if [ -n "$OURS_ORT" ]; then
	pinned=$(G grep -F -c -e "version: \"$OURS_ORT\"" "$B" -- 'crates/**/ort_pin.rs' 2> /dev/null | awk -F: '{s += $NF} END {print s + 0}')
	echo "  install.sh ONNX Runtime $OURS_ORT: pinned by upstream's ort_pin.rs at b: $pinned time(s)"
	[ "$pinned" -eq 0 ] && minor "install.sh pins ONNX Runtime $OURS_ORT, upstream's ort_pin.rs at $B does not accept it: update ort_version and its sha256 in install.sh"
fi

# --- 5. Cargo.lock -------------------------------------------------------------------------------------------
echo
echo "== 5. Cargo.lock crates (name version)"
crates_at() { show "$1" Cargo.lock | awk '/^name = /{gsub(/"/,"",$3); n=$3} /^version = /{gsub(/"/,"",$3); if (n!="") print n, $3; n=""}' | LC_ALL=C sort; }
crates_at "$A" > "$WORK/c.a"
crates_at "$B" > "$WORK/c.b"
LC_ALL=C comm -23 "$WORK/c.a" "$WORK/c.b" > "$WORK/c.removed"
LC_ALL=C comm -13 "$WORK/c.a" "$WORK/c.b" > "$WORK/c.added"
echo "  removed or changed-from: $(awk 'END{print NR+0}' "$WORK/c.removed")   added or changed-to: $(awk 'END{print NR+0}' "$WORK/c.added")"
head -n 20 "$WORK/c.removed" | sed 's/^/    - /'
head -n 20 "$WORK/c.added" | sed 's/^/    + /'
for c in $NEEDED_CRATES; do
	if [ -s "$WORK/c.b" ] && ! awk -v c="$c" '$1==c{f=1} END{exit !f}' "$WORK/c.b"; then
		minor "crate '$c' (used by our adapter module) is no longer in Cargo.lock: our code needs a Cargo change, which breaks the thin rule"
	fi
done

# --- 6. hot files we do not touch ------------------------------------------------------------------------------
echo
echo "== 6. hot files we do NOT touch (context only: a big change here can still change behaviour)"
for p in crates/aft/src/data_layer/callgraph_store_adapter.rs crates/aft/src/lib.rs; do
	printf '  %-60s %s commits\n' "$p" "$(G rev-list --count "$A..$B" -- "$p")"
done

# --- 7. suggestion ---------------------------------------------------------------------------------------------
echo
echo "== 7. suggestion (a hint from simple rules; the maintainer decides)"
SUGGEST=none
if [ "$TOUCHED" -gt 0 ] || [ -s "$REASONS_MINOR" ]; then SUGGEST=minor; fi
if [ -s "$REASONS_MAJOR" ]; then SUGGEST=major; fi
echo "suggested: $SUGGEST"
if [ "$SUGGEST" = none ]; then
	echo "reason: no seam file is touched in this range"
else
	[ -s "$REASONS_MAJOR" ] && sed 's/^/reason (major): /' "$REASONS_MAJOR"
	[ -s "$REASONS_MINOR" ] && sed 's/^/reason (minor): /' "$REASONS_MINOR"
	if [ ! -s "$REASONS_MAJOR" ] && [ ! -s "$REASONS_MINOR" ]; then echo "reason: seam files are touched (section 1); read the diff"; fi
fi
exit 0
