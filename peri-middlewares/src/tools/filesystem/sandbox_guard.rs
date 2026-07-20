//! Sandbox path validation guard — extracted from WriteSandboxTool::validate_path().
//!
//! Used by both SandboxedWriteTool and the deprecated WriteSandboxTool.

use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors returned by path sandbox validation.
#[derive(Debug, Error)]
pub enum SandboxPathError {
    #[error("Absolute paths are not allowed: {path}")]
    AbsolutePath { path: String },
    #[error("Path traversal detected: {path}")]
    PathTraversal { path: String },
    #[error("Path '{path}' is outside allowed directories: {dirs:?}")]
    OutsideSandbox { path: String, dirs: Vec<PathBuf> },
    #[error("Failed to create parent directory: {source}")]
    CreateDirError { source: std::io::Error },
    #[error("Failed to canonicalize path: {source}")]
    CanonicalizeError { source: std::io::Error },
}

/// Validate that `file_path` (relative to `cwd`) resolves within the sandbox roots.
///
/// Returns the canonicalized absolute target path on success.
///
/// # Arguments
/// * `cwd` - Already-canonicalized working directory (project root)
/// * `file_path` - Relative path to validate (rejects absolute paths and ".." traversal)
/// * `sandbox_roots` - Already-canonicalized sandbox directory paths
///
/// # Validation pipeline (5 layers)
/// 1. Reject absolute paths (lexical check)
/// 2. Reject `..` path traversal (segment check)
/// 3. Find longest existing ancestor → canonicalize → sandbox prefix check
/// 4. Create remaining parent directories → canonicalize parent → join filename
/// 5. Final prefix match against all sandbox roots
pub fn validate_sandbox_path(
    cwd: &str,
    file_path: &str,
    sandbox_roots: &[PathBuf],
) -> Result<PathBuf, SandboxPathError> {
    // ① Lexically reject absolute paths
    if Path::new(file_path).is_absolute() {
        return Err(SandboxPathError::AbsolutePath {
            path: file_path.to_string(),
        });
    }

    // ② Lexically reject path traversal (.., ..\, etc.)
    let normalized = file_path.replace('\\', "/");
    for segment in normalized.split('/') {
        if segment == ".." {
            return Err(SandboxPathError::PathTraversal {
                path: file_path.to_string(),
            });
        }
    }

    let raw = Path::new(cwd).join(file_path);

    // ③ Find longest existing ancestor → canonicalize + sandbox check
    // ④ Create remaining parent dirs → canonicalize parent → join filename
    let canonical_target = if raw.exists() {
        raw.canonicalize()
            .map_err(|e| SandboxPathError::CanonicalizeError { source: e })?
    } else {
        let ancestor = {
            let mut p = raw.as_path();
            loop {
                match p.parent() {
                    Some(parent) if !parent.exists() => p = parent,
                    Some(parent) => break parent.to_path_buf(),
                    None => break p.to_path_buf(),
                }
            }
        };
        let canon_ancestor = ancestor
            .canonicalize()
            .map_err(|e| SandboxPathError::CanonicalizeError { source: e })?;

        let is_ancestor_in_sandbox = sandbox_roots
            .iter()
            .any(|root| canon_ancestor.starts_with(root));
        if !is_ancestor_in_sandbox {
            return Err(SandboxPathError::OutsideSandbox {
                path: file_path.to_string(),
                dirs: sandbox_roots.to_vec(),
            });
        }

        // Create remaining parent dirs (safe: ancestor is within sandbox)
        if let Some(parent) = raw.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| SandboxPathError::CreateDirError { source: e })?;
        }

        if let (Some(parent), Some(file_name)) = (raw.parent(), raw.file_name()) {
            parent
                .canonicalize()
                .map_err(|e| SandboxPathError::CanonicalizeError { source: e })?
                .join(file_name)
        } else {
            return Err(SandboxPathError::CanonicalizeError {
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "Unable to resolve path '{}': missing parent or filename",
                        file_path
                    ),
                ),
            });
        }
    };

    // ⑤ Final prefix match against all sandbox roots
    let is_in_sandbox = sandbox_roots
        .iter()
        .any(|root| canonical_target.starts_with(root));
    if !is_in_sandbox {
        return Err(SandboxPathError::OutsideSandbox {
            path: file_path.to_string(),
            dirs: sandbox_roots.to_vec(),
        });
    }

    Ok(canonical_target)
}
