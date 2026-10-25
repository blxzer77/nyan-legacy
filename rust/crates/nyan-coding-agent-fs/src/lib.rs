//! File replacement that a crash cannot tear.
//!
//! [`write_atomic`] writes a complete new file next to the target and renames it over
//! the target. A reader, or a process that dies mid-write, sees the old file or the new
//! one, never an empty or half-written file the way a truncate-then-write sees it.
//!
//! `PARITY-DEVIATION(atomic-write)`: the TypeScript tools write in place with
//! `writeFile`, which keeps the file's inode. Replacing the file gives it a new inode, so
//! a hard link to the old file no longer sees later writes, and an owner, an ACL, or an
//! extended attribute that the process cannot recreate is not carried over. A symlink is
//! followed, so the link stays a link and the file it points to is replaced. When the
//! directory cannot be written but the file can, the content is written in place instead
//! of failing, which is the only case that is not atomic.

#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How the Unix permission bits of the written file are chosen. Windows ignores it and
/// keeps the read-only attribute of a file that is replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileMode {
    /// A file that is replaced keeps its mode; a new file gets the process default.
    Preserve,
    /// The file always gets this mode (for example `0o600` for credentials).
    Exactly(u32),
}

/// Delays between attempts to rename over a file another process has open. Windows
/// refuses the rename while a virus scanner or an editor holds the target.
const RENAME_RETRY_DELAYS_MS: [u64; 10] = [1, 2, 4, 8, 16, 32, 32, 32, 32, 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Replaces `path` with `contents` all at once.
///
/// # Errors
///
/// Returns the operating system error when the new file cannot be written or renamed.
/// A failure leaves the old file untouched and removes the temporary file.
pub fn write_atomic(path: &Path, contents: &[u8], mode: FileMode) -> io::Result<()> {
    write_atomic_with(path, contents, mode, &mut |from, to| fs::rename(from, to))
}

/// The rename step is a parameter so tests can fail it.
fn write_atomic_with(
    path: &Path,
    contents: &[u8],
    mode: FileMode,
    rename: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let target = resolve_target(path);
    let existing = fs::metadata(&target).ok();
    let temporary = temporary_path(&target);

    let written = create_temporary(&temporary, mode).and_then(|mut file| {
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        if let (FileMode::Preserve, Some(existing)) = (mode, existing.as_ref()) {
            fs::set_permissions(&temporary, existing.permissions())?;
        }
        Ok(())
    });
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    match rename_with_retry(
        &temporary,
        &target,
        rename,
        &is_transient_rename_error,
        true,
    ) {
        Ok(()) => {
            sync_directory(&target);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            // The directory refuses new names but the file itself may be writable.
            if error.kind() == io::ErrorKind::PermissionDenied && existing.is_some() {
                return fs::write(&target, contents);
            }
            Err(error)
        }
    }
}

fn rename_with_retry(
    from: &Path,
    to: &Path,
    rename: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
    transient: &dyn Fn(&io::Error) -> bool,
    sleep: bool,
) -> io::Result<()> {
    let mut delays = RENAME_RETRY_DELAYS_MS.iter();
    loop {
        match rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if transient(&error) => match delays.next() {
                Some(delay) => {
                    if sleep {
                        std::thread::sleep(Duration::from_millis(*delay));
                    }
                }
                None => return Err(error),
            },
            Err(error) => return Err(error),
        }
    }
}

/// Windows reports a rename blocked by another process's open handle as access denied,
/// a sharing violation, or a lock violation. Nowhere else is a rename worth repeating.
fn is_transient_rename_error(error: &io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

/// A symlink is followed so the link survives; a path that does not exist yet is itself.
fn resolve_target(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| {
        match (path.parent().map(fs::canonicalize), path.file_name()) {
            (Some(Ok(parent)), Some(name)) => parent.join(name),
            _ => path.to_path_buf(),
        }
    })
}

/// A name in the target's directory that another process cannot guess in advance, so the
/// file is never created through a link planted at a predictable name.
fn temporary_path(target: &Path) -> PathBuf {
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let name = target.file_name().map_or_else(
        || "file".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    target.with_file_name(format!(
        ".{name}.{}.{counter}.{nanos:x}.tmp",
        std::process::id()
    ))
}

fn create_temporary(path: &Path, mode: FileMode) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let FileMode::Exactly(bits) = mode {
            options.mode(bits);
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
    options.open(path)
}

/// Makes the rename itself durable. A directory cannot be opened for syncing on Windows,
/// and a failure here does not undo a rename that happened, so errors are ignored.
fn sync_directory(target: &Path) {
    #[cfg(unix)]
    if let Some(directory) = target.parent()
        && let Ok(handle) = File::open(directory)
    {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = target;
}

#[cfg(test)]
mod tests;
