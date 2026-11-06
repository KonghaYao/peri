//! Private temp storage for persisted tool output.
//!
//! Truncated tool output keeps its full payload here so the model can read it
//! back later. A process-private directory (0700) and owner-only files (0600)
//! keep other local users from reading payloads that may contain secrets. The
//! directory lock lets the sweep distinguish live sessions from crash leftovers.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// Prefix shared by the private directory name and persisted payload files.
pub(crate) const OUTPUT_PREFIX: &str = "peri-tool-output-";
const LOCK_FILE: &str = ".lock";
/// Crash leftovers are swept on the next process start after this age. Live
/// sessions keep their `.lock` held, so only abandoned directories qualify.
const DIR_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

struct PrivateDir {
    path: PathBuf,
    // Held for the process lifetime; a lockable `.lock` means the owner is gone.
    _lock: std::fs::File,
}

static PRIVATE_DIR: OnceLock<Option<PrivateDir>> = OnceLock::new();

/// Directory shared by every persisted output in this process.
pub(crate) fn private_output_dir() -> Option<&'static Path> {
    PRIVATE_DIR
        .get_or_init(|| match create_private_dir(&std::env::temp_dir()) {
            Ok(dir) => Some(dir),
            Err(error) => {
                tracing::warn!(%error, "failed to create private tool output directory");
                None
            }
        })
        .as_ref()
        .map(|dir| dir.path.as_path())
}

/// Persist `content` as a new owner-only file and return its path.
pub(crate) fn write_output(content: &str) -> io::Result<PathBuf> {
    let directory = private_output_dir()
        .ok_or_else(|| io::Error::other("private tool output directory is unavailable"))?;
    let name = format!("{OUTPUT_PREFIX}{}.txt", uuid::Uuid::new_v4());
    write_private_file(directory, &name, content.as_bytes())
}

fn create_private_dir(parent: &Path) -> io::Result<PrivateDir> {
    sweep_stale_dirs(parent);
    let path = parent.join(format!("{OUTPUT_PREFIX}{}", uuid::Uuid::new_v4()));
    #[cfg(unix)]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path)?;
    match prepare_private_dir(&path) {
        Ok(lock) => Ok(PrivateDir { path, _lock: lock }),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&path);
            Err(error)
        }
    }
}

fn prepare_private_dir(path: &Path) -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // umask can strip bits from the requested mode; pin the exact bits so
        // the directory is always usable and private.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(path.join(LOCK_FILE))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        lock.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            io::Error::other("private output directory lock is held")
        }
        std::fs::TryLockError::Error(error) => error,
    })?;
    Ok(lock)
}

fn write_private_file(directory: &Path, name: &str, content: &[u8]) -> io::Result<PathBuf> {
    let path = directory.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    if let Err(error) = write_owner_only(&mut file, content) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

fn write_owner_only(file: &mut std::fs::File, content: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // umask can strip bits from the requested mode; pin the exact bits on
        // the open descriptor so a path swap cannot redirect the chmod.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    io::Write::write_all(file, content)
}

/// Remove abandoned output directories owned by an earlier process.
///
/// Best effort: every failure only means the leftover survives until the next
/// process start.
fn sweep_stale_dirs(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(OUTPUT_PREFIX)
        {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // `file_type` does not follow symlinks, so a link cannot redirect the sweep.
        if !file_type.is_dir() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now.duration_since(modified).unwrap_or_default() < DIR_RETENTION {
            continue;
        }
        if lock_is_held(&entry.path()) {
            continue;
        }
        if let Err(error) = std::fs::remove_dir_all(entry.path()) {
            tracing::debug!(%error, path = %entry.path().display(), "stale tool output cleanup failed");
        }
    }
}

fn lock_is_held(directory: &Path) -> bool {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    let Ok(file) = options.open(directory.join(LOCK_FILE)) else {
        // A live writer always creates the lock first; a directory without one
        // is a leftover.
        return false;
    };
    match file.try_lock() {
        Ok(()) => false,
        Err(std::fs::TryLockError::WouldBlock) => true,
        // Locking unsupported: keep the directory rather than risk deleting
        // output a live session may still reference.
        Err(std::fs::TryLockError::Error(_)) => true,
    }
}

#[cfg(test)]
#[path = "private_temp_test.rs"]
mod tests;
