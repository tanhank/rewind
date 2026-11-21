//! Integration tests for SEC-006: Trash storage capacity management and user recoverability protection.

mod common;

use std::fs;

use common::*;
use rewind::config::{Config, DEFAULT_MAX_BYTES};
use rewind::journal::Journal;

#[test]
fn default_configuration_uses_10_gib_soft_cap() {
    let config = Config::default();
    assert_eq!(
        config.trash.max_bytes, DEFAULT_MAX_BYTES,
        "default trash.max_bytes must be 10 GiB"
    );
    assert_eq!(config.trash.max_bytes, 10 * 1024 * 1024 * 1024);
}

#[test]
fn explicit_zero_preserves_unlimited_capacity() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    // Write config with explicit max_bytes = 0
    let config_content = r#"
[trash]
max_bytes = 0
"#;
    fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
    fs::write(&paths.config_path, config_content).unwrap();

    let loaded = Config::load(&paths.config_path).unwrap();
    assert_eq!(loaded.trash.max_bytes, 0, "explicit 0 means unlimited");

    // Can stage and remove files without being refused
    sandbox.write("large_file.txt", &"x".repeat(10_000));
    sandbox.exec_ok("rm", &["large_file.txt"]);
    assert!(!sandbox.exists("large_file.txt"));
}

#[test]
fn backward_compatibility_preserves_default_cap_when_key_is_missing() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    // Configuration file with other settings but omitting trash.max_bytes
    let config_content = r#"
[trash]
retention_days = 14

[wrap]
rm = true
"#;
    fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
    fs::write(&paths.config_path, config_content).unwrap();

    let loaded = Config::load(&paths.config_path).unwrap();
    assert_eq!(loaded.trash.retention_days, 14);
    assert_eq!(
        loaded.trash.max_bytes, DEFAULT_MAX_BYTES,
        "missing max_bytes key must receive 10 GiB default"
    );
}

#[test]
fn exceeding_capacity_cap_refuses_destructive_command_and_protects_files() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    // Configure a small cap: 1000 bytes
    let config_content = r#"
[trash]
max_bytes = 1000
"#;
    fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
    fs::write(&paths.config_path, config_content).unwrap();

    // Create a 5000-byte file (exceeds 1000-byte cap)
    sandbox.write("important_document.txt", &"A".repeat(5000));

    // Attempt to remove via rewind exec rm
    let output = sandbox.exec("rm", &["important_document.txt"]);

    // Command MUST be refused
    assert!(
        !output.status.success(),
        "rm must fail when capacity cap is exceeded"
    );

    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("trash capacity limit of 1000 B exceeded")
            || stderr.contains("trash capacity cap")
            || stderr.contains("exceeded"),
        "stderr was: {stderr}"
    );

    // CRITICAL: The user's file MUST NOT be deleted!
    assert!(
        sandbox.exists("important_document.txt"),
        "destructive command must not run when staging capacity is exceeded"
    );
    assert_eq!(sandbox.read("important_document.txt"), "A".repeat(5000));
}

#[test]
fn valid_undoable_entries_are_never_automatically_purged_merely_to_satisfy_cap() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    // Configure 5000-byte cap
    let config_content = r#"
[trash]
max_bytes = 5000
retention_days = 30
"#;
    fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
    fs::write(&paths.config_path, config_content).unwrap();

    // Stage entry 1: 3000 bytes
    sandbox.write("first.txt", &"1".repeat(3000));
    sandbox.exec_ok("rm", &["first.txt"]);

    // Verify first entry is undoable
    assert!(sandbox.log().contains("undoable"));

    // Run rewind clean without --all
    sandbox.run_ok(&["clean"]);

    // The valid undoable entry MUST NOT be purged!
    assert!(
        sandbox.log().contains("undoable"),
        "valid undo snapshots within retention must never be automatically purged by clean"
    );

    // User can still undo first entry
    sandbox.run_ok(&["undo"]);
    assert!(sandbox.exists("first.txt"));
    assert_eq!(sandbox.read("first.txt"), "1".repeat(3000));
}

#[test]
fn expired_entries_are_cleaned_to_free_space_when_cap_is_reached() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    // Configure 5000-byte cap with 1-day retention
    let config_content = r#"
[trash]
max_bytes = 5000
retention_days = 1
"#;
    fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
    fs::write(&paths.config_path, config_content).unwrap();

    // Insert an old entry in the journal created 3 days ago (outside retention)
    {
        let mut journal = Journal::open(&paths.journal_path).unwrap();
        let entry_id = journal
            .begin_entry("rm", &["old.txt".into()], &sandbox.work)
            .unwrap();
        let item = rewind::journal::NewStaged::new(
            "old.txt",
            "old_stored",
            rewind::journal::StagedKind::Move,
        )
        .with_size(4000);
        journal.add_staged(entry_id, &item).unwrap();
        journal
            .finish_entry(
                entry_id,
                rewind::journal::EntryStatus::Done,
                Some(0),
                true,
                None,
            )
            .unwrap();

        // Artificially age the entry to 3 days ago
        let old_time = rewind::util::now_unix() - (3 * 86_400);
        journal
            .conn()
            .execute(
                "UPDATE entries SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![old_time, entry_id],
            )
            .unwrap();
    }

    // Now attempt to remove a 3000-byte file.
    // Total was 4000 bytes (expired). 4000 + 3000 = 7000 > 5000 cap.
    // The capacity check will auto-expire the 3-day-old entry and permit the new removal!
    sandbox.write("new_file.txt", &"N".repeat(3000));
    let output = sandbox.exec_ok("rm", &["new_file.txt"]);
    assert!(output.status.success());
    assert!(!sandbox.exists("new_file.txt"));

    // New file is safely staged and undoable
    sandbox.run_ok(&["undo"]);
    assert!(sandbox.exists("new_file.txt"));
}

#[test]
fn storage_accounting_is_accurate_in_journal() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    sandbox.write("file1.txt", "12345");
    sandbox.write("file2.txt", "1234567890");

    sandbox.exec_ok("rm", &["file1.txt", "file2.txt"]);

    let journal = Journal::open_query_only(&paths.journal_path).unwrap();
    let stats = journal.stats().unwrap();
    assert_eq!(
        stats.staged_bytes, 15,
        "staged_bytes must match exact operand sizes (5 + 10 = 15)"
    );
}

#[test]
fn dead_pid_reservation_is_swept_and_capacity_reclaimed() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    let mut journal = Journal::open(&paths.journal_path).unwrap();
    let entry_dead = journal
        .begin_entry("rm", &["dead.txt".into()], &sandbox.work)
        .unwrap();

    // Manually insert a reservation with a definitely non-existent PID (999_999)
    journal
        .conn()
        .execute(
            "INSERT INTO reservations (entry_id, pid, bytes, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![entry_dead, 999_999, 800, rewind::util::now_unix()],
        )
        .unwrap();

    // With 1000 byte cap: if dead PID is not swept, 800 + 400 = 1200 > 1000 (would fail).
    // Atomic reservation must sweep the dead PID and permit 400 bytes.
    let entry_live = journal
        .begin_entry("rm", &["live.txt".into()], &sandbox.work)
        .unwrap();
    let res = journal.reserve_capacity(entry_live, 400, 1000, 30);
    assert!(
        res.is_ok(),
        "reservation must sweep dead PID and succeed: {:?}",
        res.err()
    );

    // Verify reservations table only has entry_live
    let count: i64 = journal
        .conn()
        .query_row("SELECT COUNT(*) FROM reservations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn active_reservation_blocks_concurrent_overflow_and_releases_on_finish() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    let mut journal = Journal::open(&paths.journal_path).unwrap();
    let entry1 = journal
        .begin_entry("rm", &["first.txt".into()], &sandbox.work)
        .unwrap();
    let entry2 = journal
        .begin_entry("rm", &["second.txt".into()], &sandbox.work)
        .unwrap();

    // Reserve 700 bytes for entry1 against 1000 byte cap
    journal.reserve_capacity(entry1, 700, 1000, 30).unwrap();

    // Attempt to reserve 500 bytes for entry2 (700 + 500 = 1200 > 1000)
    let err = journal.reserve_capacity(entry2, 500, 1000, 30);
    assert!(
        err.is_err(),
        "entry2 must be refused because entry1 holds 700 byte reservation"
    );

    // Finish entry1 (simulating command completion)
    journal
        .finish_entry(
            entry1,
            rewind::journal::EntryStatus::Done,
            Some(0),
            false,
            None,
        )
        .unwrap();

    // Now entry2 can reserve 500 bytes
    let ok = journal.reserve_capacity(entry2, 500, 1000, 30);
    assert!(
        ok.is_ok(),
        "entry2 reservation must succeed once entry1 reservation is released: {:?}",
        ok.err()
    );
}

#[test]
fn live_process_reservation_not_swept_after_arbitrary_time() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    let mut journal = Journal::open(&paths.journal_path).unwrap();
    let entry_live = journal
        .begin_entry("rm", &["live_long.txt".into()], &sandbox.work)
        .unwrap();

    let my_pid = std::process::id();
    let my_start_time = rewind::util::get_process_start_time(my_pid).unwrap_or(0);

    // Simulate a reservation made 3 hours ago (now - 10800) by this live process
    let three_hours_ago = rewind::util::now_unix().saturating_sub(10800);
    journal
        .conn()
        .execute(
            "INSERT INTO reservations (entry_id, pid, start_time, bytes, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![entry_live, my_pid, my_start_time as i64, 800, three_hours_ago],
        )
        .unwrap();

    // Now another entry attempts to reserve 400 bytes against a 1000 byte cap.
    // Since the live reservation of 800 bytes MUST NOT be swept despite being 3 hours old,
    // 800 + 400 = 1200 > 1000, so it must be refused!
    let entry_other = journal
        .begin_entry("rm", &["other.txt".into()], &sandbox.work)
        .unwrap();
    let res = journal.reserve_capacity(entry_other, 400, 1000, 30);
    assert!(
        res.is_err(),
        "live process reservation must NOT be swept merely because time elapsed"
    );
}

#[test]
fn pid_reuse_reservation_is_swept_and_capacity_reclaimed() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();

    let mut journal = Journal::open(&paths.journal_path).unwrap();
    let entry_reused = journal
        .begin_entry("rm", &["reused.txt".into()], &sandbox.work)
        .unwrap();

    let my_pid = std::process::id();
    let my_start_time = rewind::util::get_process_start_time(my_pid).unwrap_or(0);
    // Mismatched start_time (simulating PID was recycled by the OS)
    let fake_old_start_time = my_start_time.saturating_add(99999);

    journal
        .conn()
        .execute(
            "INSERT INTO reservations (entry_id, pid, start_time, bytes, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![entry_reused, my_pid, fake_old_start_time as i64, 800, rewind::util::now_unix()],
        )
        .unwrap();

    // With a 1000 byte cap, 800 + 400 = 1200 > 1000.
    // Because start_time does not match current process start_time, PID reuse is detected,
    // the stale reservation is swept, and 400 bytes reservation succeeds.
    let entry_new = journal
        .begin_entry("rm", &["new.txt".into()], &sandbox.work)
        .unwrap();
    let res = journal.reserve_capacity(entry_new, 400, 1000, 30);
    assert!(
        res.is_ok(),
        "PID reuse with mismatched start_time must be swept: {:?}",
        res.err()
    );
}
