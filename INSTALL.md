# Installation & Distribution Guide: rewind

`rewind` is a lightweight Unix command-line utility providing universal undo for destructive shell operations (`rm`, `mv`, `cp`, `chmod`, `chown`, and destructive `git` commands).

It operates entirely in user space without daemons, background agents, kernel drivers, or interactive graphical prompts.

---

## 1. System Requirements & Supported Platforms

### Supported Architectures
- **Linux x86_64:** Compatible with all modern Linux distributions (glibc 2.17+ or static musl).
- **Linux aarch64 (ARM64):** Compatible with 64-bit ARM distributions (Raspberry Pi 4/5, AWS Graviton, Ampere).
- **macOS (Darwin):** Apple Silicon (ARM64) and Intel (x86_64), macOS 13 (Ventura) and newer.

### Minimum Supported Rust Version (MSRV)
- Building from source requires **Rust 1.89.0+** (Edition 2024).

---

## 2. Installation Methods

### Method A: Static Linux Binary (Recommended for Linux)

Statically compiled binaries link against `musl-libc` and embed SQLite directly. They have **zero external runtime dependencies** and run on any Linux kernel 3.2+.

#### Quick Install (x86_64)

```bash
install -d -m 0755 "$HOME/.local/bin"
curl -fsSL https://github.com/tanhank/rewind/releases/latest/download/rewind-x86_64-unknown-linux-musl.tar.gz | \
  tar -xz --strip-components=1 -C "$HOME/.local/bin" '*/rewind'
chmod 0755 "$HOME/.local/bin/rewind"
"$HOME/.local/bin/rewind" --version
```

*(For ARM64, replace `x86_64` with `aarch64` in the URL above).*

#### Integrity Verification (SHA-256)

Every release artifact is published alongside `SHA256SUMS`:

```bash
# Download binary tarball and checksum file
curl -fsSLO https://github.com/tanhank/rewind/releases/latest/download/rewind-x86_64-unknown-linux-musl.tar.gz
curl -fsSLO https://github.com/tanhank/rewind/releases/latest/download/SHA256SUMS

# Verify checksum
sha256sum --ignore-missing -c SHA256SUMS
```

Ensure `~/.local/bin` is in your shell `PATH`:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

---

### Method B: Debian & Ubuntu (`.deb` via Signed APT Repository)

Official static Debian packages are maintained for `amd64` and `arm64`:

```bash
# 1. Install keyrings directory and import GPG signing key
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://tanhank.github.io/apt/rewind-archive-keyring.gpg | \
  sudo tee /etc/apt/keyrings/rewind.gpg >/dev/null

# 2. Add repository source list
echo "deb [signed-by=/etc/apt/keyrings/rewind.gpg] https://tanhank.github.io/apt stable main" | \
  sudo tee /etc/apt/sources.list.d/rewind.list

# 3. Update and install
sudo apt-get update
sudo apt-get install rewind
```

**Installed file locations:**
- Binary: `/usr/bin/rewind`
- Man page: `/usr/share/man/man1/rewind.1.gz`
- Shell integration hooks: `/usr/share/rewind/{bash.sh, zsh.sh, fish.fish}`
- Shell completions:
  - Bash: `/usr/share/bash-completion/completions/rewind`
  - Zsh: `/usr/share/zsh/vendor-completions/_rewind`
  - Fish: `/usr/share/fish/vendor_completions.d/rewind.fish`

---

### Method C: Fedora, RHEL & CentOS (`.rpm` via Fedora Copr)

RPM packages are built via Fedora Copr (`janghotan/rewind`):

```bash
# Enable Copr repository
sudo dnf copr enable janghotan/rewind

# Install package
sudo dnf install rewind
```

**Installed file locations:**
- Binary: `/usr/bin/rewind`
- Man page: `/usr/share/man/man1/rewind.1.gz`
- Shell integration hooks: `/usr/share/rewind/{bash.sh, zsh.sh, fish.fish}`
- Shell completions:
  - Bash: `/usr/share/bash-completion/completions/rewind`
  - Zsh: `/usr/share/zsh/site-functions/_rewind`
  - Fish: `/usr/share/fish/vendor_completions.d/rewind.fish`

---

### Method D: macOS via Homebrew

```bash
brew install tanhank/tap/rewind
```

Homebrew automatically links completions for Bash, Zsh, and Fish into Homebrew's prefix.

---

### Method E: Building from Source (Cargo)

```bash
# 1. Clone repository
git clone https://github.com/tanhank/rewind.git
cd rewind

# 2. Build release binary (optimized and stripped)
cargo build --release --locked

# 3. Install to user bin directory
install -Dm755 target/release/rewind "$HOME/.local/bin/rewind"
```

---

## 3. Enabling Shell Integration

Command interception operates through shell function hooks. Distro packages install hooks into `/usr/share/rewind/`, while `rewind init <shell>` outputs the hook directly.

Add the initialization command to your interactive shell configuration:

### Bash (`~/.bashrc`)
```bash
eval "$(rewind init bash)"
```

### Zsh (`~/.zshrc`)
```bash
eval "$(rewind init zsh)"
```

### Fish (`~/.config/fish/config.fish`)
```fish
rewind init fish | source
```

Restart your shell or reload configuration (`source ~/.bashrc`).

---

## 4. Upgrades & Data Compatibility

### Upgrading the Binary
Upgrading `rewind` (via package manager, curl replacement, or Cargo) replaces only the executable binary and system-level shell hook files.

### Recovery Data Safety During Upgrades
- **Zero Loss Guarantee:** Upgrades **never** overwrite, delete, or reset your recovery journal or trash snapshots.
- **Automatic Schema Migration:** When an upgraded binary runs, `Journal::open` automatically applies any pending SQLite schema migrations in an atomic transaction (`BEGIN IMMEDIATE`).
- **Backwards Compatibility:** Database format migrations preserve all existing entries, timestamps, and restore paths.

---

## 5. Uninstallation & Data Retention Policy

`rewind` strictly adheres to Unix filesystem separation principles:

### Package Removal
Removing `rewind` via your system package manager:
- `sudo apt-get remove rewind`
- `sudo dnf remove rewind`
- `brew uninstall rewind`

Removes **only** the package-owned files (`/usr/bin/rewind`, `/usr/share/rewind/`, completions, and man pages).

### User Recovery Data Retention
Because `rewind` stores state in standard per-user XDG directories, system uninstallation **does not** touch user data:
- SQLite Journal: `$XDG_DATA_HOME/rewind/journal.db` (`~/.local/share/rewind/journal.db`)
- Staged Snapshots: `$XDG_DATA_HOME/rewind/trash/` (`~/.local/share/rewind/trash/`)
- User Configuration: `$XDG_CONFIG_HOME/rewind/config.toml` (`~/.config/rewind/config.toml`)

### Complete Data Removal (Purge)
If you wish to permanently delete all recovery history and cached snapshots before or after removing the binary:

```bash
# 1. Purge all staged snapshots and entries via rewind
rewind clean --all

# 2. Alternatively, manually delete the user directories
rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/rewind"
rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/rewind"

# 3. Remove the init line from your ~/.bashrc, ~/.zshrc, or config.fish
```

---

## 6. Environment Variables

| Variable | Default Value | Description |
|---|---|---|
| `REWIND_DATA_DIR` | `$XDG_DATA_HOME/rewind` | Overrides the root directory for SQLite journal, lock, and trash. |
| `REWIND_JOURNAL` | `$REWIND_DATA_DIR/journal.db` | Overrides the SQLite journal database file path. |
| `REWIND_CONFIG_PATH` | `$XDG_CONFIG_HOME/rewind/config.toml` | Overrides configuration file path. |
| `REWIND_DISABLE` | `0` | When set to `1`, bypasses interception for the current shell or command. |
| `NO_COLOR` | *(unset)* | When set, disables ANSI color output across all subcommands. |
