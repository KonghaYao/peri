use std::fs;
use std::path::{Path, PathBuf};

use crate::domain::error::SwarmError;
use crate::domain::identity::LogicalId;

/// Resolve the session workspace directory (`design §2.2`, §4.6).
///
/// - Explicit `cwd` must be an absolute, existing, normalized directory.
/// - Omitted `cwd` falls back to `<workspace_root>/<logicalId>` and is created
///   when `create_missing` is set (SDK clients do not send `cwd`).
pub fn resolve_cwd(
    requested: Option<&str>,
    workspace_root: &Path,
    logical_id: &LogicalId,
    create_missing: bool,
) -> Result<PathBuf, SwarmError> {
    let candidate = match requested {
        Some(value) => {
            let path = Path::new(value);
            if !path.is_absolute() {
                return Err(SwarmError::cwd_not_normalizable("not absolute"));
            }
            path.to_path_buf()
        }
        None => {
            let path = workspace_root.join(logical_id.as_str());
            if create_missing {
                fs::create_dir_all(&path)
                    .map_err(|_| SwarmError::cwd_not_normalizable("workspace directory"))?;
            }
            path
        }
    };

    let canonical = fs::canonicalize(&candidate).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SwarmError::cwd_not_found()
        } else {
            SwarmError::cwd_not_normalizable("canonicalize failed")
        }
    })?;

    if !canonical.is_dir() {
        return Err(SwarmError::cwd_not_normalizable("not a directory"));
    }
    if canonical.to_str().is_none() {
        return Err(SwarmError::cwd_not_normalizable("not valid UTF-8"));
    }
    Ok(canonical)
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;
