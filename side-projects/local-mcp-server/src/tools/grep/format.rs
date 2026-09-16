//! Grep 行格式化与预算 sink（迁移自源 `grep_format.rs`）。
//!
//! 停止语义与源一致，两者分离：
//!
//! - `cancelled`（超时/请求取消）：任何模式下立即停止当前文件搜索，walker 在
//!   检查点 `Quit` 终止整个遍历。
//! - `stopped`（输出预算）：`Default` 模式停止当前文件；`CountOnly`/
//!   `FilesWithoutMatch` **忽略** stopped 数完当前文件（保证计数与"无匹配"判断正确），
//!   由 walker 的文件级检查停止新文件。
//!
//! 行数预算取 `head_limit + 1`（多收 1 行）：恰好 N 行不标 truncated，`> N` 才截断。

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use grep::searcher::{Searcher, Sink, SinkContext, SinkContextKind, SinkMatch};

use super::args::OutputMode;
use super::{MAX_LINE_BYTES, MAX_OUTPUT_BYTES};

/// 单行超长时追加的可见标记（源逐字）。
pub const LINE_TRUNCATED_MARKER: &str = "… [line truncated]";

/// 按字节截断字符串，保证不拆分 UTF-8 字符边界（源：`async_tasks::shell::truncate_bytes`）。
pub fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// 行内容按 [`MAX_LINE_BYTES`] 截断，超限时追加可见标记。
pub fn trim_line(content: &str) -> String {
    trim_line_outcome(content).0
}

/// [`trim_line`] 的完整结果：截断后的行 + **是否真的截断**（截断前长度差）。
///
/// 返回值里的布尔量是截断事实的结构化来源（GAP-023），不依赖正文里是否出现标记字面量。
fn trim_line_outcome(content: &str) -> (String, bool) {
    let truncated = truncate_bytes(content, MAX_LINE_BYTES);
    if truncated.len() < content.len() {
        (format!("{truncated}{LINE_TRUNCATED_MARKER}"), true)
    } else {
        (truncated, false)
    }
}

/// 单文件搜索 sink（源：`SearchSink`）。
pub struct SearchSink {
    /// 输出模式。
    pub output_mode: OutputMode,
    /// 当前文件的行缓冲（Default 模式），文件完成后一次性 flush。
    pub local_lines: Vec<String>,
    /// 全局行计数（跨文件共享）。
    pub total_lines: Arc<AtomicUsize>,
    /// 全局字节计数（跨文件共享）。
    pub total_bytes: Arc<AtomicUsize>,
    /// 被 [`trim_line`] 实际截断的行数（跨文件共享）。
    ///
    /// 这是「截断事实」的**结构化**来源之一：`truncated` 不再由正文文本里是否出现
    /// [`LINE_TRUNCATED_MARKER`] 推断（匹配行自身含有同一字面量时会误报，GAP-023）。
    pub trimmed_lines: Arc<AtomicUsize>,
    /// 行数预算（`head_limit`，`0` = unlimited）。
    pub max_limit: usize,
    /// 超时/请求取消标志。
    pub cancelled: Arc<AtomicBool>,
    /// 输出预算停止标志。
    pub stopped: Arc<AtomicBool>,
    /// 显示路径（相对 cwd）。
    pub display_path: String,
    /// 当前文件匹配计数（`count` 模式）。
    pub match_count: Cell<usize>,
    /// 当前文件是否出现过匹配（`files_with_matches`/`files_without_matches`）。
    pub has_match: Cell<bool>,
    /// 匹配后上下文行数。
    pub after_context: usize,
    /// 匹配前上下文行数。
    pub before_context: usize,
    /// 是否显示行号。
    pub show_line_numbers: bool,
}

impl SearchSink {
    /// 单行内容按 [`MAX_LINE_BYTES`] 截断；**实际发生截断**时累加 [`Self::trimmed_lines`]。
    ///
    /// 判据来自 [`trim_line_outcome`] 的布尔量（截断前的长度差），不是正文里是否有标记字面量。
    fn trim_line_recorded(&self, content: &str) -> String {
        let (trimmed, did_trim) = trim_line_outcome(content);
        if did_trim {
            self.trimmed_lines.fetch_add(1, Ordering::Relaxed);
        }
        trimmed
    }

    /// 行数/字节预算检查后入本地缓冲（源：`SearchSink::push_line`）。
    fn push_line(&mut self, line: String) -> Result<bool, std::io::Error> {
        let total = self.total_lines.fetch_add(1, Ordering::Relaxed) + 1;
        let mut stop = false;
        if self.max_limit > 0 && total > self.max_limit.saturating_add(1) {
            stop = true;
        }
        let bytes = self.total_bytes.fetch_add(line.len(), Ordering::Relaxed) + line.len();
        if bytes > MAX_OUTPUT_BYTES {
            stop = true;
        }
        self.local_lines.push(line);
        if stop {
            self.stopped.store(true, Ordering::Relaxed);
            Ok(false)
        } else {
            Ok(true)
        }
    }
}

impl Sink for SearchSink {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        // 超时/取消：所有模式立即停止当前文件搜索。
        if self.cancelled.load(Ordering::Relaxed) {
            return Ok(false);
        }

        match self.output_mode {
            OutputMode::Default => {
                let line_number = mat.line_number().unwrap_or(0);
                let content = String::from_utf8_lossy(mat.bytes());
                let content = content.trim_end_matches(['\n', '\r']);
                let content = self.trim_line_recorded(content);
                let line = if self.show_line_numbers {
                    format!("{}:{}: {}", self.display_path, line_number, content)
                } else {
                    format!("{}: {}", self.display_path, content)
                };
                self.push_line(line)
            }
            OutputMode::CountOnly => {
                // 忽略 stopped：计数必须数完当前文件，避免其他线程预算置位导致低计数。
                self.match_count.set(self.match_count.get() + 1);
                Ok(true)
            }
            OutputMode::FilesOnly => {
                self.has_match.set(true);
                Ok(false)
            }
            OutputMode::FilesWithoutMatch => {
                self.has_match.set(true);
                // 不 early return：需要确认整个文件无匹配才能列入结果。
                Ok(true)
            }
        }
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if self.output_mode != OutputMode::Default {
            return Ok(true);
        }
        // 非对称上下文：before/after 分别控制。
        match ctx.kind() {
            SinkContextKind::After if self.after_context == 0 => return Ok(true),
            SinkContextKind::Before if self.before_context == 0 => return Ok(true),
            _ => {}
        }

        let line_number = ctx.line_number().unwrap_or(0);
        let content = String::from_utf8_lossy(ctx.bytes());
        let content = content.trim_end_matches(['\n', '\r']);
        let content = self.trim_line_recorded(content);

        // 上下文行标记：前置 `-`、后置 `+`（源逐字）。
        let separator = match ctx.kind() {
            SinkContextKind::Before => '-',
            SinkContextKind::After => '+',
            SinkContextKind::Other => '-',
        };

        let line = if self.show_line_numbers {
            format!(
                "{}:{}{}: {}",
                self.display_path, line_number, separator, content
            )
        } else {
            format!("{}{}: {}", self.display_path, separator, content)
        };

        self.push_line(line)
    }
}
