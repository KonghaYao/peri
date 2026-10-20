//! Shared invocation pipeline for outer Act batches and nested effective calls.
//! Approval, cancellation, immediate completion events and result hooks settle here;
//! transcript commit and after_tools_batch remain with the outer batch owner.
//!
//! The pipeline reads only [`InvocationContext`] facts/ports: session snapshot, turn
//! facts, queue and render ports. Act-side projections (model transcript commit,
//! after_tools_batch, failure counter, metrics) stay with `tool_dispatch.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use super::super::middleware_runner::{
    run_after_tool, run_before_bound_tools_batch, run_invocation_on_error,
};
use super::super::{InvocationContext, ModelTurnFacts};
use super::effective_dispatcher::StageEffectiveToolDispatcher;
use crate::agent::events_v2::RenderEvent;
use crate::agent::react::{ToolCall, ToolResult};
use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, MessageId};
use crate::session::subagent::SubagentFailure;
use crate::session::tool_catalog::SessionToolCatalogSnapshot;
use crate::tools::{
    normalize_params, BaseTool, EffectiveToolError, EffectiveToolErrorCode, InvocationId,
    ToolCallId, ToolExecutionEvidence, ToolExecutionStatus, ToolOutcome, ToolOutput,
};

/// 拒绝原因 → 稳定错误分类（**单一权威**：模型面结算、协议错误面与
/// [`effective_tool_error`] 共用同一张表，不各自硬编码）。
///
/// 「无人可答」与「本端策略拒绝」都**不是**用户拒绝（裁决 R9）：前者是没有人可以
/// 决定，后者是策略 / 机制拒绝，两者一律映射为 [`EffectiveToolErrorCode::PermissionDenied`]
///（「未获批准的策略拒绝」），只有**已作出的用户决定**才映射为
/// [`EffectiveToolErrorCode::UserRejected`]。
pub(super) fn rejection_error_code(
    cause: peri_acp_types::error::ToolRejectionCause,
) -> EffectiveToolErrorCode {
    match cause {
        peri_acp_types::error::ToolRejectionCause::UserRejected => {
            EffectiveToolErrorCode::UserRejected
        }
        peri_acp_types::error::ToolRejectionCause::ApprovalUnavailable(_)
        | peri_acp_types::error::ToolRejectionCause::PolicyDenied => {
            EffectiveToolErrorCode::PermissionDenied
        }
    }
}

/// `AgentError` → 稳定错误分类（同一权威表的入口）。
pub(super) fn effective_error_code(error: &AgentError) -> EffectiveToolErrorCode {
    match error {
        AgentError::ToolNotFound(_) => EffectiveToolErrorCode::UnknownTool,
        AgentError::SerializationError(_) => EffectiveToolErrorCode::InvalidInput,
        AgentError::ToolRejected { cause, .. } => rejection_error_code(*cause),
        AgentError::Interrupted => EffectiveToolErrorCode::Cancelled,
        _ => EffectiveToolErrorCode::ToolFailed,
    }
}

pub(super) fn effective_tool_error(error: AgentError) -> EffectiveToolError {
    let mut effective =
        EffectiveToolError::new(effective_error_code(&error), error.user_facing_message());
    // typed 事实随错误一起过边界（裁决 R9）：消费方无需任何带外记录点即可区分
    // 「没有人可以决定」与「用户拒绝」。
    if let AgentError::ToolRejected {
        cause: peri_acp_types::error::ToolRejectionCause::ApprovalUnavailable(cause),
        ..
    } = error
    {
        effective = effective.with_unanswered_cause(cause);
    }
    effective
}

fn effective_tool_error_from_boxed(
    error: Box<dyn std::error::Error + Send + Sync>,
) -> EffectiveToolError {
    let error = match error.downcast::<EffectiveToolError>() {
        Ok(error) => return *error,
        Err(error) => error,
    };
    let error = match error.downcast::<AgentError>() {
        Ok(error) => return effective_tool_error(*error),
        Err(error) => error,
    };
    let mut effective =
        EffectiveToolError::new(EffectiveToolErrorCode::ToolFailed, error.to_string());
    if let Some(failure) = error.downcast_ref::<SubagentFailure>() {
        if failure.execution_finished() {
            effective.code = EffectiveToolErrorCode::ApplicationFailed;
        }
        if let Some(safe_failure) = failure.safe_failure() {
            effective = effective.with_subagent_failure(safe_failure);
        }
    }
    effective
}

fn execution_for_effective_error(code: EffectiveToolErrorCode) -> Option<ToolExecutionEvidence> {
    let status = match code {
        EffectiveToolErrorCode::Cancelled => ToolExecutionStatus::Cancelled,
        EffectiveToolErrorCode::Timeout => ToolExecutionStatus::TimedOut,
        _ => return None,
    };
    Some(ToolExecutionEvidence {
        status,
        exit_code: None,
        output_ref: None,
        output_truncated: false,
        task_id: None,
    })
}

/// 收集阶段产物（内部使用）
pub(super) struct CollectOutcome {
    pub(super) results: Vec<SettledInvocation>,
    pub(super) was_cancelled: bool,
    pub(super) deferred_error: Option<String>,
}

impl CollectOutcome {
    /// `(canonical 调用, 已结算结果)` 视图：`after_tools_batch` 等 Act 侧投影使用。
    pub(super) fn result_pairs(&self) -> Vec<(ToolCall, ToolResult)> {
        self.results
            .iter()
            .map(|settled| (settled.call.clone(), settled.result.clone()))
            .collect()
    }
}

/// 单条已结算的 canonical 执行。
///
/// `outcome` 是工具结果事实（raw + 真实 `isError`），与模型面文本投影分开保存：
/// Act 侧据此投影 transcript / 失败计数，Apps relay 据此投影原始结果载荷。
pub(super) struct SettledInvocation {
    pub(super) call: ToolCall,
    pub(super) result: ToolResult,
    pub(super) outcome: ToolOutcome,
}

impl SettledInvocation {
    /// 未报告结果事实的结算（审批前结算、解析失败等在工具执行之前完成的结果）。
    pub(super) fn unreported(call: ToolCall, result: ToolResult) -> Self {
        Self {
            call,
            result,
            outcome: ToolOutcome::Unreported,
        }
    }
}

/// 分配一次 canonical 执行的执行身份。
///
/// 当前由 Agent 侧本地生成（`InvocationId::generate`）；阶段 2 把分配点移到
/// session 服务（执行身份由服务端颁发、跨进程可重放）后，本函数与其调用点一并
/// 移除，Agent 侧不再自行生成执行身份。
fn next_invocation_id() -> InvocationId {
    InvocationId::generate()
}

/// before_tool 审批阶段的产出
struct ApprovalOutcome {
    /// 通过审批、准备并发执行的调用
    ready_calls: Vec<ToolCall>,
    /// 已在审批阶段就结算完成的（例如 ToolRejected）结果
    settled_results: Vec<SettledInvocation>,
}

/// 执行 before_tool 审批 + 并发工具调用，收集所有结果（不写 transcript）
///
/// Orchestrator：按顺序调用三个子阶段函数。
#[allow(clippy::too_many_arguments)] // 阶段边界显式传递同一 Reason 固定的 catalog 与调用上下文
pub(super) async fn collect_tool_results(
    ctx: &InvocationContext,
    original_calls: Vec<ToolCall>,
    event_calls: &HashMap<String, ToolCall>,
    all_tools: &HashMap<String, Arc<dyn BaseTool>>,
    catalog: &Arc<SessionToolCatalogSnapshot>,
    cancel: &CancellationToken,
    // ai_msg_id 保留为 API 契约（未来 ToolEnd 事件可携带 message_id）
    ai_msg_id: MessageId,
    ai_msg: &BaseMessage,
    model_tool_call_id: Option<&str>,
) -> AgentResult<CollectOutcome> {
    let _ = ai_msg_id;
    // turn 是**可选事实**：模型来源的执行（模型批次 / 显式模型调用）必须提供真实
    // turn（render 事件纽带、step、turn_generation 都绑定它），缺失即 typed 不可用；
    // App / Host / 嵌套来源按构造没有 turn 绑定义务，管线照常执行但：
    // ① 不产生任何 `RenderEvent`（无事件端口 + 无 turn_id）；
    // ② 不写 `turn_generation`；
    // 需要 turn 的事后钩子在 `run_after_tool` / `run_invocation_on_error` 显式记录跳过。
    let turn = ctx.require_model_turn("tool dispatch")?;

    // 阶段一：批量 before_tool 审批
    let approval =
        run_before_tool_approvals(ctx, original_calls, event_calls, all_tools, cancel).await?;

    // yield 使 EventBus forwarder task 排空 render_tx 中由阶段一 emit 的
    // ToolStarted 事件（转发到 event_tx），保证在 SubAgent 工具 invoke 内部
    // 通过 handler.on_event(SubagentStarted) 直发 event_tx 之前，ToolStart
    // 已就位。否则 forwarder 的两个 hops 延迟会让 SubagentStarted 抢先到达
    // event_tx，导致 TUI segment 顺序反转（SubAgent 段落在 ToolCard(Agent) 前），
    // SubAgent 工具调用跑到 Agent 卡片上方。
    tokio::task::yield_now().await;

    // 阶段二：并发执行（snapshot messages + ai_msg 只读视图）
    let tool_results = dispatch_concurrent(
        ctx,
        turn,
        &approval.ready_calls,
        event_calls,
        all_tools,
        catalog,
        cancel,
        ai_msg,
        model_tool_call_id,
    )
    .await;

    // 阶段三：聚合 + 错误延迟
    Ok(settle_results(
        ctx,
        approval,
        tool_results,
        cancel.is_cancelled(),
        all_tools,
    )
    .await)
}

/// 阶段一：批量 before_tool 审批。
///
/// 遍历 `run_before_tools_batch` 结果，emit `ToolStarted`，分流：
/// - `Ok(call)` → 推入 ready_calls
/// - `Err(ToolRejected)` → emit ToolStart + ToolEnd，推入 settled_results
/// - `Err(e)` → run_on_error + 为已 emit ToolStart 的补发 ToolEnd，向上传播错误
///
/// 取消检查发生在 zip 迭代开头：若已取消，为 ready_calls 补发 ToolEnd 后返回 Interrupted。
async fn run_before_tool_approvals(
    ctx: &InvocationContext,
    original_calls: Vec<ToolCall>,
    event_calls: &HashMap<String, ToolCall>,
    targets: &HashMap<String, Arc<dyn BaseTool>>,
    cancel: &CancellationToken,
) -> AgentResult<ApprovalOutcome> {
    // 审批输入全是 session 级事实（会话身份 + 归属 + 工具表）：无 turn 的 App /
    // Host 入口照常审批；模型来源缺 turn 时逐条 typed 不可用（由
    // `run_before_bound_tools_batch` 逐条重建同一错误）。
    let turn = ctx.require_model_turn("before_tools_batch")?;
    let agent_id = ctx.agent_id();

    let mut ready_calls: Vec<ToolCall> = Vec::with_capacity(original_calls.len());
    let mut settled_results: Vec<SettledInvocation> = Vec::new();

    let before_results = run_before_bound_tools_batch(ctx, &original_calls, targets).await;

    // 事件纽带只在真实 turn 下存在：无 turn 的执行（idle App / Host）一条
    // `RenderEvent` 都不发（也不冒充模型工具调用）。
    let turn_id = turn.map(|turn| turn.turn_id());

    for (tool_call, before_result) in original_calls.iter().zip(before_results) {
        if cancel.is_cancelled() {
            // 为已 emit ToolStart 的 ready_calls 补发 ToolEnd（无 turn 的执行没有
            // 事件端口，也没有 turn 事件纽带：按构造零事件）。
            if let Some(turn_id) = turn_id {
                for tc in &ready_calls {
                    let raw_call = event_calls.get(&tc.id).unwrap_or(tc);
                    ctx.emit_render(RenderEvent::ToolEnded {
                        turn_id,
                        agent_id,
                        tool_call_id: raw_call.id.clone(),
                        name: raw_call.name.clone(),
                        output: "interrupted by user".to_string(),
                        is_error: true,
                        subagent_failure: None,
                    });
                }
            }
            return Err(AgentError::Interrupted);
        }
        match before_result {
            Ok(modified_call) => {
                if modified_call.id != tool_call.id || modified_call.name != tool_call.name {
                    let reason = "middleware cannot modify tool call id or name".to_string();
                    let raw_call = event_calls.get(&tool_call.id).unwrap_or(tool_call);
                    let rejection_result = ToolResult::error(&raw_call.id, &tool_call.name, reason);
                    settled_results.push(SettledInvocation::unreported(
                        raw_call.clone(),
                        rejection_result,
                    ));
                    continue;
                }
                let raw_call = event_calls.get(&tool_call.id).unwrap_or(tool_call);
                if let Some(turn_id) = turn_id {
                    ctx.emit_render(RenderEvent::ToolStarted {
                        turn_id,
                        agent_id,
                        tool_call_id: raw_call.id.clone(),
                        name: raw_call.name.clone(),
                        input: modified_call.input.clone(),
                    });
                }
                ready_calls.push(modified_call);
            }
            Err(AgentError::ToolRejected {
                ref reason,
                ref cause,
                ..
            }) => {
                let raw_call = event_calls.get(&tool_call.id).unwrap_or(tool_call);
                let mut rejection_result =
                    ToolResult::error(&tool_call.id, &tool_call.name, reason.clone());
                // typed 原因同时落到结果事实（裁决 R9）：模型面只看到文本，协议错误面
                // （`EffectiveToolDispatcher::dispatch`）据此还原「无人可答 ⇒ 未执行」。
                rejection_result.effective_error_code = Some(rejection_error_code(*cause));
                rejection_result.rejection_cause = Some(*cause);
                if let Some(turn_id) = turn_id {
                    ctx.emit_render(RenderEvent::ToolStarted {
                        turn_id,
                        agent_id,
                        tool_call_id: raw_call.id.clone(),
                        name: raw_call.name.clone(),
                        input: raw_call.input.clone(),
                    });
                    ctx.emit_render(RenderEvent::ToolEnded {
                        turn_id,
                        agent_id,
                        tool_call_id: raw_call.id.clone(),
                        name: raw_call.name.clone(),
                        output: rejection_result.output.clone(),
                        is_error: true,
                        subagent_failure: rejection_result.subagent_failure.clone(),
                    });
                }
                settled_results.push(SettledInvocation::unreported(
                    tool_call.clone(),
                    rejection_result,
                ));
            }
            Err(e) => {
                let _ = run_invocation_on_error(ctx, &e).await;
                if let Some(turn_id) = turn_id {
                    for tc in &ready_calls {
                        ctx.emit_render(RenderEvent::ToolEnded {
                            turn_id,
                            agent_id,
                            tool_call_id: tc.id.clone(),
                            name: tc.name.clone(),
                            output: e.to_string(),
                            is_error: true,
                            subagent_failure: None,
                        });
                    }
                }
                return Err(e);
            }
        }
    }

    Ok(ApprovalOutcome {
        ready_calls,
        settled_results,
    })
}

/// 阶段二：并发执行 ready_calls（snapshot messages + ai_msg 只读视图）。
///
/// 每个调用走 `biased` select：cancel.cancelled() 优先于 invoke_fut，
/// 命中时返回 `ToolExecutionFailed { reason: "interrupted by user" }`。
#[allow(clippy::too_many_arguments)] // 与 collect_tool_results 同一决策：阶段边界显式传递调用上下文，不分组
async fn dispatch_concurrent(
    ctx: &InvocationContext,
    turn: Option<&ModelTurnFacts>,
    ready_calls: &[ToolCall],
    event_calls: &HashMap<String, ToolCall>,
    all_tools: &HashMap<String, Arc<dyn BaseTool>>,
    catalog: &Arc<SessionToolCatalogSnapshot>,
    cancel: &CancellationToken,
    ai_msg: &BaseMessage,
    model_tool_call_id: Option<&str>,
) -> Vec<Result<ToolOutput, EffectiveToolError>> {
    if ready_calls.is_empty() {
        return Vec::new();
    }

    let messages_snapshot: Arc<Vec<BaseMessage>> = {
        let mut msgs = ctx.messages().to_vec();
        msgs.push(ai_msg.clone());
        Arc::new(msgs)
    };
    let cwd_snapshot = ctx.cwd().to_owned();
    // 会话上下文用量快照（dispatch 时刻的只读事实，与 StateSnapshot / 自动 Compact
    // 同口径）：工具在启动子执行前据此评估父会话上下文压力；无预算或无估算时为 None。
    let context_usage = ctx.context_usage();
    // turn 事件纽带（可选）：无 turn 的执行不写 turn_generation、不发 ToolEnded。
    let turn_id = turn.map(|turn| turn.turn_id());
    let agent_id = ctx.agent_id();
    // 会话身份是真实事实：缺失即 `None`（工具侧的交互归属据此 fail-closed），
    // 不从 agent_id / turn 身份反推。
    let session_id = ctx.session_id().map(str::to_owned);
    let session_resources = ctx.session_resources().cloned();
    let dispatch_context = ctx.clone();
    let dispatch_catalog = Arc::clone(catalog);

    let futures: Vec<_> = ready_calls
        .iter()
        .map(|call| {
            let tool_name = call.name.clone();
            let call_id = call.id.clone();
            let raw_call = event_calls
                .get(&call.id)
                .cloned()
                .unwrap_or_else(|| call.clone());
            let model_tool_call_id = model_tool_call_id.unwrap_or(&raw_call.id).to_owned();
            let tool = all_tools.get(&call.id).cloned();
            let input = match &tool {
                Some(t) => normalize_params(call.input.clone(), Some(t.as_ref())),
                None => call.input.clone(),
            };
            let cancel = cancel.clone();
            let messages = Arc::clone(&messages_snapshot);
            let cwd = cwd_snapshot.clone();
            let dispatch_context = dispatch_context.clone();
            let dispatch_catalog = Arc::clone(&dispatch_catalog);
            // [Fix] span 在 async 块外创建、用 .instrument() 包裹整个 future：
            // span.enter() 的 guard 跨 await 持有在 tokio multi-thread 下会随 task
            // 线程迁移错误重置 thread-local current span，导致 tracing-subscriber
            // `lookup_current` panic（'the subscriber should have data for the current span'）。
            // instrument 在每次 poll 时重新 enter，跨线程安全。
            let span = tracing::info_span!(
                "agent.tool_call",
                tool.name = %tool_name,
                tool.call_id = %call_id,
            );
            let output_limit = tool.as_ref().and_then(|tool| tool.output_char_limit());
            let origin = ctx.origin_for(&ToolCallId::new(model_tool_call_id.clone()));
            let session_id = session_id.clone();
            // turn_generation 只有真实 turn 才有：无 turn 的执行不写（不伪造）。
            let turn_generation = turn_id.map(|turn_id| turn_id.to_string());
            // 归属事实按**单条执行的来源**投影：App / Host 来源由来源自身派生，
            // 模型 / 嵌套来源沿用环境归属（真实 turn 事实）。
            let attribution = ctx.attribution_for(&origin);
            let session_resources = session_resources.clone();
            async move {
                let timeout_opt = tool.as_ref().and_then(|t| t.timeout());
                let invoke_fut = async {
                    let mut ctx_param = crate::tools::ToolContext::new(&messages, &cwd)
                        .with_invocation(
                            Arc::new(
                                StageEffectiveToolDispatcher::new(
                                    dispatch_context.clone(),
                                    dispatch_catalog,
                                )
                                .with_tool_call_id(ToolCallId::new(model_tool_call_id.clone())),
                            ),
                            next_invocation_id(),
                            origin,
                            cancel.clone(),
                        )
                        .with_tool_call_id(model_tool_call_id)
                        .with_context_usage(context_usage);
                    // 会话身份缺失即 `None`：不调用 `with_session_identity`（它会把
                    // 身份写成 `Some`），否则工具侧的交互归属会拿到伪造的会话事实。
                    // turn_generation 来自真实 turn（本管线前面已 `require_turn`），
                    // 与会话身份各自独立，不得互相替代。
                    ctx_param.session_id = session_id;
                    ctx_param.turn_generation = turn_generation;
                    ctx_param.attribution = attribution;
                    ctx_param.session_resources = session_resources;
                    // 投递归属 = 直接发起会话（本 session）；路由取自该会话的
                    // canonical 持久化句柄，不接受模型参数。
                    ctx_param = ctx_param.with_task_terminal_delivery(match dispatch_context
                        .async_result_mailbox()
                    {
                        Some(mailbox) => {
                            crate::agent::async_tasks::delivery::SessionTerminalDelivery::
                                for_queue_with_mailbox(
                                    dispatch_context.queue().queue().clone(),
                                    Arc::clone(mailbox),
                                )
                        }
                        // idle App / Host 入口没有会话 mailbox：投递仍入队，只是
                        // 不参与会话级处理责任跟踪。
                        None => crate::agent::async_tasks::delivery::SessionTerminalDelivery::
                            for_queue(dispatch_context.queue().queue().clone()),
                    });
                    match tool {
                        Some(t) => t
                            .invoke_output(input, ctx_param)
                            .await
                            .map_err(effective_tool_error_from_boxed),
                        None => Err(effective_tool_error(AgentError::ToolNotFound(
                            tool_name.clone(),
                        ))),
                    }
                };
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        Err(EffectiveToolError::new(
                            EffectiveToolErrorCode::Cancelled,
                            "interrupted by user",
                        ))
                    }
                    result = async {
                        if let Some(d) = timeout_opt {
                            peri_time::timeout(d, invoke_fut).await
                        } else {
                            Ok(invoke_fut.await)
                        }
                    } => {
                        match result {
                            Ok(tool_result) => tool_result,
                            Err(_elapsed) => {
                                // 安全：Err 分支仅在 timeout_opt 为 Some 时可达
                                let secs = timeout_opt.unwrap().as_secs();
                                Err(EffectiveToolError::new(
                                    EffectiveToolErrorCode::Timeout,
                                    format!("tool call timed out after {}s", secs),
                                ))
                            }
                        }
                    }
                };
                // 工具完成即刻 emit ToolEnded，不等 join_all 返回
                // 快速工具的 Langfuse observation endTime 不再被慢工具拖高
                let (output, is_error) = match &result {
                    Ok(o) => (
                        output_limit
                            .map(|limit| o.bounded_text(limit))
                            .unwrap_or_else(|| o.projected_text(None)),
                        o.is_error() == Some(true)
                            || o.execution.as_ref().is_some_and(|e| {
                                matches!(
                                    e.status,
                                    crate::tools::ToolExecutionStatus::Failed
                                        | crate::tools::ToolExecutionStatus::Cancelled
                                        | crate::tools::ToolExecutionStatus::TimedOut
                                        | crate::tools::ToolExecutionStatus::RunningAfterTimeout
                                )
                            }),
                    ),
                    Err(e) => {
                        let output = ToolOutput {
                            text: e.to_string(),
                            execution: execution_for_effective_error(e.code),
                            outcome: ToolOutcome::Unreported,
                        };
                        (
                            output_limit
                                .map(|limit| output.bounded_text(limit))
                                .unwrap_or_else(|| output.projected_text(None)),
                            true,
                        )
                    }
                };
                let subagent_failure = result
                    .as_ref()
                    .err()
                    .and_then(|error| error.subagent_failure().cloned());
                // 无 turn 的执行没有事件纽带：一条 render 事件也不发。
                if let Some(turn_id) = turn_id {
                    dispatch_context.emit_render(RenderEvent::ToolEnded {
                        turn_id,
                        agent_id,
                        tool_call_id: raw_call.id.clone(),
                        name: raw_call.name.clone(),
                        output,
                        is_error,
                        subagent_failure,
                    });
                }
                result
            }
            .instrument(span)
        })
        .collect();
    futures::future::join_all(futures).await
}

/// 阶段三：串行处理结果（ToolEnd 已在 dispatch_concurrent 中 emit）
/// + after_tool + 截断 + 聚合。
///
/// 不变量：deferred_error 取首个 after_tool 错误，后续错误不覆盖。
async fn settle_results(
    ctx: &InvocationContext,
    approval: ApprovalOutcome,
    tool_results: Vec<Result<ToolOutput, EffectiveToolError>>,
    was_cancelled: bool,
    all_tools: &HashMap<String, Arc<dyn BaseTool>>,
) -> CollectOutcome {
    let all_tools_ref = all_tools;

    let ApprovalOutcome {
        ready_calls,
        mut settled_results,
    } = approval;

    let mut deferred_error: Option<String> = None;
    let mut exec_results: Vec<SettledInvocation> = Vec::with_capacity(ready_calls.len());

    for (modified_call, tool_result) in ready_calls.into_iter().zip(tool_results) {
        let outcome = match &tool_result {
            Ok(output) => output.outcome.clone(),
            Err(_) => ToolOutcome::Unreported,
        };
        let mut result = match tool_result {
            Ok(output) => ToolResult::from_output(&modified_call.id, &modified_call.name, output),
            Err(ref e) => {
                let mut result =
                    ToolResult::error(&modified_call.id, &modified_call.name, e.to_string());
                result.effective_error_code = Some(e.code);
                result.execution = execution_for_effective_error(e.code);
                result.subagent_failure = e.subagent_failure.as_deref().cloned();
                if let Some(failure) = &result.subagent_failure {
                    result.output.push('\n');
                    result.output.push_str(&failure.render_model_summary());
                }
                result
            }
        };

        if result.is_error {
            tracing::warn!(
                tool.name = %result.tool_name,
                tool.is_error = true,
                error_len = result.output.len(),
                "tool call failed"
            );
            record_tool_failure(ctx, &modified_call, &result);
        }

        // ToolEnd 已在 dispatch_concurrent 中 emit（工具完成即刻发射）
        // 此处仅处理 after_tool + 截断等后处理逻辑

        if let Err(e) = run_after_tool(ctx, &modified_call, &result).await {
            let _ = run_invocation_on_error(ctx, &e).await;
            deferred_error = deferred_error.or(Some(e.to_string()));
        }

        // output_char_limit 截断
        post_process_result(&modified_call, &mut result, all_tools_ref);

        exec_results.push(SettledInvocation {
            call: modified_call,
            result,
            outcome,
        });
    }

    settled_results.extend(exec_results);

    CollectOutcome {
        results: settled_results,
        was_cancelled,
        deferred_error,
    }
}

/// 工具失败的可观测性投影（`tool.error` 指标）。
///
/// 事实取自执行上下文快照（会话身份、run 关联键、turn step），不读阶段上下文：
/// 无 turn 时不写 `step`，不用伪造的 `0` 占位。
fn record_tool_failure(ctx: &InvocationContext, call: &ToolCall, result: &ToolResult) {
    let input_summary: String = call
        .input
        .as_str()
        .unwrap_or("")
        .chars()
        .take(200)
        .collect();
    let mut data = serde_json::json!({
        "name": result.tool_name,
        "tool_call_id": call.id,
        "error": result.output,
        "input_summary": input_summary,
    });
    if let Some(turn) = ctx.turn() {
        data["step"] = serde_json::json!(turn.step());
    }
    crate::metrics::emit("tool.error", data, ctx.session_id(), ctx.run_id());
}

/// 单条结果的后处理：按工具声明的 `output_char_limit` 截断。
fn post_process_result(
    modified_call: &ToolCall,
    result: &mut ToolResult,
    all_tools: &HashMap<String, Arc<dyn BaseTool>>,
) {
    // output_char_limit 截断：已经解析完成的 target 工具声明输出上限时按字符截断
    if let Some(tool) = all_tools.get(&modified_call.id) {
        let limit = tool.output_char_limit();
        let original = result.output.clone();
        let projected = ToolOutput {
            text: original.clone(),
            execution: result.execution.clone(),
            outcome: ToolOutcome::Unreported,
        }
        .projected_text(limit);
        if projected != original {
            let body_truncated = ToolOutput {
                text: original.clone(),
                execution: result.execution.clone(),
                outcome: ToolOutcome::Unreported,
            }
            .body_was_truncated(limit);
            if let Some(evidence) = result.execution.as_mut() {
                evidence.output_truncated |= body_truncated;
            }
            result.output = projected;
        }
    }
}

#[cfg(test)]
#[path = "execution_test.rs"]
mod tests;
