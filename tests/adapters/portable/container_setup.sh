#!/bin/sh
# container_setup.sh — runs INSIDE a test container (as root). Installs what accept_P1.sh needs, then runs it.
# Supported images: Debian family (apt: ubuntu, debian) and Arch (pacman).
# Environment: P1_ZIP_TOOL=unzip|bsdtar|python3 (the only tool installed to read the ONNX Runtime package;
#              default: the distro's usual one), P1_SETUP_ONLY=1 (stop after the package setup).
# The tree is at /src (read-only), the release folder at /release, an optional download cache at /cache.
set -eu
. /etc/os-release
GO_MIN=$(sed -n 's/^go \([0-9][0-9.]*\).*/\1/p' /src/adapters/go/go.mod | head -n 1)
# Go from go.dev when the distro's Go is older than the go line of adapters/go/go.mod. Each archive is pinned.
GO_TARBALL_VERSION=go1.26.11
GO_SHA256_amd64=9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec
GO_SHA256_arm64=6c2f29b458b5add0835f18b776f69c8881a6d88428f1484a6da55755e620757e

zip_tool=${P1_ZIP_TOOL:-}
case " $ID ${ID_LIKE:-} " in
	*" debian "* | *" ubuntu "*)
		export DEBIAN_FRONTEND=noninteractive
		zip_tool=${zip_tool:-unzip}
		case $zip_tool in unzip) zip_pkg=unzip ;; bsdtar) zip_pkg=libarchive-tools ;; python3) zip_pkg=python3 ;; esac
		apt-get update -qq
		apt-get install -y -qq --no-install-recommends git ca-certificates curl bash zsh fish nodejs golang-go "$zip_pkg" > /dev/null
		;;
	*" arch "*)
		zip_tool=${zip_tool:-bsdtar}
		case $zip_tool in unzip) zip_pkg=unzip ;; bsdtar) zip_pkg=libarchive ;; python3) zip_pkg=python ;; esac
		# Under CPU emulation (an amd64 image on an arm64 host) pacman's download sandbox cannot start (seccomp).
		sandbox_flag=""
		if pacman -S --help 2>&1 | grep -q -e --disable-sandbox; then sandbox_flag=--disable-sandbox; fi
		pacman -Sy $sandbox_flag --noconfirm --needed git ca-certificates curl bash zsh fish nodejs go diffutils "$zip_pkg" > /dev/null
		;;
	*) echo "container_setup: unsupported distribution: $ID" >&2; exit 1 ;;
esac

# The zip tools that must NOT be there, so the install uses the intended fallback.
for t in unzip bsdtar python3; do
	if [ "$t" != "$zip_tool" ] && command -v "$t" > /dev/null 2>&1; then
		echo "container_setup: note: $t is present too; the install prefers unzip, then bsdtar, then python3" >&2
	fi
done

go_version=$(go env GOVERSION 2> /dev/null | sed 's/^go//' || true)
if [ -z "$go_version" ] || [ "$(printf '%s\n%s\n' "$GO_MIN" "$go_version" | sort -V | head -n 1)" != "$GO_MIN" ]; then
	echo "container_setup: distro Go '${go_version:-none}' is older than $GO_MIN; using $GO_TARBALL_VERSION from go.dev"
	case $(uname -m) in
		x86_64) go_arch=amd64; go_sha=$GO_SHA256_amd64 ;;
		aarch64 | arm64) go_arch=arm64; go_sha=$GO_SHA256_arm64 ;;
		*) echo "container_setup: no Go archive for $(uname -m)" >&2; exit 1 ;;
	esac
	curl -fsSL --proto '=https' --tlsv1.2 "https://go.dev/dl/$GO_TARBALL_VERSION.linux-$go_arch.tar.gz" -o /tmp/go.tar.gz
	[ "$(sha256sum /tmp/go.tar.gz | cut -d' ' -f1)" = "$go_sha" ] || { echo "container_setup: Go archive checksum mismatch" >&2; exit 1; }
	rm -rf /usr/local/go && tar -C /usr/local -xzf /tmp/go.tar.gz && rm -f /tmp/go.tar.gz
	PATH=/usr/local/go/bin:$PATH
	export PATH
fi
echo "container_setup: $ID $VERSION_ID $(uname -m): $(go version); zip tool: $zip_tool; shells: $(for s in bash zsh fish; do command -v $s; done | tr '\n' ' ')"
[ "${P1_SETUP_ONLY:-}" != 1 ] || exit 0

export TREE=/src AFT_RELEASE_DIR=/release AFT_ACCEPT_WORK=/tmp/p1-work
[ ! -d /cache ] || export AFT_P1_CACHE=/cache
exec bash /src/tests/adapters/accept_P1.sh
