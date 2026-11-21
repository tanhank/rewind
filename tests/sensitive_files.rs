//! Integration tests for SEC-002: Sensitive file protection and restrictive permissions.

mod common;

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::*;

fn contains_file(dir: &Path, target: &str) -> bool {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains(target))
            {
                return true;
            }
            if path.is_dir() && contains_file(&path, target) {
                return true;
            }
        }
    }
    false
}

#[test]
fn rm_sensitive_files_bypasses_trash_with_warning() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    // Configure exclude patterns for sensitive files
    let config_content = r#"
[trash]
exclude_patterns = ["*.env", "*id_rsa*", "*.key"]
"#;
    fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
    fs::write(&paths.config_path, config_content).unwrap();

    // Create a mix of sensitive files and regular files
    sandbox.write("production.env", "DATABASE_URL=postgres://...\n");
    sandbox.write("id_rsa", "-----BEGIN OPENSSH PRIVATE KEY-----\n");
    sandbox.write("service.key", "SECRET_KEY_HEX\n");
    sandbox.write("normal.txt", "harmless text content\n");

    // Execute rm on all four files
    let output = sandbox.exec(
        "rm",
        &["production.env", "id_rsa", "service.key", "normal.txt"],
    );
    assert!(output.status.success());

    let stderr = stderr_of(&output);
    // Verify clear warning emitted to stderr for each excluded pattern
    assert!(stderr.contains("rewind: 'production.env' matches trash.exclude_patterns; permanently deleted without undo snapshot"));
    assert!(stderr.contains(
        "rewind: 'id_rsa' matches trash.exclude_patterns; permanently deleted without undo snapshot"
    ));
    assert!(stderr.contains("rewind: 'service.key' matches trash.exclude_patterns; permanently deleted without undo snapshot"));

    // Verify all files were removed from the workspace
    assert!(!sandbox.exists("production.env"));
    assert!(!sandbox.exists("id_rsa"));
    assert!(!sandbox.exists("service.key"));
    assert!(!sandbox.exists("normal.txt"));

    // Inspect trash directory: MUST NOT contain sensitive files anywhere
    if paths.trash_dir.exists() {
        assert!(!contains_file(&paths.trash_dir, "production.env"));
        assert!(!contains_file(&paths.trash_dir, "id_rsa"));
        assert!(!contains_file(&paths.trash_dir, "service.key"));
    }

    // Now test undo:
    // normal.txt must be restored from trash.
    // Sensitive files were deleted directly and cannot be restored.
    sandbox.run_ok(&["undo"]);

    assert!(sandbox.exists("normal.txt"));
    assert_eq!(sandbox.read("normal.txt"), "harmless text content\n");
    assert!(!sandbox.exists("production.env"));
    assert!(!sandbox.exists("id_rsa"));
    assert!(!sandbox.exists("service.key"));
}

#[test]
fn cp_and_mv_skip_snapshot_for_excluded_destination() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    let config_content = r#"
[trash]
exclude_patterns = ["*.key"]
"#;
    fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
    fs::write(&paths.config_path, config_content).unwrap();

    // Setup files
    sandbox.write("new_secret.key", "NEW_KEY_VALUE\n");
    sandbox.write("existing.key", "EXISTING_KEY_VALUE\n");
    sandbox.write("new_plain.txt", "NEW_PLAIN\n");
    sandbox.write("existing_plain.txt", "EXISTING_PLAIN\n");

    // Overwrite existing.key via cp
    sandbox.exec_ok("cp", &["new_secret.key", "existing.key"]);

    // Overwrite existing_plain.txt via cp
    sandbox.exec_ok("cp", &["new_plain.txt", "existing_plain.txt"]);

    // Inspect trash: existing_plain.txt snapshot should exist, but NOT existing.key!
    if paths.trash_dir.exists() {
        assert!(!contains_file(&paths.trash_dir, "existing.key"));
        assert!(contains_file(&paths.trash_dir, "existing_plain.txt"));
    }
}

#[test]
#[cfg(unix)]
fn permissions_are_restrictive_on_directories_and_journal() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    sandbox.write("file.txt", "data");
    sandbox.exec_ok("rm", &["file.txt"]);

    // Verify paths.data_dir is 0700
    let data_mode = fs::metadata(&paths.data_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        data_mode, 0o700,
        "data_dir mode is {:o}, expected 0700",
        data_mode
    );

    // Verify paths.trash_dir is 0700
    let trash_mode = fs::metadata(&paths.trash_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        trash_mode, 0o700,
        "trash_dir mode is {:o}, expected 0700",
        trash_mode
    );

    // Verify entry directories in trash are 0700
    for entry in fs::read_dir(&paths.trash_dir).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_dir() {
            let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "entry dir {:?} mode is {:o}, expected 0700",
                entry.path(),
                mode
            );
        }
    }

    // Verify journal.db is 0600
    let journal_mode = fs::metadata(&paths.journal_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        journal_mode, 0o600,
        "journal.db mode is {:o}, expected 0600",
        journal_mode
    );

    // If WAL / SHM exist, verify 0600
    let wal = paths.journal_path.with_extension("db-wal");
    if wal.exists() {
        let wal_mode = fs::metadata(&wal).unwrap().permissions().mode() & 0o777;
        assert_eq!(wal_mode, 0o600, "wal mode is {:o}, expected 0600", wal_mode);
    }
    let shm = paths.journal_path.with_extension("db-shm");
    if shm.exists() {
        let shm_mode = fs::metadata(&shm).unwrap().permissions().mode() & 0o777;
        assert_eq!(shm_mode, 0o600, "shm mode is {:o}, expected 0600", shm_mode);
    }
}
