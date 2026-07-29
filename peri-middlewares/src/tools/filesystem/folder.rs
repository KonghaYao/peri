use std::path::Path;

use chrono::{TimeZone, Utc};
use peri_agent::tools::BaseTool;
use serde_json::Value;
use tracing::debug;

use super::resolve_path;
use super::should_skip_dir;
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

const FOLDER_OPERATIONS_DESCRIPTION: &str = include_str!("descriptions/folder.md");

pub fn list_folder(resolved: &Path) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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

/// 递归扫描目录树，使用 unicode tree formatting 输出。
/// `max_depth`: 1 = 仅根目录直接子项, 2 = 含一级子目录内容, ...
fn deep_scan_folder(
    resolved: &Path,
    max_depth: usize,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let walker = walkdir::WalkDir::new(resolved)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() && e.depth() > 0 {
                !should_skip_dir(&e.file_name().to_string_lossy())
            } else {
                true
            }
        });

    // 收集所有非根条目
    struct Entry {
        path: std::path::PathBuf,
        name: String,
        is_dir: bool,
        depth: usize,
        size: u64,
        modified: String,
        is_last: bool,
    }

    let mut entries: Vec<Entry> = Vec::new();

    for item in walker {
        let item = match item {
            Ok(e) => e,
            Err(e) => {
                debug!(error = %e, "deep_scan walk error (skipped)");
                continue;
            }
        };
        if item.depth() == 0 {
            continue;
        }
        let metadata = match item.metadata() {
            Ok(m) => m,
            Err(e) => {
                debug!(error = %e, "deep_scan metadata error (skipped): {}", item.path().display());
                continue;
            }
        };
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

        entries.push(Entry {
            path: item.path().to_path_buf(),
            name: item.file_name().to_string_lossy().to_string(),
            is_dir: item.file_type().is_dir(),
            depth: item.depth(),
            size,
            modified,
            is_last: false,
        });
    }

    // 按路径排序保证确定性输出
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    // 计算 is_last：若下一个条目的父目录与当前条目不同，则当前为 last
    for i in 0..entries.len() {
        let current_parent = entries[i].path.parent();
        let next_parent = entries.get(i + 1).and_then(|e| e.path.parent());
        entries[i].is_last = current_parent != next_parent;
    }

    // 构建 tree 输出
    let mut output = format!("\u{1F4C1} {}\n\n", resolved.display());

    // ancestor_last[depth] 表示深度 depth 处的祖先在其父目录中是否为最后一项
    let mut ancestor_last: Vec<bool> = Vec::new();

    for entry in &entries {
        // 确保 ancestor_last 长度与当前深度匹配
        while ancestor_last.len() < entry.depth {
            ancestor_last.push(false);
        }
        ancestor_last.truncate(entry.depth);

        // 构建前缀：对于深度 0..depth-1 的每一层，决定用 "│   " 还是 "    "
        let mut prefix = String::new();
        for d in 0..entry.depth.saturating_sub(1) {
            if d < ancestor_last.len() && ancestor_last[d] {
                prefix.push_str("    ");
            } else {
                prefix.push_str("\u{2502}   ");
            }
        }
        // 当前条目的树连接符
        if entry.is_last {
            prefix.push_str("\u{2514}\u{2500}\u{2500} ");
        } else {
            prefix.push_str("\u{251C}\u{2500}\u{2500} ");
        }

        // 更新祖先标记
        if entry.depth > 0 && ancestor_last.len() >= entry.depth {
            ancestor_last[entry.depth - 1] = entry.is_last;
        }

        let icon = if entry.is_dir {
            "\u{1F4C1}"
        } else {
            "\u{1F4C4}"
        };
        let trailing = if entry.is_dir { "/" } else { "" };
        output.push_str(&format!(
            "{}{} {}{} ({} bytes, {})\n",
            prefix, icon, entry.name, trailing, entry.size, entry.modified
        ));
    }

    // 截断逻辑
    let total = entries.len();
    let truncated = total > MAX_LIST_ENTRIES;
    let mut persist_hint = String::new();

    if truncated {
        let full_text = output.clone();
        persist_hint = persist_truncated_output(&full_text);
        // 保留头部 + 前 MAX_LIST_ENTRIES 个条目行
        let lines: Vec<&str> = output.lines().collect();
        let header_lines = 3; // root 行 + 空行 + (无额外 header)
        let max_lines = header_lines + MAX_LIST_ENTRIES;
        if lines.len() > max_lines {
            output = lines[..max_lines].join("\n");
        }
    }

    if truncated {
        output.push_str(&format!(
            "\n[Output truncated: {} total entries, showing first {}]{}",
            total, MAX_LIST_ENTRIES, persist_hint
        ));
    }

    output.push_str(&format!("\nTotal: {} entries", total));

    Ok(output)
}

#[async_trait::async_trait]
impl BaseTool for FolderOperationsTool {
    fn name(&self) -> &str {
        "folder_operations"
    }

    fn is_direct(&self) -> bool {
        true
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
                    "enum": ["create", "list", "exists", "deep_scan"],
                    "description": "The folder operation to perform: \"create\" to create a directory, \"list\" to list directory contents, \"exists\" to check if a path exists, \"deep_scan\" to recursively scan directory tree with depth control"
                },
                "folder_path": {
                    "type": "string",
                    "description": "The absolute path to the folder for the operation"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "For \"create\" operation: whether to create parent directories if needed (default true). Ignored for other operations"
                },
                "max_depth": {
                    "type": "number",
                    "description": "For \"deep_scan\" operation: maximum recursion depth (1 = current directory only, 2 = one level deep, default 3, max 10). Ignored for other operations"
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

            "deep_scan" => {
                if !resolved.exists() {
                    return Err(format!("Folder not found: {}", resolved.display()).into());
                }
                if !resolved.is_dir() {
                    return Err(
                        format!("Path exists but is not a folder: {}", resolved.display()).into(),
                    );
                }
                let max_depth = input["max_depth"].as_u64().unwrap_or(3) as usize;
                let clamped = max_depth.clamp(1, 10);
                deep_scan_folder(&resolved, clamped)
            }

            other => Err(format!("Unknown operation: {other}").into()),
        }
    }
}

#[cfg(test)]
#[path = "folder_test.rs"]
mod tests;
