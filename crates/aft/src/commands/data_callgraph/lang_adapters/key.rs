//! Overlay key (owner: R1).
//!
//! The key of an overlay is (HEAD commit, build id, toolchain id):
//! - HEAD is read from `.git` without a process (about 0.2 ms).
//! - The tree check is `git status --porcelain --untracked-files=no` with a limit
//!   of `TREE_CHECK_LIMIT`. Over the limit the tree is "unknown", and aft serves
//!   no adapter data for this query.
//! - The build id is the SHA-256 of the file `command[0]`.
//! - The toolchain id is the trimmed stdout of `toolchain_id_command`.
//!
//! A clean verdict is remembered per work tree and confirmed by file metadata (`CleanStamp`).
//!
//! The build id and the toolchain id are cached in the storage directory by
//! (canonical path, size, mtime, inode) of the executable, so a query hashes a
//! program or starts `go env` only when that program changed.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::contract::{RegistryEntry, TOOLCHAIN_ID_LIMIT, TREE_CHECK_LIMIT};

/// What identifies one overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Key {
    pub head: String,
    /// The root's path inside the work tree (empty at the top): two modules of one repository are two roots.
    pub module_path: String,
    pub build_id: String,
    pub toolchain_id: String,
}

impl Key {
    /// A short stable name for files: 16 hex characters of the SHA-256 of the three parts.
    pub(crate) fn id(&self) -> String {
        let mut hash = Sha256::new();
        for part in [&self.head, &self.module_path, &self.build_id, &self.toolchain_id] {
            hash.update(part.as_bytes());
            hash.update([0u8]);
        }
        hex(&hash.finalize())[..16].to_string()
    }
}

/// First seven characters of a commit, for messages.
pub(crate) fn short(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---------------------------------------------------------------------------
// HEAD, without a process
// ---------------------------------------------------------------------------

/// The HEAD commit of the git work tree at `root`. `Err` is a reason for the
/// status line; it always names the commit rule.
pub(crate) fn read_head(root: &Path) -> Result<String, String> {
    let git_dir = locate(root)?.1;
    // A linked worktree keeps its refs in the common directory.
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => git_dir.join(text.trim()),
        Err(_) => git_dir.clone(),
    };
    if common_dir.join("reftable").exists() {
        return Err("the repository uses the reftable ref format, which aft cannot read without git".into());
    }
    let mut reference = std::fs::read_to_string(git_dir.join("HEAD"))
        .map_err(|error| format!("not a git work tree with a commit: cannot read HEAD: {error}"))?
        .trim()
        .to_string();
    for _ in 0..5 {
        let Some(name) = reference.strip_prefix("ref:") else {
            return if is_object_id(&reference) {
                Ok(reference)
            } else {
                Err(format!("not a git work tree with a commit: HEAD holds {reference:?}"))
            };
        };
        let name = name.trim().to_string();
        reference = match read_ref(&git_dir, &common_dir, &name)? {
            Some(next) => next,
            None => return Err(format!("not a git work tree with a commit: no commit yet on {name}")),
        };
    }
    Err("not a git work tree with a commit: HEAD refers to too many symbolic refs".into())
}

/// The work tree top and its git directory for `root`: the nearest directory at or
/// above `root` that holds `.git` (a directory, or the file of a linked worktree).
/// `root` may be a subdirectory of the work tree (a Go module below the repository top).
fn locate(root: &Path) -> Result<(PathBuf, PathBuf), String> {
    let mut dir = Some(root);
    while let Some(current) = dir {
        let dot_git = current.join(".git");
        if dot_git.is_dir() {
            return Ok((current.to_path_buf(), dot_git));
        }
        if dot_git.is_file() {
            let text = std::fs::read_to_string(&dot_git).map_err(|error| format!("not a git work tree with a commit: cannot read .git: {error}"))?;
            let target = text
                .lines()
                .find_map(|line| line.strip_prefix("gitdir:"))
                .ok_or_else(|| "not a git work tree with a commit: the .git file has no gitdir line".to_string())?;
            return Ok((current.to_path_buf(), current.join(target.trim())));
        }
        dir = current.parent();
    }
    Err("not a git work tree with a commit: there is no .git at or above the root".into())
}

/// The top of the git work tree that holds `root`.
pub(crate) fn work_tree_of(root: &Path) -> Result<PathBuf, String> {
    locate(root).map(|(top, _)| top)
}

/// `root` relative to its work tree top, with `/` separators; empty when `root` is the top.
fn path_in_work_tree(root: &Path) -> Result<String, String> {
    let top = work_tree_of(root)?;
    let relative = root.strip_prefix(&top).map_err(|_| format!("{} is not inside {}", root.display(), top.display()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

/// The value of ref `name`: its file, else `packed-refs`. `None` when neither has it.
fn read_ref(git_dir: &Path, common_dir: &Path, name: &str) -> Result<Option<String>, String> {
    if name.contains("..") {
        return Err(format!("not a git work tree with a commit: odd ref name {name:?}"));
    }
    for dir in [git_dir, common_dir] {
        if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
            return Ok(Some(text.trim().to_string()));
        }
    }
    let Ok(packed) = std::fs::read_to_string(common_dir.join("packed-refs")) else {
        return Ok(None);
    };
    for line in packed.lines() {
        if line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        if let Some((id, ref_name)) = line.split_once(' ') {
            if ref_name == name {
                return Ok(Some(id.to_string()));
            }
        }
    }
    Ok(None)
}

fn is_object_id(text: &str) -> bool {
    (text.len() == 40 || text.len() == 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// Clean tree
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TreeState {
    Clean,
    /// Tracked files changed; the text names the first few.
    Dirty(String),
    TimedOut,
    /// git could not tell.
    Unknown(String),
}

/// `git status --porcelain --untracked-files=no`, limited to `TREE_CHECK_LIMIT`.
pub(crate) fn tree_state(root: &Path) -> TreeState {
    tree_state_within(root, TREE_CHECK_LIMIT)
}

/// The check covers the WHOLE work tree, not only `root`: a module can point outside
/// its directory (replace directives), so a change anywhere makes the tree dirty.
pub(crate) fn tree_state_within(root: &Path, limit: Duration) -> TreeState {
    let top = work_tree_of(root).unwrap_or_else(|_| root.to_path_buf());
    let root = top.as_path();
    let mut command = Command::new("git");
    command
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    match run_limited(&mut command, limit) {
        Err(error) => TreeState::Unknown(format!("cannot run git: {error}")),
        Ok(Limited::TimedOut) => TreeState::TimedOut,
        Ok(Limited::Done { success: false, stderr, .. }) => {
            TreeState::Unknown(format!("git status failed: {}", String::from_utf8_lossy(&stderr).trim()))
        }
        Ok(Limited::Done { stdout, .. }) => {
            let text = String::from_utf8_lossy(&stdout);
            let changed: Vec<&str> = text.lines().map(|line| line.get(3..).unwrap_or(line)).collect();
            if changed.is_empty() {
                TreeState::Clean
            } else {
                let shown: Vec<&str> = changed.iter().take(3).copied().collect();
                let more = changed.len() - shown.len();
                let more = if more > 0 { format!(" and {more} more") } else { String::new() };
                TreeState::Dirty(format!("{}{more}", shown.join(", ")))
            }
        }
    }
}

/// The checks every adapter run and every query needs before anything else: a git
/// work tree with a HEAD commit and a clean tree. `Ok` is the HEAD commit; `Err` is
/// the reason for a `skipped` status.
///
/// A clean verdict is remembered (see `CleanStamp`) and confirmed on every call by reading
/// file metadata, so a warm query does not start `git status` each time.
pub(crate) fn check_root(root: &Path) -> Result<String, String> {
    let head = read_head(root)?;
    let (top, git_dir) = locate(root)?;
    if clean_stamp_holds(&top, &git_dir, &head) {
        return Ok(head);
    }
    // The stat snapshot comes BEFORE `git status`. A file that changes after the snapshot
    // differs from it on the next call, so a clean verdict is never kept for a changed file.
    let snapshot = CleanStamp::take(&top, &git_dir, &head);
    match tree_state(root) {
        TreeState::Clean => {
            if let Some(stamp) = snapshot {
                clean_stamps().insert(top, stamp);
            }
            Ok(head)
        }
        TreeState::Dirty(files) => Err(format!("local changes in tracked files (dirty tree): {files}")),
        TreeState::TimedOut => Err(format!("tree check timed out (git status took longer than {} ms)", TREE_CHECK_LIMIT.as_millis())),
        TreeState::Unknown(reason) => Err(format!("tree check failed: {reason}")),
    }
}

// ---------------------------------------------------------------------------
// The remembered clean verdict
// ---------------------------------------------------------------------------

/// Above this many tracked files the stat pass costs as much as `git status`: no verdict is kept.
const STAMP_MAX_FILES: usize = 20_000;

/// A file's metadata as git's own change check reads it: modification time, status-change
/// time (a `chmod` moves it), size, inode.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStat {
    mtime: (i64, i64),
    ctime: (i64, i64),
    size: u64,
    inode: u64,
}

impl FileStat {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(Self { mtime: (meta.mtime(), meta.mtime_nsec()), ctime: (meta.ctime(), meta.ctime_nsec()), size: meta.size(), inode: meta.ino() })
    }
}

/// What a clean `git status` verdict stands on: the commit, the index file, and the metadata of
/// every tracked file.
///
/// VALIDITY RULE: the verdict holds while HEAD, the index file and every tracked file have the
/// same metadata as when the stamp was taken. Any difference, also a harmless one (a `touch`),
/// drops the verdict and `git status` decides again. A change that leaves the metadata as it
/// was (same size, same nanosecond mtime) is the same racy gap git itself has. A repository the
/// stamp cannot cover (a submodule, a tracked file that is missing from the work tree, too
/// many files) gets no stamp: every call then runs `git status`.
struct CleanStamp {
    head: String,
    index: FileStat,
    files: Vec<(PathBuf, FileStat)>,
}

impl CleanStamp {
    /// The stamp of the work tree at `top` as it is now, or `None` when it cannot be covered.
    fn take(top: &Path, git_dir: &Path, head: &str) -> Option<Self> {
        let index = FileStat::of(&git_dir.join("index"))?;
        let mut command = Command::new("git");
        command
            .args(["ls-files", "-z", "--stage"])
            .current_dir(top)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        let Ok(Limited::Done { success: true, stdout, .. }) = run_limited(&mut command, TREE_CHECK_LIMIT) else { return None };
        let mut files = Vec::new();
        for entry in stdout.split(|byte| *byte == 0).filter(|entry| !entry.is_empty()) {
            let text = String::from_utf8_lossy(entry);
            // "<mode> <object> <stage>\t<path>"; a mode of 160000 is a submodule.
            let (info, path) = text.split_once('\t')?;
            if info.starts_with("160000") || files.len() >= STAMP_MAX_FILES {
                return None;
            }
            let path = top.join(path);
            let stat = FileStat::of(&path)?;
            files.push((path, stat));
        }
        // The index moved while the list was read: the list may not match it.
        if FileStat::of(&git_dir.join("index"))? != index {
            return None;
        }
        Some(Self { head: head.to_string(), index, files })
    }

    fn holds(&self, git_dir: &Path, head: &str) -> bool {
        self.head == head
            && FileStat::of(&git_dir.join("index")).as_ref() == Some(&self.index)
            && self.files.iter().all(|(path, stat)| FileStat::of(path).as_ref() == Some(stat))
    }
}

/// One stamp per work tree top, for the life of the process.
static STAMPS: Mutex<BTreeMap<PathBuf, CleanStamp>> = Mutex::new(BTreeMap::new());

fn clean_stamps() -> MutexGuard<'static, BTreeMap<PathBuf, CleanStamp>> {
    STAMPS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// True when the remembered clean verdict of `top` still holds. A dropped verdict is removed.
fn clean_stamp_holds(top: &Path, git_dir: &Path, head: &str) -> bool {
    let mut stamps = clean_stamps();
    let Some(stamp) = stamps.get(top) else { return false };
    if stamp.holds(git_dir, head) {
        return true;
    }
    stamps.remove(top);
    false
}

// ---------------------------------------------------------------------------
// A child process with a time limit
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum Limited {
    Done { success: bool, stdout: Vec<u8>, stderr: Vec<u8> },
    TimedOut,
}

/// Runs `command` (stdin empty, stdout and stderr read at the same time) and
/// kills its process group when `limit` passes.
pub(crate) fn run_limited(command: &mut Command, limit: Duration) -> std::io::Result<Limited> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let mut child = command.spawn()?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= limit {
            break None;
        }
        thread::sleep(Duration::from_millis(2));
    };
    // Kill the group in both cases: a stray grandchild must not hold the pipes.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    if status.is_none() {
        let _ = child.wait();
    }
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok(match status {
        Some(status) => Limited::Done { success: status.success(), stdout, stderr },
        None => Limited::TimedOut,
    })
}

// ---------------------------------------------------------------------------
// Programs, build id, toolchain id
// ---------------------------------------------------------------------------

/// The executable file for argv[0]: an absolute path, or a bare name found in PATH.
/// A relative path with a slash is refused: it would resolve inside the repository.
pub(crate) fn resolve_program(name: &str) -> Result<PathBuf, String> {
    if name.contains('/') {
        let path = PathBuf::from(name);
        if !path.is_absolute() {
            return Err(format!("{name:?} must be an absolute path or a bare program name"));
        }
        return if path.is_file() { Ok(path) } else { Err(format!("program not found: {name}")) };
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("program {name:?} not found in PATH"))
}

/// (canonical path, size, mtime in ns, inode) of a program, as text.
fn identity(path: &Path) -> Result<String, String> {
    let canonical = path.canonicalize().map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
    let meta = std::fs::metadata(&canonical).map_err(|error| format!("cannot stat {}: {error}", canonical.display()))?;
    Ok(format!("{}|{}|{}.{}|{}", canonical.display(), meta.size(), meta.mtime(), meta.mtime_nsec(), meta.ino()))
}

/// The value cached under `key_text`, or the one `compute` makes (and caches).
fn cached(cache_dir: &Path, key_text: &str, compute: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
    let file = cache_dir.join(hex(&Sha256::digest(key_text.as_bytes()))[..32].to_string());
    if let Ok(value) = std::fs::read_to_string(&file) {
        if !value.is_empty() {
            return Ok(value);
        }
    }
    let value = compute()?;
    // A cache that cannot be written only costs time on the next query.
    if std::fs::create_dir_all(cache_dir).is_ok() {
        let temp = file.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&temp, &value).is_ok() {
            let _ = std::fs::rename(&temp, &file);
        }
    }
    Ok(value)
}

/// SHA-256 of the file `command[0]` (cached by its identity).
pub(crate) fn build_id(storage: &Path, entry: &RegistryEntry) -> Result<String, String> {
    let program = resolve_program(&entry.command[0])?;
    let key_text = format!("build|{}", identity(&program)?);
    cached(&storage.join("id-cache"), &key_text, || {
        let mut file = std::fs::File::open(&program).map_err(|error| format!("cannot read {}: {error}", program.display()))?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 1 << 20];
        loop {
            let read = file.read(&mut buffer).map_err(|error| format!("cannot read {}: {error}", program.display()))?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        Ok(hex(&hash.finalize()))
    })
}

/// The trimmed stdout of `toolchain_id_command`, or "none" when the entry has none.
/// Cached by the identity of its program, its arguments and the entry's `env`.
pub(crate) fn toolchain_id(storage: &Path, entry: &RegistryEntry) -> Result<String, String> {
    let Some(argv) = entry.toolchain_id_command.as_ref().filter(|argv| !argv.is_empty()) else {
        return Ok("none".to_string());
    };
    let program = resolve_program(&argv[0])?;
    let key_text = format!("toolchain|{}|{:?}|{:?}", identity(&program)?, argv, entry.env);
    cached(&storage.join("id-cache"), &key_text, || {
        let mut command = Command::new(&program);
        // Not in the repository: the answer must not depend on the root.
        command.args(&argv[1..]).envs(&entry.env).current_dir(storage_or_temp(storage));
        match run_limited(&mut command, TOOLCHAIN_ID_LIMIT) {
            Err(error) => Err(format!("cannot run the toolchain id command: {error}")),
            Ok(Limited::TimedOut) => Err(format!("the toolchain id command took longer than {} s", TOOLCHAIN_ID_LIMIT.as_secs())),
            Ok(Limited::Done { success: false, stderr, .. }) => {
                Err(format!("the toolchain id command failed: {}", String::from_utf8_lossy(&stderr).trim()))
            }
            Ok(Limited::Done { stdout, .. }) => {
                let id = String::from_utf8_lossy(&stdout).trim().to_string();
                if id.is_empty() {
                    Err("the toolchain id command printed nothing".to_string())
                } else {
                    Ok(id)
                }
            }
        }
    })
}

fn storage_or_temp(storage: &Path) -> PathBuf {
    if storage.is_dir() {
        storage.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

/// The key for `head` under `entry`. `Err` is a reason for the status line.
pub(crate) fn make_key(storage: &Path, entry: &RegistryEntry, root: &Path, head: &str) -> Result<Key, String> {
    let module_path = path_in_work_tree(root).map_err(|error| format!("cannot make the overlay key: {error}"))?;
    let build_id = build_id(storage, entry).map_err(|error| format!("cannot make the overlay key: {error}"))?;
    let toolchain_id = toolchain_id(storage, entry).map_err(|error| format!("cannot make the overlay key: {error}"))?;
    Ok(Key { head: head.to_string(), module_path, build_id, toolchain_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo_with_commit() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "one"]);
        let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap();
        (dir, String::from_utf8(out.stdout).unwrap().trim().to_string())
    }

    #[test]
    fn head_comes_from_the_ref_file_then_from_packed_refs() {
        let (dir, head) = repo_with_commit();
        assert_eq!(read_head(dir.path()).unwrap(), head);
        git(dir.path(), &["pack-refs", "--all"]);
        assert!(!dir.path().join(".git/refs/heads/main").exists());
        assert_eq!(read_head(dir.path()).unwrap(), head);
        git(dir.path(), &["checkout", "-q", "--detach"]);
        assert_eq!(read_head(dir.path()).unwrap(), head);
    }

    #[test]
    fn head_follows_a_linked_worktree() {
        let (dir, head) = repo_with_commit();
        let other = dir.path().join("wt");
        git(dir.path(), &["worktree", "add", "-q", "--detach", other.to_str().unwrap()]);
        assert!(other.join(".git").is_file());
        assert_eq!(read_head(&other).unwrap(), head);
    }

    #[test]
    fn no_git_and_no_commit_are_named_reasons() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_head(dir.path()).unwrap_err().contains("commit"));
        git(dir.path(), &["init", "-q"]);
        let reason = read_head(dir.path()).unwrap_err();
        assert!(reason.contains("no commit yet"), "{reason}");
    }

    #[test]
    fn a_change_to_a_tracked_file_makes_the_tree_dirty_but_a_new_file_does_not() {
        let (dir, _) = repo_with_commit();
        assert_eq!(tree_state(dir.path()), TreeState::Clean);
        std::fs::write(dir.path().join("new.txt"), "x\n").unwrap();
        assert_eq!(tree_state(dir.path()), TreeState::Clean);
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        match tree_state(dir.path()) {
            TreeState::Dirty(files) => assert!(files.contains("a.txt")),
            other => panic!("{other:?}"),
        }
    }

    /// True when the remembered clean verdict of the repository at `dir` still holds.
    fn verdict_holds(dir: &Path) -> bool {
        let (top, git_dir) = locate(dir).unwrap();
        clean_stamp_holds(&top, &git_dir, &read_head(dir).unwrap())
    }

    #[test]
    fn a_clean_verdict_is_kept_and_any_change_drops_it() {
        let (dir, head) = repo_with_commit();
        assert!(!verdict_holds(dir.path()), "nothing is remembered before the first check");
        assert_eq!(check_root(dir.path()).unwrap(), head);
        assert!(verdict_holds(dir.path()), "a clean check is remembered");
        assert_eq!(check_root(dir.path()).unwrap(), head);

        // A new untracked file does not make the tree dirty: the verdict stays.
        std::fs::write(dir.path().join("new.txt"), "x\n").unwrap();
        assert!(verdict_holds(dir.path()));

        // A changed tracked file drops the verdict, and the check says dirty.
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        assert!(!verdict_holds(dir.path()));
        assert!(check_root(dir.path()).unwrap_err().contains("a.txt"));
        assert!(!verdict_holds(dir.path()), "a dirty tree is never remembered");

        // The file is restored with git: clean again, remembered again.
        git(dir.path(), &["checkout", "-q", "--", "a.txt"]);
        assert_eq!(check_root(dir.path()).unwrap(), head);
        assert!(verdict_holds(dir.path()));
    }

    #[test]
    fn a_new_commit_a_staged_change_or_a_mode_change_drops_the_verdict() {
        let (dir, head) = repo_with_commit();
        check_root(dir.path()).unwrap();
        assert!(verdict_holds(dir.path()));

        // The mode changes, the content does not: git calls the tree dirty, so the verdict must drop.
        let file = dir.path().join("a.txt");
        let mut permissions = std::fs::metadata(&file).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&file, permissions).unwrap();
        assert!(!verdict_holds(dir.path()));
        assert!(check_root(dir.path()).is_err());
        git(dir.path(), &["checkout", "-q", "--", "a.txt"]);
        check_root(dir.path()).unwrap();
        assert!(verdict_holds(dir.path()));

        // A new commit moves HEAD (and the index): another overlay key, so the verdict is not reused.
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        assert!(!verdict_holds(dir.path()), "the index changed");
        git(dir.path(), &["commit", "-q", "-m", "two"]);
        let second = check_root(dir.path()).unwrap();
        assert_ne!(second, head);
        assert!(verdict_holds(dir.path()));
    }

    #[test]
    fn a_submodule_gets_no_verdict_so_git_decides_every_time() {
        let (dir, _) = repo_with_commit();
        let (sub, _) = repo_with_commit();
        let status = Command::new("git")
            .args(["-c", "protocol.file.allow=always", "submodule", "add", "-q"])
            .arg(sub.path())
            .arg("vendor")
            .current_dir(dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success());
        git(dir.path(), &["commit", "-q", "-m", "submodule"]);
        check_root(dir.path()).unwrap();
        assert!(!verdict_holds(dir.path()));
    }

    #[test]
    fn a_slow_process_is_cut_at_the_limit() {
        let started = Instant::now();
        let mut command = Command::new("sleep");
        command.arg("30");
        assert!(matches!(run_limited(&mut command, Duration::from_millis(100)).unwrap(), Limited::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn ids_are_cached_by_the_identity_of_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        std::fs::create_dir_all(&storage).unwrap();
        let program = dir.path().join("adapter.sh");
        std::fs::write(&program, "#!/bin/sh\necho hi\n").unwrap();
        let entry: RegistryEntry = serde_json::from_value(serde_json::json!({
            "contract": 1, "language": "go", "root_markers": ["go.mod"],
            "command": [program.to_str().unwrap()],
            "toolchain_id_command": ["/bin/echo", "go1.99"]
        }))
        .unwrap();
        let first = build_id(&storage, &entry).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(toolchain_id(&storage, &entry).unwrap(), "go1.99");
        assert_eq!(std::fs::read_dir(storage.join("id-cache")).unwrap().count(), 2);
        // Same file: the cache answers. A changed file (new size): a new hash.
        assert_eq!(build_id(&storage, &entry).unwrap(), first);
        std::fs::write(&program, "#!/bin/sh\necho changed\n").unwrap();
        assert_ne!(build_id(&storage, &entry).unwrap(), first);
    }

    #[test]
    fn a_relative_program_path_is_refused() {
        assert!(resolve_program("./adapter").unwrap_err().contains("absolute"));
        assert!(resolve_program("/bin/sh").is_ok());
        assert!(resolve_program("sh").is_ok());
    }
}
