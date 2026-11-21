//! Inter-process synchronization lock (`rewind.lock`).
//!
//! Provides RAII-based shared and exclusive process locking via POSIX `flock(2)`.
//! The lock file lives under `~/.local/share/rewind/rewind.lock`.
//!
//! Concurrency policy:
//! - `rewind undo`: Exclusive lock. Reverses journal entries, moves files from
//!   trash, removes database rows, and cleans up trash directories. Must run
//!   in strict isolation.
//! - `rewind clean`: Exclusive lock. Purges expired staged data and vacuums
//!   the database. Must not run concurrently with `undo` or command staging.
//! - `rewind exec`: Shared lock during staging and finalization. Allows multiple
//!   commands to stage concurrently without blocking each other, while ensuring
//!   `undo` or `clean` cannot mutate state during active staging or reconciliation.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::{Error, IoContext, Result};

/// Lock acquisition mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Shared lock: allows multiple concurrent holders (used by `exec`).
    Shared,
    /// Exclusive lock: allows exactly one holder (used by `undo` and `clean`).
    Exclusive,
}

/// RAII inter-process lock guard.
///
/// Automatically releases the lock and closes the file descriptor when dropped.
/// The kernel also automatically releases the lock if the holding process exits
/// or crashes.
#[derive(Debug)]
pub struct ProcessLock {
    file: File,
    mode: LockMode,
}

impl ProcessLock {
    /// Acquire an exclusive lock on `path`, retrying until `timeout`.
    pub fn acquire_exclusive(path: &Path, timeout: Duration) -> Result<Self> {
        Self::acquire(path, LockMode::Exclusive, timeout)
    }

    /// Acquire a shared lock on `path`, retrying until `timeout`.
    pub fn acquire_shared(path: &Path, timeout: Duration) -> Result<Self> {
        Self::acquire(path, LockMode::Shared, timeout)
    }

    /// Current lock mode of this guard.
    pub fn mode(&self) -> LockMode {
        self.mode
    }

    /// Acquire a lock with the given mode and timeout.
    pub fn acquire(path: &Path, mode: LockMode, timeout: Duration) -> Result<Self> {
        if let Some(parent) = path.parent() {
            crate::paths::create_private_dir(parent)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .path_context(path)?;

        let op = match mode {
            LockMode::Shared => libc::LOCK_SH,
            LockMode::Exclusive => libc::LOCK_EX,
        };

        let start = Instant::now();
        let retry_interval = Duration::from_millis(20);

        loop {
            let res = unsafe { libc::flock(file.as_raw_fd(), op | libc::LOCK_NB) };
            if res == 0 {
                return Ok(Self { file, mode });
            }

            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                || err.raw_os_error() == Some(libc::EAGAIN)
            {
                if start.elapsed() >= timeout {
                    let mode_str = match mode {
                        LockMode::Shared => "shared",
                        LockMode::Exclusive => "exclusive",
                    };
                    return Err(Error::Refused(format!(
                        "timed out after {:.1}s waiting for {mode_str} lock on {} (another rewind process is active)",
                        timeout.as_secs_f64(),
                        path.display()
                    )));
                }
                std::thread::sleep(retry_interval);
                continue;
            }

            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }

            return Err(Error::Io {
                context: format!("acquiring lock on {}", path.display()),
                source: err,
            });
        }
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn exclusive_lock_blocks_another_exclusive_lock() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");

        let lock1 = ProcessLock::acquire_exclusive(&lock_path, Duration::from_millis(50)).unwrap();
        assert_eq!(lock1.mode(), LockMode::Exclusive);

        let err =
            ProcessLock::acquire_exclusive(&lock_path, Duration::from_millis(60)).unwrap_err();
        assert!(matches!(err, Error::Refused(_)));

        drop(lock1);

        let lock2 = ProcessLock::acquire_exclusive(&lock_path, Duration::from_millis(50)).unwrap();
        assert_eq!(lock2.mode(), LockMode::Exclusive);
    }

    #[test]
    fn multiple_shared_locks_coexist() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");

        let lock1 = ProcessLock::acquire_shared(&lock_path, Duration::from_millis(50)).unwrap();
        let lock2 = ProcessLock::acquire_shared(&lock_path, Duration::from_millis(50)).unwrap();
        assert_eq!(lock1.mode(), LockMode::Shared);
        assert_eq!(lock2.mode(), LockMode::Shared);

        // Exclusive lock must fail while shared locks exist
        let err =
            ProcessLock::acquire_exclusive(&lock_path, Duration::from_millis(60)).unwrap_err();
        assert!(matches!(err, Error::Refused(_)));

        drop(lock1);
        drop(lock2);

        // Exclusive lock succeeds once all shared locks are dropped
        let lock3 = ProcessLock::acquire_exclusive(&lock_path, Duration::from_millis(50)).unwrap();
        assert_eq!(lock3.mode(), LockMode::Exclusive);
    }

    #[test]
    fn exclusive_lock_blocks_shared_lock() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");

        let lock1 = ProcessLock::acquire_exclusive(&lock_path, Duration::from_millis(50)).unwrap();

        let err = ProcessLock::acquire_shared(&lock_path, Duration::from_millis(60)).unwrap_err();
        assert!(matches!(err, Error::Refused(_)));

        drop(lock1);

        let lock2 = ProcessLock::acquire_shared(&lock_path, Duration::from_millis(50)).unwrap();
        assert_eq!(lock2.mode(), LockMode::Shared);
    }
}
