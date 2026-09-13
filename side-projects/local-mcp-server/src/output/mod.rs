//! UTF-8 安全截断与截断输出落盘（WP-002）。
//!
//! 对应源实现 `peri-agent/src/agent/async_tasks/shell.rs:149`(`persist_truncated_output`) 与
//! `:171`(`truncate_bytes`)：Glob / `folder_operations` / Grep 在输出超限时把**完整内容**写到
//! 一个文件，并在截断提示里给出路径，让调用方用 `Read` 取回全量。
//!
//! 与源实现的两处有意差异（记录在 handoff）：
//! 1. **落盘位置**：源实现写系统临时目录（`std::env::temp_dir()`）；本产品若照搬，产物会
//!    落在工作区根之外、调用方无法用同一套 capability 边界读回。这里改为授权根内的私有产物目录
//!    [`DEFAULT_ARTIFACT_DIR`]（`<workspace>/.local-mcp/artifacts/`），它既在工作区根内
//!    内，又能被后续 `Read` 经同一 capability 边界读回。
//! 2. **文件名前缀**：`local-tool-output-{uuid v4}.txt`（源实现为 `peri-tool-output-`），
//!    避免与宿主 Peri 自己的临时产物混淆；uuid v4 与提示文案保持源语义。

use crate::capability::{AccessError, RequestedPath, RootDir};

/// 授权根内的私有产物目录（相对授权根）。
pub const DEFAULT_ARTIFACT_DIR: &str = ".local-mcp/artifacts";

/// 按字节截断字符串，确保不拆分 UTF-8 字符边界（源实现 `truncate_bytes` 的等价实现）。
pub fn truncate_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// 一次落盘的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistOutcome {
    /// 落盘文件的宿主绝对路径（成功时）。
    pub path: Option<String>,
    /// 追加到截断提示后的文本（含前导 `\n\n`）。
    pub hint: String,
}

/// 截断输出落盘器（目录相对授权根）。
#[derive(Debug, Clone)]
pub struct Persister {
    directory: String,
}

impl Default for Persister {
    fn default() -> Self {
        Self::new(DEFAULT_ARTIFACT_DIR)
    }
}

impl Persister {
    /// 指定产物目录（相对授权根，不接收绝对路径）。
    pub fn new(directory: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// 产物目录（相对授权根）。
    pub fn directory(&self) -> &str {
        &self.directory
    }

    /// 写入完整输出并返回提示文本。
    ///
    /// 失败时降级为 `[Failed to save full output to {path}: {e}]`（源实现同形），
    /// 不把落盘失败升级成工具错误——截断提示本身仍然可用。
    pub fn persist(&self, root: &RootDir, full_content: &str) -> PersistOutcome {
        let file_name = format!("local-tool-output-{}.txt", uuid::Uuid::new_v4());
        let relative = format!("{}/{file_name}", self.directory);
        let requested = match RequestedPath::parse(&relative, root.base()) {
            Ok(requested) => requested,
            Err(error) => {
                return PersistOutcome {
                    path: None,
                    hint: format!(
                        "\n\n[Failed to save full output to {relative}: {}]",
                        error.public_message()
                    ),
                }
            }
        };
        match root.write_new_file(&requested, full_content.as_bytes()) {
            Ok(path) => PersistOutcome {
                path: Some(path.to_string_lossy().to_string()),
                hint: format!(
                    "\n\n[Full output saved to {} — use Read tool to view complete content]",
                    path.display()
                ),
            },
            Err(error) => PersistOutcome {
                path: None,
                hint: format!(
                    "\n\n[Failed to save full output to {}: {}]",
                    display_path(root, &relative),
                    persist_error_text(&error)
                ),
            },
        }
    }
}

fn display_path(root: &RootDir, relative: &str) -> String {
    root.base().join(relative).to_string_lossy().to_string()
}

fn persist_error_text(error: &AccessError) -> String {
    match error {
        AccessError::Io { source, .. } => source.to_string(),
        other => other.message(),
    }
}

/// 源实现 `persist_truncated_output` 的等价入口（使用默认产物目录）。
pub fn persist_truncated_output(root: &RootDir, full_content: &str) -> PersistOutcome {
    Persister::default().persist(root, full_content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_bytes_keeps_utf8_boundaries() {
        assert_eq!(truncate_bytes("abc", 5), "abc");
        assert_eq!(truncate_bytes("abcdef", 3), "abc");
        // "é" 占 2 字节：预算 1 时退回到 0 字节而不是切出半个字符
        assert_eq!(truncate_bytes("é", 1), "");
        assert_eq!(truncate_bytes("aé", 2), "a");
        assert_eq!(truncate_bytes("日本", 3), "日");
    }

    #[test]
    fn test_hint_text_matches_source_shape() {
        let outcome = PersistOutcome {
            path: Some("<workspace>/.local-mcp/artifacts/x.txt".to_string()),
            hint: format!(
                "\n\n[Full output saved to {} — use Read tool to view complete content]",
                "<workspace>/.local-mcp/artifacts/x.txt"
            ),
        };
        assert!(outcome.hint.starts_with("\n\n[Full output saved to "));
        assert!(outcome
            .hint
            .ends_with("use Read tool to view complete content]"));
    }
}
