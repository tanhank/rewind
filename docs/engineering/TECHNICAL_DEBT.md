# Technical Debt & Architecture Roadmap: rewind

> **Document Version:** 1.10.0  
> **Target Software Release Version:** `0.2.0`  
> **Commit SHA:** `08a325fc468709928f8e7f5401b418a69d253d66` (+ Phases 2-10 Hardening)  
> **Classification:** Engineering Assessment & Prioritized Debt Register

---

## 1. Architectural & Code Quality Assessment

The codebase exhibits exceptional craftsmanship: zero clippy warnings under strict `-D warnings`, zero format discrepancies, strong module boundaries, and comprehensive end-to-end integration tests.

However, several architectural compromises and areas of technical debt were identified during discovery:

### 1.1 Unused Synchronization Primitive (`rewind.lock`) — RESOLVED (Phase 2)
- **Location:** [src/paths.rs](file:///home/tan/Projects/rewind/src/paths.rs), [src/lock.rs](file:///home/tan/Projects/rewind/src/lock.rs)
- **Status:** **Resolved in Phase 2.** Implemented POSIX `flock(2)` process synchronization (`ProcessLock`) with shared locks for command interceptors (scoped around staging/reconciliation) and exclusive locks for `undo` and `clean`. Restrictive mode `0600` enforced. Concurrency test suite added in `tests/lock.rs`.

### 1.2 Custom Hand-Rolled Argument Parsing in Interceptors — RESOLVED (Phase 7)
- **Location:** [src/engine/rm.rs](file:///home/tan/Projects/rewind/src/engine/rm.rs), [src/engine/transfer.rs](file:///home/tan/Projects/rewind/src/engine/transfer.rs), [src/engine/metadata.rs](file:///home/tan/Projects/rewind/src/engine/metadata.rs), [src/engine/git.rs](file:///home/tan/Projects/rewind/src/engine/git.rs).
- **Status:** **Resolved in Phase 7.** Unified into [src/engine/args.rs](file:///home/tan/Projects/rewind/src/engine/args.rs) via `ArgScanner`, `ArgToken`, and `ShortCluster`. Provides reusable, zero-copy support for short option clusters (`-rf`, `-fdx`), attached and separated option values (`-tDIR`, `-t DIR`, `--target-directory=DIR`), `--` operand boundary, and standalone `-`. Verified across all coreutils interceptors and regression tested in `tests/parser.rs`.

### 1.3 Synchronous Directory Tree Traversals on Large Filesystems — OPTIMIZED (Phase 7)
- **Location:** [src/staging.rs](file:///home/tan/Projects/rewind/src/staging.rs) (`tree_size`), [src/engine/metadata.rs](file:///home/tan/Projects/rewind/src/engine/metadata.rs) (`walk`), [src/commands.rs](file:///home/tan/Projects/rewind/src/commands.rs) (`summarize_tree`).
- **Status:** **Optimized in Phase 7.** Sized directory traversal implemented in `read_dir_nofollow` returning entry sizes from `fstatat` directly. Eliminated redundant `lstat` calls for files and symlinks in `tree_size`, achieving a **2.28x speedup (>1.23M items/s)** on 10,000-file directories.

### 1.4 CI Testing Gap: macOS Integration Testing — RESOLVED & MAINTAINED (Phases 4, 7, 11)
- **Location:** [.github/workflows/ci.yml](file:///home/tan/Projects/rewind/.github/workflows/ci.yml)
- **Status:** **Resolved in Phase 4 and maintained across Phases 7 and 11.** Configured full test gates on macOS runners. In Phase 11, updated runner matrix to dual active runners: `macos-15` (Apple Silicon) and `macos-15-intel` (Intel x86_64) across all workflows (`ci.yml`, `validate-release.yml`, `release.yml`).

### 1.5 CLI Ergonomics & Developer Workflow — RESOLVED (Phase 8)
- **Location:** [src/main.rs](file:///home/tan/Projects/rewind/src/main.rs), [src/commands.rs](file:///home/tan/Projects/rewind/src/commands.rs), [src/undo.rs](file:///home/tan/Projects/rewind/src/undo.rs), [src/journal.rs](file:///home/tan/Projects/rewind/src/journal.rs), [shell/](file:///home/tan/Projects/rewind/shell/).
- **Status:** **Resolved in Phase 8.** Implemented Unix-native CLI design: standardized exit codes (0, 1, 2, 3, 127), machine-readable JSON across all reporting commands, targeted entry undo (`--id`), multi-criteria log filtering, `config list`, broken-pipe safe completions, and expanded shell wrappers for `git commit`, `rebase`, and `stash`.

### 1.6 Distribution Engineering & Packaging Portability — RESOLVED (Phase 9)
- **Location:** [packaging/build-static-deb.sh](file:///home/tan/Projects/rewind/packaging/build-static-deb.sh), [.github/workflows/release.yml](file:///home/tan/Projects/rewind/.github/workflows/release.yml), [tests/distribution.rs](file:///home/tan/Projects/rewind/tests/distribution.rs).
- **Status:** **Resolved in Phase 9.** Added GNU/BSD `ar` + `tar` fallback to `build-static-deb.sh`, enabling static `.deb` package generation across non-Debian host platforms. Configured multi-architecture release workflow for macOS Darwin runners (`macos-15`) and static musl binaries. Implemented automated distribution test suite verifying metadata consistency, legacy database migrations, and uninstallation recovery data retention.

---

## 2. Prioritized Engineering Roadmap

### Priority 0: Critical Security & Operational Risks
- **[P0-1] Enforce Process Lock on Critical Operations:** **COMPLETED (Phase 2 & Phase 3).** Implemented POSIX `flock(2)` via `src/lock.rs` across `undo`, `clean`, and all command interceptors (`rm`, `mv`, `cp`, `chmod`, `chown`, `git`). Scoped shared lock across the entire command execution in Phase 3 to eliminate the child process race window. Subprocess tests added in `tests/process_concurrency.rs`.
- **[P0-2] Secure Shared-Directory Traversals against TOCTOU:** **COMPLETED (Phase 2 & Phase 3).** Rewrote directory traversal and tree removal in `src/staging.rs` to use descriptor-relative `openat(2)`, `O_NOFOLLOW`, `unlinkat(2)`, and `fstatat(2)`. Hardened `copy_tree`, `link_tree`, and `tree_size`. Gated test seams behind `cfg(test)`. Added adversarial symlink tests.
- **[P0-3] Prevent Snapshot Mutation via Hard-Links (SEC-004):** **COMPLETED (Phase 4).** Replaced ordinary hard links in `src/staging.rs` with `clone_tree` and `clone_file_isolated`, utilizing Copy-on-Write cloning (`FICLONE` on Linux, `clonefile` on macOS) with independent copy fallback. Tested in `tests/snapshot_isolation.rs`.

### Priority 1: High-Impact Bug Fixes & Reliability Improvements
- **[P1-1] Enable Integration Test Suite on macOS CI:** **COMPLETED (Phase 4).** Updated `.github/workflows/ci.yml` to run full `cargo fmt`, `cargo clippy`, `cargo test`, and `cargo build --release` on `macos-14` runners.
- **[P1-2] Scrub Credentials from Command History (SEC-003):** **COMPLETED (Phase 3).** Centralized argument scrubbing module `src/redact.rs` with `redact_argv` scrubbing inline/separate passwords, tokens, API keys, client secrets, HTTP authorization headers, Git `-c` extra headers, and embedded URL credentials before SQLite insertion. Added `Journal::sanitize_historical_entries()` and tests in `tests/redact.rs`.
- **[P1-3] Default Soft Cap for Trash Storage (SEC-006):** **COMPLETED (Phase 4, Phase 5 & Phase 6).** Set `TrashConfig.max_bytes` to 10 GiB (`DEFAULT_MAX_BYTES`), implemented pre-execution staging capacity verification in `Journal::reserve_capacity` with atomic SQLite `BEGIN IMMEDIATE` transactions. In Phase 6, eliminated naive 1-hour timeout and implemented OS process `start_time` verification (`/proc/<pid>/stat` on Linux, `sysctl KERN_PROC_PID` on macOS) to cleanly detect PID reuse while never prematurely expiring active long-running operations. Verified in `tests/storage_capacity.rs`.

### Priority 2: Important Engineering Improvements
- **[P2-1] Unify Coreutils Option Parsing:** **COMPLETED (Phase 7).** Created `src/engine/args.rs` with `ArgScanner`, `ArgToken`, and `ShortCluster`. Refactored `rm.rs`, `transfer.rs`, `metadata.rs`, and `git.rs` to use shared argument scanner. Regression tested in `tests/parser.rs`.
- **[P2-2] Sized Traversal & Copy Buffer Optimizations:** **COMPLETED (Phase 7).** Optimized `read_dir_nofollow` to return entry sizes from `fstatat`, eliminating redundant `lstat` calls in `tree_size` and achieving a 2.28x speedup. Increased non-CoW copy buffers to 128 KB, improving throughput by +71%. Documented in `docs/engineering/PERFORMANCE.md`.
- **[P2-3] SQLite WAL Automatic Checkpointing:** **COMPLETED (Phase 7).** Implemented `Journal::wal_checkpoint` (`PRAGMA wal_checkpoint(TRUNCATE)`) and invoked it automatically during `rewind clean` to reclaim disk space immediately. Removed duplicate table migrations in `Journal::open`.

### Priority 3: Desirable Features & Maintainability
- **[P3-1] Expand Destructive Git Interception (SEC-007):** **COMPLETED & HARDENED (Phase 5 & Phase 6).** Implemented interception, atomic ref pinning, and reliable undo orchestration for `git commit --amend`, `git stash drop`, `git stash clear`, `git stash pop`, and `git rebase`. In Phase 6, hardened stash stack reconstruction to preserve exact stack index ordering and subsequent stashes, implemented dual-state restoration for `stash pop` (reverting worktree, index, untracked files from `commit^3`, and stash entry), added conflict detection (no false undo recorded), added linked worktree support via `--show-toplevel`, `--git-dir`, `--git-common-dir`, and established an honest unsupported passthrough boundary for `rebase/merge --abort`. Verified by 49 integration tests in `tests/git.rs`.
- **[P3-2] Selective Secret Exclusion Rules (SEC-002):** **PARTIALLY COMPLETED (Phase 3).** Implemented `trash.exclude_patterns` in `src/config.rs` with glob matching to bypass trash staging on `rm` (with permanent deletion and stderr warning) and skip backup snapshots in `cp` and `mv`. Enforced `0700` directory and `0600` journal permissions.
- **[P3-3] Interactive Undo Selection UI (TUI):** **REJECTED / ARCHITECTURAL PRINCIPLE (Phase 8).** Interactive terminal UIs (TUI), curses dashboards, menu-driven navigation, desktop windows, and background daemons are explicitly rejected to preserve Rewind's identity as a lightweight, pipeable, non-interactive Unix CLI utility (in the spirit of `curl`, `rsync`, `git`, `find`). Fine-grained targeting is provided via `rewind undo --id <ID>` (or `-i <ID>`) and `rewind log --cmd <CMD> --undoable --status <STATUS>`.
- **[P3-4] Unix CLI Ergonomics & Developer Workflow:** **COMPLETED (Phase 8).** Implemented standardized exit codes (0: success, 1: general/recovery error, 2: invalid syntax/arguments, 3: refused operation, 127: missing binary), schema-stable `--json` output across `status`, `log`, `inspect`, and `clean`, shell completion generation with broken pipe resilience, global `--color` control (`auto|always|never`, `NO_COLOR`, `TERM=dumb`), `config list`, and updated man pages and packaging specs (`debian/control`, `packaging/rewind.spec`).
- **[P3-5] Upstream rusqlite Dependency Maintenance:** **RESOLVED (Phase 10).** Upgraded `rusqlite` to `0.43.0` and `libsqlite3-sys` to `0.40.0`, embedding SQLite **3.54.1** and eliminating CVE-2026-119375 / RUSTSEC-2026-0459. Rust MSRV updated to `1.89+`.
- **[P3-6] Chrono Maintenance Tracking:** Informational advisory RUSTSEC-2026-0543 identifies chrono as unmaintained. Minimal usage in `src/util.rs` (safe local formatting, no vulnerability); track for future migration to maintained alternative if an upstream fork or replacement stabilizes without timezone lock issues.
- **[P3-7] Remote CI Pipeline Execution on First Authorized Tag/Branch Push:** **VALIDATED & GATED (Phase 11).** All packaging, hardening, and non-publishing validation workflows (`validate-release.yml`, `release.yml`, `ci.yml`) are configured and verified locally. Because all changes across Phases 2–11 have been maintained in the local working tree per instructions without pushing commits or tags, remote execution on GitHub Actions runners (`macos-15`, `cross` musl containers, Fedora container spec-check) is awaiting user authorization to push a branch or release tag.

