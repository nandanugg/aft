// Build-time compilation of the optional Go helper binary.
//
// If `go` is on PATH, this script compiles `go-helper/` and emits
// `AFT_GO_HELPER_BAKED_PATH` so the Rust binary can locate it without
// requiring it to be on PATH in dev environments.
//
// Absent Go or a compilation failure, it emits a cargo warning and
// continues — the binary still builds, it just won't have automatic
// interface-dispatch resolution for Go projects.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Paths relative to this crate's manifest dir (crates/aft/)
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let go_helper_dir = manifest_dir.join("../../go-helper");
    let go_helper_dir = match go_helper_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // Workspace layout unexpected — skip silently.
            return;
        }
    };

    // Re-run if sources change (keeps incremental builds fast).
    println!("cargo:rerun-if-changed={}", go_helper_dir.join("main.go").display());
    println!("cargo:rerun-if-changed={}", go_helper_dir.join("go.mod").display());
    println!("cargo:rerun-if-changed={}", go_helper_dir.join("go.sum").display());

    // Check for Go toolchain.
    let has_go = Command::new("go")
        .arg("version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !has_go {
        println!(
            "cargo:warning=Go toolchain not found; aft-go-helper will not be built. \
             Install Go to enable type-accurate interface dispatch resolution for Go projects."
        );
        return;
    }

    let out_dir = match std::env::var("OUT_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => return,
    };

    // Binary name is platform-aware.
    let bin_name = if cfg!(target_os = "windows") {
        "aft-go-helper.exe"
    } else {
        "aft-go-helper"
    };
    let helper_out = out_dir.join(bin_name);

    let status = Command::new("go")
        .args(["build", "-o", helper_out.to_str().unwrap_or_default(), "."])
        .current_dir(&go_helper_dir)
        .status();

    match status {
        Ok(s) if s.success() => {
            // Bake the absolute path so dev builds auto-discover the helper.
            // This path is machine-local and only valid in the same build tree.
            // For CI and install scenarios, AFT_GO_HELPER_PATH or PATH lookup
            // takes precedence — see go_helper::find_helper_binary.
            println!(
                "cargo:rustc-env=AFT_GO_HELPER_BAKED_PATH={}",
                helper_out.display()
            );
        }
        Ok(_) => {
            println!(
                "cargo:warning=aft-go-helper build failed; interface dispatch resolution \
                 will fall back to tree-sitter for Go projects."
            );
        }
        Err(e) => {
            println!(
                "cargo:warning=Failed to spawn `go build` for aft-go-helper: {}",
                e
            );
        }
    }
}
