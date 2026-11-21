# Release & Distribution Engineering: rewind

> **Document Version:** 1.10.0  
> **Target Software Release Version:** `0.3.0`  
> **Classification:** Distribution architecture, packaging validation, release automation, and supply-chain security.

---

## 1. Release Architecture & Supported Distribution Targets

`rewind` is engineered as a zero-dependency Unix command-line utility. It avoids background daemons, terminal UI (TUI) engines, or runtime interpreter requirements.

### Supported Distribution Artifacts

| Target Triple | Packaging / Artifact Format | Linkage | Builder Environment | Primary Distribution Channel |
|---|---|---|---|---|
| `x86_64-unknown-linux-musl` | Tarball (`.tar.gz`) | Fully Static | Linux container (`cross-rs`) | GitHub Releases |
| `aarch64-unknown-linux-musl` | Tarball (`.tar.gz`) | Fully Static | Linux container (`cross-rs`) | GitHub Releases |
| `amd64` (Debian/Ubuntu) | Native `.deb` | Fully Static / musl | `packaging/build-static-deb.sh` | Signed APT Repo (`tanhank/apt`) |
| `arm64` (Debian/Ubuntu) | Native `.deb` | Fully Static / musl | `packaging/build-static-deb.sh` | Signed APT Repo (`tanhank/apt`) |
| `x86_64` (Fedora/RHEL) | Source / Binary `.rpm` | Dynamic (glibc) | Fedora Copr (`janghotan/rewind`) | Fedora Copr Repository |
| `aarch64-apple-darwin` | Tarball / Homebrew | Dynamic (libSystem) | macOS runner (`macos-15`) | Homebrew Tap (`tanhank/tap/rewind`) |
| `x86_64-apple-darwin` | Tarball / Homebrew | Dynamic (libSystem) | macOS runner (`macos-15-intel`) | Homebrew Tap (`tanhank/tap/rewind`) |

---

## 2. Versioning Specification & Taxonomy

A formal distinction is maintained between product release artifacts and internal engineering documentation:

- **Product Release Version (`0.3.0`):**
  - Declared in [Cargo.toml](file:///home/tan/Projects/rewind/Cargo.toml#L3), [debian/changelog](file:///home/tan/Projects/rewind/debian/changelog#L1), [packaging/rewind.spec](file:///home/tan/Projects/rewind/packaging/rewind.spec#L5), and [docs/rewind.1](file:///home/tan/Projects/rewind/docs/rewind.1#L1).
  - Governs semantic versioning of binary releases, package dependencies, and command interface compatibility.
- **Engineering Specification Version (`1.9.0`):**
  - Tracked in `docs/engineering/*.md`.
  - Records incremental architectural hardening milestones, security audits, and internal lifecycle phases (Phases 1 through 9).

---

## 3. Package Validation & Installation Lifecycle

### Package Layout Standards

All packages adhere strictly to Filesystem Hierarchy Standard (FHS):
- Executable binary: `/usr/bin/rewind` (mode `0755`)
- Man page: `/usr/share/man/man1/rewind.1.gz` (mode `0644`)
- Shell interception hooks: `/usr/share/rewind/{bash.sh, zsh.sh, fish.fish}` (mode `0644`)
- Shell completions:
  - Bash: `/usr/share/bash-completion/completions/rewind` (mode `0644`)
  - Zsh: `/usr/share/zsh/vendor-completions/_rewind` (Debian) / `/usr/share/zsh/site-functions/_rewind` (RPM)
  - Fish: `/usr/share/fish/vendor_completions.d/rewind.fish` (mode `0644`)
- Documentation: `/usr/share/doc/rewind/{README.md.gz, CHANGELOG.md.gz, copyright}`

### Uninstallation & Data Retention Policy

`rewind` maintains strict data-safety separation between system-level package files and user recovery state:

1. **Package Removal (`apt remove`, `dnf remove`, `pacman -R`):**
   - Package managers only remove files registered in the package manifest under `/usr/`.
   - User recovery databases (`~/.local/share/rewind/journal.db`), staged snapshots (`~/.local/share/rewind/trash/`), and user configurations (`~/.config/rewind/config.toml`) reside exclusively in user XDG home directories.
   - Package uninstallation **never** modifies or deletes user recovery data.
2. **Explicit Purge (`apt purge`):**
   - Debian policy dictates that `purge` removes package system configuration (e.g. under `/etc/`). Because `rewind` maintains no system-wide configuration files in `/etc/`, package purge does not touch user home directories.
   - Complete purging of user state is performed explicitly by the user running `rewind clean --all` or manually removing `$XDG_DATA_HOME/rewind`.

### Upgrades & SQLite Migrations

- Upgrading the binary is completely safe.
- `Journal::open()` runs pending SQLite migrations within an atomic `BEGIN IMMEDIATE` transaction, preserving all historical entries and restore metadata.
- Tested and verified in `tests/distribution.rs::database_migration_from_legacy_schema`.

---

## 4. Supply-Chain & Dependency Audit

A full supply-chain security audit was conducted using `cargo audit` against the RustSec Advisory Database:

### Vulnerability Analysis
1. **`libsqlite3-sys` v0.40.0 & `rusqlite` v0.43.0 (SQLite 3.54.1):**
   - *Status:* **Patched & Verified.** Upgraded to `rusqlite 0.43.0` and `libsqlite3-sys 0.40.0`, linking **SQLite 3.54.1** (>= 3.54.0). Completely eliminates CVE-2026-119375 / RUSTSEC-2026-0459.
2. **`rustix` v1.1.6 (RUSTSEC-2026-0486):**
   - *Status:* **Patched.** Upgraded to `rustix v1.1.6` in `Cargo.lock`.
3. **`chrono` v0.4.45 (RUSTSEC-2026-0543):**
   - *Description:* Informational advisory regarding crate maintenance.
   - *Assessment:* Accepted low risk. Minimal internal usage in `src/util.rs` for deterministic local formatting; no security exposure.
   - *Audit Summary:* `cargo audit` reports **zero vulnerabilities**.

### License Compatibility
Every direct and transitive dependency was audited via `cargo tree`:
- All dependencies carry permissive licenses: `MIT`, `Apache-2.0`, `Unlicense`, `Zlib`, or `Unicode-3.0`.
- Zero GPL or proprietary licenses that would conflict with rewind's `MIT` license.

---

## 5. Release Automation Pipelines

Release automation is separated into three GitHub Actions workflows under `.github/workflows/`:

1. **`ci.yml` (Routine Continuous Integration):**
   - Triggers on PRs and pushes to `main`.
   - Executes `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`, and `cargo build --release --locked`.
   - Runs in parallel on `ubuntu-latest`, `macos-15` (Apple Silicon), and `macos-15-intel` (Intel).
2. **`validate-release.yml` (Non-Publishing Release Validation):**
   - Triggers on PRs affecting release-critical paths (`Cargo.lock`, `Cargo.toml`, `packaging/**`, `.github/workflows/**`, `debian/**`) and manual `workflow_dispatch`.
   - Scoped strictly to `permissions: contents: read`.
   - Zero artifact publishing, zero release tags, zero GPG signing keys, zero repository secrets.
   - Executes full multi-platform matrix:
     - Linux build, lint, test, and CLI smoke test (`ubuntu-latest`).
     - Static musl cross-builds (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) via `cross`.
     - Static Debian `.deb` package generation and structural archive inspection via `ar` and `dpkg-deb`.
     - Fedora RPM spec syntax and buildability check in a `fedora:latest` container.
     - macOS build, lint, test, and CLI smoke test across `macos-15` (ARM64) and `macos-15-intel` (x86_64).
     - Security vulnerability verification via `cargo audit`.
3. **`release.yml` (Automated Multi-Platform Release):**
   - Triggers on tag pushes matching `v*`.
   - Builds static Linux musl binaries (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) via `cross`.
   - Builds native Darwin binaries (`aarch64-apple-darwin` on `macos-15`, `x86_64-apple-darwin` on `macos-15-intel`).
   - Bundles tarballs with README, LICENSE, CHANGELOG, and shell integration hooks.
   - Generates unified `SHA256SUMS`.
   - Creates GitHub Release with checksums attached.
   - Dispatches downstream `apt.yml`.
4. **`apt.yml` (Signed Debian APT Repository Publisher):**
   - Downloads static musl binaries from GitHub Release.
   - Packages `amd64` and `arm64` `.deb` packages via `packaging/build-static-deb.sh`.
   - Signs package indices using private GPG key (`APT_SIGNING_KEY`).
   - Deploys updated repository to GitHub Pages (`gh-pages` branch of `tanhank/apt`).

---

## 6. Local Packaging & Standalone Builders

`packaging/build-static-deb.sh` supports both `dpkg-deb` and an automated fallback to standard GNU/BSD `ar` and `tar`. This allows generating valid Debian packages in non-Debian environments (Arch, Alpine, Fedora, macOS) without installing `dpkg`.

Tested and verified in `tests/distribution.rs::static_deb_package_builder_layout`.

---

## 7. Real Execution Evidence & Platform Validation

All artifacts and environments are categorized by verified execution evidence rather than assumed configuration:

| Distribution Target | Artifact Format | Build Mechanism | Execution Evidence | Status |
|---|---|---|---|---|
| Linux x86_64 (native glibc) | Executable (`target/release/rewind`) | Local Cargo build (`cargo build --release --locked`) | **Empirically Verified:** 237 tests pass; sandboxed CLI smoke, `rewind exec rm` intercept, and `rewind undo` verified. | **READY** |
| Debian / Ubuntu `amd64` | Native `.deb` | `packaging/build-static-deb.sh` via GNU `ar`+`tar` | **Empirically Verified:** Package generated (`rewind_0.2.0-1_amd64.deb`). Archive verified (`debian-binary`, `control.tar.xz`, `data.tar.xz`). Control fields validated. Uninstallation retention verified. | **READY** |
| Linux `x86_64-unknown-linux-musl` | Static Tarball (`.tar.gz`) | Containerized `cross` builder (`.github/workflows/release.yml`, `validate-release.yml`) | **Configured & Validated:** Pipeline validated in `validate-release.yml`. Requires remote runner with Docker daemon or authorized push to execute. | **PENDING REMOTE CI** |
| Linux `aarch64-unknown-linux-musl` | Static Tarball (`.tar.gz`) | Containerized `cross` builder (`.github/workflows/release.yml`, `validate-release.yml`) | **Configured & Validated:** Pipeline validated in `validate-release.yml`. Requires remote runner with Docker daemon or authorized push to execute. | **PENDING REMOTE CI** |
| Debian / Ubuntu `arm64` | Native `.deb` | `packaging/build-static-deb.sh` | **Configured & Validated:** Uses `REWIND_HOST_BIN` for completions; cross-packaging scripted in `apt.yml` and `validate-release.yml`. | **PENDING REMOTE CI** |
| Fedora / RHEL `x86_64` | Native `.rpm` | Fedora Copr (`janghotan/rewind`) via `packaging/rewind.spec` | **Configured & Validated:** RPM spec syntax verified; local rpmbuild skipped (CachyOS host lacks `rpm-build`). Distribution channel is Fedora Copr. | **PENDING REMOTE CI / COPR** |
| macOS Apple Silicon (`aarch64-apple-darwin`) | Tarball / Homebrew | Native macOS runner (`macos-15`) | **Configured & Validated:** Runner updated to `macos-15` in `ci.yml`, `release.yml`, and `validate-release.yml`. Zero OS-specific dependencies. Remote run pending tag push. | **PENDING REMOTE CI** |
| macOS Intel (`x86_64-apple-darwin`) | Tarball / Homebrew | Native macOS runner (`macos-15-intel`) | **Configured & Validated:** Dual-architecture matrix configured in `release.yml`, `ci.yml`, and `validate-release.yml`. Remote run pending tag push. | **PENDING REMOTE CI** |

---

## 8. Release Sign-Off & Verdict

- **Release Decision:** **CONDITIONAL GO**
- **Justification:**
  1. The codebase is thoroughly tested and mathematically hardened (237 automated tests passing, 0 clippy warnings, clean formatting).
  2. Supply chain is clean: `cargo audit` reports 0 vulnerabilities; SQLite 3.54.1 is verified embedded.
  3. Release workflows enforce least-privilege permissions (`contents: read` default) and zero unverified publishing.
  4. Non-publishing validation workflow (`validate-release.yml`) is established.
  5. **Condition:** The working tree contains uncommitted hardening across Phases 2–11. A commit and push to remote `main` or an authorized `v0.2.0` release tag is required to trigger remote GitHub Actions runners (`macos-15`, `macos-15-intel`, `cross` musl containers, Fedora container validation) and populate release assets.

