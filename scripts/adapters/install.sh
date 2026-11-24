#!/usr/bin/env bash
# install.sh --prefix <dir> (--release-url <url> | --binary <file>) [--without-semantic] [--guide-local <file>]
#
# Installs aft with its language adapters under <dir>. Writes only inside <dir>.
#   <dir>/libexec/aft-adapters/aft                      the aft binary
#   <dir>/libexec/aft-adapters/adapters/...             the adapters that exist in adapters/
#   <dir>/libexec/aft-adapters/adapters-registry.json   the registry, with absolute paths
#   <dir>/libexec/aft-adapters/guide.local.md       only with --guide-local: the consumer's addendum, appended by `aft guide`
#   <dir>/libexec/aft-adapters/onnxruntime/<version>/libonnxruntime.{dylib,so}   ONNX Runtime (semantic search)
#   <dir>/libexec/aft-adapters/models/models--Qdrant--all-MiniLM-L6-v2-onnx/snapshots/<rev>/{model.onnx,tokenizer.json}
#   <dir>/bin/aft                                   the shim (runs `aft cli`)
#   <dir>/share/aft-adapters/install-manifest.txt       every file and directory this script made
# Platforms (uname -s / uname -m): darwin-arm64, darwin-x64, linux-x64 (x86_64, amd64), linux-arm64 (aarch64, arm64).
# --release-url: <url>/aft-<platform> and <url>/checksums.sha256 are fetched (https:// or file://) and the
#   binary is checked with its sha256 before anything is installed.
# --binary: use a local aft binary (no checksum).
# Semantic search (`aft search`, `aft similar`) needs ONNX Runtime and the embedding model. They are
#   installed by default, inside <dir>, so uninstall.sh removes them. The shim points aft to them
#   (ORT_DYLIB_PATH, FASTEMBED_CACHE_DIR). --without-semantic skips them: aft then ranks by words only.
#   Downloads (https only, each checked with a pinned sha256; nothing else uses the network):
#     ONNX Runtime 1.24.4: Microsoft's NuGet package, about 125 MB; only the library of this platform
#       (about 35 MB) is kept. Upstream aft pins the same libraries (crates/aft-base/src/ort_pin.rs).
#       There is no library for darwin-x64 (Intel Mac): use --without-semantic there.
#     Model all-MiniLM-L6-v2: huggingface.co/Qdrant/all-MiniLM-L6-v2-onnx at one fixed revision,
#       model.onnx (86 MB) and tokenizer.json (0.7 MB).
# --guide-local <file>: copy the file to <dir>/libexec/aft-adapters/guide.local.md, next to the registry.
#   `aft guide` appends it to the guide. Without the option, the guide has no addendum.
# AFT_INSTALL_CACHE=<dir> (optional): the pinned downloads are read from <dir> when their sha256 matches, and
#   stored there after a download. Without it, nothing is written outside <dir>.
# Go adapter: built with the caller's Go (Go environment as it is, GOFLAGS=-mod=readonly). Go is needed
#   only to build it. Its modules come from GOPROXY and are checked by go.sum.
# Needs: bash 3.2 or newer, POSIX tools, curl or wget, sha256sum or shasum. Semantic search also needs
#   unzip, bsdtar or python3 (to read the ONNX Runtime package).
# Runs from bash, zsh and fish (a bash script with a shebang). It writes no shell rc file.
set -eu

usage() {
	echo "usage: install.sh --prefix <dir> (--release-url <url> | --binary <file>) [--without-semantic] [--guide-local <file>]" >&2
	exit 2
}
die() {
	echo "install: $*" >&2
	exit 1
}

prefix=""
release_url=""
binary=""
semantic=yes
guide_local=""
while [ $# -gt 0 ]; do
	case $1 in
		--prefix) [ $# -ge 2 ] || usage; prefix=$2; shift 2 ;;
		--release-url) [ $# -ge 2 ] || usage; release_url=$2; shift 2 ;;
		--binary) [ $# -ge 2 ] || usage; binary=$2; shift 2 ;;
		--without-semantic) semantic=no; shift ;;
		--guide-local) [ $# -ge 2 ] || usage; guide_local=$2; shift 2 ;;
		*) usage ;;
	esac
done
[ -n "$prefix" ] || usage
if [ -n "$release_url" ] && [ -n "$binary" ]; then usage; fi
if [ -z "$release_url" ] && [ -z "$binary" ]; then usage; fi

mkdir -p "$prefix"
prefix=$(cd "$prefix" && pwd)
# The registry text is written with sed and read as JSON: keep the path plain.
case $prefix in
	*[!A-Za-z0-9._/+@=,\ -]*) die "the prefix may contain only letters, digits and . _ / + @ = , - and space: $prefix" ;;
esac

src_root=$(cd "$(dirname "$0")/../.." && pwd)
libexec="$prefix/libexec/aft-adapters"
manifest="$prefix/share/aft-adapters/install-manifest.txt"

# --- platform: asset name, ONNX Runtime library of this platform -------------------------------------
case "$(uname -s)-$(uname -m)" in
	Darwin-arm64) platform=darwin-arm64 ;;
	Darwin-x86_64) platform=darwin-x64 ;;
	Linux-x86_64 | Linux-amd64) platform=linux-x64 ;;
	Linux-aarch64 | Linux-arm64) platform=linux-arm64 ;;
	*) die "unsupported system $(uname -s) $(uname -m). Supported: darwin-arm64, darwin-x64, linux-x64, linux-arm64." ;;
esac
asset=aft-$platform

# Pinned downloads for semantic search. Change a version only together with its sha256 values.
ort_version=1.24.4
ort_url="https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/$ort_version/microsoft.ml.onnxruntime.$ort_version.nupkg"
ort_nupkg_sha256=4b978d5065b85e7004b6c6f60ca494bd978fbe6836cbf0a0b52d82b61ab99638
# Library of the platform in the package, its file name, and its sha256 (the same values as ort_pin.rs).
case $platform in
	darwin-arm64)
		ort_member=runtimes/osx-arm64/native/libonnxruntime.dylib
		ort_lib_sha256=0db33e160224e480c17932799d1dfaea59853b8e722b57bf61932c87e03586f0 ;;
	linux-x64)
		ort_member=runtimes/linux-x64/native/libonnxruntime.so
		ort_lib_sha256=d132535d051344ff5c64c9c200004150559049a81ed330eb4422c1962fb6b7e4 ;;
	linux-arm64)
		ort_member=runtimes/linux-arm64/native/libonnxruntime.so
		ort_lib_sha256=52b2a0e75e79468404284fec38ad5ee1a7a996232274f5e1b84f3e793fb07554 ;;
	*)
		ort_member=""
		ort_lib_sha256="" ;;
esac
ort_lib_name=$(basename "${ort_member:-libonnxruntime.dylib}")
model_repo=Qdrant/all-MiniLM-L6-v2-onnx
model_rev=d13954661f83248295ba75c1ed411eef3b7b936e
model_sha256=bbd7b466f6d58e646fdc2bd5fd67b2f5e93c0b687011bd4548c420f7bd46f0c5
tokenizer_sha256=851ca67100d372ca3ae031a6abd168f53489eebfd7d89523f35c5c9b4d372c3c
model_dir_name=models--Qdrant--all-MiniLM-L6-v2-onnx

stage=$(mktemp -d "${TMPDIR:-/tmp}/aft-adapters-install.XXXXXX")
trap 'rm -rf "$stage"' EXIT

# --- tools: each has a fallback; a missing tool gives one error that names the package. -------------
sha256_of() { # sha256_of <file>: prints the hash
	if command -v sha256sum > /dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	elif command -v shasum > /dev/null 2>&1; then
		shasum -a 256 "$1" | cut -d' ' -f1
	else
		die "sha256sum or shasum is needed. Install coreutils (apt: coreutils, pacman: coreutils, brew: coreutils)."
	fi
}
download() { # download <url> <file>: https:// (curl, else wget) or file:// (copy)
	case $1 in
		file://*) cp "${1#file://}" "$2" 2> /dev/null || return 1 ;;
		https://*)
			# Up to 3 tries: one dropped or refused connection must not stop a long install.
			local try
			for try in 1 2 3; do
				if command -v curl > /dev/null 2>&1; then
					curl -fsSL --proto '=https' --tlsv1.2 "$1" -o "$2" && return 0
				elif command -v wget > /dev/null 2>&1; then
					wget -q --https-only -O "$2" "$1" && return 0
				else
					die "curl or wget is needed (apt: curl, pacman: curl, brew: curl)."
				fi
				[ "$try" = 3 ] || { echo "download failed (try $try of 3); next try in 3 s: $1" >&2; sleep 3; }
			done
			return 1 ;;
		*) die "only https:// and file:// URLs are allowed: $1" ;;
	esac
}
unzip_member() { # unzip_member <zip> <member> <dir>: the member is extracted to <dir>/<member>
	if command -v unzip > /dev/null 2>&1; then
		unzip -q -o "$1" "$2" -d "$3"
	elif command -v bsdtar > /dev/null 2>&1; then
		mkdir -p "$3" && bsdtar -xf "$1" -C "$3" "$2"
	elif command -v python3 > /dev/null 2>&1; then # thin-ok
		python3 -m zipfile -e "$1" "$3" # thin-ok: the last fallback to read a zip file
	else
		die "unzip, bsdtar or python3 is needed to read ONNX Runtime (apt: unzip, pacman: unzip, brew: unzip). Or use --without-semantic." # thin-ok
	fi
}
cache_dir=${AFT_INSTALL_CACHE:-}
fetch_checked() { # fetch_checked <url> <file> <sha256>: https only; a cache hit needs no network; fail on a bad checksum
	if [ -n "$cache_dir" ] && [ -f "$cache_dir/$3" ] && [ "$(sha256_of "$cache_dir/$3")" = "$3" ]; then
		cp "$cache_dir/$3" "$2"
		return 0
	fi
	download "$1" "$2" || die "cannot download $1"
	[ "$(sha256_of "$2")" = "$3" ] || die "checksum mismatch for $1. Nothing was installed."
	if [ -n "$cache_dir" ]; then mkdir -p "$cache_dir" && cp "$2" "$cache_dir/$3"; fi
}

# --- 1. get the binary and check it. Nothing is installed yet. -----------------------------------
if [ -n "$release_url" ]; then
	download "$release_url/$asset" "$stage/$asset" || die "cannot download $release_url/$asset (this release may have no binary for $platform; use --binary)"
	download "$release_url/checksums.sha256" "$stage/checksums.sha256" || die "cannot download $release_url/checksums.sha256"
	expected=$(grep -E "[ *]$asset\$" "$stage/checksums.sha256" | head -n 1 | cut -d' ' -f1)
	[ -n "$expected" ] || die "checksums.sha256 has no line for $asset"
	[ "$(sha256_of "$stage/$asset")" = "$expected" ] || die "checksum mismatch for $asset. Nothing was installed."
	binary=$stage/$asset
else
	[ -f "$binary" ] || die "no such file: $binary"
fi
[ -z "$guide_local" ] || [ -f "$guide_local" ] || die "no such file: $guide_local"

# --- 1b. get ONNX Runtime and the model, and check them. Nothing is installed yet. ----------------
if [ "$semantic" = yes ]; then
	[ -n "$ort_member" ] || die "no ONNX Runtime library exists for $platform. Use --without-semantic."
	mkdir -p "$stage/semantic/ort" "$stage/semantic/model"
	if [ -n "$cache_dir" ] && [ -f "$cache_dir/$ort_lib_sha256" ] && [ "$(sha256_of "$cache_dir/$ort_lib_sha256")" = "$ort_lib_sha256" ]; then
		cp "$cache_dir/$ort_lib_sha256" "$stage/semantic/ort/$ort_lib_name"
	else
		echo "downloading ONNX Runtime $ort_version (about 125 MB) and the embedding model (about 87 MB)..."
		fetch_checked "$ort_url" "$stage/semantic/ort.nupkg" "$ort_nupkg_sha256"
		unzip_member "$stage/semantic/ort.nupkg" "$ort_member" "$stage/semantic/ort-unpacked" || die "cannot unpack ONNX Runtime"
		rm -f "$stage/semantic/ort.nupkg"
		cp "$stage/semantic/ort-unpacked/$ort_member" "$stage/semantic/ort/$ort_lib_name"
		rm -rf "$stage/semantic/ort-unpacked"
		[ "$(sha256_of "$stage/semantic/ort/$ort_lib_name")" = "$ort_lib_sha256" ] || die "checksum mismatch for the ONNX Runtime library. Nothing was installed."
		if [ -n "$cache_dir" ]; then mkdir -p "$cache_dir" && cp "$stage/semantic/ort/$ort_lib_name" "$cache_dir/$ort_lib_sha256"; fi
	fi
	fetch_checked "https://huggingface.co/$model_repo/resolve/$model_rev/model.onnx" "$stage/semantic/model/model.onnx" "$model_sha256"
	fetch_checked "https://huggingface.co/$model_repo/resolve/$model_rev/tokenizer.json" "$stage/semantic/model/tokenizer.json" "$tokenizer_sha256"
fi

# --- 2. build the adapters that exist in adapters/ (in the stage). ---------------------------------
languages=""
if [ -f "$src_root/adapters/go/go.mod" ]; then
	go_min=$(sed -n 's/^go \([0-9][0-9.]*\).*/\1/p' "$src_root/adapters/go/go.mod" | head -n 1)
	command -v go > /dev/null 2>&1 || die "go is needed to build the Go adapter: Go $go_min or newer (apt: golang-go or the go.dev tarball, pacman: go, brew: go). Or remove adapters/go."
	mkdir -p "$stage/adapters/go"
	# The caller's Go environment as it is (GOPATH, GOPROXY, GOTOOLCHAIN...). go.sum checks every module.
	(
		cd "$src_root/adapters/go"
		export GOFLAGS=-mod=readonly
		go build -buildvcs=false -o "$stage/adapters/go/aft-go-adapter" .
	) || die "cannot build the Go adapter. It needs Go $go_min or newer (the go line of adapters/go/go.mod; found: $(go version 2> /dev/null || echo none)). Its modules come from GOPROXY ($(go env GOPROXY 2> /dev/null))."
	languages="$languages go"
fi
if [ -f "$src_root/adapters/js/aft-js-adapter.js" ]; then
	mkdir -p "$stage/adapters/js"
	cp "$src_root/adapters/js/aft-js-adapter.js" "$stage/adapters/js/"
	# The entry file loads lib/ (its modules) and vendor/ (acorn). test/ is not installed.
	for part in lib vendor; do
		[ ! -d "$src_root/adapters/js/$part" ] || cp -R "$src_root/adapters/js/$part" "$stage/adapters/js/$part"
	done
	languages="$languages javascript"
fi

# --- 3. the registry: the example entries of the installed languages, with absolute paths. ---------
# Entries in registry.example.json start with "    {" and end with "    }" or "    },".
awk -v languages="$languages" '
	BEGIN { n = split(languages, want, " "); kept = 0; print "{\n  \"adapters\": [" }
	/^    \{$/ { inside = 1; entry = ""; lang = "" }
	inside {
		if (match($0, /"language": "[a-z]+"/)) { lang = substr($0, RSTART + 13, RLENGTH - 14) }
		line = $0
		if (line ~ /^    \},?$/) { line = "    }" }
		entry = entry (entry == "" ? "" : "\n") line
	}
	inside && /^    \},?$/ {
		inside = 0
		for (i = 1; i <= n; i++) if (want[i] == lang) {
			printf "%s%s", (kept ? ",\n" : ""), entry
			kept++
		}
	}
	END { print (kept ? "\n" : "") "  ]\n}" }
' "$src_root/docs/adapters/registry.example.json" |
	sed -e "s#/opt/aft/adapters/go/aft-go-adapter#$libexec/adapters/go/aft-go-adapter#" \
		-e "s#/opt/aft/adapters/js/aft-js-adapter.js#$libexec/adapters/js/aft-js-adapter.js#" > "$stage/adapters-registry.json"

# --- 4. install. A previous install of this prefix is removed first. -------------------------------
if [ -f "$manifest" ]; then
	bash "$(dirname "$0")/uninstall.sh" --prefix "$prefix" > /dev/null
fi

manifest_lines="$stage/manifest.lines"
: > "$manifest_lines"
make_dir() { # make_dir <dir>: mkdir -p, and record each directory that did not exist before
	local missing="" dir=$1
	while [ ! -d "$dir" ]; do
		missing="$dir $missing"
		dir=$(dirname "$dir")
	done
	for d in $missing; do
		mkdir "$d"
		echo "dir $d" >> "$manifest_lines"
	done
}
put_file() { # put_file <source> <destination> [mode]
	make_dir "$(dirname "$2")"
	cp "$1" "$2"
	chmod "${3:-644}" "$2"
	echo "file $2" >> "$manifest_lines"
}

put_file "$binary" "$libexec/aft" 755
put_file "$stage/adapters-registry.json" "$libexec/adapters-registry.json"
put_file "$(dirname "$0")/aft" "$prefix/bin/aft" 755
if [ -n "$guide_local" ]; then
	put_file "$guide_local" "$libexec/guide.local.md"
fi
if [ -f "$stage/adapters/go/aft-go-adapter" ]; then
	put_file "$stage/adapters/go/aft-go-adapter" "$libexec/adapters/go/aft-go-adapter" 755
fi
if [ -d "$stage/adapters/js" ]; then
	(cd "$stage/adapters/js" && find . -type f | sort) | while IFS= read -r f; do
		put_file "$stage/adapters/js/$f" "$libexec/adapters/js/${f#./}"
	done
fi
if [ "$semantic" = yes ]; then
	put_file "$stage/semantic/ort/$ort_lib_name" "$libexec/onnxruntime/$ort_version/$ort_lib_name" 755
	for f in model.onnx tokenizer.json; do
		put_file "$stage/semantic/model/$f" "$libexec/models/$model_dir_name/snapshots/$model_rev/$f"
	done
fi
make_dir "$(dirname "$manifest")"
echo "file $manifest" >> "$manifest_lines"
cp "$manifest_lines" "$manifest"

echo "installed aft in $prefix"
echo "  binary:   $prefix/bin/aft"
echo "  adapters: ${languages:-none}"
echo "  guide:    $([ -n "$guide_local" ] && echo "addendum from $guide_local" || echo "no addendum")"
echo "  semantic: $([ "$semantic" = yes ] && echo "ONNX Runtime $ort_version and all-MiniLM-L6-v2" || echo "not installed (--without-semantic)")"
echo "  platform: $platform"
echo "  remove:   scripts/adapters/uninstall.sh --prefix $prefix"
case ":$PATH:" in
	*":$prefix/bin:"*) ;;
	*)
		current_shell=$(basename "${SHELL:-}")
		echo "  note: $prefix/bin is not in PATH. Add it for your shell (this script changes no rc file):"
		for sh_name in bash zsh fish; do
			case $sh_name in
				bash) line="add to ~/.bashrc:  export PATH=\"$prefix/bin:\$PATH\"" ;;
				zsh) line="add to ~/.zshrc:   export PATH=\"$prefix/bin:\$PATH\"" ;;
				fish) line="run once:          fish_add_path \"$prefix/bin\"" ;;
			esac
			mark=""
			[ "$current_shell" != "$sh_name" ] || mark="   <- your shell"
			echo "    $sh_name: $line$mark"
		done ;;
esac
