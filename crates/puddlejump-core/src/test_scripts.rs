//! Test-only helpers for writing executable scripts.
//!
//! Tests run in parallel threads of one process. If any thread forks while
//! another still has a script open for writing, the child briefly inherits
//! that write descriptor, and `execve` of the script fails with `ETXTBSY`
//! ("text file busy") until the child has exec'd. Running a hook retries that
//! one transient failure itself, so a script written here can be run safely;
//! the lock below only keeps writers from piling up on each other.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static SCRIPT_LOCK: Mutex<()> = Mutex::new(());

fn script_lock() -> MutexGuard<'static, ()> {
    // A test that panicked while holding the lock leaves nothing in an
    // inconsistent state, so a poisoned lock is safe to reuse.
    SCRIPT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Writes `body` as a `#!/bin/sh` script named `name` in `dir` with `mode`.
pub fn write_script(dir: &Path, name: &str, body: &str, mode: u32) -> PathBuf {
    let _guard = script_lock();
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}
