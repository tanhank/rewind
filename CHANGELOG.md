# Changelog

All notable changes to this project are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-11-21

Major production release featuring deep reliability hardening, cross-platform distribution pipelines, strict security boundaries, and enhanced Git recovery.

### Added

- **Filesystem security & path containment**: Enforced canonical traversal checks preventing staging outside repository boundaries or across untrusted symlink loops.
- **Process synchronization**: File-descriptor backed per-repository lockfile (`.lock`) preventing concurrent data corruption, with automatic stale PID detection and process liveness recovery.
- **Sensitive data redaction**: Automatic redaction of credentials, API tokens, authorization headers, and sensitive filenames in SQLite journal entries and staging directory trees.
- **Independent snapshot isolation**: Copy-on-write `FICLONE` / `clonefile` snapshot staging with fallback to hardlinks and atomic copy reservations, isolating backups from concurrent mutations.
- **Storage capacity management**: Configurable storage quotas, atomic quota pre-allocation checks, and LRU garbage collection.
- **Expanded Git recovery**: Full undo support for `git reset --hard`, `git clean -fd`, `git commit --amend`, `git rebase` (abort/continue), and stash reconstruction (`stash drop`, `stash pop`, `stash clear`).
- **Linked worktree support**: Safe ref pinning and staging isolation across linked Git worktrees without shared repository state corruption.
- **Consolidated coreutils argument parser**: Shared zero-allocation argument parsing across `rm`, `cp`, `mv`, `chmod`, `chown`, and `git` commands with full POSIX compatibility.
- **Performance engineering**: Zero-copy staging paths, optimized SQLite statement caching, and Criterion benchmark suite (`benches/workloads.rs`).
- **Unix-native CLI & completions**: Machine-readable `--json` schema across all commands, manual page (`rewind.1`), and native shell completions for Bash, Zsh, and Fish.
- **Cross-platform distribution**: Debian package (`.deb`), Fedora RPM (`.rpm`), static `musl` binaries for `x86_64` and `aarch64`, and native macOS builds for Apple Silicon (`macos-15`) and Intel (`macos-15-intel`).

### Changed

- **MSRV updated**: Minimum Supported Rust Version increased to **1.89.0** (Rust Edition 2024).
- **SQLite upgraded**: Embedded SQLite engine upgraded to **3.54.1** via patched `libsqlite3-sys`.

### Known Limitations

- Binary execution testing for `aarch64-unknown-linux-musl` is limited to static analysis and compilation verification on x86_64 CI runners without native ARM64 virtual execution hardware.

## [0.2.0] - 2026-10-27

### Added

- **`rewind inspect <ID>` command**: preview journal entry metadata, staged item paths, types, modes, timestamps, directory summaries, and recovery status before attempting an undo. Supports machine-readable `--json` output with conflict and missing data diagnostics.
- **`git branch -d` and `git branch -D` undo**: reversible branch deletion. Captures the branch head and pins it under `refs/rewind/<id>-*` to prevent Git garbage collection even when no other ref points to the commit. Restoring safely refuses to overwrite branches recreated at a different commit.
- **Shell hook branch interception**: bash, zsh, and fish hooks route branch deletion commands through rewind while keeping branch listing and non-destructive Git operations on the fast path.
- **`wrap.git_branch_delete` configuration toggle**: enables or disables branch deletion interception independently (defaults to `true`).

### Fixed

- **`git clean` empty directory restoration**: empty directories deleted by `git clean -fd` (including nested empty directory trees, ignored directories, and empty directories within untracked file hierarchies) are now accurately recorded and restored in parent-first order with original permissions preserved.
- APT publishing now refreshes release `SHA256SUMS` so it covers the attached
  `.deb` files as well as the static binary archives.

## [0.1.1] - 2026-10-22

Distribution and documentation release. No runtime behavior changes.

### Added

- Signed APT repository at <https://tanhank.github.io/apt/> with
  statically linked `amd64` and `arm64` packages.
- `packaging/build-static-deb.sh` for building dependency-free `.deb`
  packages from the musl release binaries.
- Simplified Chinese README, a language switcher, and a project icon.
- The release workflow now builds `.deb` packages, attaches them to the
  GitHub Release, and publishes them to the APT repository.

### Changed

- Restructured both READMEs around a quick start, coverage table, usage,
  and development workflow.
- The Debian package description no longer calls the dynamically linked
  distro package static.

### Fixed

- Release `SHA256SUMS` entries are now relative to the download directory.

## [0.1.0] - 2026-10-21

First public release.

### Added

- **Shell integration** for bash, zsh, and fish via `rewind init <shell>`.
  Wrapped commands preserve native stdout/stderr, stdin handling, prompts, and
  exit codes. `REWIND_DISABLE=1`, `command`, and absolute paths bypass the
  hook.
- **`rm` undo** implemented as an atomic rename into a per-entry trash
  directory. Supports `-r/-R`, `-f`, `-i`, `-I`, `-d`, `-v`, `--`,
  `--preserve-root`, and `--no-preserve-root` with GNU-compatible messages and
  exit codes. Cross-device staging falls back to copy-then-remove.
- **`mv` undo** that snapshots overwritten destinations, reconciles which
  moves actually happened, and restores both sides on undo.
- **`cp` undo** that snapshots overwritten destinations and records newly
  created paths so they can be removed. Supports recursive trees,
  multi-source copies, `-t`, and `--no-clobber`.
- **`chmod` and `chown` undo** that snapshot prior modes and ownership
  (recursively with `-R`) and restore them.
- **Git undo**:
  - `git reset --hard` — snapshots `HEAD`, the index tree, and uncommitted
    worktree changes with `git stash create`, pinned under `refs/rewind/*`.
  - `git clean -f…` — hard-link snapshots of every candidate path, honoring
    `-d`, `-x`, `-X`, `-e`, and pathspecs.
  - `git checkout -- <paths>` and `git restore` — worktree file snapshots plus
    index tree restoration, including `--staged`, `--source`, and
    `--worktree`.
- **Journal** backed by SQLite with per-item staging metadata, statuses
  (`running`, `done`, `undone`, `failed`, `interrupted`, `expired`), and
  resumable undo.
- **Commands**: `undo [N] [--dry-run]`, `log [--json] [-n]`,
  `status [--json]`, `clean [--older-than] [--all] [--dry-run]`,
  `init`, `completions`, and `config {path,show,get,set,init,edit}`.
- **Configuration** at `~/.config/rewind/config.toml`: per-command wrapper
  toggles, per-git-operation toggles, trash retention and size cap, journal
  retention, and git snapshot options. Changes take effect on the next
  command.
- **Safety guards**: refuses to stage the data directory or its ancestors,
  refuses to overwrite recreated paths during undo, creates private `0700`
  state directories, and degrades to native behavior when staging fails.
- **Test suite**: 117 unit and integration tests covering the wrapped
  commands, shell hooks, git round trips, JSON output, config, expiry, and
  failure paths.
- **CI**: build, format, clippy, and tests on push and pull request; static
  musl binaries for Linux x86_64 and aarch64 plus GitHub Releases on tags.
- **Packaging**: RPM spec for Fedora/COPR and Debian packaging for
  Debian/Ubuntu, including shell completion and hook installation.

[Unreleased]: https://github.com/tanhank/rewind/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/tanhank/rewind/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tanhank/rewind/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/tanhank/rewind/releases/tag/v0.1.1
[0.1.0]: https://github.com/tanhank/rewind/releases/tag/v0.1.0
