//! File locks for the runner (owner: R1): one lock per (root, language) and the
//! machine-wide run slots. `flock` is tied to the open file, so a process that
//! dies (also by SIGKILL) frees its locks. Rust opens files with close-on-exec,
//! so an adapter never inherits a lock. One short exception: a child started
//! with `fork` + `exec` (the detached runner uses `pre_exec(setsid)`) holds a
//! copy of every open lock between its fork and its exec, so a lock dropped in
//! that window stays taken for a few milliseconds. Callers that must take a
//! lock use `try_lock_for`, which retries; `is_locked` is only a status probe.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use super::super::contract::GLOBAL_RUN_LIMIT;

/// A held lock. It is released when the value is dropped.
pub(crate) struct FileLock {
    _file: File,
}

fn flock_nonblocking(file: &File) -> io::Result<bool> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(error)
    }
}

/// Takes the lock at `path` when nobody holds it, and writes our pid into the file.
pub(crate) fn try_lock(path: &Path) -> io::Result<Option<FileLock>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(path)?;
    if !flock_nonblocking(&file)? {
        return Ok(None);
    }
    file.set_len(0)?;
    writeln!(file, "{}", std::process::id())?;
    Ok(Some(FileLock { _file: file }))
}

/// Like `try_lock`, but tries again for up to `wait`: a probe by `is_locked` holds
/// the lock for a moment, and that must not make a runner give up.
pub(crate) fn try_lock_for(path: &Path, wait: Duration) -> io::Result<Option<FileLock>> {
    let started = Instant::now();
    loop {
        if let Some(lock) = try_lock(path)? {
            return Ok(Some(lock));
        }
        if started.elapsed() >= wait {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// True when another process (or file) holds the lock at `path`.
pub(crate) fn is_locked(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).open(path) else {
        return false;
    };
    match flock_nonblocking(&file) {
        Ok(acquired) => !acquired,
        Err(_) => false,
    }
}

/// Takes one of the `GLOBAL_RUN_LIMIT` machine-wide run slots, waiting up to `wait`.
/// `None` when no slot became free in time.
pub(crate) fn acquire_slot(slots_dir: &Path, wait: Duration) -> io::Result<Option<FileLock>> {
    let started = Instant::now();
    loop {
        for number in 0..GLOBAL_RUN_LIMIT {
            if let Some(lock) = try_lock(&slots_dir.join(format!("slot-{number}.lock")))? {
                return Ok(Some(lock));
            }
        }
        if started.elapsed() >= wait {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_exclusive_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/run.lock");
        let first = try_lock(&path).unwrap().expect("free");
        assert!(is_locked(&path));
        assert!(try_lock(&path).unwrap().is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), std::process::id().to_string());
        drop(first);
        // Other tests start child processes at the same time; a child between
        // its fork and its exec holds a copy of the lock for a few milliseconds
        // (see the module comment). So wait, with a limit, for the release.
        let released_by = Instant::now() + Duration::from_secs(2);
        while is_locked(&path) && Instant::now() < released_by {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!is_locked(&path), "the lock is still held 2 s after it was dropped");
        assert!(try_lock_for(&path, Duration::from_millis(50)).unwrap().is_some());
    }

    #[test]
    fn at_most_the_global_limit_of_slots_are_taken() {
        let dir = tempfile::tempdir().unwrap();
        let held: Vec<FileLock> = (0..GLOBAL_RUN_LIMIT).map(|_| acquire_slot(dir.path(), Duration::ZERO).unwrap().expect("slot")).collect();
        assert!(acquire_slot(dir.path(), Duration::from_millis(150)).unwrap().is_none());
        drop(held);
        // A child that another test forks while these slots are held keeps a copy of
        // them until its exec (see the module comment): wait, with a limit, as runners do.
        assert!(acquire_slot(dir.path(), Duration::from_secs(2)).unwrap().is_some());
    }
}
