//! Sandboxed Write tool — wraps WriteFileTool with directory sandbox restrictions.
//!
//! Replaces the standalone WriteSandbox tool. When an agent declares `allowedWriteDirs`,
//! the parent's unrestricted `Write` tool is replaced with this sandboxed version that
//! validates every write path against the allowed directory whitelist.

use peri_agent::tools::BaseTool;
use serde_json::Value;
use std::path::PathBuf;

use super::sandbox_guard::validate_sandbox_path;
use super::write::WRITE_FILE_DESCRIPTION;
use super::WriteFileTool;

/// A Write tool wrapper that restricts writes to a set of sandbox directories.
///
/// Named "Write" so LLMs use a single unified tool name. The description is
/// dynamically generated to include the allowed directory list.
pub struct SandboxedWriteTool {
    /// Inner unrestricted WriteFileTool — unchanged per open/closed principle
    inner: WriteFileTool,
    /// Canonicalized sandbox directory roots
    sandbox_dirs: Vec<PathBuf>,
    /// Dynamically generated description (pre-built at construction time)
    desc: String,
}

impl SandboxedWriteTool {
    /// Construct a sandboxed Write tool.
    ///
    /// `allowed_dirs` are relative paths (from frontmatter `allowedWriteDirs`).
    /// Directories are auto-created if they don't exist.
    ///
    /// Returns `Err` if any sandbox directory cannot be created or canonicalized.
    pub fn new(
        cwd: impl Into<String>,
        allowed_dirs: Vec<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let cwd_raw = cwd.into();
        let cwd = std::path::Path::new(&cwd_raw)
            .canonicalize()
            .map(|p| p.display().to_string())
            .unwrap_or(cwd_raw);

        let mut sandbox_dirs = Vec::new();
        for dir in &allowed_dirs {
            let raw = std::path::Path::new(&cwd).join(dir);
            if !raw.exists() {
                std::fs::create_dir_all(&raw).map_err(|e| {
                    format!(
                        "SandboxedWrite: failed to create sandbox dir '{}': {}",
                        dir, e
                    )
                })?;
            }
            let canonical = raw.canonicalize().map_err(|e| {
                format!(
                    "SandboxedWrite: failed to canonicalize sandbox dir '{}': {}",
                    dir, e
                )
            })?;
            if !canonical.is_dir() {
                return Err(
                    format!("SandboxedWrite: sandbox path '{}' is not a directory", dir).into(),
                );
            }
            sandbox_dirs.push(canonical);
        }

        let dirs_display: Vec<String> = sandbox_dirs
            .iter()
            .map(|d| format!("- {}", d.display()))
            .collect();

        let desc = format!(
            "{}\n\n**Restriction**: You can only write to files within the following directories:\n{}",
            WRITE_FILE_DESCRIPTION,
            dirs_display.join("\n")
        );

        Ok(Self {
            inner: WriteFileTool::new(cwd),
            sandbox_dirs,
            desc,
        })
    }
}

#[async_trait::async_trait]
impl BaseTool for SandboxedWriteTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        &self.desc
    }

    fn parameters(&self) -> Value {
        self.inner.parameters()
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        self.inner.timeout()
    }

    fn aliases(&self) -> &[&str] {
        self.inner.aliases()
    }

    async fn invoke(
        &self,
        input: Value,
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or("The 'file_path' parameter is required for the Write tool.")?;

        // Validate path is within sandbox → returns canonicalized absolute path
        let canonicalized = validate_sandbox_path(&self.inner.cwd, file_path, &self.sandbox_dirs)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

        // Modify input: replace relative file_path with canonicalized absolute path
        // so inner WriteFileTool's resolve_path uses it directly (no re-resolution)
        let mut modified_input = input;
        modified_input["file_path"] =
            serde_json::Value::String(canonicalized.to_string_lossy().to_string());

        // Delegate actual write to inner WriteFileTool
        self.inner.invoke(modified_input, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("sandboxed_write_test.rs");
}
