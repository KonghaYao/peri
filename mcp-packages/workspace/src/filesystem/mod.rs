pub(crate) mod draft;
pub mod edit;
pub mod folder;
pub mod glob;
pub(crate) mod glob_patterns;
pub mod grep;
pub(crate) mod grep_args;
pub(crate) mod grep_format;
pub(crate) mod path_hints;
pub mod read;
pub(crate) mod transaction;
pub mod write;

use std::path::{Component, Path, PathBuf};

pub use edit::EditFileTool;
pub use folder::FolderOperationsTool;
pub use glob::GlobFilesTool;
pub use grep::GrepTool;
pub use read::ReadFileTool;
pub use write::WriteFileTool;

/// 统一路径解析：相对路径基于 cwd，绝对路径直接使用。
///
/// 通过 `canonicalize` 解析 `..`、`.`、symlinks，使 HITL 审批时显示真实路径，
/// 防止路径遍历攻击被用户忽略。对于尚不存在的文件，逐组件解析已有祖先并保留缺失后缀。
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
    // 路径不存在：逐组件解析已有祖先，先跟随链接再处理 ..，保留缺失后缀。
    let mut resolved = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => {
                resolved.push(other.as_os_str());
                if let Ok(canonical) = resolved.canonicalize() {
                    resolved = canonical;
                }
            }
        }
    }
    resolved
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

/// A dropped RPC must also stop blocking searches at their next checkpoint.
pub(crate) struct SearchCancellation(pub std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for SearchCancellation {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
