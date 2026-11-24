# Install aft with its language adapters

`scripts/adapters/install.sh` installs aft, the Go and JavaScript adapters, and (by default) semantic search
under one folder, the prefix. It writes nothing outside the prefix and changes no shell rc file.
`scripts/adapters/uninstall.sh` removes exactly what the install made.

## Supported systems

| System | CPU | Release binary | Semantic search |
|---|---|---|---|
| macOS (darwin) | arm64 | `aft-darwin-arm64` | yes |
| macOS (darwin) | x86_64 (Intel) | not in the releases: build aft and use `--binary` | no: there is no ONNX Runtime library. Use `--without-semantic`. |
| Linux (Ubuntu, Debian, Arch) | x86_64 | `aft-linux-x64` | yes |
| Linux (Ubuntu, Debian, Arch) | aarch64 | `aft-linux-arm64` | yes |

The install script runs with bash 3.2 or newer (the `/bin/bash` of macOS works). You can start it from bash, zsh
or fish: it is a bash script with a shebang.

## Tools you need

| Tool | Why | Ubuntu / Debian | Arch | macOS (Homebrew) |
|---|---|---|---|---|
| bash, awk, sed, find, tar | the scripts | preinstalled | preinstalled | preinstalled |
| curl or wget | downloads (https only) | `apt install curl` | `pacman -S curl` | preinstalled |
| sha256sum or shasum | checksums | `apt install coreutils` | `pacman -S coreutils` | preinstalled (`shasum`) |
| go, version 1.22 or newer | builds the Go adapter | `apt install golang-go`, or the tarball from go.dev if the distro Go is older (Debian 12) | `pacman -S go` | `brew install go` |
| unzip, or bsdtar, or python3 | reads the ONNX Runtime package (semantic search only) | `apt install unzip` | `pacman -S unzip` (`bsdtar` is in the base system) | preinstalled (`bsdtar`) |
| node | runs the JavaScript adapter when aft indexes JavaScript | `apt install nodejs` | `pacman -S nodejs` | `brew install node` |
| git | aft answers only in a git work tree with a commit | `apt install git` | `pacman -S git` | `xcode-select --install` |

Go builds the Go adapter with your own Go settings (`GOPROXY`, `GOPATH`, `GOTOOLCHAIN`). Its modules come from
`GOPROXY` and `go.sum` checks each one. If the build fails, the error names the Go version that is needed.

## Install

From the source tree (a clone of this repository at the release tag; the install builds the adapters from it):

```sh
scripts/adapters/install.sh --prefix "$HOME/.local/aft" --release-url <release url>
```

`--release-url` is a folder URL (`https://` or `file://`) that holds `aft-<platform>` and `checksums.sha256`, for
example the download folder of a GitHub release: `https://github.com/<owner>/aft/releases/download/<tag>`. The
binary is checked with its sha256 before anything is installed. Use `--binary <file>` for a binary that you built
yourself (no checksum).

The prefix may contain letters, digits and `. _ / + @ = , -` and space.

Options:

- `--without-semantic`: skip ONNX Runtime and the embedding model. `aft search` then ranks by words only.
  Without this option the install downloads ONNX Runtime 1.24.4 (about 125 MB; 35 MB are kept) and the model
  (about 87 MB), each checked with a pinned sha256.
- `--guide-local <file>`: copy a file of your own notes to the install. `aft guide` appends it to the guide.
- `AFT_INSTALL_CACHE=<dir>` (environment): keep the pinned downloads in `<dir>`. A later install, even for another
  prefix, reads them from there when the sha256 matches. Without it, the install writes nothing outside the prefix.

## Put aft in PATH

If `<prefix>/bin` is not in PATH, the install prints the line for each shell and marks the one in `$SHELL`:

| Shell | Line |
|---|---|
| bash | add to `~/.bashrc`: `export PATH="<prefix>/bin:$PATH"` |
| zsh | add to `~/.zshrc`: `export PATH="<prefix>/bin:$PATH"` |
| fish | run once: `fish_add_path "<prefix>/bin"` |

## Check

```sh
aft guide
aft outline path/to/file.go
aft callers path/to/file.go SomeFunction
aft search "what the code does" path/to/repository
```

aft finds the shell PATH of your login shell (`$SHELL`: bash, zsh or fish) to run `go` and `node`. If a tool
is missing there, add it to the PATH of that shell.

## Uninstall

```sh
scripts/adapters/uninstall.sh --prefix "$HOME/.local/aft"
```

It removes the files and the folders that the install made (a folder only when it is empty). The prefix itself and
the `AFT_INSTALL_CACHE` folder stay.

## Test the install

- `tests/adapters/accept_P1.sh` (needs `AFT_RELEASE_DIR=<release folder>`) installs from each shell that exists
  (bash, zsh, fish) and runs `aft` on a small Go and a small JavaScript project.
- `tests/adapters/portable/run_matrix.sh --release <release folder>` runs it in containers: Ubuntu 24.04, Debian 12
  and Arch, on arm64 and x86_64. The engine is `AFT_CONTAINER_ENGINE` (default `docker`). The scripts use only flags that podman also accepts.
