//! 有界诊断查询；不登记执行、不取消任务、不产生第二条完成通知。
use std::io::{Read, Seek, SeekFrom};

use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::{
    model::{CallToolResult, ContentBlock, TaskPayload, TaskStatus},
    ErrorData as McpError,
};
use serde_json::{json, Value};

use crate::shell_tasks::ShellTasks;

const MAX_WAIT_MS: u64 = 10_000;
const DEFAULT_MAX_BYTES: u64 = 8192;
const MAX_BYTES: u64 = 32768;
// 两流均为此值表示终态全文回退已消费，避免随后日志出现时重复读取。
const FALLBACK_DONE: u64 = u64::MAX;

#[derive(Clone, Default)]
pub(crate) struct TaskOutputLogs {
    pub(crate) stdout_log: Option<String>,
    pub(crate) stderr_log: Option<String>,
}

#[derive(Default)]
pub(crate) struct QueryProgress {
    last: Option<(String, u64, u64)>,
    unchanged: u64,
    total: u64,
}

impl QueryProgress {
    pub(crate) fn observe(&mut self, status: &str, sizes: (u64, u64)) -> (bool, u64) {
        let current = (status.to_owned(), sizes.0, sizes.1);
        self.unchanged = if self.last.as_ref() == Some(&current) {
            self.unchanged + 1
        } else {
            1
        };
        self.last = Some(current);
        self.total += 1;
        (self.unchanged >= 3, self.total)
    }
}

/// Workspace handler 在可信 request context 中校验 owner；此类型只声明公共工具契约。
pub struct TaskOutputTool;

#[async_trait::async_trait]
impl BaseTool for TaskOutputTool {
    fn name(&self) -> &str {
        "TaskOutput"
    }
    fn is_direct(&self) -> bool {
        true
    }
    fn namespace(&self) -> Option<&str> {
        Some("execution")
    }
    fn description(&self) -> &str {
        "用于诊断后台任务，不要作为轮询循环；等待到期不取消任务；没有新信息时依赖完成通知。输出按字节 cursor 增量读取，不要用 sleep 轮询。"
    }
    fn prompt_declaration(&self) -> Option<String> {
        Some("Diagnose a background task → `{{name}}` ({{title}}). Do not poll or sleep; rely on completion notifications when there is no new information.".into())
    }
    fn parameters(&self) -> Value {
        json!({"type":"object", "properties": {
            "task_id":{"type":"string", "description":"本会话后台回执中的公共 task_id"},
            "wait_ms":{"type":"integer", "minimum":0, "maximum":10000, "default":0, "description":"最多等待 10000ms；超上限收敛到 10000，等待到期不取消任务"},
            "cursor":{"type":"string", "description":"上次响应的不透明增量 cursor"},
            "max_bytes":{"type":"integer", "minimum":1, "maximum":32768, "default":8192, "description":"stdout 与 stderr 合计的最大字节数"}
        }, "required":["task_id"]})
    }
    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Err("TaskOutput requires the Workspace MCP request handler".into())
    }
}

fn integer(input: &Value, name: &str, default: u64) -> Result<u64, McpError> {
    match input.get(name) {
        None => Ok(default),
        Some(value) => value.as_u64().ok_or_else(|| {
            McpError::invalid_params(format!("{name} must be a non-negative integer"), None)
        }),
    }
}

fn cursor(input: &Value) -> Result<(u64, u64), McpError> {
    let Some(value) = input.get("cursor") else {
        return Ok((0, 0));
    };
    let invalid = || McpError::invalid_params("invalid TaskOutput cursor", None);
    let (a, b) = value
        .as_str()
        .and_then(|s| s.split_once(':'))
        .ok_or_else(invalid)?;
    if a.is_empty() || b.is_empty() || !a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit()) {
        return Err(invalid());
    }
    Ok((
        a.parse().map_err(|_| invalid())?,
        b.parse().map_err(|_| invalid())?,
    ))
}

struct Chunk {
    text: String,
    next: u64,
    size: u64,
}

fn utf8_chunk(bytes: &[u8]) -> Result<&str, McpError> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(error) if error.error_len().is_none() => {
            Ok(std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("validated UTF-8 prefix"))
        }
        Err(_) => Err(McpError::internal_error(
            "task output contains invalid UTF-8",
            None,
        )),
    }
}

fn read_stream(path: Option<&str>, offset: u64, limit: usize) -> Result<Option<Chunk>, McpError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let size = file.metadata().map_err(io_error)?.len();
    if offset > size {
        return Err(McpError::invalid_params("cursor exceeds output size", None));
    }
    file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
    if offset < size {
        let mut first = [0];
        file.read_exact(&mut first).map_err(io_error)?;
        if first[0] & 0xc0 == 0x80 {
            return Err(McpError::invalid_params(
                "cursor is not a UTF-8 boundary",
                None,
            ));
        }
        file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
    }
    let mut bytes = vec![0; limit.min(size.saturating_sub(offset) as usize)];
    let count = file.read(&mut bytes).map_err(io_error)?;
    let text = utf8_chunk(&bytes[..count])?.to_owned();
    Ok(Some(Chunk {
        next: offset + text.len() as u64,
        text,
        size,
    }))
}

fn io_error(error: std::io::Error) -> McpError {
    tracing::error!(%error, "TaskOutput log read failed");
    McpError::internal_error(format!("task output read failed: {error}"), None)
}

struct OutputPage {
    stdout: String,
    stderr: String,
    cursor: String,
    has_more: bool,
    sizes: (u64, u64),
}

fn read_output(
    logs: TaskOutputLogs,
    offsets: (u64, u64),
    limit: usize,
    fallback: Option<String>,
) -> Result<OutputPage, McpError> {
    if offsets == (FALLBACK_DONE, FALLBACK_DONE) {
        return Ok(OutputPage {
            stdout: String::new(),
            stderr: String::new(),
            cursor: format!("{FALLBACK_DONE}:{FALLBACK_DONE}"),
            has_more: false,
            sizes: (fallback.as_ref().map_or(0, |text| text.len() as u64), 0),
        });
    }
    let stdout = if offsets.1 == FALLBACK_DONE {
        None
    } else {
        read_stream(logs.stdout_log.as_deref(), offsets.0, limit)?
    };
    let stderr = if offsets.1 == FALLBACK_DONE {
        None
    } else {
        read_stream(
            logs.stderr_log.as_deref(),
            offsets.1,
            limit - stdout.as_ref().map_or(0, |c| c.text.len()),
        )?
    };
    if stdout.is_none() && stderr.is_none() {
        if let Some(text) = fallback {
            // stderr_offset 的保留值标记全文回退，禁止混用日志流 cursor。
            if (offsets != (0, 0) && offsets.1 != FALLBACK_DONE) || offsets.0 > text.len() as u64 {
                return Err(McpError::invalid_params(
                    "cursor unavailable after task logs were removed",
                    None,
                ));
            }
            let start = offsets.0 as usize;
            if !text.is_char_boundary(start) {
                return Err(McpError::invalid_params("invalid UTF-8 cursor", None));
            }
            let chunk =
                utf8_chunk(&text.as_bytes()[start..text.len().min(start + limit)])?.to_owned();
            let next = start + chunk.len();
            let more = next < text.len();
            let cursor = if more {
                format!("{next}:{FALLBACK_DONE}")
            } else {
                format!("{FALLBACK_DONE}:{FALLBACK_DONE}")
            };
            return Ok(OutputPage {
                stdout: chunk,
                stderr: String::new(),
                cursor,
                has_more: more,
                sizes: (text.len() as u64, 0),
            });
        }
    }
    let empty = |offset| Chunk {
        text: String::new(),
        next: offset,
        size: offset,
    };
    let stdout = stdout.unwrap_or_else(|| empty(offsets.0));
    let stderr = stderr.unwrap_or_else(|| empty(offsets.1));
    let more = stdout.next < stdout.size || stderr.next < stderr.size;
    Ok(OutputPage {
        stdout: stdout.text,
        stderr: stderr.text,
        cursor: format!("{}:{}", stdout.next, stderr.next),
        has_more: more,
        sizes: (stdout.size, stderr.size),
    })
}

pub(crate) async fn query(
    tasks: &ShellTasks,
    input: Value,
    owner: Option<&str>,
) -> Result<CallToolResult, McpError> {
    let id = input
        .get("task_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| McpError::invalid_params("missing task_id", None))?;
    let offsets = cursor(&input)?;
    let requested = integer(&input, "wait_ms", 0)?;
    let wait_ms = requested.min(MAX_WAIT_MS);
    let max_bytes = integer(&input, "max_bytes", DEFAULT_MAX_BYTES)?;
    if !(1..=MAX_BYTES).contains(&max_bytes) {
        return Err(McpError::invalid_params("max_bytes must be 1..32768", None));
    }
    // 先订阅再检查状态；Lagged 后重查，deadline 不随任何通知重置。
    let mut updates = tasks.subscribe();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
    let mut wait_timed_out = false;
    let detail = loop {
        let detail = match owner {
            Some(owner) => tasks.get_owned(id, owner)?,
            None => tasks.get(id)?,
        }
        .task;
        if detail.task.status.is_terminal() || wait_ms == 0 || wait_timed_out {
            break detail;
        }
        if tokio::time::Instant::now() >= deadline {
            wait_timed_out = true;
            break detail;
        }
        match tokio::time::timeout_at(deadline, updates.recv()).await {
            Err(_) => wait_timed_out = true,
            Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return Err(McpError::internal_error("task updates closed", None))
            }
        }
    };
    let terminal = detail.task.status.is_terminal();
    let facts = match &detail.payload {
        TaskPayload::Completed { result } => result
            .get("structuredContent")
            .cloned()
            .unwrap_or(Value::Null),
        TaskPayload::Cancelled => tasks
            .cancelled_result(id)
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| McpError::internal_error(error.to_string(), None))?
            .unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let status = match detail.task.status {
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Failed => "failed",
        _ if terminal && facts.get("success").and_then(Value::as_bool) == Some(false) => "failed",
        _ if terminal => "completed",
        _ => "running",
    };
    let logs = tasks.output_logs(id);
    let fallback = terminal
        .then(|| {
            facts
                .get("output")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .flatten();
    let OutputPage {
        stdout,
        stderr,
        cursor,
        has_more,
        sizes,
    } = tokio::task::spawn_blocking(move || {
        read_output(logs, offsets, max_bytes as usize, fallback)
    })
    .await
    .map_err(|error| {
        McpError::internal_error(format!("task output reader failed: {error}"), None)
    })??;
    let (no_progress, consecutive_queries) = tasks.query_progress(id, status, sizes);
    tracing::info!(
        wait_ms,
        consecutive_queries,
        no_progress,
        "TaskOutput diagnostic query"
    );
    let note = if no_progress {
        "交回完成通知，不要连续查询。"
    } else {
        "没有新信息时依赖完成通知，不要 sleep 轮询。"
    };
    let wait_note = if requested > MAX_WAIT_MS {
        "wait_ms 已收敛到 10000ms。"
    } else {
        ""
    };
    let content = json!({"status": status, "terminal":terminal, "wait_timed_out":wait_timed_out && !terminal,
        "success":facts.get("success"), "timed_out":facts.get("timed_out"), "duration_ms":facts.get("duration_ms"),
        "output":{"stdout":stdout, "stderr":stderr}, "cursor":cursor, "has_more":has_more,
        "no_progress":no_progress, "consecutive_queries":consecutive_queries, "wait_ms":wait_ms, "note":format!("{wait_note}{note}")});
    let mut result = CallToolResult::success(vec![ContentBlock::text(format!("{content}"))]);
    result.structured_content = Some(content);
    Ok(result)
}

#[cfg(test)]
#[path = "task_output_test.rs"]
mod tests;
