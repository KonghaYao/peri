//! `session/execute-command` dispatch handler.
//!
//! Accepts a slash command string and delegates to the registered
//! [`CommandRegistry`] entries in [`crate::session::command`].
//! This mirrors the in-process interception done by
//! [`crate::session::executor::intercept_immediate_command`] but exposes
//! it as a standalone ACP JSON-RPC method so that external clients (IDE,
//! stdio transport) can execute slash commands without going through
//! the full `session/prompt` pipeline.

use serde_json::Value;

use peri_acp_types::command::{
    CommandFeedback, CommandLevel, CommandOutcome, FeedbackChannel, FeedbackLevel,
};
use peri_agent::session::exec::executor_helpers::emit_command_feedback;
use peri_controller::Controller;

use crate::session::command::{
    register_builtins, CommandContext, CommandRegistry, CommandResult, RouteEntry,
};
use crate::session::executor::PromptStopReason;
use crate::transport::types::AcpError;
use peri_acp_types::command::command_route::CommandSource;

/// Immediate 语义 = core/ui 域第一等级（替代旧 `kind() != Immediate` 判断，
/// Phase 5 Step 6）：execute-command RPC 无 agent 管线，只执行第一等级
/// （core/ui）条目的本地确定性语义；第二等级（mcp/plugin/user）条目为
/// 外部来源动态注入，无本地 Immediate 执行语义。
///
/// 决策 D 例外：`CommandSource::Mcp` 放行——`McpSkillReleaser`
/// 在 RPC 上下文（`supports_inject == false`）不依赖 agent 管线，直接返回
/// skill 全文 + 来源/工具通路标注（与 SkillPreload 预载注入内容同源）；
/// 交互式输入走 preload 注入（语义差异见返回消息）。plugin/user 第二等级
/// 条目维持拒绝（其 handler 无 RPC 直返语义，Inject/Delegate 分支显式报错）。
fn check_immediate_level(entry: &RouteEntry) -> Result<(), AcpError> {
    // 按 provenance 判断而非 kind（审查 Minor）：kind 属元数据形态，伪造
    // 的 McpSkill kind 非生产路径无 handler 语义；provenance 是注册面事实。
    if entry.level() == CommandLevel::Level1
        || matches!(entry.provenance.source, CommandSource::Mcp { .. })
    {
        Ok(())
    } else {
        Err(AcpError::new(
            -32602,
            format!(
                "command '{}' 非 Immediate 命令（等级 {:?}）；execute-command RPC 仅支持 core/ui 域第一等级与 MCP skill 命令",
                entry.fullname,
                entry.level()
            ),
        ))
    }
}

/// Execute a slash command against the given session.
///
/// Accepts `{ session_id, command, args }` in `params`, looks up the command
/// in a freshly built [`CommandRegistry`] (内置命令注册，Phase 5 归属裁决时
/// 随 `ui:` 域迁移统一处理，不引入 session_manager 依赖), and runs it
/// synchronously (blocking the caller) and returns the updated message list.
///
/// 存储访问经 `controller.sessions()`（ARC-BOUNDARY-001 方向），不再由调用方
/// 直传 `thread_store`。
///
/// # Errors
///
/// Returns `AcpError` when:
/// - `session_id` is missing
/// - `command` is missing
/// - The command string does not match any registered command
/// - The matched command returns `Inject` / `Delegate`（execute-command RPC
///   无 agent 管线可注入，显式错误；需经 `session/prompt`。决策 D：
///   `McpSkill` 条目除外——handler 在 RPC 上下文返回 skill 全文 + 标注）
#[allow(clippy::too_many_arguments)]
pub async fn execute_command(
    params: &Value,
    session_history: Vec<peri_acp_types::messages::BaseMessage>,
    cwd: &str,
    peri_config: &std::sync::Arc<crate::provider::PeriConfig>,
    event_sink: &std::sync::Arc<dyn crate::session::event_sink::EventSink>,
    auxiliary_model: Option<std::sync::Arc<dyn peri_model::Model>>,
    cancel_token: &tokio_util::sync::CancellationToken,
    controller: &Controller,
    thread_id: Option<String>,
    bg_event_tx: Option<tokio::sync::mpsc::UnboundedSender<peri_acp_types::event::ExecutorEvent>>,
    task_manager: Option<std::sync::Arc<dyn peri_acp_types::tasks::TaskManager>>,
    frozen_claude_md: Option<std::sync::Arc<String>>,
    frozen_claude_local_md: Option<std::sync::Arc<String>>,
    frozen_skill_summary: Option<std::sync::Arc<String>>,
    frozen_system_prompt: Option<std::sync::Arc<String>>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();

    let command_str = params
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing command"))?;

    let args_value = params.get("args").cloned().unwrap_or(Value::Null);

    // 自建注册表（函数无生产调用者，Phase 5 归属裁决时随 `ui:` 域迁移统一
    // 处理，不引入 session_manager 依赖）；resolve 严格精确（无前缀匹配，
    // 设计 §55）；RPC 路径无 agent 管线可注入，未命中显式报错（与 prompt
    // 路径的 fall-through 语义不同）。
    let registry = CommandRegistry::new();
    register_builtins(&registry);
    let resolved = registry
        .resolve(command_str)
        .ok_or_else(|| AcpError::new(-32602, format!("unknown command: {command_str}")))?;

    // Immediate 语义 = core/ui 域第一等级 + MCP skill 命令（决策 D 放行，
    // 旧 kind() 检查由 RouteEntry 域/等级检查取代；plugin/user Level2 动态
    // 注入条目无本地 Immediate 执行语义 → 显式报错）。
    check_immediate_level(&resolved.entry)?;

    // args：消费 resolved.args（注册表词法切分，不变式 3）——RPC 未显式传
    // args 时回退 command 字符串内嵌参数；显式 args 仅接受 slash 形态字符串
    // （如 `/rewind <id> [--no-revert-files]`）。[P2-1] 旧 JSON 形态（rewind
    // RPC wire）已随 Phase 5 Step 5 废弃：rewind 参数解析已迁 ArgsSchema，
    // JSON 字符串会被 `split_whitespace` 当作 positional 整体，产生误导性
    // 错误「未找到目标消息 {json}」——调用方不得再传 JSON 对象。参数权威
    // 校验见下方 args_schema.parse 前置检查（P1-1，与拦截层同构）。
    let args_string = match args_value {
        Value::Null => resolved.args.clone(),
        Value::String(s) => s,
        other => other.to_string(),
    };

    let cancel_history = session_history.clone();

    // P1-1：复用拦截层同款前置校验（executor_helpers 拦截处同构）——
    // `args_schema.parse` 失败 → 不进入 handler，立即返回 Done +
    // feedback(Error)（错误不进会话、走 UI 通道，设计 §81），使 RPC 路径
    // 与 slash 路径词法严格性一致（未知 option / missing required 均拦截）；
    // 成功 → `ParsedArgs` 经 `ctx.parsed_args` 传入 handler（P1-1 联动：
    // handler 消费统一解析结果，不再自研解析）。
    let parsed_args = match &resolved.entry.args_schema {
        Some(schema) => match schema.parse(&args_string) {
            Ok(parsed) => Some(parsed),
            Err(err) => {
                let name = resolved
                    .entry
                    .fullname
                    .rsplit(':')
                    .next()
                    .unwrap_or(&resolved.entry.fullname);
                let mut result = CommandResult {
                    messages: cancel_history.clone(),
                    stop_reason: PromptStopReason::EndTurn,
                    feedback: Some(CommandFeedback {
                        level: FeedbackLevel::Error,
                        message: format!("{name} 参数解析失败: {err}"),
                        channel: FeedbackChannel::UiOnly,
                    }),
                };
                emit_command_feedback(event_sink, &session_id, &mut result).await;
                event_sink.push_done(&session_id, "end_turn", None).await;
                let messages_json: Vec<Value> = result
                    .messages
                    .iter()
                    .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
                    .collect();
                return Ok(serde_json::json!({
                    "messages": messages_json,
                    "stop_reason": format!("{:?}", result.stop_reason),
                }));
            }
        },
        None => None,
    };
    // Phase 2 拆层：deps 私有化后构造面封闭，core 5 字段经 new() 就位；
    // 旧字段显式赋值保持原字面量语义（行为等价零漂移，字段一个未删）。
    let mut ctx = CommandContext::new(
        session_id.clone(),
        session_history,
        cwd.to_string(),
        std::sync::Arc::clone(event_sink),
        cancel_token.clone(),
        // 扩展依赖接口注册表（本步空表；旧字段迁移归消费方适配任务，
        // 迁移前以 deps/dep::<T>() 形态按接口注入）。
        peri_acp_types::command::DependencyBag::new(),
    );
    // L5：compact 配置由装配点预填（env overrides 每轮重新应用）
    ctx.compact_config = crate::host::compact_config::load_compact_config(peri_config);
    ctx.auxiliary_model = auxiliary_model;
    // 命令原文透传：RPC 路径仅命令名文本（无 `/` 前缀保证，与拦截层整段
    // 透传不同源）；`supports_inject` 保持默认 false——McpSkillReleaser
    // 依此降级为直返 skill 全文（决策 D），其余 Inject 类 handler 由调用方
    // 显式报错（execute_command.rs 语义）。
    ctx.raw_text = command_str.to_string();
    ctx.args = args_string;
    ctx.parsed_args = parsed_args;
    ctx.thread_store = Some(controller.sessions());
    ctx.thread_id = thread_id;
    ctx.bg_event_sender = bg_event_tx;
    ctx.task_manager = task_manager;
    ctx.frozen_claude_md = frozen_claude_md;
    ctx.frozen_claude_local_md = frozen_claude_local_md;
    ctx.frozen_skill_summary = frozen_skill_summary;
    ctx.frozen_system_prompt = frozen_system_prompt;

    // 预取消短路：token 已取消时不进入 handler。`tokio::select!` 非 biased
    // 模式分支随机——若 handler 同步完成（如 compact 无模型时无 await 即
    // EarlyReturn），execute 分支会抢先胜出，导致外层已取消的调用仍执行
    // 命令并发射反馈（outer_cancel 测试用 PendingEventSink，push_event 恒
    // pending → 挂死）。此处提前裁决，语义与测试锁定一致：预取消 →
    // 直接返回 Cancelled（history 原样，不进入 handler）。
    let outcome = if cancel_token.is_cancelled() {
        tracing::info!(session_id = %session_id, "execute_command: cancelled (pre-cancelled)");
        CommandOutcome::Done(CommandResult {
            messages: cancel_history,
            stop_reason: PromptStopReason::Cancelled,
            feedback: None,
        })
    } else {
        tokio::select! {
            r = resolved.entry.handler.execute(ctx) => r,
            _ = cancel_token.cancelled() => {
                tracing::info!(session_id = %session_id, "execute_command: cancelled");
                CommandOutcome::Done(CommandResult {
                    messages: cancel_history,
                    stop_reason: PromptStopReason::Cancelled,
                    feedback: None,
                })
            }
        }
    };
    // Outcome 匹配：Done 走现状 JSON 序列化；Inject/Delegate → AcpError
    // （RPC 无 agent 管线可注入，显式错误；McpSkill 在 RPC 上下文恒返回
    // Done——决策 D 放行语义，见 check_immediate_level）。
    let (messages, stop_reason) = match outcome {
        CommandOutcome::Done(mut result) => {
            // 反馈统一出口：与 executor_helpers 拦截处同源（复用同一 helper），
            // handler.execute 之后、push_done 之前发射 CommandFeedback 事件
            // （channel=Session 额外追加系统消息）。[P2-1] 占位日志退役。
            emit_command_feedback(event_sink, &session_id, &mut result).await;
            (result.messages, result.stop_reason)
        }
        CommandOutcome::Inject(_) | CommandOutcome::Delegate(_) => {
            return Err(AcpError::new(
                -32602,
                format!(
                    "command '{}' 返回 Inject/Delegate；execute-command RPC 无 agent 管线可注入，请改用 session/prompt",
                    resolved.entry.fullname
                ),
            ));
        }
    };

    // Immediate command bypasses the agent event pump, so we must manually
    // signal completion. Otherwise the TUI stays in loading state.
    // [TRAP] See issue_2026-05-29-immediate-command-missing-push-done.
    // Command turns carry no request_id (None).
    event_sink.push_done(&session_id, "end_turn", None).await;

    // Serialize the result messages into a compact JSON array of { role, content }.
    let messages_json: Vec<Value> = messages
        .iter()
        .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
        .collect();

    Ok(serde_json::json!({
        "messages": messages_json,
        "stop_reason": format!("{:?}", stop_reason),
    }))
}

/// Extract and validate the required parameters for `session/execute-command`.
///
/// Returns `(session_id, command, args)` on success.
/// This is a lightweight extraction that does **not** execute the command.
pub fn extract_execute_command_params(params: &Value) -> Result<(String, String, Value), AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();

    let command = params
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing command"))?
        .to_string();

    let args = params.get("args").cloned().unwrap_or(Value::Null);

    Ok((session_id, command, args))
}

#[cfg(test)]
#[path = "execute_command_test.rs"]
mod tests;
