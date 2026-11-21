# Project Context & Agent Operating Guide: rewind

> **Document Version:** 1.11.0  
> **Software Product Release Version:** `0.2.0`  
> **Repository Commit SHA:** `08a325fc468709928f8e7f5401b418a69d253d66` (+ Phases 2-11 Hardening & Release Validation)  
> **Discovery Date:** 2026-11-21  
> **Scope Assessed:** Complete repository inventory (src, shell, tests, packaging, CI/CD, docs)  
> **Freshness Rule:** Future coding sessions must read this file first and verify if HEAD matches before making architectural assumptions.

> [!NOTE]
> **Version Taxonomy Distinction:**
> - **Product Release Version (`0.2.0`):** Semantic release version declared in `Cargo.toml`, `debian/changelog`, `packaging/rewind.spec`, and `docs/rewind.1`. Governs binary artifacts and distribution packages.
> - **Engineering Knowledge Base Specification Version (`1.11.0`):** Internal document version tracking incremental development, security audits, and lifecycle phases (Phases 1-11).

---

## 1. Project Overview & Unix Philosophy

**rewind** is a fast, resilient, daemon-less command-line utility written in Rust that provides a universal "undo" stack for destructive shell commands. It sits transparently between user interactive shells (Bash, Zsh, Fish) and the filesystem/version-control tools that mutate state.

### Product Vision & Boundaries
Rewind adheres strictly to Unix command-line utility philosophy (analogous to `curl`, `rsync`, `git`, `ls`, `grep`, and `find`).
- **Command-Driven & Noninteractive:** It operates exclusively via composable CLI subcommands, flags, exit codes, and pipes.
- **NO TUI / Dashboards:** It intentionally avoids terminal UIs, curses dashboards, menu-driven navigation, desktop UIs, or background daemons.
- **Script-Friendly:** Machine-readable JSON output (`--json`) and quiet modes support automation and shell pipelines. Standard output carries data; standard error carries diagnostics.

### What rewind Protects
- **File Deletions:** `rm` (files, recursive directories, empty directories).
- **Overwrites & Moves:** `mv`, `cp` (files, directories, target directories).
- **Metadata Modifications:** `chmod`, `chown` (single targets and recursive `-R`).
- **Destructive Git Operations:** `git reset --hard`, `git clean -f`, `git checkout -- <paths>`, `git restore`, `git branch -d / -D`, `git commit --amend`, `git stash drop / clear / pop`, and `git rebase` (full support for linked worktrees and stack reconstruction). (`git rebase --abort` and `git merge --abort` are treated as an honest unsupported boundary: passed through cleanly without false undo promises, since multi-stage conflict indices, MERGE_HEAD, and sequencer metadata cannot be portably synthesized).

### Core Engineering Principles
1. **Zero Daemons / Zero FUSE:** Operates as a single static binary linked against `musl` with shell hook shims. Stays completely dormant until a wrapped command is invoked.
2. **Byte-for-Byte Native Parity:** Commands like `mv`, `cp`, `chmod`, `chown`, and `git` snapshot pre-mutation state and immediately hand control to the host binary via `run_passthrough`, preserving stdio, TTY interaction, error strings, and exit codes. `rm` is natively implemented to achieve atomic $O(1)$ renaming into the trash.
3. **Resumable Atomic Undo:** Undo operations record and restore individual staged items. If a destination conflict occurs, the remaining items remain in the journal, and undo can be retried safely.
4. **Transparent Script Invisibility:** Detects non-terminal environments and suppresses extraneous messages unless an error occurs, ensuring scripts and pipelines do not break.

---

## 2. Main Technologies & Runtimes

- **Language:** Rust (2024 Edition, Minimum Supported Rust Version: `1.89`).
- **Primary Database Engine:** SQLite 3.54.1 via `rusqlite 0.43.0` (statically bundled with WAL mode enabled).
- **CLI Framework:** `clap 4.6.7` (derive API) + `clap_complete 4.6.11`.
- **Serialization:** `serde 1.0.229`, `serde_json 1.0.154`, `toml 1.1.8`.
- **System & File Management:** `filetime 0.2.29`, standard POSIX libc syscalls.
- **Testing Tools:** `assert_cmd 2.1.1`, `predicates 3.1.3`, `tempfile 3.27.0`.
- **Target Architectures:** Linux (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, GNU libc distros), macOS (compile support).

---

## 3. High-Level Architecture

The system consists of three operational planes:

```
[Interactive Shell (Bash / Zsh / Fish)]
                 │
   Hook Function: __rewind_run <cmd> "$@"
                 │
                 ▼
         [rewind exec <cmd> -- <args>]
                 │
       ┌─────────┴─────────┐
       ▼                   ▼
 [Engine Dispatch]   [Config Check & Bypass]
       │                   │ (if disabled: passthrough to real binary)
       ├── 1. Begin journal entry (SQLite entries table)
       ├── 2. Pre-action Staging:
       │      • rm: atomic rename into trash/<id>/
       │      • mv/cp: copy destination before overwrite
       │      • chmod/chown: serialize mode/owner snapshots
       │      • git: update-ref pins under refs/rewind/*
       ├── 3. Execute real binary or internal engine
       ├── 4. Reconcile outcomes & update staged metadata
       └── 5. Mark entry status: Done | Failed | Interrupted
                 │
                 ▼
          [rewind undo [N]]
                 │
       Reverses journal entries in strict LIFO order
```

---

## 4. Engineering Knowledge Base Index

All detailed engineering specifications are located under [docs/engineering/](file:///home/tan/Projects/rewind/docs/engineering/):

1. [CODEBASE_MAP.md](file:///home/tan/Projects/rewind/docs/engineering/CODEBASE_MAP.md)  
   *Complete directory and file inventory, component responsibilities, symbol exports, and rapid navigation guide.*
2. [ARCHITECTURE.md](file:///home/tan/Projects/rewind/docs/engineering/ARCHITECTURE.md)  
   *Deep system architecture, SQLite data dictionary, transaction boundaries, Mermaid execution flowcharts, and engine specs.*
3. [SECURITY_POSTURE.md](file:///home/tan/Projects/rewind/docs/engineering/SECURITY_POSTURE.md)  
   *Threat model, trust boundaries, built-in protections, vulnerability discovery audit, and prioritized security risk register.*
4. [TESTING_AND_OPERATIONS.md](file:///home/tan/Projects/rewind/docs/engineering/TESTING_AND_OPERATIONS.md)  
   *Toolchain setup, build/test commands, integration testing harness, CI/CD pipeline, and package deployment procedures.*
5. [TECHNICAL_DEBT.md](file:///home/tan/Projects/rewind/docs/engineering/TECHNICAL_DEBT.md)  
   *Architectural bottlenecks, concurrency gaps, cross-platform limitations, and prioritized engineering debt roadmap.*
6. [PERFORMANCE.md](file:///home/tan/Projects/rewind/docs/engineering/PERFORMANCE.md)  
   *Performance profiling, benchmark workload suite (A-E), filesystem comparison (Btrfs CoW vs tmpfs), bottleneck analysis, and optimizations.*

---

## 5. Critical Development Commands

All standard development tasks should use the following vetted commands:

```bash
# Compilation check across all targets (binaries, tests, benches)
cargo check --all-targets --locked

# Run the complete test suite (229 tests across 15 suites)
cargo test --locked

# Run the repeatable benchmark workload suite
cargo bench --bench workloads

# Enforce code style and formatting
cargo fmt --all --check

# Enforce strict compiler and clippy lints
cargo clippy --all-targets --locked -- -D warnings

# Build optimized production release binary
cargo build --release --locked
```

---

## 6. Critical Implementation Constraints

When working on any future task, you MUST adhere to these non-negotiable rules:

1. **Do Not Break Coreutils Parity:** Wrapped commands must faithfully replicate standard command exit codes, error text on standard error, and behavior with flags like `-i`, `-f`, and `--`.
2. **Transparent Passthrough Fallback:** Whenever an argument cannot be modeled with 100% confidence (e.g. non-UTF-8 characters, `--one-file-system`, interactive patch modes `-p`, git merge conflicts), the engine must immediately hand control to the underlying binary without failing or corrupting state.
3. **Owner-Only Security (`0700`):** All runtime state directories (`~/.local/share/rewind` and `trash/`) must be created with `0700` permissions. Never write unencrypted or unprotected state to shared global directories like `/tmp`.
4. **Never Overwrite During Undo:** Undoing an `rm` or `git clean` MUST refuse to overwrite any destination path that has since been recreated by the user, preserving both files.
5. **Atomic Metadata & SQLite WAL:** Never execute schema updates without transaction wrapping. SQLite connections must maintain `PRAGMA journal_mode = WAL`, `PRAGMA synchronous = NORMAL`, and `PRAGMA busy_timeout = 5000`. Read-only commands must enforce `PRAGMA query_only = ON`.

---

## 7. Future Agent Operating Rules

Every future agent session must follow this protocol:

1. **Step 1:** Read this document (`PROJECT_CONTEXT.md`).
2. **Step 2:** Query [CODEBASE_MAP.md](file:///home/tan/Projects/rewind/docs/engineering/CODEBASE_MAP.md) to locate relevant modules and symbols.
3. **Step 3:** Check git status (`git status -s`) and HEAD commit (`git rev-parse HEAD`). Verify if relevant files have changed since commit `08a325fc468709928f8e7f5401b418a69d253d66`.
4. **Step 4:** Inspect only the specific files required for the task. Do not re-scan the entire repository.
5. **Step 5:** Write unit/integration tests for every non-trivial logic change.
6. **Step 6:** Run `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, and `cargo test --locked` before finishing.
7. **Step 7:** Update the corresponding engineering documents in `docs/engineering/` if architectural boundaries, schemas, or behaviors change.
