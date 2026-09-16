//! `Bash` 工具：三字段解析、前台/后台判定与结果投影。
//!
//! 公开 schema 严格保持源的三字段 `command` / `timeout` / `run_in_background`
//! （夹具 `tests/fixtures/schemas/bash.json`），**不新增**任何任务控制输入字段。
//! 任务查询/停止/日志的 **MCP 可访问路径**（不新增工具、不改七工具 schema）：
//! 标准资源 `sandbox://tasks`、`sandbox://tasks/{task_id}` 查询状态与日志路径；
//! 用 `Read` 工具或 Bash 读日志文件；用普通 Bash 命令 `kill`/`kill -- -<pgid>`
//! 停止进程组。注册表内的 [`crate::tasks::TaskRegistry::stop`] 等是同进程内嵌
//! API（测试与嵌入方使用），**不**经 MCP wire 暴露。
//!
//! 执行链固定为 `BashTool → TaskRegistry → BashTasks`：三个环节都在**本进程内**，
//! 任务表直接持有子进程句柄（单进程本机执行，无独立执行进程）。
//!
//! `structuredContent` 的投影不变量：`ok == !isError`（成功分支 `ok: true`，
//! 错误分支 `ok: false`，并附 `error` 文本）。`status` 是**调用级**状态
//! （`completed` = 本次调用拿到终态；`running` = 本次调用返回了一个仍在跑的任务），
//! 任务级状态以资源 `sandbox://tasks/{task_id}` 为准。前台超时提升属错误分支
//! （本次调用失败），但进程被提升为后台任务、仍在运行。
//!
//! 源文案保留策略：缺失参数、超时提升（有/无输出两版）、提升失败、后台启动、
//! spawn 失败全部逐字迁移；仅把源中指向 Peri TUI "Tasks panel" 的一句改成本服务
//! 真实可用的资源路径（该改动已登记为后端差异）。

pub mod limits;

use std::sync::Arc;

use serde_json::Value;

use crate::tasks::log::OutputPersist;
use crate::tasks::params::BashTaskState;
use crate::tasks::registry::{task_resource_uri, BashRun, BashRunArgs, BashRunError, TaskRegistry};
use crate::wire::{RequestContext, StructuredOutput, ToolResponse};

pub use limits::{
    exceeds_limits, host_projection_note, merge_output, parse_timeout, persist_partial_output,
    truncate_bytes, truncate_output, TruncatedOutput, DEFAULT_FOREGROUND_TIMEOUT_MS,
    HOST_OUTPUT_CHAR_LIMIT, MAX_OUTPUT_CHARS, MAX_OUTPUT_LINES, MAX_TIMEOUT_MS, MIN_TIMEOUT_MS,
};

/// `Bash` 的三字段输入（解析结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashArgs {
    /// 原始命令。
    pub command: String,
    /// 解析后的 timeout（毫秒）：前台默认 15000，`0` = 不超时；后台缺省不超时。
    pub timeout_ms: Option<u64>,
    /// 是否显式后台。
    pub background: bool,
}

/// 解析 `Bash` 的三个公开字段；其余字段一律忽略（源行为）。
///
/// 缺失/类型不符时返回源文案 `Missing command parameter`。
pub fn parse_arguments(arguments: &Value) -> Result<BashArgs, String> {
    let command = match arguments.get("command").and_then(|value| value.as_str()) {
        Some(command) => command.to_string(),
        None => return Err("Missing command parameter".to_string()),
    };
    let background = arguments
        .get("run_in_background")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let timeout_ms = parse_timeout(arguments, background);
    Ok(BashArgs {
        command,
        timeout_ms,
        background,
    })
}

/// `Bash` 工具。
pub struct BashTool {
    registry: Arc<TaskRegistry>,
}

impl BashTool {
    /// 绑定任务注册表（持久化 sink 复用注册表配置中的同一目录）。
    pub fn new(registry: Arc<TaskRegistry>) -> Self {
        Self { registry }
    }

    /// 注册表引用。
    pub fn registry(&self) -> &Arc<TaskRegistry> {
        &self.registry
    }

    fn persist(&self) -> Arc<dyn OutputPersist> {
        Arc::clone(&self.registry.config().persist)
    }

    /// 执行一次 `Bash` 调用。
    pub async fn invoke(&self, arguments: &Value, ctx: &RequestContext) -> ToolResponse {
        let parsed = match parse_arguments(arguments) {
            Ok(parsed) => parsed,
            Err(message) => return error_response(message, None),
        };
        let command = parsed.command.clone();
        let timeout_ms = parsed.timeout_ms;
        let args = BashRunArgs {
            command: command.clone(),
            timeout_ms,
            background: parsed.background,
        };

        match self.registry.start_bash(ctx, args).await {
            Ok(BashRun::Finished { state }) => {
                let text = final_text(&state, self.persist().as_ref());
                success_response(text, &state, Some("completed"), None)
            }
            Ok(BashRun::Running {
                snapshot,
                state,
                promoted: false,
            }) => {
                let text = background_message(&snapshot, &state);
                success_response(
                    text,
                    &state,
                    Some("running"),
                    Some(snapshot.task_id.clone()),
                )
            }
            Ok(BashRun::Running {
                snapshot,
                state,
                promoted: true,
            }) => {
                // 前台超时提升：源以 tool error 返回，进程继续存活。
                let text = promotion_message(&command, timeout_ms, &state, self.persist().as_ref());
                error_response(text, Some((*snapshot, *state)))
            }
            Err(BashRunError::ConcurrentLimit { limit }) => error_response(
                format!("Maximum {limit} concurrent background tasks reached"),
                None,
            ),
            Err(BashRunError::PromotionUnavailable { reason, state }) => {
                let text = promotion_failure_message(
                    &command,
                    timeout_ms,
                    &state,
                    &reason,
                    self.persist().as_ref(),
                );
                error_response(text, None)
            }
            Err(BashRunError::WorkerFailure { message, .. }) => {
                error_response(format!("Error executing command: {message}"), None)
            }
            Err(BashRunError::Worker(error)) => {
                error_response(format!("Error executing command: {error}"), None)
            }
            Err(BashRunError::Payload { message }) => {
                error_response(format!("Error executing command: {message}"), None)
            }
        }
    }
}

/// 前台完成文本：工具已按内部限额合并与截断，直接使用其终态文本。
fn final_text(state: &BashTaskState, persist: &dyn OutputPersist) -> String {
    match &state.final_output {
        Some(text) => text.clone(),
        None => {
            // 兜底路径（理论上不可达）：终态缺少摘要文本时在工具层补齐同样的语义。
            let merged = merge_output(&state.stdout, &state.stderr, state.exit_code);
            truncate_output(&merged, persist).text
        }
    }
}

/// 后台启动文本（源 `terminal.rs` 后台段，Monitor 一句改为本产品的资源路径）。
fn background_message(snapshot: &crate::wire::TaskSnapshot, state: &BashTaskState) -> String {
    let task_id = &snapshot.task_id;
    let mut message = format!(
        "Background shell task started.\ntask_id: {task_id}\nThe command is running in the background."
    );
    match snapshot.pid {
        Some(pid) => {
            message.push_str(&format!(
                "\npid: {pid}\n\
                 - Kill it: run `kill {pid}` in another shell command (`kill -- -{pid}` kills the whole process group including child processes)\n\
                 - Live output: Read the log file {}",
                snapshot.stdout_log.as_deref().unwrap_or("<unavailable>")
            ));
            if let Some(stderr_log) = snapshot.stderr_log.as_deref() {
                message.push_str(&format!(" (stderr: {stderr_log})"));
            }
            if snapshot.stdout_log.is_some() {
                message
                    .push_str(" — it appends while the command runs (use the Read tool to view)");
            }
            message.push_str(&format!(
                "\n- Monitor: read the resource `{}` for status, exit code and log paths; status changes are published as resource update notifications",
                task_resource_uri(task_id)
            ));
        }
        None => message
            .push_str("\n(process failed to spawn — a failure notification will arrive shortly)"),
    }
    let _ = state;
    message
}

/// 前台超时提升文本（源两版：有输出 / 无输出）。
fn promotion_message(
    command: &str,
    timeout_ms: Option<u64>,
    state: &BashTaskState,
    persist: &dyn OutputPersist,
) -> String {
    let seconds = timeout_ms.unwrap_or_default() as f64 / 1000.0;
    let task_id = &state.task_id;
    let pid = state.pid.unwrap_or_default();
    let ps_line = state
        .process_state
        .as_deref()
        .map(|snapshot| format!("Process state: {snapshot}"))
        .unwrap_or_default();
    let partial = merge_output(&state.stdout, &state.stderr, None);
    let partial_hint = persist_partial_output(&partial, persist).hint;
    let has_output = !state.stdout.is_empty() || !state.stderr.is_empty();

    if has_output {
        format!(
            "Command timed out after {seconds:.1}s. The process is still running and has been promoted to a background task (it was producing output, so it is likely progressing).\ntask_id: {task_id}\npid: {pid}\n{ps_line}\n- It continues running in the background; you will be notified when it completes.\n- Kill it: run `kill {pid}` in another shell command (`kill -- -{pid}` kills the whole process group including child processes)\n{partial_hint}\nCommand that timed out: {command}"
        )
    } else {
        format!(
            "Command timed out after {seconds:.1}s with no output produced. The process is still running and has been promoted to a background task, but it may never complete on its own.\ntask_id: {task_id}\npid: {pid}\n{ps_line}\nLikely causes:\n- The command is waiting for input or for a resource (network, lock, another process) that will never arrive.\n- It is a long-running service/daemon; it should have been started with run_in_background: true.\n- It is a slow command still in a silent startup phase (e.g. compile/install with no output yet).\nIf it does not complete on its own, terminate it: run `kill {pid}` in another shell command (`kill -- -{pid}` kills the whole process group including child processes)\n{partial_hint}\nCommand that timed out: {command}"
        )
    }
}

/// 提升失败文本（容量已满；进程组已终止）。
fn promotion_failure_message(
    command: &str,
    timeout_ms: Option<u64>,
    state: &BashTaskState,
    reason: &str,
    persist: &dyn OutputPersist,
) -> String {
    let seconds = timeout_ms.unwrap_or_default() as f64 / 1000.0;
    let ps_line = state
        .process_state
        .as_deref()
        .map(|snapshot| format!("Process state: {snapshot}"))
        .unwrap_or_default();
    let partial = merge_output(&state.stdout, &state.stderr, None);
    let partial_hint = persist_partial_output(&partial, persist).hint;
    format!(
        "Command timed out after {seconds:.1}s and could not be promoted to a background task: {reason}. The process group has been terminated.\n{ps_line}\n{partial_hint}\nCommand that timed out: {command}"
    )
}

fn structured(
    state: &BashTaskState,
    status_override: Option<&str>,
    task_id: Option<String>,
) -> StructuredOutput {
    structured_with_base(
        StructuredOutput::ok("Bash"),
        state,
        status_override,
        task_id,
    )
}

/// 错误分支的 structured 投影：基线 `ok: false`。
///
/// `ok` 的契约是「与 [`crate::wire::ToolResponse`] 的 `is_error` 一致」（见
/// [`StructuredOutput`] 的 `ok` 字段说明），错误分支因此不得复用成功形状，否则
/// 同一响应内 `ok: true` 与 `isError: true` 互相矛盾。
fn structured_error(
    state: &BashTaskState,
    status_override: Option<&str>,
    task_id: Option<String>,
) -> StructuredOutput {
    structured_with_base(
        StructuredOutput::error("Bash"),
        state,
        status_override,
        task_id,
    )
}

fn structured_with_base(
    mut output: StructuredOutput,
    state: &BashTaskState,
    status_override: Option<&str>,
    task_id: Option<String>,
) -> StructuredOutput {
    output.truncated = state.truncated;
    output.persisted_path = state.persisted_path.clone();
    output.task_id = task_id;
    output.exit_code = state.exit_code;
    output.elapsed_ms = Some(state.elapsed_ms);
    let status = status_override
        .map(|value| value.to_string())
        .unwrap_or_else(|| format!("{:?}", state.status).to_lowercase());
    output = output
        .with_extra("status", Value::String(status))
        .with_extra("pid", json_opt(state.pid))
        .with_extra("pgid", json_opt(state.pgid))
        .with_extra("stdout_log", json_opt(state.stdout_log.clone()))
        .with_extra("stderr_log", json_opt(state.stderr_log.clone()))
        .with_extra("promoted", Value::Bool(state.promoted))
        // `timed_out` 是**任务级**标志（因期限到期被终止），不是"本次调用是否超时"：
        // 前台超时提升本身是调用级失败，由 `isError`/`ok`/`error` 表达；进程被提升后
        // 仍在运行，故此处保持任务的真实值（`false`），与 `status: "running"`、
        // `promoted: true` 自洽。
        .with_extra("timed_out", Value::Bool(state.timed_out))
        .with_extra("cancelled", Value::Bool(state.cancelled))
        .with_extra(
            "host_projection",
            serde_json::json!({
                "output_char_limit_chars": HOST_OUTPUT_CHAR_LIMIT,
                "note": host_projection_note(),
            }),
        )
        .with_extra(
            "tool_limits",
            serde_json::json!({
                "max_output_chars": MAX_OUTPUT_CHARS,
                "max_output_lines": MAX_OUTPUT_LINES,
            }),
        );
    if let Some(process_state) = state.process_state.as_deref() {
        output = output.with_extra("process_state", Value::String(process_state.to_string()));
    }
    output
}

fn json_opt<T: serde::Serialize>(value: Option<T>) -> Value {
    match value {
        Some(value) => serde_json::to_value(value).unwrap_or(Value::Null),
        None => Value::Null,
    }
}

fn success_response(
    text: String,
    state: &BashTaskState,
    status: Option<&str>,
    task_id: Option<String>,
) -> ToolResponse {
    ToolResponse {
        text,
        structured: serde_json::to_value(structured(state, status, task_id)).unwrap_or(Value::Null),
        is_error: false,
        meta: None,
    }
}

/// 工具错误结果。
///
/// `promoted` 为前台超时提升时携带的 `(快照, 任务状态)`：该分支是**调用级失败 +
/// 任务级存活**——`isError`/`ok`/`error` 如实表达"这次调用失败"，`promoted`/
/// `status`/`pid` 如实表达"进程被提升为后台任务且在跑"。任务若因期限到期被终止，
/// 其 `timed_out` 由任务注册表置位；提升路径不终止进程，故该字段保持任务真实状态
/// （与源实现把续跑任务的 `timed_out` 记为 `false` 一致），不再与成功标志混用。
fn error_response(
    text: String,
    promoted: Option<(crate::wire::TaskSnapshot, BashTaskState)>,
) -> ToolResponse {
    let structured = match promoted {
        Some((snapshot, state)) => {
            let mut value = structured_error(&state, Some("running"), Some(snapshot.task_id));
            value = value.with_extra("error", Value::String(text.clone()));
            value
        }
        None => {
            let mut value = StructuredOutput::error("Bash");
            value = value.with_extra("error", Value::String(text.clone()));
            value
        }
    };
    ToolResponse {
        text,
        structured: serde_json::to_value(structured).unwrap_or(Value::Null),
        is_error: true,
        meta: None,
    }
}
