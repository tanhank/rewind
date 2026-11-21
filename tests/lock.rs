//! Concurrency and locking tests for `ProcessLock`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use rewind::lock::{LockMode, ProcessLock};
use tempfile::tempdir;

#[test]
fn shared_locks_allow_multiple_readers() {
    let dir = tempdir().unwrap();
    let lock_path = dir.path().join("rewind.lock");

    let lock1 = ProcessLock::acquire(&lock_path, LockMode::Shared, Duration::from_secs(1)).unwrap();
    let lock2 = ProcessLock::acquire(&lock_path, LockMode::Shared, Duration::from_secs(1)).unwrap();
    let lock3 = ProcessLock::acquire(&lock_path, LockMode::Shared, Duration::from_secs(1)).unwrap();

    drop(lock1);
    drop(lock2);
    drop(lock3);
}

#[test]
fn exclusive_locks_are_mutually_exclusive() {
    let dir = tempdir().unwrap();
    let lock_path = dir.path().join("rewind.lock");

    let lock1 =
        ProcessLock::acquire(&lock_path, LockMode::Exclusive, Duration::from_secs(1)).unwrap();

    let start = std::time::Instant::now();
    let err = ProcessLock::acquire(&lock_path, LockMode::Exclusive, Duration::from_millis(100))
        .unwrap_err();
    assert!(start.elapsed() >= Duration::from_millis(90));
    assert!(err.to_string().contains("another rewind process is active"));

    drop(lock1);

    // After drop, acquiring exclusive lock succeeds immediately.
    let lock2 =
        ProcessLock::acquire(&lock_path, LockMode::Exclusive, Duration::from_secs(1)).unwrap();
    drop(lock2);
}

#[test]
fn exclusive_lock_blocks_shared_lock() {
    let dir = tempdir().unwrap();
    let lock_path = dir.path().join("rewind.lock");

    let lock_ex =
        ProcessLock::acquire(&lock_path, LockMode::Exclusive, Duration::from_secs(1)).unwrap();

    let err =
        ProcessLock::acquire(&lock_path, LockMode::Shared, Duration::from_millis(100)).unwrap_err();
    assert!(err.to_string().contains("another rewind process is active"));

    drop(lock_ex);

    let lock_sh =
        ProcessLock::acquire(&lock_path, LockMode::Shared, Duration::from_secs(1)).unwrap();
    drop(lock_sh);
}

#[test]
fn shared_lock_blocks_exclusive_lock() {
    let dir = tempdir().unwrap();
    let lock_path = dir.path().join("rewind.lock");

    let lock_sh =
        ProcessLock::acquire(&lock_path, LockMode::Shared, Duration::from_secs(1)).unwrap();

    let err = ProcessLock::acquire(&lock_path, LockMode::Exclusive, Duration::from_millis(100))
        .unwrap_err();
    assert!(err.to_string().contains("another rewind process is active"));

    drop(lock_sh);

    let lock_ex =
        ProcessLock::acquire(&lock_path, LockMode::Exclusive, Duration::from_secs(1)).unwrap();
    drop(lock_ex);
}

#[test]
fn threads_contention_and_handoff() {
    let dir = tempdir().unwrap();
    let lock_path = dir.path().join("rewind.lock");

    let held = Arc::new(AtomicBool::new(true));
    let held_clone = Arc::clone(&held);
    let path_clone = lock_path.clone();

    let handle = thread::spawn(move || {
        let _lock =
            ProcessLock::acquire(&path_clone, LockMode::Exclusive, Duration::from_secs(2)).unwrap();
        thread::sleep(Duration::from_millis(150));
        held_clone.store(false, Ordering::SeqCst);
    });

    thread::sleep(Duration::from_millis(20));
    // Must succeed after the thread releases within 2s
    let _lock2 =
        ProcessLock::acquire(&lock_path, LockMode::Exclusive, Duration::from_secs(2)).unwrap();
    assert!(!held.load(Ordering::SeqCst));

    handle.join().unwrap();
}
