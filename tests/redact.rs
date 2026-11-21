//! Comprehensive regression and end-to-end tests for secret redaction (SEC-003).

mod common;

use rewind::journal::Journal;
use rewind::redact::redact_argv;
use tempfile::tempdir;

const SENTINEL_PASSWORD: &str = "SENTINEL_SUPER_SECRET_PASSWORD_98765";
const SENTINEL_TOKEN: &str = "SENTINEL_API_TOKEN_XYZ_12345";
const SENTINEL_API_KEY: &str = "SENTINEL_API_KEY_SECRET_VALUE";
const SENTINEL_CLIENT_SECRET: &str = "SENTINEL_CLIENT_SECRET_TOKEN";
const SENTINEL_DB_PASS: &str = "SENTINEL_DATABASE_PASSWORD_555";
const SENTINEL_BEARER: &str = "SENTINEL_JWT_BEARER_TOKEN_999";

#[test]
fn test_all_redaction_patterns() {
    let argv = vec![
        "program".into(),
        // Inline password
        format!("--password={SENTINEL_PASSWORD}"),
        // Separate password
        "--password".into(),
        SENTINEL_PASSWORD.into(),
        // Inline token
        format!("--token={SENTINEL_TOKEN}"),
        // Separate token
        "--token".into(),
        SENTINEL_TOKEN.into(),
        // Inline API key
        format!("--api-key={SENTINEL_API_KEY}"),
        // Separate API key
        "--api-key".into(),
        SENTINEL_API_KEY.into(),
        // Client secret
        format!("--client-secret={SENTINEL_CLIENT_SECRET}"),
        // Authorization header
        "-H".into(),
        format!("Authorization: Bearer {SENTINEL_BEARER}"),
        // Database URL
        format!("postgres://admin:{SENTINEL_DB_PASS}@localhost:5432/mydb"),
        // Mixed capitalization
        format!("--PASSWORD={SENTINEL_PASSWORD}"),
        "--Token".into(),
        SENTINEL_TOKEN.into(),
        // Normal non-secret arguments
        "-p".into(),
        "--parents".into(),
        "--path=/tmp/normal/path".into(),
        "normal_operand.txt".into(),
    ];

    let redacted = redact_argv(&argv);
    let joined = redacted.join(" ");

    // Verify NONE of the sentinel secrets appear anywhere in the redacted output
    assert!(!joined.contains(SENTINEL_PASSWORD));
    assert!(!joined.contains(SENTINEL_TOKEN));
    assert!(!joined.contains(SENTINEL_API_KEY));
    assert!(!joined.contains(SENTINEL_CLIENT_SECRET));
    assert!(!joined.contains(SENTINEL_DB_PASS));
    assert!(!joined.contains(SENTINEL_BEARER));

    // Verify non-secret arguments are preserved unmodified
    assert!(redacted.contains(&"-p".to_string()));
    assert!(redacted.contains(&"--parents".to_string()));
    assert!(redacted.contains(&"--path=/tmp/normal/path".to_string()));
    assert!(redacted.contains(&"normal_operand.txt".to_string()));
}

#[test]
fn test_end_to_end_journal_redaction() {
    let dir = tempdir().unwrap();
    let journal_path = dir.path().join("journal.db");
    let mut journal = Journal::open(&journal_path).unwrap();

    let secret_argv = vec![
        "git".to_string(),
        format!("--password={SENTINEL_PASSWORD}"),
        "--token".to_string(),
        SENTINEL_TOKEN.to_string(),
        format!("https://user:{SENTINEL_PASSWORD}@github.com/repo.git"),
        "-H".to_string(),
        format!("Authorization: Bearer {SENTINEL_BEARER}"),
        "checkout".to_string(),
        "main.rs".to_string(),
    ];

    let entry_id = journal
        .begin_entry("git", &secret_argv, dir.path())
        .unwrap();
    journal
        .finish_entry(
            entry_id,
            rewind::journal::EntryStatus::Done,
            Some(0),
            true,
            None,
        )
        .unwrap();

    // Check SQLite journal directly: MUST NOT contain sentinel secrets
    let entries = journal.list(10, false).unwrap();
    assert!(!entries.is_empty());
    for entry in &entries {
        assert!(!entry.argv.iter().any(|arg| arg.contains(SENTINEL_PASSWORD)));
        assert!(!entry.argv.iter().any(|arg| arg.contains(SENTINEL_TOKEN)));
        assert!(!entry.argv.iter().any(|arg| arg.contains(SENTINEL_BEARER)));
        assert!(entry.argv.iter().any(|arg| arg.contains("[REDACTED]")));

        let cmdline = entry.command_line();
        assert!(!cmdline.contains(SENTINEL_PASSWORD));
        assert!(!cmdline.contains(SENTINEL_TOKEN));
        assert!(!cmdline.contains(SENTINEL_BEARER));
        assert!(cmdline.contains("[REDACTED]"));
    }
}

#[test]
fn test_sanitize_historical_journal_entries() {
    let dir = tempdir().unwrap();
    let journal_path = dir.path().join("journal.db");
    let mut journal = Journal::open(&journal_path).unwrap();

    // Directly insert historical row containing unredacted secrets (emulating pre-fix database)
    let historical_argv = vec![
        "git".to_string(),
        format!("--password={SENTINEL_PASSWORD}"),
        "--token".to_string(),
        SENTINEL_TOKEN.to_string(),
        "checkout".to_string(),
    ];
    let historical_json = serde_json::to_string(&historical_argv).unwrap();
    journal
        .conn()
        .execute(
            "INSERT INTO entries (created_at, command, argv, cwd, status, meta)
             VALUES (?1, 'git', ?2, '/tmp', 'done', '{}')",
            rusqlite::params![1_700_000_000, historical_json],
        )
        .unwrap();

    // Verify row initially contains the sentinel secret
    let count: i64 = journal
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM entries WHERE argv LIKE ?1",
            [format!("%{SENTINEL_PASSWORD}%")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);

    // Run historical migration / sanitization
    let updated = journal.sanitize_historical_entries().unwrap();
    assert_eq!(updated, 1);

    // Verify sentinel secret is now gone from SQLite database
    let count_after: i64 = journal
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM entries WHERE argv LIKE ?1",
            [format!("%{SENTINEL_PASSWORD}%")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count_after, 0);

    // Running again updates 0 entries (idempotent)
    let updated_second = journal.sanitize_historical_entries().unwrap();
    assert_eq!(updated_second, 0);
}
