#!/usr/bin/env bash
# build-release.sh --out <dir> [--platform <p>]... | build-release.sh --out <dir> --kit-only
#
# Builds the aft release binaries into <dir>, one file per platform, plus the install kit, and writes
# <dir>/checksums.sha256 (format of `shasum -a 256`: "<sha256>  <file name>"). install.sh --release-url reads the
# binary and checksums.sha256.
#   <p> is linux-x64, linux-arm64 or darwin-arm64 (the option can repeat).
#   Default: linux-x64 and linux-arm64, plus darwin-arm64 when the host is macOS on arm64.
#   Output names: aft-linux-x64, aft-linux-arm64, aft-darwin-arm64, aft-install-kit.tar.gz.
#
# The install kit (aft-install-kit.tar.gz) is always written: the files that install.sh needs from the source tree
# (scripts/adapters, the adapter sources without their tests, docs/adapters, LICENSE), from the commit HEAD, under
# the folder aft-install-kit/. With it, a machine installs without a clone of this repository:
#   tar -xzf aft-install-kit.tar.gz && aft-install-kit/scripts/adapters/install.sh --prefix <dir> --release-url <url>
# Release assets come from a commit: the script stops when those paths have uncommitted changes.
# --kit-only writes only the kit and its line in checksums.sha256 (no binary, no container engine needed).
#
# Linux platforms: built in a container from scripts/adapters/release.Dockerfile (old glibc, pinned Rust).
#   The engine is a setting: AFT_CONTAINER_ENGINE (default: docker). Only flags that podman also accepts are used.
#   The only command used is
#     <engine> buildx build --platform linux/<amd64|arm64> -f scripts/adapters/release.Dockerfile \
#         [--build-arg AFT_BUILD_GIT_SHA=<sha>] --output type=local,dest=<dir> <repository root>
#   Every flag is accepted by `docker buildx build` and by `podman buildx build`, which is an alias of
#   `podman build` (https://docs.podman.io/en/latest/markdown/podman-build.1.html):
#     --platform         "Set the os/arch of the built image"
#     -f / --file        "Specifies a Containerfile which contains instructions for building the image"
#     --build-arg        "Specifies a build argument and its value, which is interpolated in instructions"
#     --output           "Output destination (format: type=local,dest=path)"
#   The Dockerfile uses RUN --mount=type=cache with id= and sharing=locked; Buildah documents them
#   (https://github.com/containers/buildah/blob/main/docs/buildah-run.1.md, "--mount type=cache").
#   The context is the repository root: the upstream .dockerignore there excludes target/ and .git/, and
#   both engines read it. No second context directory is needed.
#
# darwin-arm64: built on this host with cargo (no container). CARGO_TARGET_DIR comes from the environment,
#   else <dir>/.cargo-target. Needs cargo and cmake (>= 3.18) on PATH; rustc must be >= rust-version in Cargo.toml.
#
# Exit codes: 0 ok, 1 build failed (or engine/tool missing), 2 wrong use.
set -eu

usage() {
	echo "usage: build-release.sh --out <dir> [--platform linux-x64|linux-arm64|darwin-arm64]... | --out <dir> --kit-only" >&2
	exit 2
}
die() {
	echo "build-release: $*" >&2
	exit 1
}

out=""
platforms=""
kit_only=0
while [ $# -gt 0 ]; do
	case $1 in
		--out) [ $# -ge 2 ] || usage; out=$2; shift 2 ;;
		--kit-only) kit_only=1; shift ;;
		--platform)
			[ $# -ge 2 ] || usage
			case $2 in
				linux-x64|linux-arm64|darwin-arm64) platforms="$platforms $2" ;;
				*) echo "build-release: unknown platform '$2' (use linux-x64, linux-arm64 or darwin-arm64)" >&2; exit 2 ;;
			esac
			shift 2 ;;
		*) usage ;;
	esac
done
[ -n "$out" ] || usage
[ "$kit_only" = 0 ] || [ -z "$platforms" ] || usage

if [ "$kit_only" = 0 ] && [ -z "$platforms" ]; then
	platforms="linux-x64 linux-arm64"
	if [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ]; then
		platforms="$platforms darwin-arm64"
	fi
fi

# The repository root is two levels above this script.
root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)

engine=${AFT_CONTAINER_ENGINE:-docker}
sha=""
if command -v git >/dev/null 2>&1; then
	sha=$(git -C "$root" rev-parse HEAD 2>/dev/null || true)
fi

build_linux() { # $1 = x64 or arm64
	local arch=$1 docker_arch tmp
	case $arch in x64) docker_arch=amd64 ;; arm64) docker_arch=arm64 ;; esac
	tmp=$(mktemp -d "$out/.build-$arch.XXXXXX")
	echo "== linux-$arch: $engine buildx build --platform linux/$docker_arch"
	# The build runs in $root so that -f and the context are relative paths (same for both engines).
	if ! (cd "$root" && "$engine" buildx build \
		--platform "linux/$docker_arch" \
		-f scripts/adapters/release.Dockerfile \
		--build-arg "AFT_BUILD_GIT_SHA=$sha" \
		--output "type=local,dest=$tmp" \
		.); then
		rm -rf "$tmp"
		die "container build failed for linux-$arch"
	fi
	[ -f "$tmp/aft-linux-$arch" ] || { rm -rf "$tmp"; die "build did not produce aft-linux-$arch"; }
	mv -f "$tmp/aft-linux-$arch" "$out/aft-linux-$arch"
	rm -rf "$tmp"
}

# The kit paths, also used by the uncommitted-changes check below.
kit_paths="LICENSE scripts/adapters docs/adapters adapters"
build_kit() {
	echo "== install kit: git archive of HEAD ($sha)"
	# --format=tar.gz compresses with git's default "gzip -cn" (no time stamp: the same commit gives the same file).
	# shellcheck disable=SC2086 # kit_paths is a list of paths without spaces
	git -C "$root" archive --format=tar.gz --prefix=aft-install-kit/ -o "$out/aft-install-kit.tar.gz" HEAD -- $kit_paths \
		':(exclude)adapters/go/testdata' ':(exclude)adapters/js/test' ':(exclude)adapters/go/*_test.go' \
		|| die "git archive failed for the install kit"
}

build_darwin_arm64() {
	[ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || die "darwin-arm64 can only be built on a macOS arm64 host"
	command -v cargo >/dev/null 2>&1 || die "cargo not found (needed for darwin-arm64)"
	echo "== darwin-arm64: cargo build on this host ($(cargo --version))"
	local target=${CARGO_TARGET_DIR:-$out/.cargo-target}
	local cargo_home=${CARGO_HOME:-$HOME/.cargo}
	# The binary keeps source paths (panic messages, file!()). Remap the host paths (source tree, cargo registry,
	# target dir with the build-script output) so that no local folder or user name is in a published binary.
	# rustc uses the last prefix that matches, so the target dir (it can be inside the tree) comes last.
	# CARGO_ENCODED_RUSTFLAGS (flags separated by 0x1f) also works with spaces in a path; a RUSTFLAGS from the
	# environment is not used for a release build.
	# cargo sees the tree by its physical path (getcwd resolves symlinks, for example /tmp -> /private/tmp).
	local root_physical sep=$'\x1f'
	root_physical=$(cd "$root" && pwd -P)
	local flags="--remap-path-prefix=$root_physical=/aft${sep}--remap-path-prefix=$cargo_home=/cargo${sep}--remap-path-prefix=$target=/target"
	(cd "$root" && CARGO_TARGET_DIR="$target" CARGO_ENCODED_RUSTFLAGS="$flags" cargo build --locked --release \
		-p agent-file-tools --bin aft --features wasm-grammars) || die "cargo build failed for darwin-arm64"
	cp "$target/release/aft" "$out/aft-darwin-arm64"
	# Gate: a host path in the binary means the remap missed something. Such a binary is not kept.
	local host_path
	for host_path in "$root_physical" "$cargo_home" "$target" "$HOME"; do
		if LC_ALL=C grep -a -q -F "$host_path" "$out/aft-darwin-arm64"; then
			rm -f "$out/aft-darwin-arm64"
			die "aft-darwin-arm64 holds the host path $host_path; the binary was removed"
		fi
	done
}

# Check the tools before any long build.
command -v git >/dev/null 2>&1 && [ -n "$sha" ] || die "the install kit needs git and a checkout with a commit"
# shellcheck disable=SC2086
git -C "$root" diff --quiet HEAD -- $kit_paths || die "uncommitted changes in: $kit_paths (commit them first)"
case " $platforms " in
	*" linux-"*)
		command -v "$engine" >/dev/null 2>&1 || die "container engine '$engine' not found (set AFT_CONTAINER_ENGINE)"
		echo "container engine: $engine"
		"$engine" --version || die "'$engine --version' failed"
		# `buildx build --help`, not `buildx version`: podman has `buildx build` (an alias of `podman build`) only.
		"$engine" buildx build --help >/dev/null 2>&1 || die "'$engine buildx build' is not available"
		;;
esac

for p in $platforms; do
	case $p in
		linux-x64) build_linux x64 ;;
		linux-arm64) build_linux arm64 ;;
		darwin-arm64) build_darwin_arm64 ;;
	esac
done
build_kit

# checksums.sha256 lists exactly the files built now (the binaries and the kit).
if command -v sha256sum >/dev/null 2>&1; then sum="sha256sum"; else sum="shasum -a 256"; fi
: >"$out/checksums.sha256"
for p in $platforms; do
	(cd "$out" && $sum "aft-$p" >>checksums.sha256)
done
(cd "$out" && $sum aft-install-kit.tar.gz >>checksums.sha256)
echo "== done: $out"
cat "$out/checksums.sha256"
