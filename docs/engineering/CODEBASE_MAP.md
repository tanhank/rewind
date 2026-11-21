# Codebase Map: rewind

> **Target Commit SHA:** `08a325fc468709928f8e7f5401b418a69d253d66`  
> **Last Verified:** 2026-11-20  
> **Primary Purpose:** Accurate navigation index for future coding agents to jump directly to code locations without recursive repository exploration.

---

## 1. Directory Structure Overview

```
.
├── .github/workflows/      # CI/CD: lint, test, release, and APT packaging
├── assets/                 # Project icons, banners, and preview scripts
├── debian/                 # Debian packaging definitions (changelog, control, rules)
├── docs/                   # Man page and domain documentation
│   ├── agents/             # Agent guidelines for triage, issues, and glossary
│   └── engineering/        # Core engineering knowledge base (this directory)
├── packaging/              # Distro packaging scripts (deb, rpm, static musl)
├── shell/                  # Shell integration scripts for bash, zsh, and fish
├── src/                    # Rust source tree (binary CLI and library)
│   └── engine/             # Command-specific interceptor implementations
├── benches/                # Repeatable performance benchmark suites (workloads.rs)
└── tests/                  # Integration test suites and isolated sandbox harness
```

---

## 2. Source Code Inventory (`src/`)

### 2.1 Core Library & CLI Infrastructure

| File Path | Primary Responsibility | Key Types & Symbols | Callers & Dependencies | Affected Workflows |
|---|---|---|---|---|
| [src/main.rs](file:///home/tan/Projects/rewind/src/main.rs) | CLI entry point, argument parsing via `clap`, exit code management, subcommand dispatcher. | `Cli`, `Commands`, `ShellName`, `ConfigCommand`, `run()`, `run_exec()`, `passthrough_os()` | Invokes `engine::dispatch`, `undo::run`, `commands::*`, `config::load`. | CLI execution, shell hook dispatch, configuration management. |
| [src/lib.rs](file:///home/tan/Projects/rewind/src/lib.rs) | Library root exporting public modules and error types. | `pub mod commands, config, engine, error, journal, paths, staging, undo, util` | Re-exports `Error`, `Result`. Consumed by `src/main.rs` and all integration tests under `tests/`. | Library interface for internal modules and test suites. |
| [src/paths.rs](file:///home/tan/Projects/rewind/src/paths.rs) | Resolves XDG and custom runtime paths (`data_dir`, `trash_dir`, `journal_path`, `config_path`, `lock_path`), process locking helpers. | `Paths`, `Paths::discover()`, `Paths::ensure()`, `Paths::lock_exclusive()`, `Paths::lock_shared()`, `create_private_dir()` | Consumed by `main.rs`, `journal.rs`, `staging.rs`, `commands.rs`, `engine::*`. | Path discovery, permission enforcement (`0700`), sandbox overrides, process synchronization. |
| [src/lock.rs](file:///home/tan/Projects/rewind/src/lock.rs) | Process synchronization via POSIX `flock(2)` on `rewind.lock`. Non-blocking retry loops, configurable timeouts, restrictive permissions (`0600`), RAII unlock. | `ProcessLock`, `LockMode` (Shared, Exclusive), `ProcessLock::acquire()`, `ProcessLock::acquire_exclusive()`, `ProcessLock::acquire_shared()` | Consumed by `paths.rs`, `main.rs`, `engine::*`. | Cross-process mutual exclusion (`undo`, `clean`) and shared staging coordination. |
| [src/redact.rs](file:///home/tan/Projects/rewind/src/redact.rs) | Sensitive argument detection and credential redaction before persistence or logging. Preserves non-sensitive arguments and untouched in-memory child process exec. | `redact_argv()`, `redact_cmdline()`, `redact_inline_flag()`, `is_secret_flag()`, `redact_url_credentials()`, `redact_header_value()` | Consumed by `journal.rs`. | Command logging, journal persistence security (SEC-003). |
| [src/config.rs](file:///home/tan/Projects/rewind/src/config.rs) | TOML configuration model, key-value validator, default policies (`DEFAULT_MAX_BYTES` 10 GiB), atomic disk writing. | `Config`, `WrapConfig`, `TrashConfig`, `JournalConfig`, `GitConfig`, `DEFAULT_MAX_BYTES`, `Config::load()`, `Config::save()`, `Config::set()` | Consumed by `main.rs`, `engine::dispatch`, `commands::clean`, `commands::status`. | Feature toggles, retention policies, git snapshot settings. |
| [src/journal.rs](file:///home/tan/Projects/rewind/src/journal.rs) | SQLite persistence engine for undo history and staged file records. Schema definition, storage checks, and queries. | `Journal`, `Entry`, `EntryStatus`, `StagedItem`, `NewStaged`, `StagedKind`, `Stats`, `SCHEMA`, `current_staged_bytes()`, `check_capacity()` | Wrapped by `rusqlite`. Used by `main.rs`, `engine::*`, `undo.rs`, `commands::*`. | Durable recording of commands, capacity checking, state queries, retention pruning. |
| [src/staging.rs](file:///home/tan/Projects/rewind/src/staging.rs) | Moves/copies/clones data into trash, manages collision-free paths, CoW clones, copies trees, safe restores. | `Stager`, `StagedPayload`, `SnapshotMode` (Copy, Clone), `clone_tree()`, `clone_file_isolated()`, `copy_tree()`, `move_path()`, `remove_tree()`, `tree_size()`, `absolutize()` | Used by `engine::*`, `undo.rs`, `commands::inspect`, `commands::clean`. | File trash operations, isolated snapshotting (SEC-004), collision safety, undo restoration. |
| [src/undo.rs](file:///home/tan/Projects/rewind/src/undo.rs) | Reversal execution coordinator. Processes entries LIFO, supports targeted entry undo (`UndoTarget::Entry(id)`), handles resumable partial failures. | `UndoReport`, `UndoTarget`, `run()`, `run_target()`, `undo_entry()`, `undo_mv()`, `restore_clean_directories()`, `undo_metadata()` | Calls `journal::mark_undone`, `stager.restore`, `git::undo_*`, `metadata::restore_*`. | `rewind undo [N]`, `rewind undo --id <ID>`. |
| [src/commands.rs](file:///home/tan/Projects/rewind/src/commands.rs) | User-facing query and maintenance subcommands (`log`, `inspect`, `clean`, `status`). | `log()`, `inspect()`, `status()`, `clean()`, `analyze()`, `CleanOptions`, `CleanReport`, `TreeSummary` | Queries `Journal` and filesystem `Stager`. Formats text and JSON. | `rewind log`, `rewind inspect`, `rewind clean`, `rewind status`. |
| [src/error.rs](file:///home/tan/Projects/rewind/src/error.rs) | Centralized error type definition (`thiserror`), exit code mapping (`exit_code()`), and I/O context extension trait. | `Error` (Io, Config, Journal, Staging, Git, InvalidInput, Refused), `Error::exit_code()`, `IoContext`, `io_err()`, `path_to_string()` | Used across all internal modules for fail-safe error bubbling. | Error reporting, standard Unix exit code mapping (0, 1, 2, 3). |
| [src/util.rs](file:///home/tan/Projects/rewind/src/util.rs) | Zero-dependency pure utilities (human byte sizing, durations, timestamps, booleans). | `now_unix()`, `format_timestamp()`, `time_ago()`, `human_size()`, `parse_duration()`, `parse_bool()`, `ellipsize()` | Used by `commands.rs`, `journal.rs`, `config.rs`. | Display formatting, retention string parsing. |

---

### 2.2 Interception Engines (`src/engine/`)

| File Path | Intercepted Command | Primary Logic & Mechanics | Passthrough Triggers |
|---|---|---|---|
| [src/engine/mod.rs](file:///home/tan/Projects/rewind/src/engine/mod.rs) | Engine dispatcher | Dispatches commands to submodules, runs transparent child processes via `run_passthrough()`, extracts exit codes. | Unknown commands, non-UTF-8 arguments. |
| [src/engine/rm.rs](file:///home/tan/Projects/rewind/src/engine/rm.rs) | `rm` | Re-implements coreutils `rm` behavior. Renames operands directly into trash ($O(1)$). Interactive prompt `-i`/`-I` support, root protection `--preserve-root`. | `--help`, `--version`, `--one-file-system`, trailing slash on symlinks (`link/`). |
| [src/engine/mv.rs](file:///home/tan/Projects/rewind/src/engine/mv.rs) | `mv` | Analyzes source/destination pairs. Snapshots destinations before overwrite. Runs host `mv`. Reconciles which moves succeeded. | `--backup`, exchange mode, unresolvable pairs. |
| [src/engine/cp.rs](file:///home/tan/Projects/rewind/src/engine/cp.rs) | `cp` | Snapshots destinations that would be overwritten. Runs host `cp`. Records newly created destinations as `StagedKind::Created`. | Unresolvable destinations, `--backup`, unknown flags. |
| [src/engine/chmod.rs](file:///home/tan/Projects/rewind/src/engine/chmod.rs) | `chmod` | Walks operand paths (recursively if `-R`), records initial permission modes in JSON, runs host `chmod`. | Empty targets, serialization errors. |
| [src/engine/chown.rs](file:///home/tan/Projects/rewind/src/engine/chown.rs) | `chown` | Walks operand paths (recursively if `-R`), records initial `uid:gid` in JSON, runs host `chown`. | Empty targets, serialization errors. |
| [src/engine/git.rs](file:///home/tan/Projects/rewind/src/engine/git.rs) | `git reset`, `clean`, `checkout`, `restore`, `branch`, `commit --amend`, `stash drop/clear/pop`, `rebase` | Strategies: 1) `reset --hard`, `commit --amend`, `rebase`: pins HEAD, index, stash to `refs/rewind/*`, tracks linked worktrees; 2) `clean`: CoW clone snapshot + empty directory tracking; 3) `checkout`/`restore`: snapshot index tree + overwritten files; 4) `branch -d/-D`: pin branch commit to `refs/rewind/*`; 5) `stash`: pin commits and reconstruct stack preserving exact positions and subsequent stashes (`pop` reverts worktree and untracked files); 6) `rebase/merge --abort`: honest passthrough boundary. | Non-destructive git commands (`commit` without `--amend`, `push`, `status`), interactive `-p`/`--patch`, `rebase/merge --abort`, unmerged trees. |
| [src/engine/transfer.rs](file:///home/tan/Projects/rewind/src/engine/transfer.rs) | `mv` / `cp` Shared | Parses GNU transfer arguments (`-t`, `-T`, `-n`, `-r`, `-f`), resolves source-to-dest mapping pairs. | Ambiguous syntax, multi-source with `-T`. |
| [src/engine/metadata.rs](file:///home/tan/Projects/rewind/src/engine/metadata.rs) | `chmod` / `chown` Shared | Filesystem walker collecting mode/owner arrays, batches undo operations in 256-path chunks to avoid `ARG_MAX`. | More than 200,000 entries (safety cap). |
| [src/engine/args.rs](file:///home/tan/Projects/rewind/src/engine/args.rs) | Coreutils Arg Parser | Unified zero-copy command-line scanner (`ArgScanner`, `ArgToken`, `ShortCluster`). Reusable handling for short flag clustering (`-rf`), inline/separated option values (`-tDIR`, `-t DIR`), `--` boundary, and `-`. | Unknown options, passthrough conditions. |

---

## 3. Shell Integration & Scripts (`shell/`)

- [shell/bash.sh](file:///home/tan/Projects/rewind/shell/bash.sh): Defines `__rewind_run` and alias functions for `rm`, `mv`, `cp`, `chmod`, `chown`, and selective `git` routing. Emitted via `rewind init bash`.
- [shell/zsh.sh](file:///home/tan/Projects/rewind/shell/zsh.sh): Equivalent zsh hooks utilizing `(( $+commands[rewind] ))` check and `REWIND_DISABLE` guard. Emitted via `rewind init zsh`.
- [shell/fish.fish](file:///home/tan/Projects/rewind/shell/fish.fish): Native fish shell functions with `switch $argv[1]` for git. Emitted via `rewind init fish`.

---

## 4. Test Suite Inventory (`tests/`)

- [tests/common/mod.rs](file:///home/tan/Projects/rewind/tests/common/mod.rs): Sandboxed test harness (`Sandbox`). Creates temporary isolated `$HOME`, `$XDG_DATA_HOME`, and work directories for parallel test isolation.
- [tests/cli.rs](file:///home/tan/Projects/rewind/tests/cli.rs): Verifies CLI commands (`--help`, `--version`, `config`, `log --json`, `status --json`, `clean --dry-run`, `clean --all`, `inspect`).
- [tests/rm.rs](file:///home/tan/Projects/rewind/tests/rm.rs): Integration tests for `rm` files, trees, symlinks, missing files, empty directory removal (`-d`), `-v`, and interactive confirmations.
- [tests/transfer.rs](file:///home/tan/Projects/rewind/tests/transfer.rs): End-to-end tests for `mv` overwrites, directory moves, no-clobber, and `cp` overwrites and newly created tree removals.
- [tests/git.rs](file:///home/tan/Projects/rewind/tests/git.rs): Tests for `git reset --hard` (worktree, index, stash restoration), `git clean -fd` and `-fx`, `git checkout --`, `git restore`, `git branch -D`, `git commit --amend`, `git stash drop / clear / pop`, `git rebase`, and `git rebase / merge --abort`.
- [tests/lock.rs](file:///home/tan/Projects/rewind/tests/lock.rs): Concurrency and synchronization tests for `ProcessLock` (shared concurrency, exclusive mutual exclusion, timeouts, thread contention handoff).
- [tests/process_concurrency.rs](file:///home/tan/Projects/rewind/tests/process_concurrency.rs): Deterministic OS subprocess tests for `ProcessLock` (multiprocess shared coexistence, exclusive mutual exclusion, kernel flock release on SIGKILL, active execution blocking exclusive ops, independent data dir concurrency).
- [tests/redact.rs](file:///home/tan/Projects/rewind/tests/redact.rs): Comprehensive regression and end-to-end tests for sensitive argument redaction and historical journal migration (SEC-003).
- [tests/sensitive_files.rs](file:///home/tan/Projects/rewind/tests/sensitive_files.rs): Integration tests for `trash.exclude_patterns` (`rm` bypass with warning, `cp`/`mv` snapshot skip) and restrictive filesystem permissions `0700`/`0600` (SEC-002).
- [tests/parser.rs](file:///home/tan/Projects/rewind/tests/parser.rs): Coreutils CLI parity and argument scanner regression tests (short-flag clustering, attached/separate option values, `--` delimiter, standalone `-`, interspersed operands, unicode paths).
- [tests/shell.rs](file:///home/tan/Projects/rewind/tests/shell.rs): Real subprocess tests spawning `bash` and `zsh` subshells to verify script hooking, environment bypass (`REWIND_DISABLE=1`), and coreutils error compatibility under `LC_ALL=C`.
- [tests/snapshot_isolation.rs](file:///home/tan/Projects/rewind/tests/snapshot_isolation.rs): Regression tests for SEC-004 verifying snapshot independence from subsequent file writes, truncations, replacements, sparse files, and concurrent writer simulations.
- [tests/storage_capacity.rs](file:///home/tan/Projects/rewind/tests/storage_capacity.rs): Integration tests for SEC-006 testing 10 GiB default cap, custom limits, refusal of destructive operations when capacity is exceeded, and preservation of valid undo entries.
- [tests/distribution.rs](file:///home/tan/Projects/rewind/tests/distribution.rs): Packaging validation, metadata consistency across Cargo/Debian/RPM/Man, backwards-compatible SQLite migrations, configuration and recovery data preservation on uninstallation/upgrade, and SHA-256 release verification.

---

## 5. Navigation Index: Where to Make Changes

| If You Need To... | Look In File(s) | Key Symbols |
|---|---|---|
| Add support for a new destructive command (e.g. `truncate`) | `src/engine/mod.rs`, `src/config.rs`, `shell/*.sh` | `engine::dispatch()`, `WrapConfig`, `__rewind_run` |
| Add a new Git subcommand (e.g. `git merge --abort`) | `src/engine/git.rs`, `shell/*.sh`, `src/config.rs` | `git::execute()`, `GitConfig` |
| Modify SQLite journal schema or indexing | `src/journal.rs` | `SCHEMA`, `Journal::open()` |
| Adjust retention, expiration, or disk-cap pruning rules | `src/commands.rs`, `src/journal.rs`, `src/config.rs` | `commands::clean()`, `Journal::expire_staged_before()` |
| Improve path traversal or trash escape validation | `src/staging.rs` | `Stager::validate_stored()`, `Stager::guard_not_ancestor_of_trash()` |
| Alter coreutils parity error messages | `src/engine/mod.rs`, `src/engine/rm.rs` | `coreutils_message()` |
| Update shell completion scripts | `src/main.rs` | `Commands::Completions`, `clap_complete::generate` |
| Change package generation or release targets | `.github/workflows/`, `packaging/` | `release.yml`, `ci.yml`, `rewind.spec` |
