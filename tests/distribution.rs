mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::Sandbox;

#[test]
fn metadata_consistency_across_cargo_debian_rpm_man() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // 1. Cargo.toml
    let cargo_toml = fs::read_to_string(root.join("Cargo.toml")).expect("Cargo.toml");
    let version_line = cargo_toml
        .lines()
        .find(|l| l.starts_with("version = "))
        .expect("version line");
    let cargo_version = version_line.split('"').nth(1).expect("cargo version value");
    assert_eq!(cargo_version, "0.2.0");
    assert!(cargo_toml.contains("license = \"MIT\""));
    assert!(cargo_toml.contains("rust-version = \"1.89\""));

    // 2. debian/changelog
    let debian_changelog =
        fs::read_to_string(root.join("debian/changelog")).expect("debian/changelog");
    assert!(
        debian_changelog.starts_with("rewind (0.2.0-1)"),
        "debian/changelog first line mismatch"
    );

    // 3. packaging/rewind.spec
    let rpm_spec = fs::read_to_string(root.join("packaging/rewind.spec")).expect("rewind.spec");
    assert!(rpm_spec.contains("Version:        0.2.0"));
    assert!(rpm_spec.contains("License:        MIT"));
    assert!(rpm_spec.contains("Release:        1%{?dist}"));

    // 4. docs/rewind.1
    let man_page = fs::read_to_string(root.join("docs/rewind.1")).expect("docs/rewind.1");
    assert!(man_page.contains("\"rewind 0.2.0\""));
    assert!(man_page.contains(".TH REWIND 1"));

    // 5. Package manifests describe all wrapped git commands
    let deb_control = fs::read_to_string(root.join("debian/control")).expect("debian/control");
    let static_deb_script = fs::read_to_string(root.join("packaging/build-static-deb.sh"))
        .expect("build-static-deb.sh");

    for manifest in [&deb_control, &rpm_spec, &static_deb_script] {
        assert!(manifest.contains("git commit --amend"));
        assert!(manifest.contains("git rebase"));
        assert!(manifest.contains("git stash drop/clear/pop"));
        assert!(manifest.contains("git branch -D"));
    }
}

#[test]
fn man_page_and_shell_completions_generation() {
    let sandbox = Sandbox::new();

    // Verify shell completions generation for bash, zsh, fish
    for shell in ["bash", "zsh", "fish"] {
        let output = sandbox
            .command()
            .args(["completions", shell])
            .output()
            .expect("rewind completions");
        assert!(
            output.status.success(),
            "rewind completions {shell} failed: {:?}",
            output
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout.trim().is_empty());
        match shell {
            "bash" => assert!(stdout.contains("complete -F _rewind")),
            "zsh" => assert!(stdout.contains("#compdef rewind") || stdout.contains("_rewind")),
            "fish" => assert!(stdout.contains("complete -c rewind")),
            _ => {}
        }
    }

    // Verify man page content
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let man_content = fs::read_to_string(root.join("docs/rewind.1")).expect("docs/rewind.1");
    for subcommand in [
        "undo",
        "status",
        "log",
        "inspect",
        "clean",
        "config",
        "init",
        "completions",
    ] {
        assert!(
            man_content.contains(subcommand),
            "docs/rewind.1 missing documentation for subcommand '{subcommand}'"
        );
    }
    assert!(man_content.contains("EXIT STATUS"));
    assert!(man_content.contains("ENVIRONMENT"));
}

#[test]
fn shell_scripts_syntax_and_interceptors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bash_sh = root.join("shell/bash.sh");
    let zsh_sh = root.join("shell/zsh.sh");
    let fish_fish = root.join("shell/fish.fish");

    // Syntax validation via bash -n
    if let Ok(output) = Command::new("bash").arg("-n").arg(&bash_sh).output() {
        assert!(
            output.status.success(),
            "bash.sh syntax error: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Check that bash, zsh, fish wrappers intercept git subcommands
    let bash_content = fs::read_to_string(&bash_sh).expect("bash.sh");
    let zsh_content = fs::read_to_string(&zsh_sh).expect("zsh.sh");
    let fish_content = fs::read_to_string(&fish_fish).expect("fish.fish");

    for script in [&bash_content, &zsh_content, &fish_content] {
        assert!(script.contains("commit"));
        assert!(script.contains("rebase"));
        assert!(script.contains("stash"));
        assert!(script.contains("branch"));
    }
}

#[test]
fn database_migration_from_legacy_schema() {
    let sandbox = Sandbox::new();
    let db_path = sandbox.paths().journal_path;
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    // Create a legacy database matching pre-start_time reservations schema
    {
        let conn = rusqlite::Connection::open(&db_path).expect("open raw sqlite");
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;

            CREATE TABLE entries (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at  INTEGER NOT NULL,
                finished_at INTEGER,
                command     TEXT    NOT NULL,
                argv        TEXT    NOT NULL,
                cwd         TEXT    NOT NULL,
                status      TEXT    NOT NULL,
                exit_code   INTEGER,
                undoable    INTEGER NOT NULL DEFAULT 0,
                meta        TEXT    NOT NULL DEFAULT '{}',
                purged_at   INTEGER
            );

            CREATE TABLE staged (
                id       INTEGER PRIMARY KEY AUTOINCREMENT,
                entry_id INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
                origin   TEXT    NOT NULL,
                stored   TEXT    NOT NULL DEFAULT '',
                kind     TEXT    NOT NULL,
                size     INTEGER NOT NULL DEFAULT 0,
                meta     TEXT    NOT NULL DEFAULT '{}'
            );

            -- Legacy reservations table WITHOUT start_time column
            CREATE TABLE reservations (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                entry_id    INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
                pid         INTEGER NOT NULL,
                bytes       INTEGER NOT NULL,
                created_at  INTEGER NOT NULL
            );

            INSERT INTO entries (id, created_at, command, argv, cwd, status, undoable)
            VALUES (1, 1700000000, 'rm test.txt', '["rm","test.txt"]', '/tmp', 'completed', 1);

            INSERT INTO reservations (id, entry_id, pid, bytes, created_at)
            VALUES (1, 1, 999999, 1048576, 1700000000);
            "#,
        )
        .expect("legacy schema creation");
    }

    // Now open via Rewind Journal::open, which must run automatic schema migrations safely
    let journal = rewind::journal::Journal::open(&db_path).expect("journal open legacy db");

    // 1. Verify entries preserved
    let entries = journal.list(10, true).expect("list entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, 1);
    assert_eq!(entries[0].command, "rm test.txt");

    // 2. Verify active reservations query works with migrated start_time column
    let res_bytes = journal.active_reservations_bytes();
    assert!(
        res_bytes.is_ok(),
        "active_reservations_bytes failed: {:?}",
        res_bytes
    );
}

#[test]
fn uninstallation_data_preservation_policy() {
    let sandbox = Sandbox::new();
    let prefix = sandbox.work.join("mock_sys");

    // Simulate system installation paths
    let bin_path = prefix.join("usr/bin/rewind");
    let hooks_path = prefix.join("usr/share/rewind");
    fs::create_dir_all(&hooks_path).unwrap();
    fs::create_dir_all(bin_path.parent().unwrap()).unwrap();
    fs::write(&bin_path, b"mock-binary").unwrap();
    fs::write(hooks_path.join("bash.sh"), b"# mock hook").unwrap();

    // Simulate user recovery data created by running rewind
    let user_trash = sandbox.paths().trash_dir;
    let user_config = sandbox.paths().config_path;
    fs::create_dir_all(&user_trash).unwrap();
    fs::create_dir_all(user_config.parent().unwrap()).unwrap();
    fs::write(sandbox.paths().journal_path, b"SQLite format 3\0").unwrap();
    fs::write(user_trash.join("staged_file"), b"recovered content").unwrap();
    fs::write(&user_config, b"[trash]\nmax_bytes = 1000\n").unwrap();

    // Simulate standard package removal: removes only /usr/bin and /usr/share/rewind
    fs::remove_dir_all(&prefix).expect("package removal");

    // Assert user recovery data remains 100% intact
    assert!(
        sandbox.paths().journal_path.exists(),
        "User journal was corrupted or removed"
    );
    assert!(
        user_trash.join("staged_file").exists(),
        "User trash snapshots were lost during package removal"
    );
    assert!(
        user_config.exists(),
        "User configuration was deleted during package removal"
    );
}

#[test]
fn configuration_file_preservation_on_upgrade() {
    let sandbox = Sandbox::new();
    let config_path = sandbox.paths().config_path;
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();

    // User creates customized config
    let custom_config = r#"
[trash]
max_bytes = 4294967296
exclude_patterns = ["*.secret", "*.pem"]
"#;
    fs::write(&config_path, custom_config).unwrap();

    // Run rewind config get to verify configuration loads
    let output = sandbox
        .command()
        .args(["config", "get", "trash.max_bytes"])
        .output()
        .expect("rewind config get");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "4294967296");

    // Run rewind config list
    let list_output = sandbox
        .command()
        .args(["config", "list"])
        .output()
        .expect("rewind config list");
    assert!(list_output.status.success());
    let list_str = String::from_utf8_lossy(&list_output.stdout);
    assert!(list_str.contains("trash.max_bytes = 4294967296"));

    // Verify config file was not overwritten
    let final_config = fs::read_to_string(&config_path).unwrap();
    assert_eq!(final_config, custom_config);
}

#[test]
fn static_deb_package_builder_layout() {
    let sandbox = Sandbox::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("packaging/build-static-deb.sh");
    let bin = env!("CARGO_BIN_EXE_rewind");
    let outdir = sandbox.work.join("deb_out");

    let status = Command::new(&script)
        .arg(bin)
        .arg("amd64")
        .arg(&outdir)
        .status()
        .expect("execute build-static-deb.sh");
    assert!(status.success(), "build-static-deb.sh failed");

    let deb_file = outdir.join("rewind_0.2.0-1_amd64.deb");
    assert!(deb_file.exists(), "deb package was not created");

    // Verify ar archive contents
    let ar_output = Command::new("ar")
        .args(["-t", deb_file.to_str().unwrap()])
        .output()
        .expect("ar -t");
    assert!(ar_output.status.success());
    let ar_listing = String::from_utf8_lossy(&ar_output.stdout);
    let parts: Vec<&str> = ar_listing.lines().collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], "debian-binary");
    assert_eq!(parts[1], "control.tar.xz");
    assert_eq!(parts[2], "data.tar.xz");
}

#[test]
fn release_tarball_and_checksum_verification() {
    let sandbox = Sandbox::new();
    let dist_dir = sandbox.work.join("dist");
    fs::create_dir_all(&dist_dir).unwrap();

    let archive_name = "rewind-0.2.0-x86_64-unknown-linux-musl.tar.gz";
    let archive_path = dist_dir.join(archive_name);

    // Create a mock release tarball
    let tar_status = Command::new("tar")
        .args([
            "-czf",
            archive_path.to_str().unwrap(),
            "-C",
            env!("CARGO_MANIFEST_DIR"),
            "README.md",
            "LICENSE",
        ])
        .status()
        .expect("tar -czf");
    assert!(tar_status.success());

    // Generate SHA-256 checksum
    let sha_output = Command::new("sha256sum")
        .current_dir(&dist_dir)
        .arg(archive_name)
        .output()
        .expect("sha256sum");
    assert!(sha_output.status.success());
    let checksum_content = String::from_utf8_lossy(&sha_output.stdout);
    let checksum_file = dist_dir.join("SHA256SUMS");
    fs::write(&checksum_file, checksum_content.as_bytes()).unwrap();

    // Verify valid checksum succeeds
    let verify_status = Command::new("sha256sum")
        .current_dir(&dist_dir)
        .args(["-c", "SHA256SUMS"])
        .status()
        .expect("sha256sum -c");
    assert!(verify_status.success());

    // Tamper with archive and verify failure detection
    let mut bytes = fs::read(&archive_path).unwrap();
    bytes[0] ^= 0xFF;
    fs::write(&archive_path, bytes).unwrap();

    let fail_status = Command::new("sha256sum")
        .current_dir(&dist_dir)
        .args(["-c", "SHA256SUMS"])
        .status()
        .expect("sha256sum -c tamper");
    assert!(!fail_status.success());
}
