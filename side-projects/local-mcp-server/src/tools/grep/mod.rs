//! `Grep` 工具：别名解析、遍历搜索、四输出模式与截断落盘。
//!
//! 本模块由进程内执行内核 [`crate::runtime::InProcessExecutor`] 直接调用
//! （单进程本机执行）：遍历、排序、四输出模式与截断都在本进程内
//! 完成，MCP core 只做协议与身份。
//!
//! ## 授权 seam
//!
//! 唯一的路径入口是 `arguments.path`（默认工作区根）。它必须由调用方通过
//! [`GrepContext::resolve`] 解析：该闭包是 capability 层的消费者侧接口
//! （WP-002 的 `CapabilitySet` 或等价实现），本模块**不做**路径授权判断，也不
//! 提供宽松默认值——没有 guard 就无法构造 [`GrepContext`]，因此不存在"绕过
//! capability 直接读宿主"的编译期路径。
//!
//! ## 与源实现的对应
//!
//! | 源 | 本模块 |
//! | --- | --- |
//! | `GrepTool::parameters()` 字段与默认 | [`args::GrepInput`] |
//! | 语义别名优先于 CLI 别名 | [`args::GrepInput::from_arguments`] |
//! | `execute_search`（walker/matcher/排序/截断） | [`search::execute_search`] |
//! | `Sink`（行格式、上下文、预算） | [`format::SearchSink`] |
//! | `offset` 在截断之后应用 | [`search::apply_offset`] |
//! | 15s 搜索超时 | [`SEARCH_TIMEOUT`]，由 [`GrepContext::search_timeout`] 可调（测试） |

pub mod args;
pub mod format;
pub mod search;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::CapabilityError;
use crate::tasks::log::OutputPersist;
use crate::wire::{StructuredOutput, ToolResponse};

pub use args::{GrepArgError, GrepInput, OutputMode, ParsedArgs, DEFAULT_HEAD_LIMIT};
pub use format::{trim_line, truncate_bytes, SearchSink, LINE_TRUNCATED_MARKER};
pub use search::{apply_offset, execute_search, SearchError, SearchOutcome};

/// 搜索超时（源：`grep.rs::SEARCH_TIMEOUT`，与 Glob 的扫描超时对齐）。
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

/// 输出字节预算（源：`grep.rs::MAX_OUTPUT_BYTES`）。
pub const MAX_OUTPUT_BYTES: usize = 20_000;

/// 单行字节上限（源：`grep.rs::MAX_LINE_BYTES`）。
pub const MAX_LINE_BYTES: usize = 1_000;

/// walker 线程上限（源：`grep.rs::SEARCH_THREADS_MAX`）。
pub const SEARCH_THREADS_MAX: usize = 8;

/// 取消后返回的文本（本实现新增：源在进程内没有客户端取消路径）。
pub const CANCELLED_MESSAGE: &str = "Error: Search cancelled.";

/// 字节预算溢出时的交付文案。
///
/// 工具自身的字节兜底与交付侧预算复核都要产出同一条文案：
/// 交付文本会变长，可能在**交付表示**上重新触及同一预算（FC-GREP-04 的「20000 bytes 总量」）。
/// 因此格式只定义一次，`bytes` 是**判定所用表示**的字节数。
pub fn byte_overflow_text(
    head: &str,
    total_lines: usize,
    bytes: usize,
    head_count: usize,
    hint: &str,
) -> String {
    format!(
        "{head}\n\n[Output truncated: {total_lines} lines total, {bytes} bytes; showing first {head_count} — exceeds {MAX_OUTPUT_BYTES} byte limit]{hint}"
    )
}

/// 搜索上下文：工作区根、capability 解析闭包、落盘 sink 与超时。
pub struct GrepContext<'a> {
    /// 授权工作区根（用于计算展示路径与默认搜索路径）。
    pub cwd: PathBuf,
    /// **启动时锚定**的授权根真实路径（[`crate::capability::RootDir::real_base`]）。
    ///
    /// 搜索根的包含判定与条目复核都以它为准；不得用"每次请求重新 canonicalize 根"的结果
    /// 替代（GAP-034：被替换的根会让比较恒真）。
    pub root_real: PathBuf,
    /// capability 解析：把调用方请求的路径解析为已授权的绝对路径。
    pub resolve: &'a (dyn Fn(&str) -> Result<PathBuf, CapabilityError> + Sync),
    /// 截断输出落盘 sink（`Arc` 以便移入 `spawn_blocking`）。
    pub persist: Arc<dyn OutputPersist>,
    /// 搜索超时（生产为 [`SEARCH_TIMEOUT`]）。
    pub search_timeout: Duration,
}

impl<'a> GrepContext<'a> {
    /// 使用默认超时构造上下文（所有字段都显式提供，没有默认 guard）。
    pub fn new(
        cwd: impl Into<PathBuf>,
        root_real: impl Into<PathBuf>,
        resolve: &'a (dyn Fn(&str) -> Result<PathBuf, CapabilityError> + Sync),
        persist: Arc<dyn OutputPersist>,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            root_real: root_real.into(),
            resolve,
            persist,
            search_timeout: SEARCH_TIMEOUT,
        }
    }

    /// 覆盖搜索超时（测试用；生产保持 [`SEARCH_TIMEOUT`]）。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.search_timeout = timeout;
        self
    }
}

/// 执行一次 `Grep` 调用。
///
/// `cancel` 是协议层取消信号（`notifications/cancelled` / 连接关闭）；触发后
/// walker 在检查点尽快退出并返回 [`CANCELLED_MESSAGE`]（协议层也可按规范丢弃响应）。
pub async fn invoke(
    arguments: &Value,
    ctx: &GrepContext<'_>,
    cancel: Option<&CancellationToken>,
) -> ToolResponse {
    let input = match GrepInput::from_arguments(arguments) {
        Ok(input) => input,
        Err(error) => return error_response(error.message, None),
    };
    let parsed = match input.to_parsed_args() {
        Ok(parsed) => parsed,
        Err(error) => return error_response(format!("Error: {}", error.message), None),
    };

    // 授权解析：默认搜索工作区根；显式 path 必须经 capability。
    let search_path = match input.path.as_deref() {
        Some(requested) => match (ctx.resolve)(requested) {
            Ok(path) => path,
            Err(error) => return error_response(error.public_message(), None),
        },
        None => ctx.cwd.clone(),
    };

    let head_limit = input.head_limit;
    let offset = input.offset;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cwd = ctx.cwd.clone();
    let root_real = ctx.root_real.clone();
    let persist = Arc::clone(&ctx.persist);

    let cancelled_inner = Arc::clone(&cancelled);
    let search = tokio::task::spawn_blocking(move || {
        execute_search(
            &parsed,
            &cwd,
            &root_real,
            &search_path,
            head_limit,
            cancelled_inner,
            persist.as_ref(),
        )
    });
    let timeout = ctx.search_timeout;

    let outcome = match cancel {
        None => {
            let result = tokio::time::timeout(timeout, search).await;
            match result {
                Err(_) => Err(TimeoutOrCancel::Timeout),
                Ok(Err(join_error)) => Err(TimeoutOrCancel::Failed(join_error.to_string())),
                Ok(Ok(Err(error))) => Err(TimeoutOrCancel::Search(error.message)),
                Ok(Ok(Ok(output))) => Ok(output),
            }
        }
        Some(token) => {
            let cancelled_token = token.cancelled();
            tokio::pin!(cancelled_token);
            let search = search;
            tokio::pin!(search);
            tokio::select! {
                result = tokio::time::timeout(timeout, &mut search) => match result {
                    Err(_) => Err(TimeoutOrCancel::Timeout),
                    Ok(Err(join_error)) => Err(TimeoutOrCancel::Failed(join_error.to_string())),
                    Ok(Ok(Err(error))) => Err(TimeoutOrCancel::Search(error.message)),
                    Ok(Ok(Ok(output))) => Ok(output),
                },
                () = &mut cancelled_token => Err(TimeoutOrCancel::Cancelled),
            }
        }
    };

    match outcome {
        Ok(search) => {
            let truncated = search.truncated();
            let final_output = apply_offset(search.text, offset);
            let text = final_output.clone();
            success_response(text, truncated, search.persisted_path)
        }
        Err(TimeoutOrCancel::Search(message)) => error_response(format!("Error: {message}"), None),
        Err(TimeoutOrCancel::Failed(message)) => error_response(format!("Error: {message}"), None),
        Err(TimeoutOrCancel::Timeout) => {
            cancelled.store(true, Ordering::Relaxed);
            error_response(
                format!(
                    "Error: Search timed out after {} seconds. Please use a more specific pattern.",
                    timeout.as_secs()
                ),
                None,
            )
        }
        Err(TimeoutOrCancel::Cancelled) => {
            cancelled.store(true, Ordering::Relaxed);
            error_response(CANCELLED_MESSAGE.to_string(), None)
        }
    }
}

/// 内部分支：超时/取消/搜索失败。
enum TimeoutOrCancel {
    Timeout,
    Cancelled,
    Search(String),
    Failed(String),
}

fn success_response(text: String, truncated: bool, persisted_path: Option<String>) -> ToolResponse {
    let mut structured = StructuredOutput::ok("Grep");
    structured.truncated = truncated;
    // 落盘路径结构化外发（GAP-024）：调用方据此直接取回产物；产物内容与交付文本
    // 同为宿主路径单表示（GAP-015）。
    structured.persisted_path = persisted_path;
    structured = structured.with_extra("matched", Value::Bool(!text.is_empty()));
    ToolResponse {
        text,
        structured: serde_json::to_value(structured).unwrap_or(Value::Null),
        is_error: false,
        meta: None,
    }
}

fn error_response(text: String, truncated: Option<bool>) -> ToolResponse {
    let mut structured = StructuredOutput::error("Grep");
    if let Some(truncated) = truncated {
        structured.truncated = truncated;
    }
    structured = structured.with_extra("error", Value::String(text.clone()));
    ToolResponse {
        text,
        structured: serde_json::to_value(structured).unwrap_or(Value::Null),
        is_error: true,
        meta: None,
    }
}
