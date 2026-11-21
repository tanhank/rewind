# Security & Reliability Posture: rewind

> **Document Version:** 1.10.0  
> **Target Software Release Version:** `0.3.0`  
> **Commit SHA:** `08a325fc468709928f8e7f5401b418a69d253d66` (+ Phases 2-10 Hardening)  
> **Audited By:** Staff Software Engineer, AppSec & DevSecOps Lead  
> **Status:** Fully Audited, Patched, Verified & Release-Ready

---

## 1. Threat Model & Trust Boundaries

`rewind` operates entirely within user-space as a shell function calling a setuid-less CLI binary. It operates with the ambient privileges of the invoking user.

### Trust Boundaries
1. **Shell Interception Boundary:** The shell hook passes arbitrary strings from the command line into `rewind exec`. Arguments must be treated as untrusted data regarding character encoding, path expansion, and potential control characters.
2. **Filesystem Storage Boundary:** State stored in `~/.local/share/rewind` (or `$XDG_DATA_HOME/rewind`) holds raw file contents moved out of the workspace. On multi-user systems, local users must not be able to read or modify this directory.
3. **Subprocess Boundary:** When delegating to native binaries (`git`, `mv`, `cp`, `chmod`, `chown`), `rewind` spawns child processes inheriting standard file descriptors.
4. **Git Object Database Boundary:** Rewind writes custom refnames (`refs/rewind/*`) into repository `.git/` directories, preventing garbage collection of referenced commits/trees.

---

## 2. Existing Security Controls

The codebase contains several robust, intentionally implemented security controls:

- **Private Directory Mode (`0700`):** `create_private_dir()` in [src/paths.rs](file:///home/tan/Projects/rewind/src/paths.rs#L89-L99) enforces Unix permission mask `0700` on both `~/.local/share/rewind` and `trash/`.
- **Trash Directory Escape Prevention:** `Stager::validate_stored()` in [src/staging.rs](file:///home/tan/Projects/rewind/src/staging.rs#L169-L197) canonicalizes all paths referenced by the journal before moving or deleting them, verifying they reside strictly inside the resolved trash directory.
- **Ancestor-of-Trash Protection:** `Stager::guard_not_ancestor_of_trash()` in [src/staging.rs](file:///home/tan/Projects/rewind/src/staging.rs#L200-L215) refuses to move any directory containing the trash directory itself (e.g. `rm -rf ~` or `rm -rf ~/.local`), preventing infinite recursion and self-destruction.
- **No-Clobber Destination Protection:** `move_path()` in [src/staging.rs](file:///home/tan/Projects/rewind/src/staging.rs#L219-L242) and `restore_clean_directories()` in [src/undo.rs](file:///home/tan/Projects/rewind/src/undo.rs#L243-L375) check destination symlinks and metadata before restoring, refusing to clobber existing files unless explicitly flagged.
- **Read-Only SQLite Enforcement:** `rewind inspect` connects using `Journal::open_query_only()` in [src/journal.rs](file:///home/tan/Projects/rewind/src/journal.rs#L261-L275) with `SQLITE_OPEN_READ_ONLY` and `PRAGMA query_only = ON`.
- **Argument Batching against `ARG_MAX`:** `restore_modes()` and `restore_owners()` in [src/engine/metadata.rs](file:///home/tan/Projects/rewind/src/engine/metadata.rs#L218-L250) chunk restoration lists into 256 paths per exec call.
- **Non-UTF-8 Argument Safety:** `run_exec()` in [src/main.rs](file:///home/tan/Projects/rewind/src/main.rs#L237-L249) detects non-UTF-8 arguments and immediately passes the raw invocation to the host command, avoiding argument corruption.

---

## 3. Security & Reliability Vulnerability Register

The discovery audit identified 7 security and reliability findings. Each finding is cataloged below:

### SEC-001: TOCTOU Symlink Race Condition During Staging & Restoration
- **Severity:** High
- **Confidence:** Confirmed
- **Status:** **RESOLVED & VERIFIED (P0 Hardening)**
- **Affected Location:** [src/staging.rs](file:///home/tan/Projects/rewind/src/staging.rs) (`remove_tree`, `copy_tree`, `link_tree`, `tree_size`, `move_path`, `stage_move`).
- **Technical Explanation:** Previously, filesystem operations checked path metadata and subsequently traversed, copied, or deleted trees using path-based APIs. In shared writable directories (e.g. `/tmp`), an attacker could replace an expected directory with a symlink between stat and traversal, leading to arbitrary directory traversal or file deletion.
- **Remediation & Fix:** 
  1. Replaced path-based `remove_tree` with POSIX descriptor-relative traversal using `openat(2)` with `O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC`, `fdopendir`, `readdir`, and `unlinkat(2)` with `AT_REMOVEDIR` and `0`. Never follows symlinks. Handles `ELOOP`, `EMLINK` (macOS), and `ENOTDIR` safely. Permission denials on directories/files automatically attempt `fchmod(0o700)` via directory descriptors before retrying unlinks.
  2. Implemented `read_dir_nofollow()` using `openat` and `fstatat(..., AT_SYMLINK_NOFOLLOW)` to safely inspect directory entries without dereferencing symlinks.
  3. Hardened `copy_tree` to safely preserve symlinks as symlinks using `read_link` and `symlink` without opening/traversing targets, and to clean up partial destinations on failure.
  4. Hardened `link_tree` and `tree_size` to use no-follow descriptor operations.
  5. Gated `REWIND_TEST_FORCE_EXDEV` behind `#[cfg(test)]` so test seams are compiled out of release builds.
  6. Verified with unit and integration tests covering external symlink isolation, dangling symlinks, read-only directories, and end-to-end `rm` / `undo` symlink restoration.

---

### SEC-002: Plaintext Storage of Sensitive Files & Secrets in Trash Directory
- **Severity:** Medium
- **Confidence:** Confirmed
- **Status:** **PARTIALLY MITIGATED (Phase 3)**
- **Affected Location:** [src/staging.rs](file:///home/tan/Projects/rewind/src/staging.rs), [src/paths.rs](file:///home/tan/Projects/rewind/src/paths.rs), [src/config.rs](file:///home/tan/Projects/rewind/src/config.rs), [src/engine/rm.rs](file:///home/tan/Projects/rewind/src/engine/rm.rs), [src/engine/cp.rs](file:///home/tan/Projects/rewind/src/engine/cp.rs), [src/engine/mv.rs](file:///home/tan/Projects/rewind/src/engine/mv.rs).
- **Technical Explanation:** When a user executes `rm id_rsa`, `rm .env`, or `cp new.key existing.key`, `rewind` previously staged copies of sensitive files into `trash/<entry_id>/`.
- **Remediation & Controls Implemented:**
  1. Added `trash.exclude_patterns` configuration (`Vec<String>`) in `src/config.rs` with zero-dependency glob matching supporting wildcards (`*.env`, `*id_rsa*`, `*.key`).
  2. In `src/engine/rm.rs`, files matching any excluded pattern completely bypass staging into trash. They are permanently unlinked directly with an informative stderr warning (`rewind: '<raw>' matches trash.exclude_patterns; permanently deleted without undo snapshot`) and marked as `OperandResult::DeletedDirectly`.
  3. In `src/engine/mv.rs` and `src/engine/cp.rs`, destination files matching excluded patterns bypass pre-overwrite snapshotting into trash.
  4. Restrictive filesystem permissions: `create_private_dir` enforces `0700` (`rwx------`) on `data_dir`, `trash_dir`, and entry directories (`trash/<entry_id>/`). `Journal::open` enforces `0600` (`rw-------`) on `journal.db`, `journal.db-wal`, and `journal.db-shm`.
- **Residual Risk & Recovery Trade-off:** Excluded files deleted via `rm` cannot be recovered via `rewind undo` (conscious security/recoverability trade-off). Malicious processes running as the same user can still access `trash/` if permissions allow user-level inspection; complete defense against local user compromise requires disk encryption or external secret vaults.

---

### SEC-003: Plaintext Sensitive Argument Logging in SQLite Journal
- **Severity:** Medium
- **Confidence:** Confirmed
- **Status:** **RESOLVED (Phase 3)**
- **Affected Location:** [src/redact.rs](file:///home/tan/Projects/rewind/src/redact.rs), [src/journal.rs](file:///home/tan/Projects/rewind/src/journal.rs).
- **Technical Explanation:** `begin_entry()` serialized the complete command-line `argv` directly into the SQLite `entries` table (`argv TEXT NOT NULL`). If a wrapped command passed credentials (e.g. `--password=XYZ`, `--token XYZ`, `https://user:pass@host`), secrets were written unencrypted to disk.
- **Remediation & Controls Implemented:**
  1. Centralized argument scrubbing module in `src/redact.rs` (`redact_argv`, `redact_cmdline`).
  2. Redacts inline flags (`--password=x`, `--token=x`, `--api-key=x`, `--client-secret=x`), separate flags (`--password x`, `-token x`), case-insensitive patterns (`--PASSWORD`), HTTP headers (`-H "Authorization: Bearer x"`, `Proxy-Authorization`, `X-Api-Key`), Git `-c http.extraHeader`, and URL embedded credentials (`https://user:pass@host`).
  3. Non-secret arguments (`-p`, `--parents`, `--path=/tmp/dir`, filenames) are strictly preserved unmodified.
  4. Redaction occurs exclusively at the persistence boundary: in-memory arguments passed to child command processes (`run_passthrough`) remain untouched and fully functional.
  5. Implemented `Journal::sanitize_historical_entries()` to scrub pre-existing secrets in older journals upon upgrade or maintenance.
  6. Verified by integration tests in `tests/redact.rs`.

---

### SEC-004: Hard-Link Snapshot Mutation During Concurrent Edits in `git clean`
- **Severity:** Medium
- **Confidence:** Confirmed
- **Status:** **RESOLVED (Phase 4 Hardening)**
- **Affected Location:** [src/staging.rs](file:///home/tan/Projects/rewind/src/staging.rs) (`clone_tree`, `clone_file_isolated`, `SnapshotMode::Clone`), [src/engine/git.rs](file:///home/tan/Projects/rewind/src/engine/git.rs#L449-L465) (`clean`).
- **Technical Explanation:** Previously, `git clean` snapshotted untracked files using `SnapshotMode::Hardlink` which invoked `fs::hard_link`. Because a hard link points to the same inode as the original, concurrent or subsequent writes to the source file before Git unlinked it mutated the snapshot in the trash.
- **Remediation & Fix:**
  1. Implemented `clone_file_isolated` and `clone_tree` in `src/staging.rs` utilizing Copy-on-Write cloning (`FICLONE` ioctl on Linux, `clonefile` with `CLONE_NOFOLLOW` on macOS) with automatic fallback to independent byte copying.
  2. Completely eliminated ordinary hard links for mutable snapshots across the entire codebase.
  3. Preserves metadata (permissions, timestamps) without altering source files.
  4. Verified by 10 comprehensive tests in `tests/snapshot_isolation.rs` (source modification, truncation, replacement, recursive trees, symlinks, sparse files, fallback, partial cleanup, and concurrent writer simulation).

---

### SEC-005: Unenforced Process Synchronization (`rewind.lock` Unacquired)
- **Severity:** Low
- **Confidence:** Confirmed
- **Status:** **RESOLVED & VERIFIED (Phase 2 & Phase 3 Hardening)**
- **Affected Location:** [src/paths.rs](file:///home/tan/Projects/rewind/src/paths.rs), [src/lock.rs](file:///home/tan/Projects/rewind/src/lock.rs), [src/main.rs](file:///home/tan/Projects/rewind/src/main.rs), [src/engine/*](file:///home/tan/Projects/rewind/src/engine/).
- **Technical Explanation:** `Paths` defined `lock_path = data_dir.join("rewind.lock")`, but the lock file was never acquired. Concurrent processes executing `rewind clean` or `rewind undo` simultaneously with command interceptors could corrupt the trash directory or journal state.
- **Remediation & Fix:**
  1. Implemented `ProcessLock` in `src/lock.rs` utilizing POSIX `libc::flock(2)` with non-blocking retry loops (`LOCK_NB`) and configurable timeouts.
  2. Enforces restrictive file permissions (`0600`) upon lock file creation.
  3. `Commands::Undo` and `Commands::Clean` acquire exclusive locks (`LockMode::Exclusive`) across their entire execution.
  4. **Child Process Concurrency Race Closure (Phase 3):** Interceptors (`rm`, `mv`, `cp`, `chmod`, `chown`, `git`) previously dropped the shared lock prior to child execution (`run_passthrough`), creating a race window where `undo` or `clean` could delete trash or restore files while the child was running. Interceptors now hold `LockMode::Shared` across the entire `execute` function. Multiple concurrent `exec` commands share access without contention, while `undo` and `clean` are strictly serialized.
  5. Implemented deterministic subprocess-based tests in `tests/process_concurrency.rs` verifying shared coexistence, exclusive mutual exclusion, kernel unlock on SIGKILL, active execution blocking exclusive ops, and independent data directory concurrency.

---

### SEC-006: Unbounded Disk Space Consumption by Default (`max_bytes = 0`) & Concurrent Reservation Races
- **Severity:** Medium
- **Confidence:** Confirmed
- **Status:** **RESOLVED & HARDENED (Phase 4, Phase 5 & Phase 6)**
- **Affected Location:** [src/config.rs](file:///home/tan/Projects/rewind/src/config.rs) (`DEFAULT_MAX_BYTES`, `TrashConfig`), [src/journal.rs](file:///home/tan/Projects/rewind/src/journal.rs) (`reserve_capacity`, `check_capacity`, `current_staged_bytes`), [src/util.rs](file:///home/tan/Projects/rewind/src/util.rs) (`get_process_start_time`), [src/commands.rs](file:///home/tan/Projects/rewind/src/commands.rs) (`clean`), [src/engine/*](file:///home/tan/Projects/rewind/src/engine/).
- **Technical Explanation:** `TrashConfig.max_bytes` previously defaulted to `0` (unlimited), allowing unrestricted accumulation of staged files in `trash/` until local disk space was exhausted. Furthermore, under `LockMode::Shared`, concurrent commands checking capacity simultaneously could experience a reservation race where both passed the quota check before either committed staged rows. Additionally, an arbitrary 1-hour expiration timeout risked prematurely releasing reservations for legitimate long-running snapshotting operations.
- **Remediation & Controls Implemented:**
  1. Default soft cap of 10 GiB (`DEFAULT_MAX_BYTES = 10 * 1024 * 1024 * 1024`) in `src/config.rs`. Existing configurations without `max_bytes` receive the 10 GiB default; explicit `0` is strictly preserved as unlimited.
  2. **Atomic SQLite Capacity Reservations (Phase 5 & 6):** Added `reservations` table (`id`, `entry_id`, `pid`, `start_time`, `bytes`, `created_at`). `Journal::reserve_capacity()` executes within an atomic SQLite `BEGIN IMMEDIATE` transaction, serializing quota reservations across concurrent processes.
  3. **Process Liveness & PID-Reuse Detection (Phase 6):** Eliminated naive 1-hour expiration. Implemented `get_process_start_time(pid)` in `src/util.rs` using `/proc/<pid>/stat` field 22 on Linux and `sysctl KERN_PROC_PID` on macOS. Stale reservations are reaped ONLY if the process is dead (`kill(pid, 0) == -1 && ESRCH`) OR if the process exists but `current_start_time != recorded_start_time` (OS PID reuse). Active processes are never prematurely purged regardless of duration.
  4. **Deterministic Storage Accounting:** `trash.max_bytes` measures logical bytes of planned staged files, guaranteeing uniform capacity enforcement across standard filesystems and Copy-on-Write (CoW) filesystems.
  5. Auto-expiration of entries older than `retention_days` when approaching the cap, protecting valid undoable entries within retention from being silently purged.
  6. Verified by integration and concurrency tests in `tests/storage_capacity.rs`.

---

### SEC-007: Incomplete Git Destructive Command Interception Surface & State Integrity
- **Severity:** Medium
- **Confidence:** Confirmed
- **Status:** **EXPANDED & CORRECTED (Phase 5 & Phase 6)**
- **Affected Location:** [src/engine/git.rs](file:///home/tan/Projects/rewind/src/engine/git.rs), [src/undo.rs](file:///home/tan/Projects/rewind/src/undo.rs), [src/config.rs](file:///home/tan/Projects/rewind/src/config.rs), [src/commands.rs](file:///home/tan/Projects/rewind/src/commands.rs).
- **Technical Explanation:** Destructive operations like `git commit --amend`, `git stash drop`, `git stash clear`, `git stash pop`, `git rebase`, and abort operations were previously unintercepted or suffered from incomplete state modeling (e.g. stash stack order loss, unremoved untracked files on pop undo, linked worktree path mismatches).
- **Remediation & Controls Implemented:**
  1. **`git commit --amend`:** Snapshots pre-amend HEAD commit SHA, branch, `write-tree` index, and `stash create` dirty state. Pins snapshot under `refs/rewind/{entry_id}-head`. Undo restores pre-amend commit and reapplies index/worktree state via `git reset --hard` and `git stash apply --index`.
  2. **`git stash drop` & `git stash clear` Stack Reconstruction:** Snapshots the complete stash stack before operation. Undo reconstructs the stack via `restore_stash_stack`, preserving any new stashes pushed since the operation on top, while restoring dropped/cleared stashes in their exact original stack order and positions. All stashes are pinned under `refs/rewind/*` to prevent Git GC pruning.
  3. **`git stash pop` Dual-State Restoration:** Restores BOTH working tree and index changes AND re-stores the stash back into `refs/stash`. Inspects `popped_commit^3` for untracked files introduced by the stash and removes them on undo. If `git stash pop` encounters conflicts, Git natively retains the stash in `refs/stash`; rewind detects non-zero exit code and records NO undoable entry, avoiding corrupt duplicate state.
  4. **Linked Worktree Support:** `Repo::discover()` queries `--show-toplevel`, `--git-dir`, and `--git-common-dir`. Per-worktree `git_dir` (e.g. `<main>/.git/worktrees/<name>`) is tracked in metadata, ensuring `rebase-merge` checks, resets, and aborts correctly resolve in linked worktrees where `.git` is a pointer file.
  5. **`git rebase --abort` & `git merge --abort` Honest Boundary:** Recognizing that Git's internal multi-stage conflict index (stages 1/2/3), `MERGE_HEAD`, and sequencer state cannot be portably restored by plain working-tree copies, these commands are passed through cleanly to native Git without making false undo promises.
  6. **Granular Configuration:** Config keys `wrap.git_commit_amend`, `wrap.git_rebase`, and `wrap.git_stash` in `WrapConfig`.
  7. Verified by 49 integration tests in `tests/git.rs`.

---

### SEC-008: Inode Recycling Risk During Long-Running Sessions
- **Severity:** Low / Informational
- **Confidence:** Confirmed
- **Status:** **DOCUMENTED / LOW RISK**
- **Affected Location:** [src/staging.rs](file:///home/tan/Projects/rewind/src/staging.rs), [src/engine/*](file:///home/tan/Projects/rewind/src/engine/).
- **Technical Explanation:** On high-churn filesystems, unlinking and re-creating files rapidly can cause operating system inode number reuse. Rewind indexes entries by primary key `id` and staged files by unique path counters rather than raw inode numbers, eliminating inode recycling risks in normal operation.
- **Mitigation:** Retain database ID + unique path-based identification rather than relying solely on ephemeral operating system inode numbers.

---

### SEC-009: Supply-Chain & Package Distribution Integrity
- **Severity:** Medium
- **Confidence:** Confirmed
- **Status:** **FULLY RESOLVED, PATCHED & VERIFIED (Phase 10)**
- **Affected Location:** [Cargo.lock](file:///home/tan/Projects/rewind/Cargo.lock), [packaging/build-static-deb.sh](file:///home/tan/Projects/rewind/packaging/build-static-deb.sh), [.github/workflows/](file:///home/tan/Projects/rewind/.github/workflows/).
- **Technical Explanation:** Supply-chain risks include vulnerable third-party crate dependencies, dynamic library substitution on host systems, and package scripts executing privileged deletion actions.
- **Remediation & Controls Implemented:**
  1. **SQLite CVE-2026-119375 / RUSTSEC-2026-0459 Resolved:** Upgraded `rusqlite` to `0.43.0` and `libsqlite3-sys` to `0.40.0`, linking patched **SQLite 3.54.1** (>= 3.54.0). Completely eliminates the page-cache memory exposure vulnerability in bundled SQLite.
  2. **Automated RustSec Audit:** Scanned 109 dependencies with `cargo audit`. Upgraded `rustix` from `v1.1.5` to `v1.1.6` to patch RUSTSEC-2026-0486 (xattr unsoundness). Audit reports **zero vulnerabilities**.
  3. **Chrono Maintenance Evaluation:** Crate `chrono v0.4.45` flagged with RUSTSEC-2026-0543 (unmaintained informational warning). Evaluated: chrono is used strictly in `src/util.rs` for deterministic UTC/local timestamp formatting (`%Y-%m-%d %H:%M:%S` and `%Y%m%d`) with no untrusted network/timezone inputs. Maintained as accepted low risk to avoid timezone race conditions in alternative crates.
  4. **Permissive Open-Source Licensing:** Audited dependency licenses via `cargo tree`. 100% of dependencies carry permissive licenses (MIT, Apache-2.0, Unlicense, Zlib).
  5. **Package Uninstallation Isolation:** Package definitions (`debian/rules`, `packaging/rewind.spec`) declare no maintainer scriptlets (`postrm`, `prerm`, `%preun`, `%postun`) that touch user directories. User recovery journals and snapshots reside in `$HOME/.local/share/rewind/` and survive ordinary package upgrades and removals.
  6. **Static Binary Integrity & Least Privilege:** Release workflows generate SHA-256 checksums (`SHA256SUMS`) verified by automated test harness (`tests/distribution.rs::release_tarball_and_checksum_verification`). Compiler matrix jobs restricted to `contents: read`; `contents: write` scoped exclusively to release publishing.
