pub mod edit;
pub mod folder;
pub mod glob;
pub mod grep;
pub(crate) mod grep_args;
pub(crate) mod grep_format;
pub mod read;
pub mod write;
pub mod write_sandbox;

use std::path::{Path, PathBuf};

pub use edit::EditFileTool;
pub use folder::FolderOperationsTool;
pub use glob::GlobFilesTool;
pub use grep::GrepTool;
pub use read::ReadFileTool;
pub use write::WriteFileTool;
pub use write_sandbox::WriteSandboxTool;

/// 统一路径解析：相对路径基于 cwd，绝对路径直接使用。
///
/// 通过 `canonicalize` 解析 `..`、`.`、symlinks，使 HITL 审批时显示真实路径，
/// 防止路径遍历攻击被用户忽略。对于尚不存在的文件，规范化父目录 + 文件名。
pub fn resolve_path(cwd: &str, file_path: &str) -> PathBuf {
    let raw = if Path::new(file_path).is_absolute() {
        Path::new(file_path).to_path_buf()
    } else {
        Path::new(cwd).join(file_path)
    };
    // 路径已存在：完整 canonicalize（解析 symlinks + ..）
    if raw.exists() {
        return raw.canonicalize().unwrap_or(raw);
    }
    // 路径不存在（新文件）：canonicalize 父目录 + 保留文件名
    if let (Some(parent), Some(file_name)) = (raw.parent(), raw.file_name()) {
        if let Ok(canon_parent) = parent.canonicalize() {
            return canon_parent.join(file_name);
        }
    }
    raw
}

/// 将输入字符串解析为 JSON Value，失败时原样返回为字符串
pub async fn parse_json_input(input: &str) -> serde_json::Value {
    serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()))
}

/// 递归扫描时跳过的目录名
pub(crate) fn should_skip_dir(name: &str) -> bool {
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

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
