#!/usr/bin/env bash
# run_matrix.sh --release <dir> [--image <image>[@<platform>][#<zip tool>]]... [--cache <dir>] [--logs <dir>] [--setup-only]
#
# Runs tests/adapters/accept_P1.sh (install from bash, zsh and fish) in one container per image, and prints one
# result line per image. Exit 0 only when every image passed.
#   --release <dir>   a release folder: aft-<platform> for each platform that the images use, and checksums.sha256
#   --image ...       image, an optional platform (linux/arm64, linux/amd64), an optional zip tool for ONNX Runtime
#                     (unzip, bsdtar or python3). Repeat the option. Default images:
#                       ubuntu:24.04@linux/arm64#unzip  debian:12@linux/arm64#python3
#                       archlinux:latest@linux/amd64#bsdtar  ubuntu:24.04@linux/amd64#unzip
#   --cache <dir>     a host folder for AFT_INSTALL_CACHE (the pinned ONNX Runtime and model downloads are kept
#                     there; it saves about 200 MB per install). Default: none (each container downloads).
#   --logs <dir>      where the container logs go (default: a new folder under ${TMPDIR:-/tmp})
#   --setup-only      only install the packages in each image and print the tool versions
# The container engine is AFT_CONTAINER_ENGINE (default docker). Only flags that docker and podman both accept
# are used: run --rm --platform -v -e. The tree is mounted read-only. Containers are removed (--rm); images stay.
set -u
engine=${AFT_CONTAINER_ENGINE:-docker}
tree=$(cd "$(dirname "$0")/../../.." && pwd)
release=""
cache=""
logs=""
setup_only=0
images=()
while [ $# -gt 0 ]; do
	case $1 in
		--release) release=$2; shift 2 ;;
		--image) images+=("$2"); shift 2 ;;
		--cache) cache=$2; shift 2 ;;
		--logs) logs=$2; shift 2 ;;
		--setup-only) setup_only=1; shift ;;
		*) echo "usage: run_matrix.sh --release <dir> [--image <image>[@<platform>][#<zip tool>]]... [--cache <dir>] [--logs <dir>] [--setup-only]" >&2; exit 2 ;;
	esac
done
[ -d "$release" ] || { echo "run_matrix: --release <dir> is needed" >&2; exit 2; }
release=$(cd "$release" && pwd)
if [ ${#images[@]} -eq 0 ]; then
	images=("ubuntu:24.04@linux/arm64#unzip" "debian:12@linux/arm64#python3" "archlinux:latest@linux/amd64#bsdtar" "ubuntu:24.04@linux/amd64#unzip")
fi
if [ -n "$cache" ]; then mkdir -p "$cache"; cache=$(cd "$cache" && pwd); fi
[ -n "$logs" ] || logs=$(mktemp -d "${TMPDIR:-/tmp}/aft-p1-matrix.XXXXXX")
mkdir -p "$logs"

failed=0
for spec in "${images[@]}"; do
	zip_tool=""
	case $spec in *"#"*) zip_tool=${spec#*#}; spec=${spec%%#*} ;; esac
	platform=""
	case $spec in *"@"*) platform=${spec#*@}; spec=${spec%%@*} ;; esac
	image=$spec
	label="$image${platform:+ ($platform)}${zip_tool:+ zip=$zip_tool}"
	log="$logs/$(echo "$image-$platform-$zip_tool" | tr '/:@ ' '____').log"
	args=(run --rm)
	[ -z "$platform" ] || args+=(--platform "$platform")
	args+=(-v "$tree:/src:ro" -v "$release:/release:ro")
	[ -z "$cache" ] || args+=(-v "$cache:/cache")
	[ -z "$zip_tool" ] || args+=(-e "P1_ZIP_TOOL=$zip_tool")
	[ "$setup_only" = 0 ] || args+=(-e P1_SETUP_ONLY=1)
	args+=("$image" sh /src/tests/adapters/portable/container_setup.sh)
	if "$engine" "${args[@]}" > "$log" 2>&1; then
		echo "PASS $label: $(grep -E 'accept_P1.sh: .* passed|container_setup: ' "$log" | tail -n 1)"
	else
		failed=$((failed + 1))
		echo "FAIL $label: see $log"
		tail -n 8 "$log" | sed 's/^/    /'
	fi
done
echo "---"
echo "run_matrix: ${#images[@]} images, $failed failed (logs in $logs)"
[ "$failed" = 0 ]
