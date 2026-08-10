use std::{
    cell::Cell,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use grep::searcher::{Searcher, Sink, SinkContext, SinkContextKind, SinkMatch};
use peri_agent::agent::async_tasks::truncate_bytes;

use super::grep::{MAX_LINE_BYTES, MAX_OUTPUT_BYTES};
use super::grep_args::OutputMode;

/// 单行内容超过 `MAX_LINE_BYTES` 时追加的截断标记（让 LLM 感知行被截断）
const LINE_TRUNCATED_MARKER: &str = "… [line truncated]";

/// 行内容按字节预算截断（`truncate_bytes` 保证 UTF-8 字符边界安全），
/// 超限时追加可见标记；未超限时原样返回。
fn trim_line(content: &str) -> String {
    let truncated = truncate_bytes(content, MAX_LINE_BYTES);
    if truncated.len() < content.len() {
        format!("{truncated}{LINE_TRUNCATED_MARKER}")
    } else {
        truncated
    }
}

/// 自定义 Sink，支持多种输出模式和行数限制。
///
/// 停止语义（与 glob.rs 的协作取消对齐）：
/// - `cancelled`：invoke 超时置位。任何模式下立即停止当前文件搜索（返回 false），
///   walker 在检查点 `Quit` 终止整个遍历。
/// - `stopped`：输出预算置位（行数超过 `head_limit+1` 或字节超过 `MAX_OUTPUT_BYTES`）。
///   Default 模式通过预算检查自然停止当前文件；非 Default 模式**忽略** stopped——
///   CountOnly 必须数完当前文件才能输出正确计数，FilesWithoutMatch 必须确认文件
///   无匹配——由 walker 的文件级检查（grep.rs）停止新文件。
///
/// 行数预算取 `head_limit+1`（多收 1 行）：恰好 N 行匹配时收集 N 行不置位，
/// 输出层按实际行数精确判定——`== N` 不标 truncated，`> N` 才截断标记
/// （消除"恰好 N 行被误标 truncated"，见 grep.rs 输出层）。
///
/// 每文件一个 sink 实例，行先入本地缓冲 `local_lines`（Sink 回调在单文件内
/// 顺序调用，行序稳定），文件搜索完成后由 grep.rs 批量入共享结果——避免逐行
/// 全局锁竞争，并为跨文件排序提供稳定的文件内行序。
pub(crate) struct SearchSink {
    pub(crate) output_mode: OutputMode,
    /// 当前文件的行缓冲（Default 模式），文件完成后一次性 flush
    pub(crate) local_lines: Vec<String>,
    pub(crate) total_lines: Arc<AtomicUsize>,
    pub(crate) total_bytes: Arc<AtomicUsize>,
    pub(crate) max_limit: usize,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) stopped: Arc<AtomicBool>,
    pub(crate) display_path: String,
    pub(crate) match_count: Cell<usize>,
    pub(crate) has_match: Cell<bool>,
    pub(crate) after_context: usize,
    pub(crate) before_context: usize,
    pub(crate) show_line_numbers: bool,
}

impl SearchSink {
    /// 行数/字节预算检查后入本地缓冲。
    ///
    /// - 行数预算：`total > max_limit + 1` 才置位（多收 1 行用于精确判定），
    ///   `max_limit == 0`（unlimited）时跳过——与 `matched` 的守卫对称，
    ///   修复 head_limit=0 时第一条 context 行即置 stopped 的 bug。
    /// - 字节预算：超过 `MAX_OUTPUT_BYTES` 置位；当前行仍 push（收集 ≈ 上限+1 行），
    ///   由输出层按实际字节截断并落盘提示。
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
        // 超时取消：所有模式立即停止当前文件搜索
        if self.cancelled.load(Ordering::Relaxed) {
            return Ok(false);
        }

        match self.output_mode {
            OutputMode::Default => {
                let line_number = mat.line_number().unwrap_or(0);
                let content = String::from_utf8_lossy(mat.bytes());
                let content = content.trim_end_matches(['\n', '\r']);
                let content = trim_line(content);
                let line = if self.show_line_numbers {
                    format!("{}:{}: {}", self.display_path, line_number, content)
                } else {
                    format!("{}: {}", self.display_path, content)
                };
                self.push_line(line)
            }
            OutputMode::CountOnly => {
                // 忽略 stopped：计数必须数完当前文件才能输出正确计数
                // （避免其他线程预算置位后输出错误的低计数）
                self.match_count.set(self.match_count.get() + 1);
                Ok(true)
            }
            OutputMode::FilesOnly => {
                self.has_match.set(true);
                Ok(false)
            }
            OutputMode::FilesWithoutMatch => {
                self.has_match.set(true);
                Ok(true) // 不 early return，需确认文件无匹配
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
        // 非对称上下文：before 和 after 分别控制
        match ctx.kind() {
            SinkContextKind::After if self.after_context == 0 => return Ok(true),
            SinkContextKind::Before if self.before_context == 0 => return Ok(true),
            _ => {}
        }

        let line_number = ctx.line_number().unwrap_or(0);
        let content = String::from_utf8_lossy(ctx.bytes());
        let content = content.trim_end_matches(['\n', '\r']);
        let content = trim_line(content);

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

        // 预算检查与 matched 共用（`max_limit > 0` 守卫：head_limit=0 时 unlimited）
        self.push_line(line)
    }
}
