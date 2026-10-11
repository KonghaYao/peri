//! 资源路径安全：根 canonical 化、symlink 拒绝、越界拒绝与预算检查。
//!
//! 口径（W1 冻结，plan §5.2 / X3）：
//! - 公开面**禁 symlink**：扫描发现的任何 symlink 条目（技能目录、`SKILL.md`、
//!   附件、agent 定义）一律跳过，读取按 `NotFound` 拒绝（对外不暴露 symlink 细节）；
//!   宿主**显式输入**的根是授权点，允许其路径自身经过 symlink 解析。
//! - 读取时对目标做 canonical 前缀校验（挡「扫描后目录被替换为 symlink」的
//!   逃逸窗口）；digest 与投影内容同源（单次读取的同一份字节）。
//! - 错误文案不含主机绝对路径（ARC-SECRET-001 面）。
//! - URI 段与相对路径的语法校验在契约层（`peri_acp_types::workspace_resources`）；
//!   本模块只处理已通过语法校验的相对路径。

use std::path::{Path, PathBuf};

use super::ResourceError;

/// `root` 的 canonical 形式；不存在、不可解析或不是目录时返回 `None`
/// （该根按「无资源」处理，不产生错误）。
pub(crate) fn canonical_root(root: &Path) -> Option<PathBuf> {
    let canonical = root.canonicalize().ok()?;
    if canonical.is_dir() {
        Some(canonical)
    } else {
        None
    }
}

/// 隐藏条目名（`.` 开头）。扫描与附件枚举跳过隐藏条目（含 `.git` 等工具目录；
/// `SKIP_DIR_NAMES` 之外的第二道过滤，防止把编辑器/工具产物公开到资源面）。
///
/// `.` / `..` 不是条目名，显式排除以保持判定语义单一。
pub(crate) fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

/// 读取 `root/{relative}` 的完整字节（`root` 必须是 canonical 目录）。
///
/// 失败分类：
/// - 目标不存在 / 非普通文件 / 是 symlink → [`ResourceError::NotFound`]；
/// - 已在根外（canonical 前缀校验失败）→ [`ResourceError::Denied`]；
/// - 超过 `max_bytes` → [`ResourceError::Budget`]；
/// - 其他 IO 失败 → [`ResourceError::Io`]。
pub(crate) fn safe_read_file(
    root: &Path,
    relative: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, ResourceError> {
    if relative.is_empty() {
        return Err(ResourceError::InvalidUri);
    }
    let full = root.join(relative);
    let metadata = std::fs::symlink_metadata(&full).map_err(|_| ResourceError::NotFound)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ResourceError::NotFound);
    }
    if metadata.len() > max_bytes {
        return Err(ResourceError::Budget);
    }
    let bytes = std::fs::read(&full).map_err(|_| ResourceError::Io)?;
    // 读后再验一次：文件在 stat 与 read 之间增长时同样按预算拒绝。
    if bytes.len() as u64 > max_bytes {
        return Err(ResourceError::Budget);
    }
    // canonical 前缀校验：目录段被替换为 symlink 时，canonical 会逃出根。
    let canonical = full.canonicalize().map_err(|_| ResourceError::NotFound)?;
    if !canonical.starts_with(root) {
        return Err(ResourceError::Denied);
    }
    Ok(bytes)
}

/// 文本 / 二进制判定：有效 UTF-8 且不含 NUL 视为文本（矩阵「UTF-8/NUL 判断」）。
pub(crate) fn is_text_bytes(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

#[cfg(test)]
#[path = "path_test.rs"]
mod tests;
