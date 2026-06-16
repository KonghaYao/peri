use crate::error::Result;
use std::path::Path;

/// Remove a path that may be a symlink, a regular file, or a directory.
///
/// On Windows, directory symlinks must be removed with `remove_dir`; using
/// `remove_file` on a directory symlink fails, and `remove_dir_all` would
/// follow the link and delete the target contents. On Unix, `remove_file`
/// works for all symlinks.
pub(crate) fn remove_symlink_or_dir(path: &Path) -> Result<()> {
    if path.is_symlink() {
        #[cfg(windows)]
        {
            if path.is_dir() {
                return std::fs::remove_dir(path).map_err(Into::into);
            }
        }
        std::fs::remove_file(path)?;
    } else if path.is_file() {
        std::fs::remove_file(path)?;
    } else if path.is_dir() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

/// Recursively copy a directory tree from src to dst.
///
/// dst must not already exist (the caller should remove it first).
/// Symlinks are followed — their targets are copied as regular files/directories.
pub(crate) fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(&src_path)?;
            if target.is_dir() {
                copy_dir_all(&target, &dst_path)?;
            } else {
                std::fs::copy(&target, &dst_path)?;
            }
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Check whether two paths refer to the same location, using canonicalization
/// when possible to tolerate Windows short/long path variants and symlink
/// indirection.
pub(crate) fn paths_equal(a: &Path, b: &Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        return ca == cb;
    }
    a == b
}
