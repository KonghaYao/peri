use std::path::Path;

use chrono::{TimeZone, Utc};
use peri_agent::tools::BaseTool;
use serde_json::Value;

use super::resolve_path;
use crate::tools::output_persist::persist_truncated_output;

/// folder_operations tool - 与 TypeScript folder_tool 对齐
pub struct FolderOperationsTool {
    pub cwd: String,
}

impl FolderOperationsTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }
}

/// 列表操作最多返回的条目数，防止撑爆 LLM context window
const MAX_LIST_ENTRIES: usize = 500;

const FOLDER_OPERATIONS_DESCRIPTION: &str = r#"Unified folder operations tool supporting create, list, and existence check.

Operations:
- "create": Creates a directory at the specified path. By default creates parent directories recursively (recursive: true). Use recursive: false to only create a single directory level
- "list": Lists the contents of a directory, showing files and subdirectories with sizes and modification dates. Output is truncated beyond 500 entries
- "exists": Checks whether a path exists and whether it is a directory or file

Usage:
- The folder_path parameter must be an absolute path, not a relative path
- You can call multiple tools in a single response. It is always better to check directory existence before creating or listing
- When creating a directory, the recursive parameter defaults to true, creating all necessary parent directories

Notes:
- List output shows entries with file size and modification date
- Directories are shown with a trailing / indicator
- For large directories (>500 entries), output is truncated with a summary count"#;

fn list_folder(resolved: &Path) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let entries = std::fs::read_dir(resolved)?;

    let mut folders: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let metadata = entry.metadata()?;
        let name = entry.file_name().to_string_lossy().to_string();
        let size = metadata.len();
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| {
                    Utc.timestamp_opt(d.as_secs() as i64, 0)
                        .single()
                        .map(|dt| dt.format("%Y/%m/%d").to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                })
            })
            .unwrap_or_else(|| "unknown".to_string());

        if metadata.is_dir() {
            folders.push(format!("  📁 {name}/ ({size} bytes, {modified})"));
        } else {
            files.push(format!("  📄 {name} ({size} bytes, {modified})"));
        }
    }

    let total_folders = folders.len();
    let total_files = files.len();
    let total = total_folders + total_files;
    let truncated = total > MAX_LIST_ENTRIES;
    let mut persist_hint = String::new();

    if truncated {
        // 在截断前保存完整列表用于持久化（必须在 truncate 之前）
        let full_list: String = folders
            .iter()
            .chain(files.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let total_summary = format!(
            "Total: {} directories, {} files",
            total_folders, total_files
        );
        let full_text = format!("{}\n{}", full_list, total_summary);
        persist_hint = persist_truncated_output(&full_text);

        // 公平分配截断
        let half = MAX_LIST_ENTRIES / 2;
        folders.truncate(half.min(folders.len()));
        files.truncate((MAX_LIST_ENTRIES - folders.len()).min(files.len()));
    }

    let mut result = format!("📁 {}\n\n", resolved.display());

    if !folders.is_empty() {
        result.push_str("Directories:\n");
        for f in &folders {
            result.push_str(f);
            result.push('\n');
        }
        result.push('\n');
    }

    if !files.is_empty() {
        result.push_str("Files:\n");
        for f in &files {
            result.push_str(f);
            result.push('\n');
        }
    }

    if truncated {
        result.push_str(&format!(
            "\n[Output truncated: {} total entries, showing first {}]{}",
            total, MAX_LIST_ENTRIES, persist_hint
        ));
    }

    result.push_str(&format!(
        "\nTotal: {} directories, {} files",
        total_folders, total_files
    ));

    Ok(result)
}

#[async_trait::async_trait]
impl BaseTool for FolderOperationsTool {
    fn name(&self) -> &str {
        "folder_operations"
    }

    fn description(&self) -> &str {
        FOLDER_OPERATIONS_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["create", "list", "exists"],
                    "description": "The folder operation to perform: \"create\" to create a directory, \"list\" to list directory contents, \"exists\" to check if a path exists"
                },
                "folder_path": {
                    "type": "string",
                    "description": "The absolute path to the folder for the operation"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "For \"create\" operation: whether to create parent directories if needed (default true). Ignored for other operations"
                }
            },
            "required": ["operation", "folder_path"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let operation = input["operation"]
            .as_str()
            .ok_or("Missing operation parameter")?;
        let folder_path = input["folder_path"]
            .as_str()
            .ok_or("Missing folder_path parameter")?;
        let recursive = input["recursive"].as_bool().unwrap_or(true);

        let resolved = resolve_path(&self.cwd, folder_path);

        match operation {
            "create" => {
                if recursive {
                    std::fs::create_dir_all(&resolved)?;
                } else {
                    std::fs::create_dir(&resolved)?;
                }
                Ok(format!(
                    "\u{2713} Folder created successfully at: {}",
                    resolved.display()
                ))
            }

            "exists" => {
                if resolved.exists() {
                    let kind = if resolved.is_dir() {
                        "Directory"
                    } else {
                        "File"
                    };
                    Ok(format!(
                        "\u{2713} Folder exists at: {}\n  Type: {kind}",
                        resolved.display()
                    ))
                } else {
                    Ok(format!(
                        "\u{2717} Folder does not exist at: {}",
                        resolved.display()
                    ))
                }
            }

            "list" => {
                if !resolved.exists() {
                    return Err(format!("Folder not found: {}", resolved.display()).into());
                }
                if !resolved.is_dir() {
                    return Err(
                        format!("Path exists but is not a folder: {}", resolved.display()).into(),
                    );
                }
                list_folder(&resolved)
            }

            other => Err(format!("Unknown operation: {other}").into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("folder_test.rs");
}
