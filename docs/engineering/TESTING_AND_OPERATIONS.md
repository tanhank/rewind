# Testing & Operations Guide: rewind

> **Document Version:** 1.10.0  
> **Target Software Release Version:** `0.3.0`  
> **Commit SHA:** `08a325fc468709928f8e7f5401b418a69d253d66` (+ Phases 2-10 Hardening)  
> **Coverage Overview:** 237 automated tests across 16 test suites passing cleanly. Zero Clippy warnings, clean rustfmt.

---

## 1. Development & Build Toolchain

### Prerequisites
- **Rust Toolchain:** Version 1.89+ (tested with `stable`).
- **Cargo Components:** `rustfmt`, `clippy`.
- **System Dependencies:** `git`, `bash`, `coreutils` (for integration testing).
- **Cross Compilation (Optional):** `cross` (via `cargo install cross` or `@taiki-e/install-action`).

### Build & Test Commands

```bash
# 1. Quick compile check (all targets including integration tests)
cargo check --all-targets --locked

# 2. Run the complete test suite (unit + integration suites)
cargo test --locked

# 3. Code formatting verification
cargo fmt --all --check

# 4. Strict clippy linting
cargo clippy --all-targets --locked -- -D warnings

# 5. Run performance benchmark suite (Workloads A through E)
cargo bench --bench workloads

# 6. Build production release binary (optimized, stripped)
cargo build --release --locked
```

---

## 2. Test Suite Architecture

The repository enforces testing through both unit tests embedded in source modules and black-box end-to-end integration tests in `tests/`.

### 2.1 Test Directory Structure

```
tests/
├── common/
│   └── mod.rs            # Sandbox test harness with isolated $HOME & $XDG dirs
├── cli.rs                # CLI commands, exit codes (0, 1, 2, 3), targeted undo by ID, log filtering, clean JSON, config list
├── git.rs                # git reset, git clean, git checkout, git branch -D
├── lock.rs               # Concurrency, mutual exclusion, timeouts, and process synchronization
├── process_concurrency.rs # Subprocess-based OS tests for flock behavior & kernel release
├── redact.rs             # Credential and sensitive argument redaction (SEC-003)
├── rm.rs                 # rm file, rm -r, symlinks, -d, -v, prompts
├── parser.rs             # Unified coreutils argument parser parity (short clusters, inline values, -- boundary)
├── sensitive_files.rs    # Sensitive file exclusion patterns & permissions (SEC-002)
├── shell.rs              # Real Bash/Zsh subshell execution and LC_ALL=C parity
├── snapshot_isolation.rs # Snapshot isolation, CoW cloning & mutation prevention (SEC-004)
├── storage_capacity.rs   # Trash storage capacity caps, quota checks & recoverability (SEC-006)
├── transfer.rs           # mv and cp overwrites, target directories, recursive
└── distribution.rs       # Packaging metadata consistency, legacy DB migrations, uninstallation data safety, SHA-256
```

### 2.2 The Sandbox Isolation Harness (`tests/common/mod.rs`)

To ensure tests can execute in parallel without cross-contaminating journals or user files, `Sandbox::new()` creates an isolated environment per test:
- Generates a `tempfile::TempDir`.
- Sets `$HOME` to `<tempdir>/home`.
- Sets `$XDG_DATA_HOME` to `<tempdir>/home/.local/share`.
- Sets `$XDG_CONFIG_HOME` to `<tempdir>/home/.config`.
- Sets `$TZ` to `UTC` (for deterministic timestamp testing).
- Unsets existing environment overrides (`REWIND_DATA_DIR`, `REWIND_JOURNAL`, `REWIND_DISABLE`).

### 2.3 Executing Subsets of Tests

```bash
# Run only rm integration tests
cargo test --test rm

# Run only git integration tests
cargo test --test git

# Run only CLI tests matching a keyword
cargo test --test cli status_and_log

# Run shell parity tests with visible stdout
cargo test --test shell -- --nocapture
```

---

## 3. CI/CD & Automation Pipelines

GitHub Actions workflows are located under [.github/workflows/](file:///home/tan/Projects/rewind/.github/workflows/):

### 3.1 Main CI Workflow (`.github/workflows/ci.yml`)
- **Triggers:** Push to `main`, Pull Requests, Manual `workflow_dispatch`.
- **Jobs:**
  - `test (Linux)`: Ubuntu latest runner, installs stable toolchain with `rustfmt` and `clippy`. Runs `cargo fmt`, `cargo clippy`, `cargo test`, and `cargo build --release`.
  - `macos`: macOS matrix runners (`macos-15` for Apple Silicon, `macos-15-intel` for Intel), installs stable toolchain with `rustfmt` and `clippy`. Runs `cargo fmt`, `cargo clippy`, `cargo test`, and `cargo build --release` to ensure true cross-platform functionality.
  - **Local vs Remote State Note:** The updated macOS matrix jobs running full test gates are configured in the local working tree `.github/workflows/ci.yml` (migrated from retired `macos-14`/`macos-13` to `macos-15` and `macos-15-intel`). Verification against the remote GitHub repository confirmed that remote run `#44106122457` on baseline commit `08a325f` executed the legacy `compile (macOS)` configuration (`cargo check` only). The expanded `cargo test` execution on Darwin will execute upon the next remote branch push.

### 3.2 Non-Publishing Release Validation Workflow (`.github/workflows/validate-release.yml`)
- **Triggers:** Pull requests affecting release paths (`Cargo.lock`, `Cargo.toml`, `packaging/**`, `.github/workflows/**`, `debian/**`), manual `workflow_dispatch`.
- **Permissions:** `contents: read` strictly.
- **Guarantee:** Zero publishing, zero release tag creation, zero secrets exposure.
- **Matrix Jobs:**
  - `audit`: Verifies zero dependencies have security advisories (`cargo audit`).
  - `linux`: Executes formatting check, clippy, unit/integration test suites, release build, and CLI smoke tests.
  - `musl`: Builds static musl binaries for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` via `cross`.
  - `debian`: Generates `.deb` package via `packaging/build-static-deb.sh` and verifies archive layout with `ar` and `dpkg-deb`.
  - `fedora-rpm`: Validates RPM spec syntax and non-build parsing in a `fedora:latest` container.
  - `macos`: Executes formatting, clippy, tests, release build, and CLI smoke tests on `macos-15` and `macos-15-intel`.

### 3.3 Release Workflow (`.github/workflows/release.yml`)
- **Triggers:** Push of tags matching `v*`.
- **Permissions:** `contents: read` default; `contents: write` scoped strictly to `release` job.
- **Matrix Targets:**
  - `x86_64-unknown-linux-musl`
  - `aarch64-unknown-linux-musl`
  - `aarch64-apple-darwin` (`macos-15`)
  - `x86_64-apple-darwin` (`macos-15-intel`)
- **Actions:**
  1. Installs `cross` for musl builds.
  2. Builds static binaries with `cross build --release --locked --target <target>` or native `cargo build` on macOS.
  3. Bundles tarball with `rewind` binary, `README.md`, `LICENSE`, `CHANGELOG.md`, and shell scripts (`shell/*.sh`).
  4. Generates SHA256 checksums (`SHA256SUMS`).
  5. Publishes GitHub Release using `softprops/action-gh-release@v2`.
  6. Triggers the downstream APT packaging workflow.

### 3.4 APT Repository Workflow (`.github/workflows/apt.yml`)
- **Triggers:** Called by `release.yml` or manual dispatch.
- **Actions:**
  1. Downloads static release tarballs.
  2. Runs `packaging/build-static-deb.sh` for both `amd64` and `arm64`.
  3. Imports GPG private signing key (`APT_SIGNING_KEY`).
  4. Updates Debian package indices (`Packages`, `Release`, `InRelease`).
  5. Pushes updated repository to the `gh-pages` branch of `tanhank/apt`.

---

## 4. Packaging Systems

The project maintains native distribution packages:

1. **Debian / Ubuntu (`debian/`, `packaging/build-deb.sh`, `packaging/build-static-deb.sh`):**
   - Standard source package for PPAs and static `.deb` packages for the custom APT repo.
   - Installs binary to `/usr/bin/rewind`, man page to `/usr/share/man/man1/rewind.1`, shell hooks to `/usr/share/rewind/`, and shell completions to system completion directories.
2. **RPM / Fedora / RHEL (`packaging/rewind.spec`, `packaging/build-rpm.sh`):**
   - Full RPM spec with offline vendoring support (`cargo vendor`). Built on Fedora COPR (`janghotan/rewind`).
3. **Homebrew (macOS):**
   - Maintained via tap `tanhank/tap/rewind`. Installs completions and docs.

---

## 5. Runtime Environment Variables

| Variable | Default Value | Purpose |
|---|---|---|
| `HOME` | *(Required)* | Base directory for fallback XDG paths (`~/.local/share`, `~/.config`). |
| `REWIND_DATA_DIR` | `$XDG_DATA_HOME/rewind` | Overrides the root directory for SQLite journal and trash. |
| `REWIND_JOURNAL` | `$REWIND_DATA_DIR/journal.db` | Explicit override for the SQLite database file path. |
| `REWIND_CONFIG_PATH` | `$XDG_CONFIG_HOME/rewind/config.toml` | Explicit override for the configuration TOML file path. |
| `REWIND_DISABLE` | `0` | Set to `1` or `true` to bypass interception for the current shell command. |
| `REWIND_ACTIVE` | `1` | Exported by shell hooks to indicate that interception is active in the subshell. |
| `XDG_DATA_HOME` | `~/.local/share` | Standard XDG base directory for user data. |
| `XDG_CONFIG_HOME` | `~/.config` | Standard XDG base directory for user configuration. |
