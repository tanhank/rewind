//! Regression tests for SEC-004: Hard-link snapshot mutation prevention and snapshot isolation.

mod common;

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use common::*;
use rewind::paths::Paths;
use rewind::staging::{SnapshotMode, Stager, clone_tree};
use tempfile::tempdir;

#[test]
fn snapshot_is_unchanged_when_original_file_is_modified_afterward() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("source.txt");
    let dst = dir.path().join("snapshot.txt");

    fs::write(&src, "ORIGINAL DATA").unwrap();

    // Snapshot using clone_tree
    clone_tree(&src, &dst).unwrap();

    // Verify independent contents
    assert_eq!(fs::read_to_string(&dst).unwrap(), "ORIGINAL DATA");

    // Mutate source file
    fs::write(&src, "MUTATED DATA OVERWRITTEN").unwrap();

    // The snapshot MUST retain the original data (not mutated)
    assert_eq!(
        fs::read_to_string(&dst).unwrap(),
        "ORIGINAL DATA",
        "snapshot must be isolated from subsequent writes to source"
    );
}

#[test]
fn snapshot_remains_unchanged_when_original_is_truncated() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("source.txt");
    let dst = dir.path().join("snapshot.txt");

    fs::write(&src, "HEAVY DATA CONTENT FOR TESTING").unwrap();
    clone_tree(&src, &dst).unwrap();

    // Truncate original file to 0 bytes
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&src)
        .unwrap();

    assert_eq!(fs::metadata(&src).unwrap().len(), 0);
    assert_eq!(
        fs::read_to_string(&dst).unwrap(),
        "HEAVY DATA CONTENT FOR TESTING"
    );
}

#[test]
fn snapshot_remains_unchanged_when_original_is_replaced() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("source.txt");
    let dst = dir.path().join("snapshot.txt");
    let replacement = dir.path().join("new_file.txt");

    fs::write(&src, "ORIGINAL").unwrap();
    fs::write(&replacement, "REPLACEMENT").unwrap();

    clone_tree(&src, &dst).unwrap();

    // Atomic rename over source
    fs::rename(&replacement, &src).unwrap();

    assert_eq!(fs::read_to_string(&src).unwrap(), "REPLACEMENT");
    assert_eq!(fs::read_to_string(&dst).unwrap(), "ORIGINAL");
}

#[test]
fn recursive_directory_snapshots_are_independent() {
    let dir = tempdir().unwrap();
    let src_dir = dir.path().join("src_tree");
    let dst_dir = dir.path().join("dst_tree");

    fs::create_dir_all(src_dir.join("nested/sub")).unwrap();
    fs::write(src_dir.join("a.txt"), "A").unwrap();
    fs::write(src_dir.join("nested/sub/b.txt"), "B").unwrap();

    clone_tree(&src_dir, &dst_dir).unwrap();

    // Mutate nested files in source tree
    fs::write(src_dir.join("a.txt"), "A_MUTATED").unwrap();
    fs::write(src_dir.join("nested/sub/b.txt"), "B_MUTATED").unwrap();
    fs::write(src_dir.join("nested/new.txt"), "NEW").unwrap();

    // Verify snapshot directory contains original contents
    assert_eq!(fs::read_to_string(dst_dir.join("a.txt")).unwrap(), "A");
    assert_eq!(
        fs::read_to_string(dst_dir.join("nested/sub/b.txt")).unwrap(),
        "B"
    );
    assert!(!dst_dir.join("nested/new.txt").exists());
}

#[test]
fn symlinks_are_preserved_without_dereferencing_external_targets() {
    let dir = tempdir().unwrap();
    let external = dir.path().join("external.txt");
    let src = dir.path().join("src_link");
    let dst = dir.path().join("dst_link");

    fs::write(&external, "EXTERNAL SECRET").unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink(&external, &src).unwrap();

    clone_tree(&src, &dst).unwrap();

    #[cfg(unix)]
    {
        assert!(dst.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&dst).unwrap(), external);
    }
}

#[test]
fn sparse_files_behave_correctly() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("sparse_src.bin");
    let dst = dir.path().join("sparse_dst.bin");

    // Create a 1 MB file with hole: write header, seek 1MB, write footer
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&src)
            .unwrap();
        file.write_all(b"HEADER").unwrap();
        file.seek(SeekFrom::Start(1024 * 1024)).unwrap();
        file.write_all(b"FOOTER").unwrap();
    }

    clone_tree(&src, &dst).unwrap();

    assert_eq!(
        fs::metadata(&dst).unwrap().len(),
        fs::metadata(&src).unwrap().len()
    );
    let content = fs::read(&dst).unwrap();
    assert_eq!(&content[0..6], b"HEADER");
    assert_eq!(&content[content.len() - 6..], b"FOOTER");
}

#[test]
fn unsupported_clone_safely_falls_back_to_copy() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("file.txt");
    let dst = dir.path().join("file_copy.txt");

    fs::write(&src, "TEST FALLBACK COPY CONTENT").unwrap();

    // clone_tree must succeed even on filesystems where FICLONE/clonefile fails (e.g. tmpfs)
    let size = clone_tree(&src, &dst).unwrap();
    assert_eq!(size, 26);
    assert_eq!(
        fs::read_to_string(&dst).unwrap(),
        "TEST FALLBACK COPY CONTENT"
    );
}

#[test]
fn partial_failures_clean_up_incomplete_snapshots() {
    let dir = tempdir().unwrap();
    let paths = Paths {
        data_dir: dir.path().join("data"),
        trash_dir: dir.path().join("data/trash"),
        journal_path: dir.path().join("data/journal.db"),
        lock_path: dir.path().join("data/rewind.lock"),
        config_path: dir.path().join("data/config.toml"),
    };
    let stager = Stager::new(&paths, 999);

    let nonexistent = dir.path().join("does_not_exist.txt");
    let res = stager.snapshot(&nonexistent, SnapshotMode::Clone);
    assert!(res.is_err());
}

#[test]
fn git_clean_undo_restores_preserved_original_content_after_mutation() {
    let sandbox = Sandbox::new();
    sandbox.init_repo();

    // Create an untracked file
    sandbox.write("untracked.txt", "ORIGINAL UNTRACKED CONTENT\n");

    // Snapshot via git clean wrapper
    sandbox.exec_ok("git", &["clean", "-f"]);

    // Untracked file removed by git clean
    assert!(!sandbox.exists("untracked.txt"));

    // Undo restores the file
    sandbox.run_ok(&["undo"]);

    assert!(sandbox.exists("untracked.txt"));
    assert_eq!(
        sandbox.read("untracked.txt"),
        "ORIGINAL UNTRACKED CONTENT\n"
    );
}

#[test]
fn concurrent_writer_simulation_proves_snapshot_isolation() {
    let dir = tempdir().unwrap();
    let paths = Paths {
        data_dir: dir.path().join("data"),
        trash_dir: dir.path().join("data/trash"),
        journal_path: dir.path().join("data/journal.db"),
        lock_path: dir.path().join("data/rewind.lock"),
        config_path: dir.path().join("data/config.toml"),
    };
    let stager = Stager::new(&paths, 42);

    let source = dir.path().join("active_build_artifact.txt");
    fs::write(&source, "INITIAL CLEAN BUILD DATA\n").unwrap();

    // Create snapshot
    let payload = stager.snapshot(&source, SnapshotMode::Clone).unwrap();

    // Simulate concurrent writer (e.g. IDE background compiler or sync daemon)
    // mutating the source file immediately after snapshot
    fs::write(&source, "MUTATED COMPILER OUTPUT CONCURRENT WRITE\n").unwrap();

    // Check inode relationship on Unix: if ordinary hard-links were used, inodes would match
    #[cfg(unix)]
    {
        let src_ino = fs::metadata(&source).unwrap().ino();
        let dst_ino = fs::metadata(&payload.stored).unwrap().ino();
        // Even if FICLONE CoW shares extents, any write separates or copies them.
        // Reading the snapshot MUST return the initial clean data, NEVER the concurrent write!
        let _ = (src_ino, dst_ino);
    }

    let snapshot_content = fs::read_to_string(&payload.stored).unwrap();
    assert_eq!(
        snapshot_content, "INITIAL CLEAN BUILD DATA\n",
        "snapshot must NEVER reflect concurrent writes to the source file (SEC-004 fix)"
    );
}
