//! Bash 输出合并、两层限额与 timeout 解析（迁移自源 `terminal.rs` 与
//! `async_tasks/shell.rs` 的纯函数部分）。
//!
//! **两层限额**必须都登记，不能混为一谈：
//!
//! 1. 工具内部（本模块）：`MAX_OUTPUT_LINES = 2000`（超出时落盘全量 + head/tail
//!    各 1000 行）与 `MAX_OUTPUT_CHARS = 65000` 字节兜底。
//! 2. 宿主投影（Peri Agent 层，非本 server 行为）：`output_char_limit = 10000`
//!    字符，消费点在 `peri-agent/.../tool_dispatch/execution.rs`，追加文本
//!    `[Output truncated at 10000 chars]`。本实现 **不**执行这一层，只把事实登记进
//!    `structuredContent`（见 [`HOST_OUTPUT_CHAR_LIMIT`] 与 [`host_projection_note`]）。

use serde_json::Value;

use crate::tasks::log::{OutputPersist, PersistOutcome};

/// 工具内部输出字节上限（源：`MAX_OUTPUT_CHARS`）。
pub const MAX_OUTPUT_CHARS: usize = 65_000;

/// 工具内部输出行上限（源：`MAX_OUTPUT_LINES`）。
pub const MAX_OUTPUT_LINES: usize = 2_000;

/// 前台默认 timeout（毫秒）。
pub const DEFAULT_FOREGROUND_TIMEOUT_MS: u64 = 15_000;

/// timeout 上限（毫秒）。
pub const MAX_TIMEOUT_MS: u64 = 600_000;

/// 宿主（Peri Agent 层）投影的字符上限；本实现只登记不执行。
pub const HOST_OUTPUT_CHAR_LIMIT: usize = 10_000;

/// 前景/后台 timeout 最小值（Unix 为 1ms；Windows 无进程组语义，源用 5000ms）。
#[cfg(windows)]
pub const MIN_TIMEOUT_MS: u64 = 5_000;
/// 前景/后台 timeout 最小值（Unix 为 1ms；Windows 无进程组语义，源用 5000ms）。
#[cfg(not(windows))]
pub const MIN_TIMEOUT_MS: u64 = 1;

/// 宿主投影说明（进入结构化输出的只读事实）。
pub fn host_projection_note() -> String {
    format!(
        "output_char_limit={HOST_OUTPUT_CHAR_LIMIT} 由 Peri Agent 层消费（宿主投影），本工具内部上限为 {MAX_OUTPUT_CHARS} 字节/{MAX_OUTPUT_LINES} 行"
    )
}

/// 按字节截断字符串，保证不拆分 UTF-8 字符边界。
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

/// 合并 stdout/stderr 为既有输出格式（源逐字）。
///
/// - stderr 非空时加 `[stderr]` 前缀段；
/// - 非零退出码追加 `[Exit code: N]`；
/// - 空输出时给占位：`Some(code)` → `[Command completed with exit code N]`，
///   `None` → `[no output captured yet]`（超时部分输出路径）。
pub fn merge_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> String {
    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("[stderr]\n");
        output.push_str(stderr);
    }
    if let Some(code) = exit_code {
        if code != 0 {
            output.push_str(&format!("\n[Exit code: {code}]"));
        }
    }
    if output.is_empty() {
        output = match exit_code {
            Some(code) => format!("[Command completed with exit code {code}]"),
            None => "[no output captured yet]".to_string(),
        };
    }
    output
}

/// 截断结果：文本 + 是否截断 + 落盘路径（真实落盘时存在）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncatedOutput {
    /// 最终文本（含截断标记与落盘提示）。
    pub text: String,
    /// 是否触发任一层限额。
    pub truncated: bool,
    /// 全量落盘路径。
    pub persisted_path: Option<String>,
}

/// 是否超过工具内部限额（行数或字节）。
pub fn exceeds_limits(output: &str) -> bool {
    output.split('\n').count() > MAX_OUTPUT_LINES || output.len() > MAX_OUTPUT_CHARS
}

/// 工具内部截断（源：`truncate_output`）：行数超限走 head/tail 并落盘全量，
/// 再做字节兜底。
pub fn truncate_output(output: &str, persist: &dyn OutputPersist) -> TruncatedOutput {
    let lines: Vec<&str> = output.split('\n').collect();
    if lines.len() > MAX_OUTPUT_LINES {
        let total_lines = lines.len();
        let PersistOutcome {
            hint: persist_hint,
            path,
        } = persist.persist(output);
        let head_count = MAX_OUTPUT_LINES / 2;
        let tail_count = MAX_OUTPUT_LINES - head_count;
        let head: Vec<&str> = lines.iter().take(head_count).copied().collect();
        let tail: Vec<&str> = lines
            .iter()
            .skip(total_lines - tail_count)
            .copied()
            .collect();
        let mut result = head.join("\n");
        result.push_str(&format!(
            "\n\n... [{} lines truncated, showing head {} and tail {} of {} total lines] ...\n\n",
            total_lines - MAX_OUTPUT_LINES,
            head_count,
            tail_count,
            total_lines
        ));
        result.push_str(&tail.join("\n"));
        result.push_str(&persist_hint);
        if result.len() > MAX_OUTPUT_CHARS {
            let truncated = truncate_bytes(&result, MAX_OUTPUT_CHARS);
            return TruncatedOutput {
                text: format!(
                    "{}\n\n[Output truncated: exceeds {} byte limit]{}",
                    truncated, MAX_OUTPUT_CHARS, persist_hint
                ),
                truncated: true,
                persisted_path: path.map(|path| path.to_string_lossy().to_string()),
            };
        }
        return TruncatedOutput {
            text: result,
            truncated: true,
            persisted_path: path.map(|path| path.to_string_lossy().to_string()),
        };
    }
    if output.len() > MAX_OUTPUT_CHARS {
        let PersistOutcome {
            hint: persist_hint,
            path,
        } = persist.persist(output);
        let truncated = truncate_bytes(output, MAX_OUTPUT_CHARS);
        return TruncatedOutput {
            text: format!(
                "{}\n\n[Output truncated: exceeds {} byte limit]{}",
                truncated, MAX_OUTPUT_CHARS, persist_hint
            ),
            truncated: true,
            persisted_path: path.map(|path| path.to_string_lossy().to_string()),
        };
    }
    TruncatedOutput {
        text: output.to_string(),
        truncated: false,
        persisted_path: None,
    }
}

/// 超时前捕获的部分输出落盘提示（源：`persist_partial_output`）。
pub fn persist_partial_output(output: &str, persist: &dyn OutputPersist) -> PersistOutcome {
    persist.persist_partial(output)
}

/// 解析 `timeout` 参数（源：`async_tasks::shell::parse_timeout`）。
///
/// - 后台：未传或显式 `0` → `None`（不超时）；显式 `>0` → clamp 到
///   `[MIN_TIMEOUT_MS, MAX_TIMEOUT_MS]`
/// - 前台：未传 → `Some(15000)`；显式 `0` → `None`；显式 `>0` → clamp
/// - 非数值/负数按"未传"处理（`as_u64` 语义与源一致：字符串、`null`、负数都取不到值）
pub fn parse_timeout(input: &Value, is_background: bool) -> Option<u64> {
    match input.get("timeout").and_then(|value| value.as_u64()) {
        None => {
            if is_background {
                None
            } else {
                Some(DEFAULT_FOREGROUND_TIMEOUT_MS)
            }
        }
        Some(0) => None,
        Some(ms) => Some(ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)),
    }
}
