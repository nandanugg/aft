//! Shared lock for tests that mutate process-global environment variables.
//!
//! `HOME`, `USERPROFILE`, and `XDG_CONFIG_HOME` are process-global, and the
//! libtest runner executes unit tests concurrently within one process. Any two
//! tests that mutate or depend on these variables must serialize on the SAME
//! mutex — module-local mutexes only protect against siblings in the same
//! file, not against a `HOME`-mutating test in another module running in
//! parallel.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Acquire the process-wide env-mutation lock. Poison is ignored: a panicking
/// test already restored (or failed to restore) the env; letting the next test
/// proceed keeps one failure from cascading into every env-dependent test.
pub(crate) fn process_env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
