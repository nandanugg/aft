#!/usr/bin/env bash
# uninstall.sh --prefix <dir>
# Removes exactly the files listed in <dir>/share/aft-adapters/install-manifest.txt and the
# directories install.sh created (only when they are empty). Touches nothing outside <dir>.
set -eu

usage() {
	echo "usage: uninstall.sh --prefix <dir>" >&2
	exit 2
}

prefix=""
while [ $# -gt 0 ]; do
	case $1 in
		--prefix) [ $# -ge 2 ] || usage; prefix=$2; shift 2 ;;
		*) usage ;;
	esac
done
[ -n "$prefix" ] || usage
[ -d "$prefix" ] || { echo "uninstall: no such directory: $prefix" >&2; exit 1; }
prefix=$(cd "$prefix" && pwd)
manifest="$prefix/share/aft-adapters/install-manifest.txt"
[ -f "$manifest" ] || { echo "uninstall: no manifest at $manifest. Nothing was removed." >&2; exit 1; }

# Check every entry first: a manifest must not name anything outside the prefix.
while IFS=' ' read -r kind path; do
	case $path in
		"$prefix"/*) ;;
		*) echo "uninstall: the manifest names a path outside the prefix: $path. Nothing was removed." >&2; exit 1 ;;
	esac
	case $path in
		*/../*|*/..) echo "uninstall: the manifest names a path with '..': $path. Nothing was removed." >&2; exit 1 ;;
	esac
	case $kind in
		file | dir) ;;
		*) echo "uninstall: unknown manifest line: $kind $path. Nothing was removed." >&2; exit 1 ;;
	esac
done < "$manifest"

# Files first (the manifest is last in its own list), then directories, deepest first.
manifest_copy=$(mktemp "${TMPDIR:-/tmp}/aft-adapters-uninstall.XXXXXX")
trap 'rm -f "$manifest_copy"' EXIT
cp "$manifest" "$manifest_copy"
removed=0
while IFS=' ' read -r kind path; do
	if [ "$kind" = file ] && [ -f "$path" ]; then
		rm -f "$path"
		removed=$((removed + 1))
	fi
done < "$manifest_copy"
grep '^dir ' "$manifest_copy" | sed 's/^dir //' | awk '{ print length($0) "\t" $0 }' | sort -rn | cut -f2- |
	while IFS= read -r dir; do
		[ -d "$dir" ] || continue
		rmdir "$dir" 2> /dev/null || echo "uninstall: kept $dir (not empty)" >&2
	done
echo "removed $removed files from $prefix"
