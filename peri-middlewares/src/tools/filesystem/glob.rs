use std::path::Path;

use peri_agent::tools::BaseTool;
use serde_json::Value;

use super::resolve_path;
use crate::tools::output_persist::persist_truncated_output;

/// Glob tool — aligned with the TypeScript glob_tool.
pub struct GlobFilesTool {
    pub cwd: String,
}

impl GlobFilesTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }
}

/// Maximum number of files returned; protects the LLM context window from exploding.
const MAX_RESULTS: usize = 1_000;
/// Maximum output bytes; beyond this, the full payload is persisted to a temp file and only the first N paths are returned inline with a hint.
const MAX_OUTPUT_BYTES: usize = 20_000;
/// When the byte limit is hit, how many paths to keep inline so the LLM can see them directly.
const HEAD_RESULTS_ON_BYTES_OVERFLOW: usize = 100;

const GLOB_FILES_DESCRIPTION: &str = r#"Fast file pattern matching tool that works with any codebase size. Supports glob patterns like "**/*.js" or "src/**/*.ts". Returns matching file paths sorted by modification time.

Usage:
- Use this tool when you need to find files by name patterns
- Returns file paths sorted by modification time (most recently modified first)
- Maximum 1000 results returned; results are truncated beyond this limit with a notice
- Output exceeding 20000 bytes is persisted to a temp file; only the first 100 paths are returned inline with a path hint
- Common directories like node_modules, .git, target, dist, build are automatically excluded from results
- The path parameter is optional; defaults to the current working directory
- For searching file contents, use Grep instead

When to use:
- Use Glob when searching for files by name pattern (e.g., find all TypeScript files, find a specific config file)
- Use Grep when searching for content within files (e.g., find where a function is defined)
- For open-ended searches requiring multiple rounds, consider using a sub-agent via Agent

Anti-patterns (will be warned):
- Glob("*") or Glob("**/*") produces massive directory dumps — use folder_operations or Bash ls to list directories.
- Prefer specific patterns like "**/*.rs" over "**/*" — extension filtering keeps output bounded.

Output-size protection (always active, no opt-in):
- Directories named node_modules, .git, target, dist, build, worktrees, and similar caches/copies are skipped during the walk, so globbing the project root won't enumerate worktree or build copies.
- Results exceeding 1000 entries or 20000 bytes are truncated inline; the full payload is persisted to a temp file and the path is returned in the output."#;

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | ".git"
            | "dist"
            | "build"
            | ".next"
            | ".turbo"
            | "coverage"
            | ".nyc_output"
            | "temp"
            | ".cache"
            | "vendor"
            | "venv"
            | "__pycache__"
            | "target"
            | "out"
            | ".output"
            | "worktrees"
    )
}

/// Soft-warn pattern — still executes, but prepends a warning. A hit strongly suggests the caller actually wanted to list a directory.
fn soft_warn_pattern(pattern: &str) -> Option<&'static str> {
    match pattern.trim() {
        "*" => Some("Bare `*` matches every entry in the current directory; use folder_operations or Bash ls to list a directory instead."),
        "**" | "**/*" => Some("`**/*` recursively expands the entire subtree (including every worktree/plugin copy); prefer folder_operations or a more specific pattern."),
        _ => None,
    }
}

fn glob_match(pattern: &str, path: &str) -> bool {
    glob::Pattern::new(pattern)
        .map(|p| p.matches(path))
        .unwrap_or(false)
}

fn collect_files(base: &Path, pattern: &str, results: &mut Vec<String>) {
    let walker = walkdir::WalkDir::new(base)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                !should_skip_dir(&name)
            } else {
                true
            }
        });

    for entry in walker {
        match entry {
            Ok(e) => {
                if e.file_type().is_file() {
                    let abs_path = e.path().to_string_lossy().to_string();
                    if let Ok(rel) = e.path().strip_prefix(base) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        if glob_match(pattern, &rel_str) {
                            results.push(abs_path);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "glob walk error (skipped)");
            }
        }
    }
}

#[async_trait::async_trait]
impl BaseTool for GlobFilesTool {
    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        GLOB_FILES_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The glob pattern to match files against (e.g. \"**/*.js\", \"src/**/*.rs\", \"*.config.json\"). Use ** for recursive matching"
                },
                "path": {
                    "type": "string",
                    "description": "The directory to search in. Absolute path or relative to cwd. If not specified, the current working directory is used"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or("The 'pattern' parameter is required for the Glob tool.")?;

        // Pattern syntax pre-validation: makes B3 suggester reliable.
        // Without this, glob::Pattern::new silently returns false inside glob_match,
        // and the LLM gets "No files found." instead of a syntax error.
        if let Err(e) = glob::Pattern::new(pattern) {
            return Err(format!("Error: Pattern syntax error in {pattern:?}: {e}").into());
        }

        // Pattern soft-warn — record the hint; we still execute so the LLM can see the output size and self-correct.
        let pattern_warn = soft_warn_pattern(pattern);

        let search_root = if let Some(p) = input["path"].as_str() {
            resolve_path(&self.cwd, p)
        } else {
            Path::new(&self.cwd).to_path_buf()
        };

        if !search_root.exists() {
            return Err(format!("Error: Directory not found: {}", search_root.display()).into());
        }

        let mut results = Vec::new();
        collect_files(&search_root, pattern, &mut results);

        results.sort_by(|a, b| {
            let ta = std::fs::metadata(a).and_then(|m| m.modified()).ok();
            let tb = std::fs::metadata(b).and_then(|m| m.modified()).ok();
            tb.cmp(&ta)
        });

        let body = if results.is_empty() {
            "No files found.".to_string()
        } else if results.len() > MAX_RESULTS {
            // Count guard: more than 1000 results.
            let full = results.join("\n");
            let truncated = &results[..MAX_RESULTS];
            let persist_hint = persist_truncated_output(&full);
            format!(
                "{}\n\n[Output truncated: {} files total, showing first {}]{}",
                truncated.join("\n"),
                results.len(),
                MAX_RESULTS,
                persist_hint
            )
        } else {
            let joined = results.join("\n");
            // Byte guard: many short paths can still overflow by total byte size.
            if joined.len() > MAX_OUTPUT_BYTES {
                let persist_hint = persist_truncated_output(&joined);
                let head_count = HEAD_RESULTS_ON_BYTES_OVERFLOW.min(results.len());
                let head = &results[..head_count];
                format!(
                    "{}\n\n[Output truncated: {} files total, {} bytes; showing first {} — exceeds {} byte limit]{}",
                    head.join("\n"),
                    results.len(),
                    joined.len(),
                    head_count,
                    MAX_OUTPUT_BYTES,
                    persist_hint
                )
            } else {
                joined
            }
        };

        if let Some(warn) = pattern_warn {
            Ok(format!("Note: {warn}\n\n{body}"))
        } else {
            Ok(body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("glob_test.rs");
}
