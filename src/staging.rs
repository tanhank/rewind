//! Staging: move deleted data into the trash, snapshot data that will be
//! overwritten, and put it back on undo.
//!
//! Design notes
//! ------------
//! * Deletions use `rename(2)` into the trash: O(1), atomic, metadata-exact.
//!   When the trash lives on another filesystem we fall back to a recursive
//!   copy followed by deleting the source.
//! * Overwrite snapshots must be real copies, never hard links: `cp` and `mv`
//!   may truncate and rewrite the destination in place, which would mutate a
//!   hard-linked "snapshot" through the shared inode.
//! * `git clean` snapshots may use hard links because git unlinks the original
//!   paths; that keeps large untracked trees cheap to protect.
//! * Nothing here ever overwrites an existing undo destination by accident.
//!   Overwriting is opt-in per call site.

use std::cell::Cell;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::{Error, IoContext, Result};
use crate::paths::Paths;

/// How to snapshot a path that stays in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotMode {
    /// Full recursive copy; safe when the original may be rewritten in place.
    Copy,
    /// Copy-on-write clone (using Linux FICLONE or macOS clonefile) falling
    /// back to independent copy. Guarantees snapshot isolation against
    /// concurrent or subsequent writes to the source file (SEC-004).
    Clone,
    /// Deprecated synonym for `Clone`. Retained for backwards compatibility.
    /// Never uses ordinary hard links for mutable snapshots.
    Hardlink,
}

/// Result of staging one path.
#[derive(Debug, Clone)]
pub struct StagedPayload {
    /// Where the data now lives inside the trash.
    pub stored: PathBuf,
    /// Approximate number of bytes protected.
    pub size: u64,
}

/// Per-command staging helper. Each intercepted command gets its own
/// `trash/<entry_id>/` directory, which makes cleanup and inspection trivial.
pub struct Stager<'a> {
    paths: &'a Paths,
    entry_id: i64,
    sequence: Cell<u64>,
}

impl<'a> Stager<'a> {
    pub fn new(paths: &'a Paths, entry_id: i64) -> Self {
        Self {
            paths,
            entry_id,
            sequence: Cell::new(0),
        }
    }

    pub fn entry_id(&self) -> i64 {
        self.entry_id
    }

    /// Directory holding all payloads for this entry.
    pub fn entry_dir(&self) -> PathBuf {
        self.paths.entry_dir(self.entry_id)
    }

    fn ensure_entry_dir(&self) -> Result<PathBuf> {
        let dir = self.entry_dir();
        crate::paths::create_private_dir(&dir)?;
        Ok(dir)
    }

    /// Generate a collision-free name inside the entry directory.
    fn next_path(&self, base: &OsStr) -> PathBuf {
        let seq = self.sequence.get() + 1;
        self.sequence.set(seq);
        let mut name = format!("{seq:04}-");
        let base = base.to_string_lossy();
        let sanitized: String = base
            .chars()
            .map(|c| if c == '/' || c == '\0' { '_' } else { c })
            .take(120)
            .collect();
        if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
            name.push_str("item");
        } else {
            name.push_str(&sanitized);
        }
        self.entry_dir().join(name)
    }

    /// Move `origin` into the trash (used by `rm`).
    pub fn stage_move(&self, origin: &Path) -> Result<StagedPayload> {
        self.ensure_entry_dir()?;
        self.guard_not_ancestor_of_trash(origin)?;
        let base = origin.file_name().unwrap_or_else(|| OsStr::new("item"));
        let stored = self.next_path(base);
        let size = tree_size(origin)?;

        #[cfg(test)]
        let force_exdev = std::env::var_os("REWIND_TEST_FORCE_EXDEV").is_some();
        #[cfg(not(test))]
        let force_exdev = false;

        let rename_res = if force_exdev {
            Err(std::io::Error::from_raw_os_error(18)) // simulate EXDEV in tests
        } else {
            fs::rename(origin, &stored)
        };

        match rename_res {
            Ok(()) => Ok(StagedPayload { stored, size }),
            Err(err) if is_cross_device(&err) => {
                copy_tree(origin, &stored)?;
                remove_tree(origin)?;
                Ok(StagedPayload { stored, size })
            }
            Err(err) => Err(crate::error::io_err(origin, err)),
        }
    }

    /// Copy `origin` into the trash while leaving it in place.
    pub fn snapshot(&self, origin: &Path, mode: SnapshotMode) -> Result<StagedPayload> {
        self.ensure_entry_dir()?;
        self.guard_not_ancestor_of_trash(origin)?;
        let base = origin.file_name().unwrap_or_else(|| OsStr::new("item"));
        let stored = self.next_path(base);
        let size = match mode {
            SnapshotMode::Copy => copy_tree(origin, &stored)?,
            SnapshotMode::Clone | SnapshotMode::Hardlink => clone_tree(origin, &stored)?,
        };
        Ok(StagedPayload { stored, size })
    }

    /// Write bytes (a metadata snapshot or plan) into the entry directory.
    pub fn write_aux(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        self.ensure_entry_dir()?;
        let path = self.next_path(OsStr::new(name));
        fs::write(&path, bytes).path_context(&path)?;
        Ok(path)
    }

    /// Move a staged payload back to `origin`.
    ///
    /// With `overwrite == false` an existing destination is refused, which is
    /// the right behavior for undo of `rm` and `git clean`. Overwrite is used
    /// when restoring a snapshot over a file the wrapped command rewrote.
    pub fn restore(&self, stored: &Path, origin: &Path, overwrite: bool) -> Result<()> {
        self.validate_stored(stored)?;
        move_path(stored, origin, overwrite)
    }

    /// Remove a staged payload that will not be restored (e.g. a metadata file).
    pub fn discard_stored(&self, stored: &Path) -> Result<()> {
        if stored.as_os_str().is_empty() {
            return Ok(());
        }
        self.validate_stored(stored)?;
        match remove_tree(stored) {
            Ok(()) => Ok(()),
            Err(Error::Io { source, .. }) if source.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Delete the whole entry directory after a successful undo.
    pub fn cleanup_entry_dir(&self) -> Result<()> {
        let dir = self.entry_dir();
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(crate::error::io_err(&dir, err)),
        }
    }

    fn validate_stored(&self, stored: &Path) -> Result<()> {
        let trash = fs::canonicalize(&self.paths.trash_dir)
            .unwrap_or_else(|_| self.paths.trash_dir.clone());
        let candidate = if stored.is_absolute() {
            stored.to_path_buf()
        } else {
            self.paths.trash_dir.join(stored)
        };
        // The stored path may not exist yet after a partial failure, so
        // canonicalize the parent rather than the leaf.
        let resolved = match candidate.canonicalize() {
            Ok(path) => path,
            Err(_) => {
                let parent = candidate.parent().unwrap_or(Path::new("/"));
                let name = candidate.file_name().unwrap_or_default();
                parent
                    .canonicalize()
                    .unwrap_or_else(|_| parent.to_path_buf())
                    .join(name)
            }
        };
        if !resolved.starts_with(&trash) {
            return Err(Error::Refused(format!(
                "journal references {:?}, which is outside rewind's trash directory",
                stored.display().to_string()
            )));
        }
        Ok(())
    }

    /// Never move a directory that contains the trash into the trash.
    fn guard_not_ancestor_of_trash(&self, origin: &Path) -> Result<()> {
        let origin_abs = absolutize(origin)?;
        if let (Ok(origin_real), Ok(trash_real)) = (
            origin_abs.canonicalize(),
            self.paths.trash_dir.canonicalize(),
        ) {
            if trash_real.starts_with(&origin_real) {
                return Err(Error::Refused(format!(
                    "refusing to stage {} because it contains rewind's trash directory",
                    origin.display()
                )));
            }
        }
        Ok(())
    }
}

/// Move a path (file or tree) to a new location, refusing to clobber unless
/// explicitly requested. Shared by undo of `rm`, `mv`, and `git clean`.
pub fn move_path(from: &Path, to: &Path, overwrite: bool) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).path_context(parent)?;
    }
    if to.symlink_metadata().is_ok() {
        if !overwrite {
            return Err(Error::Refused(format!(
                "refusing to overwrite {} (move it away and retry `rewind undo`)",
                to.display()
            )));
        }
        remove_tree(to)?;
    }

    #[cfg(test)]
    let force_exdev = std::env::var_os("REWIND_TEST_FORCE_EXDEV").is_some();
    #[cfg(not(test))]
    let force_exdev = false;

    let rename_res = if force_exdev {
        Err(std::io::Error::from_raw_os_error(18)) // simulated EXDEV in tests
    } else {
        fs::rename(from, to)
    };

    match rename_res {
        Ok(()) => Ok(()),
        Err(err) if is_cross_device(&err) => {
            copy_tree(from, to)?;
            remove_tree(from)?;
            Ok(())
        }
        Err(err) => Err(crate::error::io_err(to, err)),
    }
}

/// Convert a possibly relative path into an absolute one without resolving
/// symlinks (which would change rm/mv semantics).
pub fn absolutize(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let cwd = std::env::current_dir().io_context("reading current directory")?;
        Ok(cwd.join(path))
    }
}

fn is_cross_device(err: &std::io::Error) -> bool {
    #[cfg(test)]
    if std::env::var_os("REWIND_TEST_FORCE_EXDEV").is_some() {
        return true;
    }
    err.kind() == ErrorKind::CrossesDevices
        || err.raw_os_error() == Some(18) // EXDEV on Linux/macOS
        || err.raw_os_error() == Some(17) // EXDEV on some BSDs
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SafeFileType {
    File,
    Dir,
    Symlink,
    Other,
}

#[cfg(unix)]
fn safe_file_type_from_mode(mode: libc::mode_t) -> SafeFileType {
    match mode & libc::S_IFMT {
        libc::S_IFDIR => SafeFileType::Dir,
        libc::S_IFREG => SafeFileType::File,
        libc::S_IFLNK => SafeFileType::Symlink,
        _ => SafeFileType::Other,
    }
}

#[cfg(unix)]
fn open_file_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(ErrorKind::InvalidInput, e))?;

    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn create_file_nofollow(path: &Path, truncate: bool) -> std::io::Result<fs::File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(ErrorKind::InvalidInput, e))?;

    let flags = if truncate {
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC
    } else {
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC
    };

    let fd = unsafe { libc::open(c_path.as_ptr(), flags, 0o666) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn read_dir_nofollow(path: &Path) -> Result<Vec<(String, SafeFileType, u64)>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::InvalidInput(format!("path contains null byte: {}", path.display())))?;

    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(crate::error::io_err(path, std::io::Error::last_os_error()));
    }

    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        unsafe { libc::close(fd) };
        return Err(crate::error::io_err(path, std::io::Error::last_os_error()));
    }

    let mut entries = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }

        let name_ptr = unsafe { (*entry).d_name.as_ptr() };
        let name_bytes = unsafe { std::ffi::CStr::from_ptr(name_ptr).to_bytes() };
        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }

        let name = match std::str::from_utf8(name_bytes) {
            Ok(s) => s.to_string(),
            Err(_) => {
                unsafe { libc::closedir(dir) };
                return Err(Error::InvalidInput(format!(
                    "directory entry in {} is not valid UTF-8",
                    path.display()
                )));
            }
        };

        let cur_dfd = unsafe { libc::dirfd(dir) };
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        let stat_res = unsafe {
            libc::fstatat(
                cur_dfd,
                name_ptr,
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if stat_res != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == ErrorKind::NotFound {
                continue;
            }
            unsafe { libc::closedir(dir) };
            return Err(crate::error::io_err(path.join(&name), err));
        }

        let st = unsafe { st.assume_init() };
        let file_type = safe_file_type_from_mode(st.st_mode);
        let size = if file_type == SafeFileType::File {
            st.st_size.max(0) as u64
        } else {
            0
        };
        entries.push((name, file_type, size));
    }

    unsafe { libc::closedir(dir) };
    Ok(entries)
}

/// Recursively copy preserving modes, symlinks, and timestamps without following
/// symlinks. Returns the number of bytes copied. Cleans up partial destinations on error.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<u64> {
    match copy_tree_inner(src, dst) {
        Ok(size) => Ok(size),
        Err(err) => {
            let _ = remove_tree(dst);
            Err(err)
        }
    }
}

fn copy_tree_inner(src: &Path, dst: &Path) -> Result<u64> {
    let meta = fs::symlink_metadata(src).path_context(src)?;
    let file_type = meta.file_type();

    if file_type.is_symlink() {
        let target = fs::read_link(src).path_context(src)?;
        if dst.symlink_metadata().is_ok() {
            remove_tree(dst)?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, dst).path_context(dst)?;
        #[cfg(not(unix))]
        {
            fs::copy(src, dst).path_context(dst)?;
        }
        return Ok(meta.len());
    }

    if file_type.is_dir() {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).path_context(parent)?;
        }
        if !dst.exists() {
            fs::create_dir(dst).path_context(dst)?;
        }

        let mut total = 0u64;
        #[cfg(unix)]
        {
            let entries = read_dir_nofollow(src)?;
            for (name, _, _) in entries {
                let child_src = src.join(&name);
                let child_dst = dst.join(&name);
                total = total.saturating_add(copy_tree_inner(&child_src, &child_dst)?);
            }
        }
        #[cfg(not(unix))]
        {
            let entries = fs::read_dir(src).path_context(src)?;
            for entry in entries {
                let entry = entry.path_context(src)?;
                let child_src = entry.path();
                let name = entry.file_name();
                total = total.saturating_add(copy_tree_inner(&child_src, &dst.join(name))?);
            }
        }
        copy_permissions(&meta, dst)?;
        copy_times(&meta, dst);
        return Ok(total);
    }

    if file_type.is_file() {
        #[cfg(unix)]
        let copied = {
            use std::io::{BufReader, BufWriter, Write};
            let src_file = open_file_nofollow(src).path_context(src)?;
            let dst_file = create_file_nofollow(dst, true).path_context(dst)?;
            let mut reader = BufReader::with_capacity(128 * 1024, src_file);
            let mut writer = BufWriter::with_capacity(128 * 1024, dst_file);
            let n = std::io::copy(&mut reader, &mut writer).path_context(src)?;
            writer.flush().path_context(dst)?;
            n
        };
        #[cfg(not(unix))]
        let copied = fs::copy(src, dst).path_context(src)?;

        copy_permissions(&meta, dst)?;
        copy_times(&meta, dst);
        return Ok(copied);
    }

    // FIFOs, sockets, and device nodes cannot be snapshotted portably. A hard
    // link can preserve them without reading their contents.
    match fs::hard_link(src, dst) {
        Ok(()) => Ok(0),
        Err(err) => Err(Error::Staging(format!(
            "cannot snapshot special file {}: {err}",
            src.display()
        ))),
    }
}

/// Clones a regular file using Copy-on-Write (CoW) extents where supported, falling
/// back to an independent byte copy. Never uses ordinary hard links, ensuring snapshot
/// isolation even if the source file is modified concurrently or subsequently (SEC-004).
pub fn clone_file_isolated(src: &Path, dst: &Path, meta: &fs::Metadata) -> Result<u64> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        if let (Ok(src_c), Ok(dst_c)) = (
            CString::new(src.as_os_str().as_bytes()),
            CString::new(dst.as_os_str().as_bytes()),
        ) {
            unsafe extern "C" {
                fn clonefile(
                    src: *const libc::c_char,
                    dst: *const libc::c_char,
                    flags: u32,
                ) -> libc::c_int;
            }
            if dst.symlink_metadata().is_ok() {
                let _ = fs::remove_file(dst);
            }
            const CLONE_NOFOLLOW: u32 = 0x0001;
            let res = unsafe { clonefile(src_c.as_ptr(), dst_c.as_ptr(), CLONE_NOFOLLOW) };
            if res == 0 {
                return Ok(meta.len());
            }
        }
    }

    #[cfg(unix)]
    {
        let src_file = open_file_nofollow(src).path_context(src)?;
        let dst_file = create_file_nofollow(dst, true).path_context(dst)?;

        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            const FICLONE: libc::c_ulong = 0x40049409;
            let res = unsafe { libc::ioctl(dst_file.as_raw_fd(), FICLONE as _, src_file.as_raw_fd()) };
            if res == 0 {
                copy_permissions(meta, dst)?;
                copy_times(meta, dst);
                return Ok(meta.len());
            }
        }

        let copied = {
            use std::io::{BufReader, BufWriter, Write};
            let mut reader = BufReader::with_capacity(128 * 1024, src_file);
            let mut writer = BufWriter::with_capacity(128 * 1024, dst_file);
            let n = std::io::copy(&mut reader, &mut writer).path_context(src)?;
            writer.flush().path_context(dst)?;
            n
        };
        copy_permissions(meta, dst)?;
        copy_times(meta, dst);
        Ok(copied)
    }

    #[cfg(not(unix))]
    {
        let copied = fs::copy(src, dst).path_context(src)?;
        copy_permissions(meta, dst)?;
        copy_times(meta, dst);
        Ok(copied)
    }
}

/// Recursively clone a tree using Copy-on-Write cloning or independent copying.
/// Unlike ordinary hard links, this guarantees snapshot isolation even if source files
/// are modified concurrently or after staging (SEC-004).
pub fn clone_tree(src: &Path, dst: &Path) -> Result<u64> {
    let meta = fs::symlink_metadata(src).path_context(src)?;
    let file_type = meta.file_type();

    if file_type.is_symlink() {
        let target = fs::read_link(src).path_context(src)?;
        if dst.symlink_metadata().is_ok() {
            remove_tree(dst)?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, dst).path_context(dst)?;
        #[cfg(not(unix))]
        fs::copy(src, dst).path_context(dst)?;
        return Ok(meta.len());
    }

    if file_type.is_dir() {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).path_context(parent)?;
        }
        if !dst.exists() {
            fs::create_dir(dst).path_context(dst)?;
        }
        let mut total = 0u64;
        #[cfg(unix)]
        {
            let entries = read_dir_nofollow(src)?;
            for (name, _, _) in entries {
                let child_src = src.join(&name);
                let child_dst = dst.join(&name);
                total = total.saturating_add(clone_tree(&child_src, &child_dst)?);
            }
        }
        #[cfg(not(unix))]
        {
            for entry in fs::read_dir(src).path_context(src)? {
                let entry = entry.path_context(src)?;
                let name = entry.file_name();
                total = total.saturating_add(clone_tree(&entry.path(), &dst.join(name))?);
            }
        }
        copy_permissions(&meta, dst)?;
        copy_times(&meta, dst);
        return Ok(total);
    }

    if file_type.is_file() {
        clone_file_isolated(src, dst, &meta)
    } else {
        Err(Error::Staging(format!(
            "cannot snapshot special file {}: unsupported file type",
            src.display()
        )))
    }
}

/// Deprecated synonym for [`clone_tree`]. Preserved for backward compatibility.
/// Never uses ordinary hard links for mutable files.
#[inline]
pub fn link_tree(src: &Path, dst: &Path) -> Result<u64> {
    clone_tree(src, dst)
}

/// Recursively delete a path without following symlinks.
///
/// Uses POSIX directory-descriptor relative traversal (`openat`/`unlinkat`) with
/// `O_NOFOLLOW` so that symlinks inside the tree are unlinked without traversing their
/// targets, and intermediate path component swaps cannot escape the deletion boundary.
pub fn remove_tree(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(crate::error::io_err(path, err)),
    };

    if !meta.file_type().is_dir() {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(crate::error::io_err(path, err)),
        };
    }

    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            Error::InvalidInput(format!("path contains null byte: {}", path.display()))
        })?;

        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };

        if fd < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ELOOP)
                || err.raw_os_error() == Some(libc::EMLINK)
                || err.raw_os_error() == Some(libc::ENOTDIR)
            {
                // Replaced with a symlink or non-directory: unlink the leaf itself; do not follow.
                return match fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(crate::error::io_err(path, e)),
                };
            }
            if err.kind() == ErrorKind::NotFound {
                return Ok(());
            }
            return Err(crate::error::io_err(path, err));
        }

        unsafe { remove_dir_contents_fd(fd, path)? };

        match fs::remove_dir(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(crate::error::io_err(path, err)),
        }
    }

    #[cfg(not(unix))]
    {
        for entry in fs::read_dir(path).path_context(path)? {
            let entry = entry.path_context(path)?;
            remove_tree(&entry.path())?;
        }
        fs::remove_dir(path).path_context(path)?;
        Ok(())
    }
}

#[cfg(unix)]
unsafe fn remove_dir_contents_fd(dfd: libc::c_int, context_path: &Path) -> Result<()> {
    let dir = unsafe { libc::fdopendir(dfd) };
    if dir.is_null() {
        unsafe { libc::close(dfd) };
        return Err(crate::error::io_err(
            context_path,
            std::io::Error::last_os_error(),
        ));
    }

    let mut last_err = None;

    loop {
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }

        let name_ptr = unsafe { (*entry).d_name.as_ptr() };
        let name_bytes = unsafe { std::ffi::CStr::from_ptr(name_ptr).to_bytes() };
        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }

        let cur_dfd = unsafe { libc::dirfd(dir) };

        let sub_fd = unsafe {
            libc::openat(
                cur_dfd,
                name_ptr,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };

        if sub_fd >= 0 {
            if let Err(e) = unsafe { remove_dir_contents_fd(sub_fd, context_path) } {
                last_err = Some(e);
            }
            if unsafe { libc::unlinkat(cur_dfd, name_ptr, libc::AT_REMOVEDIR) } != 0 {
                let mut err = std::io::Error::last_os_error();
                if err.kind() == ErrorKind::PermissionDenied {
                    let _ = unsafe { libc::fchmod(cur_dfd, 0o700) };
                    if unsafe { libc::unlinkat(cur_dfd, name_ptr, libc::AT_REMOVEDIR) } == 0 {
                        err = std::io::Error::from_raw_os_error(0);
                    } else {
                        err = std::io::Error::last_os_error();
                    }
                }
                if err.raw_os_error() != Some(0) && err.kind() != ErrorKind::NotFound {
                    last_err = Some(crate::error::io_err(context_path, err));
                }
            }
        } else if unsafe { libc::unlinkat(cur_dfd, name_ptr, 0) } != 0 {
            let mut err = std::io::Error::last_os_error();
            if err.kind() == ErrorKind::PermissionDenied {
                let _ = unsafe { libc::fchmod(cur_dfd, 0o700) };
                if unsafe { libc::unlinkat(cur_dfd, name_ptr, 0) } == 0 {
                    err = std::io::Error::from_raw_os_error(0);
                } else {
                    err = std::io::Error::last_os_error();
                }
            }
            if err.raw_os_error() != Some(0) && err.kind() != ErrorKind::NotFound {
                last_err = Some(crate::error::io_err(context_path, err));
            }
        }
    }

    unsafe { libc::closedir(dir) };

    if let Some(err) = last_err {
        Err(err)
    } else {
        Ok(())
    }
}

/// Total logical size of a tree without traversing symlinks.
pub fn tree_size(path: &Path) -> Result<u64> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(crate::error::io_err(path, err)),
    };
    if meta.file_type().is_symlink() {
        Ok(0)
    } else if meta.file_type().is_dir() {
        let mut total = 0u64;
        #[cfg(unix)]
        {
            match read_dir_nofollow(path) {
                Ok(entries) => {
                    for (name, file_type, size) in entries {
                        match file_type {
                            SafeFileType::File => {
                                total = total.saturating_add(size);
                            }
                            SafeFileType::Dir => {
                                total = total.saturating_add(tree_size(&path.join(name))?);
                            }
                            SafeFileType::Symlink | SafeFileType::Other => {}
                        }
                    }
                }
                Err(Error::Io { source, .. }) if source.raw_os_error() == Some(libc::ELOOP) => {
                    return Ok(0);
                }
                Err(err) => return Err(err),
            }
        }
        #[cfg(not(unix))]
        {
            for entry in fs::read_dir(path).path_context(path)? {
                let entry = entry.path_context(path)?;
                total = total.saturating_add(tree_size(&entry.path())?);
            }
        }
        Ok(total)
    } else {
        Ok(meta.len())
    }
}

fn copy_permissions(meta: &fs::Metadata, dst: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        if meta.file_type().is_symlink() {
            return Ok(());
        }
    }
    let perms = meta.permissions();
    fs::set_permissions(dst, perms).path_context(dst)
}

fn copy_times(meta: &fs::Metadata, dst: &Path) {
    #[cfg(unix)]
    {
        if meta.file_type().is_symlink() {
            return;
        }
    }
    let atime = filetime::FileTime::from_last_access_time(meta);
    let mtime = filetime::FileTime::from_last_modification_time(meta);
    let _ = filetime::set_file_times(dst, atime, mtime);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_paths(root: &Path) -> Paths {
        Paths {
            data_dir: root.join("data"),
            trash_dir: root.join("data/trash"),
            journal_path: root.join("data/journal.db"),
            lock_path: root.join("data/rewind.lock"),
            config_path: root.join("config.toml"),
        }
    }

    #[test]
    fn move_and_restore_file() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        paths.ensure().unwrap();
        let original = dir.path().join("notes.txt");
        fs::write(&original, b"hello").unwrap();

        let stager = Stager::new(&paths, 1);
        let payload = stager.stage_move(&original).unwrap();
        assert!(!original.exists());
        assert_eq!(fs::read(&payload.stored).unwrap(), b"hello");
        assert_eq!(payload.size, 5);

        stager.restore(&payload.stored, &original, false).unwrap();
        assert_eq!(fs::read(&original).unwrap(), b"hello");
        assert!(!payload.stored.exists());
    }

    #[test]
    fn restore_refuses_to_clobber() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        paths.ensure().unwrap();
        let original = dir.path().join("f");
        fs::write(&original, b"old").unwrap();
        let stager = Stager::new(&paths, 2);
        let payload = stager.stage_move(&original).unwrap();
        fs::write(&original, b"new").unwrap();
        let err = stager
            .restore(&payload.stored, &original, false)
            .unwrap_err();
        assert!(matches!(err, Error::Refused(_)));
        assert_eq!(fs::read(&original).unwrap(), b"new");
    }

    #[test]
    fn snapshot_copies_nested_tree() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        paths.ensure().unwrap();
        let src = dir.path().join("project/src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.rs"), b"fn main() {}").unwrap();

        let stager = Stager::new(&paths, 3);
        let payload = stager.snapshot(&src, SnapshotMode::Copy).unwrap();
        assert!(payload.stored.join("main.rs").exists());
        assert!(src.join("main.rs").exists());
        assert_eq!(payload.size, 12);
    }

    #[test]
    fn cross_device_fallback_is_exercised_by_copy_path() {
        // A same-filesystem rename is the fast path; explicitly verify the
        // copy+remove path used for EXDEV and for snapshots.
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        paths.ensure().unwrap();
        let original = dir.path().join("tree");
        fs::create_dir_all(original.join("a/b")).unwrap();
        fs::write(original.join("a/b/c"), b"x").unwrap();

        let dest = dir.path().join("copy");
        let size = copy_tree(&original, &dest).unwrap();
        assert_eq!(size, 1);
        remove_tree(&original).unwrap();
        assert!(!original.exists());
        assert!(dest.join("a/b/c").exists());
    }

    #[test]
    fn refuses_to_stage_a_parent_of_the_trash() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        paths.ensure().unwrap();
        let stager = Stager::new(&paths, 4);
        let err = stager.stage_move(dir.path()).unwrap_err();
        assert!(matches!(err, Error::Refused(_)));
    }

    #[test]
    fn tree_size_counts_files_but_not_symlinks() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a"), b"1234").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("a", dir.path().join("link")).unwrap();
        assert_eq!(tree_size(dir.path()).unwrap(), 4);
    }

    #[test]
    fn remove_tree_does_not_follow_symlink_to_external_target() {
        let dir = tempdir().unwrap();
        let external = dir.path().join("external");
        fs::create_dir_all(&external).unwrap();
        let secret = external.join("secret.txt");
        fs::write(&secret, b"super secret").unwrap();

        let victim = dir.path().join("victim");
        fs::create_dir_all(&victim).unwrap();
        let link = victim.join("link_to_external");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, &link).unwrap();

        remove_tree(&victim).unwrap();
        assert!(!victim.exists());
        assert!(external.exists());
        assert_eq!(fs::read(&secret).unwrap(), b"super secret");
    }

    #[test]
    fn remove_tree_handles_dangling_symlink() {
        let dir = tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir_all(&victim).unwrap();
        let dangling = victim.join("dangling");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("nonexistent"), &dangling).unwrap();

        remove_tree(&victim).unwrap();
        assert!(!victim.exists());
    }

    #[test]
    fn remove_tree_handles_readonly_directory() {
        let dir = tempdir().unwrap();
        let victim = dir.path().join("readonly_dir");
        fs::create_dir_all(&victim).unwrap();
        let file = victim.join("data.txt");
        fs::write(&file, b"content").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&victim, fs::Permissions::from_mode(0o500)).unwrap();
        }

        remove_tree(&victim).unwrap();
        assert!(!victim.exists());
    }

    #[test]
    fn copy_tree_preserves_symlink_without_traversing() {
        let dir = tempdir().unwrap();
        let external = dir.path().join("external");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("file.txt"), b"outside").unwrap();

        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let link = src.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, &link).unwrap();

        let dest = dir.path().join("dest");
        let size = copy_tree(&src, &dest).unwrap();
        assert_eq!(size, external.as_os_str().len() as u64);
        assert!(!dest.join("file.txt").exists());

        #[cfg(unix)]
        {
            let dest_link = dest.join("link");
            let meta = fs::symlink_metadata(&dest_link).unwrap();
            assert!(meta.file_type().is_symlink());
            assert_eq!(fs::read_link(&dest_link).unwrap(), external);
        }
    }

    #[test]
    fn cross_device_forced_exdev_seam_works() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        paths.ensure().unwrap();

        let file = dir.path().join("file.txt");
        fs::write(&file, b"cross-device payload").unwrap();

        unsafe {
            std::env::set_var("REWIND_TEST_FORCE_EXDEV", "1");
        }
        let stager = Stager::new(&paths, 99);
        let payload = stager.stage_move(&file).unwrap();
        assert!(!file.exists());
        assert_eq!(fs::read(&payload.stored).unwrap(), b"cross-device payload");

        stager.restore(&payload.stored, &file, false).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"cross-device payload");
        assert!(!payload.stored.exists());

        unsafe {
            std::env::remove_var("REWIND_TEST_FORCE_EXDEV");
        }
    }
}
