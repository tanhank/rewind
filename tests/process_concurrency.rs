//! Deterministic subprocess-based tests for inter-process synchronization (SEC-005).

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::*;
use rewind::lock::{LockMode, ProcessLock};
use tempfile::tempdir;

/// Helper test entry point used when spawned as a sub-process worker.
/// When `REWIND_LOCK_WORKER` is not set, this test immediately passes.
#[test]
fn worker_lock_helper() {
    if std::env::var("REWIND_LOCK_WORKER").is_err() {
        return;
    }
    let lock_path = PathBuf::from(std::env::var("LOCK_PATH").expect("LOCK_PATH"));
    let mode = match std::env::var("LOCK_MODE").expect("LOCK_MODE").as_str() {
        "shared" => LockMode::Shared,
        "exclusive" => LockMode::Exclusive,
        other => panic!("unknown mode {other}"),
    };
    let timeout_ms: u64 = std::env::var("LOCK_TIMEOUT_MS")
        .unwrap_or_else(|_| "1000".into())
        .parse()
        .unwrap();

    match ProcessLock::acquire(&lock_path, mode, Duration::from_millis(timeout_ms)) {
        Ok(_lock) => {
            println!("READY");
            std::io::stdout().flush().unwrap();

            // Wait for parent signal via stdin or pipe closure
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("FAILED: {err}");
            std::process::exit(2);
        }
    }
}

struct WorkerChild {
    child: Child,
    stdout_reader: BufReader<std::process::ChildStdout>,
}

impl WorkerChild {
    fn spawn(lock_path: &Path, mode: LockMode, timeout_ms: u64) -> Self {
        let exe = std::env::current_exe().expect("current test exe");
        let mut cmd = Command::new(exe);
        cmd.arg("worker_lock_helper")
            .arg("--exact")
            .arg("--nocapture")
            .env("REWIND_LOCK_WORKER", "1")
            .env("LOCK_PATH", lock_path)
            .env(
                "LOCK_MODE",
                match mode {
                    LockMode::Shared => "shared",
                    LockMode::Exclusive => "exclusive",
                },
            )
            .env("LOCK_TIMEOUT_MS", timeout_ms.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().expect("failed to spawn worker child");
        let stdout = child.stdout.take().expect("child stdout");
        let stdout_reader = BufReader::new(stdout);

        Self {
            child,
            stdout_reader,
        }
    }

    fn wait_for_ready(&mut self) -> bool {
        let mut line = String::new();
        while self.stdout_reader.read_line(&mut line).unwrap_or(0) > 0 {
            if line.trim() == "READY" {
                return true;
            }
            line.clear();
        }
        false
    }

    fn release(mut self) {
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = writeln!(stdin, "RELEASE");
            let _ = stdin.flush();
        }
        let _ = self.child.wait();
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn wait_exit(mut self) -> std::process::ExitStatus {
        self.child.wait().expect("wait for child exit")
    }
}

#[test]
fn shared_lock_coexistence_across_subprocesses() {
    let dir = tempdir().unwrap();
    let lock_path = dir.path().join("rewind.lock");

    let mut child1 = WorkerChild::spawn(&lock_path, LockMode::Shared, 1000);
    assert!(child1.wait_for_ready(), "child1 must acquire shared lock");

    let mut child2 = WorkerChild::spawn(&lock_path, LockMode::Shared, 1000);
    assert!(
        child2.wait_for_ready(),
        "child2 must acquire shared lock concurrently"
    );

    let mut child3 = WorkerChild::spawn(&lock_path, LockMode::Shared, 1000);
    assert!(
        child3.wait_for_ready(),
        "child3 must acquire shared lock concurrently"
    );

    child1.release();
    child2.release();
    child3.release();
}

#[test]
fn exclusive_lock_mutual_exclusion_across_subprocesses() {
    let dir = tempdir().unwrap();
    let lock_path = dir.path().join("rewind.lock");

    // Child 1 acquires exclusive lock
    let mut child1 = WorkerChild::spawn(&lock_path, LockMode::Exclusive, 1000);
    assert!(
        child1.wait_for_ready(),
        "child1 must acquire exclusive lock"
    );

    // Child 2 attempts exclusive lock with 150ms timeout; must fail
    let mut child2 = WorkerChild::spawn(&lock_path, LockMode::Exclusive, 150);
    assert!(
        !child2.wait_for_ready(),
        "child2 must time out attempting exclusive lock"
    );
    let status2 = child2.wait_exit();
    assert_eq!(status2.code(), Some(2));

    // Child 3 attempts shared lock with 150ms timeout; must fail
    let mut child3 = WorkerChild::spawn(&lock_path, LockMode::Shared, 150);
    assert!(
        !child3.wait_for_ready(),
        "child3 must time out attempting shared lock"
    );
    let status3 = child3.wait_exit();
    assert_eq!(status3.code(), Some(2));

    // Release child 1
    child1.release();

    // Now child 4 can acquire exclusive lock immediately
    let mut child4 = WorkerChild::spawn(&lock_path, LockMode::Exclusive, 1000);
    assert!(
        child4.wait_for_ready(),
        "child4 must acquire exclusive lock after child1 release"
    );
    child4.release();
}

#[test]
fn lock_released_on_process_sigkill() {
    let dir = tempdir().unwrap();
    let lock_path = dir.path().join("rewind.lock");

    let mut child1 = WorkerChild::spawn(&lock_path, LockMode::Exclusive, 1000);
    assert!(
        child1.wait_for_ready(),
        "child1 must acquire exclusive lock"
    );

    // Abruptly terminate child1 via SIGKILL (no cleanup or destructors)
    child1.kill();

    // Verify OS kernel immediately released the flock lock
    let lock = ProcessLock::acquire(&lock_path, LockMode::Exclusive, Duration::from_millis(500));
    assert!(
        lock.is_ok(),
        "OS kernel must release flock when holding process is killed"
    );
}

#[test]
fn active_execution_shared_lock_blocks_undo_and_clean() {
    let dir = tempdir().unwrap();
    let lock_path = dir.path().join("rewind.lock");

    // Simulate active command execution holding Shared lock
    let mut active_exec = WorkerChild::spawn(&lock_path, LockMode::Shared, 1000);
    assert!(
        active_exec.wait_for_ready(),
        "active_exec must acquire shared lock"
    );

    // Another command execution (Shared) can run concurrently without issue
    let mut concurrent_exec = WorkerChild::spawn(&lock_path, LockMode::Shared, 150);
    assert!(
        concurrent_exec.wait_for_ready(),
        "concurrent command execution must succeed"
    );
    concurrent_exec.release();

    // An undo or clean command (Exclusive) MUST be blocked while execution is running
    let exclusive_attempt =
        ProcessLock::acquire(&lock_path, LockMode::Exclusive, Duration::from_millis(150));
    assert!(
        exclusive_attempt.is_err(),
        "undo/clean exclusive lock must be refused while execution holds shared lock"
    );
    let err_str = exclusive_attempt.unwrap_err().to_string();
    assert!(err_str.contains("another rewind process is active"));

    active_exec.release();

    // After execution finishes, undo/clean exclusive lock succeeds immediately
    let exclusive_now =
        ProcessLock::acquire(&lock_path, LockMode::Exclusive, Duration::from_millis(500));
    assert!(
        exclusive_now.is_ok(),
        "exclusive lock must succeed once execution releases shared lock"
    );
}

#[test]
fn concurrent_undo_vs_clean_mutual_exclusion() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();
    fs::create_dir_all(&paths.data_dir).unwrap();

    // Simulate an ongoing undo or clean holding Exclusive lock
    let mut holding_exclusive = WorkerChild::spawn(&paths.lock_path, LockMode::Exclusive, 1000);
    assert!(
        holding_exclusive.wait_for_ready(),
        "first exclusive holder acquired lock"
    );

    // A competing exclusive operation (e.g. concurrent clean or undo) is refused
    let competing = ProcessLock::acquire(
        &paths.lock_path,
        LockMode::Exclusive,
        Duration::from_millis(150),
    );
    assert!(
        competing.is_err(),
        "competing exclusive operation must be refused"
    );
    assert!(
        competing
            .unwrap_err()
            .to_string()
            .contains("another rewind process is active")
    );

    holding_exclusive.release();
}

#[test]
fn independent_data_dirs_no_contention() {
    let dir1 = tempdir().unwrap();
    let dir2 = tempdir().unwrap();

    let lock_path1 = dir1.path().join("rewind.lock");
    let lock_path2 = dir2.path().join("rewind.lock");

    let mut child1 = WorkerChild::spawn(&lock_path1, LockMode::Exclusive, 1000);
    assert!(child1.wait_for_ready(), "child1 acquires exclusive on dir1");

    let mut child2 = WorkerChild::spawn(&lock_path2, LockMode::Exclusive, 1000);
    assert!(
        child2.wait_for_ready(),
        "child2 acquires exclusive on dir2 simultaneously"
    );

    child1.release();
    child2.release();
}
