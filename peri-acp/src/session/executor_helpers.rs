//! [`run_session_loop`] 的 helper 子流程。
//!
//! 本文件由 `executor.rs` 拆分而来（EXECUTOR-SPLIT，选项 B）。
//! 主 orchestrator（`run_session_loop`）和 agent 构建 dispatcher（`build_and_execute_agent`）
//! 仍留在 `executor.rs`；本文件承载以下四个被 orchestrator 串起来的子流程：
//!
//! - [`intercept_immediate_command`]：slash 命令拦截（Immediate 直接返回，不构建 agent）
//! - [`spawn_event_pump`]：后台事件泵 + Langfuse tracer
//! - [`build_and_execute_agent_v2`]：v2 stages 装配与 ReAct 循环驱动（9 个 phase）
//! - [`collect_result`]：close channel + 等待 pump drain + recall 提取
//!
//! 所有 helper 标 `pub(super)`：仅在 `super::executor` 模块内部可见，
//! 由 `executor.rs` 通过 `use super::executor_helpers::*;` 引入到自身命名空间，
//! 以保持 `executor_test.rs` 的 `super::{intercept_immediate_command, InterceptRequest}`
//! 路径继续可解析。
//!
//! # Cancel 语义保持
//!
//! - `intercept_immediate_command` 内的 `tokio::select!` 分支顺序原样保留
//!   （`cmd.execute` 优先于 `cancel.cancelled()`；二者均会触发 `push_done`）
//! - `build_and_execute_agent_v2` 末尾的 cancel cascade 仍在循环失败后触发，
//!   `LoopResult::Error` 分支先发 `AgentExecutionFailed` 事件再判断 stop_reason，
//!   顺序与原实现一致
//! - `collect_result` 严格 "close → wait_for_pump(10s timeout) → drain recall"，
//!   顺序不变（pump 必须先 close sender 才能退出 recv 循环）

// 显式 `use super::{...}` 引入 executor.rs 中的共享类型（包括私有 struct——
// 子模块对父模块所有项均有可见性）。子模块无法通过 `use super::*` 继承父模块
// 的 use 语句（Rust `use` 默认私有），因此不在此使用 `use super::*`。
// 其余外部依赖（peri_agent / crate::*）单独 use。
use std::sync::Arc;

use peri_agent::{
    agent::{
        events::{
            AgentEventHandler, BackgroundTaskResult, ExecutorEvent, TurnErrorKind, TurnStatus,
        },
        state::AgentState,
        AgentCancellationToken,
    },
    error::AgentError,
    goal::GoalController,
    messages::{BaseMessage, MessageContent},
};
use peri_middlewares::{agent_define::AgentOverrides, subagent::BackgroundTaskRegistry};
use tokio::sync::oneshot;
use tracing::{debug, error};

use crate::{
    langfuse::LangfuseTracer,
    session::{agent_pool::CachedLlmInstances, event_sink::EventSink},
};

// 共享类型从 executor 模块（super）显式引入——这些类型在 executor.rs 中
// 是 `struct`（默认 private）但子模块对父模块所有项可见。
use super::{
    mark_permission_mode_notified, ExecOutcome, FrozenSessionData, ModeNoticeBooking, PromptResult,
    PromptStopReason,
};

// ── Intercept Request parameter object ─────────────────────────────────────

/// 命令拦截请求（参数对象，避免 12 个位置参数）。
pub(super) struct InterceptRequest<'a> {
    // ── 消息上下文 ──
    pub(super) content: &'a MessageContent,
    pub(super) history: &'a [BaseMessage],
    // ── 会话上下文 ──
    pub(super) cwd: &'a str,
    pub(super) session_id: &'a str,
    pub(super) cancel: &'a AgentCancellationToken,
    pub(super) thread_store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    pub(super) thread_id: Option<String>,
    // ── 运行时服务 ──
    pub(super) peri_config: &'a Arc<crate::provider::PeriConfig>,
    pub(super) event_sink: &'a Arc<dyn EventSink>,
    pub(super) auxiliary_model: &'a Option<Arc<dyn peri_model::Model>>,
    // ── 异步服务 ──
    pub(super) bg_event_tx: &'a tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    pub(super) bg_registry: &'a Arc<peri_middlewares::subagent::BackgroundTaskRegistry>,
    // ── 冻结数据 ──
    pub(super) frozen: Option<&'a FrozenSessionData>,
}

/// 命令拦截：检查 content 是否为 Immediate 类型 slash 命令。
///
/// 返回 `Some(PromptResult)` 表示已处理（agent 不构建）；
/// 返回 `None` 表示继续走 agent 管线。
///
/// [TRAP] Immediate 命令路径绕过 agent event pump，必须手动调用 `sink.push_done()`。
/// 否则 TUI 界面永久卡在 loading 状态（issue_2026-05-29-immediate-command-missing-push-done）。
pub(super) async fn intercept_immediate_command(req: InterceptRequest<'_>) -> Option<PromptResult> {
    let text = req.content.text_content();
    let stripped = text.strip_prefix('/')?;
    if stripped.is_empty() {
        return None;
    }

    let command_registry = crate::session::command::default_prompt_command_registry();
    let (cmd, args) = command_registry.find(&text)?;
    if cmd.kind() != crate::session::command::CommandKind::Immediate {
        // Passthrough/Transform → fall through to normal agent flow
        return None;
    }

    tracing::debug!(
        command = %cmd.name(),
        history_len = req.history.len(),
        "Immediate command intercepted"
    );
    let ctx = crate::session::command::CommandContext {
        session_id: req.session_id.to_string(),
        history: req.history.to_vec(),
        cwd: req.cwd.to_string(),
        peri_config: Arc::new(req.peri_config.as_ref().clone()),
        auxiliary_model: req.auxiliary_model.clone(),
        event_sink: req.event_sink.clone(),
        args: args.to_string(),
        cancel_token: req.cancel.clone(),
        thread_store: req.thread_store,
        thread_id: req.thread_id,
        bg_event_sender: Some(req.bg_event_tx.clone()),
        bg_registry: Some(req.bg_registry.clone()),
        frozen_claude_md: req
            .frozen
            .as_ref()
            .and_then(|f| f.claude_md().map(|s| Arc::new(s.to_string()))),
        frozen_claude_local_md: req
            .frozen
            .as_ref()
            .and_then(|f| f.claude_local_md().map(|s| Arc::new(s.to_string()))),
        frozen_skill_summary: req
            .frozen
            .as_ref()
            .and_then(|f| f.skill_summary().map(|s| Arc::new(s.to_string()))),
        frozen_system_prompt: req
            .frozen
            .as_ref()
            // fork/bg-fork 复用的冻结 prompt 用"无 16_workflow"版本
            //（P2-2026-08-02）：fork 链不注册 WorkflowTool。
            .map(|f| Arc::new(f.subagent_system_prompt().to_string())),
    };
    let result = tokio::select! {
        r = cmd.execute(ctx) => r,
        _ = req.cancel.cancelled() => {
            tracing::info!(session_id = %req.session_id, "Immediate command cancelled");
            crate::session::command::CommandResult {
                messages: req.history.to_vec(),
                stop_reason: PromptStopReason::Cancelled,
            }
        }
    };
    // Immediate 命令跳过 agent event pump，必须手动发送 push_done
    // 通知 TUI agent 执行完成，否则界面永久卡在 loading 状态。
    // 命令 turn 无 request_id（None）——TUI 侧跳过 id 配对、回退代际兜底。
    req.event_sink
        .push_done(req.session_id, "end_turn", None)
        .await;
    Some(PromptResult {
        messages: result.messages,
        ok: true,
        stop_reason: result.stop_reason,
        history_replaced_by_compaction: false,
        recall_items: Vec::new(),
    })
}

// ── Spawn Pump Request parameter object ─────────────────────────────────────

/// 事件泵启动请求（参数对象）。
pub(super) struct SpawnPumpRequest {
    pub(super) event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
    pub(super) stop_reason_rx: tokio::sync::oneshot::Receiver<super::PromptStopReason>,
    pub(super) sink: Arc<dyn EventSink>,
    pub(super) session_id: String,
    pub(super) effective_context_window: u32,
    pub(super) langfuse_tracer: Option<Arc<parking_lot::Mutex<LangfuseTracer>>>,
    pub(super) trace_input: String,
    /// 本轮 prompt 的 requestId——push_done 时透传回带（TUI stale 判定用）。
    pub(super) request_id: Option<String>,
}

/// 后台事件泵句柄，通过 oneshot channel 与 pump_done_rx 配对。
pub(super) struct PumpHandle {
    pub(super) pump_done_rx: oneshot::Receiver<()>,
}

/// 启动主事件泵任务。
///
/// 任务循环：
/// 1. trace_start → recv events → forward to sink
/// 2. trace_end + push_done → signal pump completion（在 Langfuse flush 之前）
/// 3. Langfuse flush（fire-and-forget，不得阻塞管线）
pub(super) fn spawn_event_pump(req: SpawnPumpRequest) -> PumpHandle {
    let SpawnPumpRequest {
        mut event_rx,
        stop_reason_rx,
        sink,
        session_id,
        effective_context_window,
        langfuse_tracer,
        trace_input,
        request_id,
    } = req;

    let (pump_done_tx, pump_done_rx) = oneshot::channel();

    if langfuse_tracer.is_some() {
        debug!(session_id = %session_id, "Langfuse tracer received for turn");
    }

    tokio::spawn(async move {
        // Start Langfuse trace
        if let Some(ref tracer) = langfuse_tracer {
            tracer.lock().on_turn_start(&trace_input);
        }

        let mut last_error: Option<String> = None;

        while let Some(exec_event) = event_rx.recv().await {
            // Capture error_kind from TurnEnded for on_turn_end at pump tail
            if let ExecutorEvent::TurnEnded { error_kind, .. } = &exec_event {
                last_error = error_kind.as_ref().map(|k| format!("{:?}", k));
            }

            // 4. bg callback: MessageAdded → TUI flush-then-push.
            //    agent ReAct 循环在消费 MQ Defer 消息时通过 EventBus 发出
            //    SyntheticUserMessage → mapper → ExecutorEvent::MessageAdded。
            //    TUI bridge 收到 BgCallbackBubble 后会先 flush current_turn 到
            //    committed，再 push bg callback，把同一轮 TurnDone 的 AI 内容
            //    分割为「bg 前」和「bg 后」两段。
            if matches!(&exec_event, ExecutorEvent::MessageAdded(_)) {
                if let ExecutorEvent::MessageAdded(msg) = &exec_event {
                    let text = msg.content();
                    sink.push_unstable_event(
                        &session_id,
                        "bg-callback-user-message".to_string(),
                        serde_json::json!({ "text": text }),
                    )
                    .await;
                }
            }

            sink.push_event(&session_id, &exec_event, effective_context_window)
                .await;
        }

        // End Langfuse trace and flush
        let langfuse_flush = if let Some(ref tracer) = langfuse_tracer {
            let handle = tracer.lock().on_turn_end(last_error.as_deref());
            Some(handle)
        } else {
            None
        };

        // Resolve stop_reason from the oneshot channel set by executor
        let stop_reason = stop_reason_rx
            .await
            .unwrap_or(super::PromptStopReason::EndTurn);
        let stop_reason_str = match stop_reason {
            super::PromptStopReason::EndTurn => "end_turn",
            super::PromptStopReason::Cancelled => "cancelled",
            super::PromptStopReason::MaxTurnRequests => "max_turn_requests",
        };
        sink.push_done(&session_id, stop_reason_str, request_id.as_deref())
            .await;

        // Signal pump completion BEFORE Langfuse flush.
        // Langfuse is telemetry — it must never block the execution pipeline.
        // Without this, a slow/unreachable Langfuse API blocks pump_done_tx,
        // which blocks wait_for_pump(), which blocks run_session_loop() from
        // returning, which holds the prompt_lock and prevents the next prompt
        // from starting. Ctrl+C can't recover because the new prompt's cancel
        // token hasn't been created yet (still waiting on the lock).
        let _ = pump_done_tx.send(());

        // Langfuse flush: fire-and-forget. The spawned task runs independently;
        // worst-case it blocks for ~150s (HTTP 30s × 3 retries + backoff) then
        // logs warnings. The pump has already signaled completion above, so this
        // never blocks the execution pipeline.
        drop(langfuse_flush);
    });

    PumpHandle { pump_done_rx }
}

// ── Collect Result Request parameter object ─────────────────────────────────

/// 结果收集请求（参数对象）。
pub(super) struct CollectRequest<'a> {
    pub(super) event_tx:
        &'a Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
    pub(super) pump_handle: PumpHandle,
    pub(super) session_id: &'a str,
    pub(super) exec_outcome: ExecOutcome,
}

/// 最终结果收集：close channel → 等待 pump drain → 提取 recall items。
///
/// 顺序约束：必须先 close event_tx，pump 才能退出 recv 循环；然后等待 pump_done。
pub(super) async fn collect_result(req: CollectRequest<'_>) -> PromptResult {
    let CollectRequest {
        event_tx,
        pump_handle,
        session_id,
        mut exec_outcome,
    } = req;

    close_channel(event_tx);
    wait_for_pump(pump_handle.pump_done_rx, session_id).await;

    let recall_items = exec_outcome.agent_state.drain_recall();
    PromptResult {
        messages: exec_outcome.agent_state.into_messages(),
        ok: exec_outcome.ok,
        stop_reason: exec_outcome.stop_reason,
        history_replaced_by_compaction: exec_outcome.history_replaced_by_compaction,
        recall_items,
    }
}

pub(super) fn close_channel(
    event_tx: &Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
) {
    let mut tx_guard = event_tx.lock();
    *tx_guard = None;
}

pub(super) async fn wait_for_pump(pump_done_rx: oneshot::Receiver<()>, session_id: &str) {
    match tokio::time::timeout(std::time::Duration::from_secs(10), pump_done_rx).await {
        Ok(Ok(())) => debug!(session_id, "Event pump done"),
        Ok(Err(_)) => error!(session_id, "Event pump done channel closed unexpectedly"),
        Err(_) => error!(
            session_id,
            "Event pump timed out (10s) — Langfuse flush may have blocked push_done"
        ),
    }
}

// ── v2 stages 装配与 ReAct 循环驱动 ────────────────────────────────────────

/// 通过 [`crate::agent::builder::build_stage_context`] 构造 StageContext，
/// 再由 [`peri_agent::agent::stages::run_react_loop`] 驱动循环（P5 后的单一执行路径）。
///
/// 关键设计：
/// - LLM/middleware 装配由 `build_agent` 完成（构造 `AgentComponents`）
/// - 工具执行由 `stages/tool_dispatch` 完成（每轮从 `shared_tools` 取）
/// - 事件出口：v2 stages 通过 EventBus emit 三层事件（Render/State/Observe），
///   本函数 spawn forwarder 将其映射为 `ExecutorEvent`，复用 event_tx / pump 管线
/// - 历史消息：seed 到 transcript；用户输入：作为 Prompt push 到 v2 queue
///
/// 调用前已完成副作用（register/deregister、event_handler、
/// workflow 消费者 spawn、goal_controller）。所有副作用与 v1 一致。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub(super) async fn build_and_execute_agent_v2(
    ctx: &super::SessionContext,
    cached_llm: Option<&CachedLlmInstances>,
    agent_input: peri_agent::agent::react::AgentInput,
    history: Vec<BaseMessage>,
    event_tx: &Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
    event_sink: &Arc<dyn EventSink>,
    langfuse_tracer: Option<Arc<parking_lot::Mutex<LangfuseTracer>>>,
    // ── AAC-only ──
    system_prompt: String,
    // 子 agent / fork 复用的冻结 prompt（无 16_workflow，P2-2026-08-02）。
    // None = 调用方未提供（防御性回退到 system_prompt）。
    subagent_system_prompt: Option<String>,
    // D2：mode 通知的入队记账委托（None = 无通知或无需记账）。
    // 仅在 Phase 6 把消息推入模型可见 v2 MessageQueue 时记账。
    mode_notice_booking: Option<ModeNoticeBooking>,
    frozen: crate::agent::builder::FrozenData,
    event_handler: Arc<dyn AgentEventHandler>,
    agent_overrides: Option<AgentOverrides>,
    preload_skills: Vec<String>,
    child_handler_factory: Option<crate::agent::builder::ChildHandlerFactory>,
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    thread_persistence: crate::agent::builder::ThreadPersistence,
    goal_controller: Option<Arc<dyn GoalController>>,
    background_registry: Option<Arc<BackgroundTaskRegistry>>,
    on_bg_complete: Option<
        Arc<dyn Fn(&BackgroundTaskResult, peri_middlewares::subagent::BgTaskKind) + Send + Sync>,
    >,
    effective_context_window: u32,
    // 内部异步续跑：不 push 空 user Prompt（AsyncContinuation 只消费已 route
    // 的 Defer/Info 消息），mode 通知记账同样跳过（下一 turn 重新检测）。
    continuation: bool,
) -> ExecOutcome {
    use peri_agent::agent::stages::{run_react_loop, LoopResult};
    use peri_agent::session::queue::{
        MessageKind, MessageSource as V2MessageSource, QueuedMessage,
    };

    // Phase 1: build StageContext（内部消费 AgentComponents）
    let (v2_out, new_cache) = crate::agent::builder::build_stage_context(
        ctx,
        cached_llm,
        system_prompt,
        subagent_system_prompt,
        frozen,
        event_handler,
        agent_overrides,
        preload_skills,
        child_handler_factory,
        auxiliary_model,
        thread_persistence,
        goal_controller,
        background_registry,
        on_bg_complete,
        langfuse_tracer.clone(),
    );
    if let Some(cache) = new_cache {
        ctx.pool.lock().store_llm(cache);
    }

    // Phase 2: bg event pump（复用 V2AgentOutput.bg_event_rx）
    {
        let mut bg_event_rx = v2_out.bg_event_rx;
        let bg_session_id = ctx.session_id.clone();
        let bg_sink = Arc::clone(event_sink);
        let bg_cw = effective_context_window;
        tokio::spawn(async move {
            let mut bg_event_count: u64 = 0;
            while let Some(bg_event) = bg_event_rx.recv().await {
                bg_event_count += 1;
                bg_sink.push_event(&bg_session_id, &bg_event, bg_cw).await;
            }
            tracing::debug!(
                total = bg_event_count,
                "bg-event-pump: all senders dropped, exiting"
            );
        });
    }

    // Phase 3: todo forwarder（同 v1，复用 V2AgentOutput.todo_rx）
    {
        let mut todo_rx = v2_out.todo_rx;
        let tx_for_todo = event_tx.clone();
        tokio::spawn(async move {
            while let Some(todos) = todo_rx.recv().await {
                let entries: Vec<peri_agent::agent::events::TodoEntry> = todos
                    .into_iter()
                    .map(|t| peri_agent::agent::events::TodoEntry {
                        content: t.content,
                        active_form: t.active_form,
                        status: match t.status {
                            peri_middlewares::tools::todo::TodoStatus::Pending => {
                                peri_agent::agent::events::TodoStatus::Pending
                            }
                            peri_middlewares::tools::todo::TodoStatus::InProgress => {
                                peri_agent::agent::events::TodoStatus::InProgress
                            }
                            peri_middlewares::tools::todo::TodoStatus::Completed => {
                                peri_agent::agent::events::TodoStatus::Completed
                            }
                        },
                    })
                    .collect();
                if let Some(tx) = tx_for_todo.lock().as_ref() {
                    let _ = tx.send(ExecutorEvent::TodoUpdate(entries));
                }
            }
        });
    }

    // Phase 4: EventBus forwarder（v2 → v1 ExecutorEvent）
    // 通过 tokio::select! 同时排空 render / state / observe 三层通道，
    // 将 v2 事件经 mapper_v2 映射为 v1 ExecutorEvent，转发到 event_tx。
    //
    // 注意：不直接 push 到 event_sink —— spawn_event_pump 已订阅 event_tx 并
    // 负责推送 sink（含 Langfuse trace + pump_done 同步）。直推会造成 TUI 双重渲染。
    //
    // [TRAP] TurnCompleted 在 render_tx 通道（与同迭代 TextChunk/ToolStarted/
    // ToolEnded 共享 FIFO），不能放回 state_tx：跨通道 biased select! 只保证
    // 单次迭代内的优先级，不保证跨迭代——iter2 的 TextChunk 会先于 iter1 的
    // TurnCompleted 被消费，污染 partial，渲染出"新文本在旧工具之前"的错乱。
    //
    // 循环实现抽取至 `crate::event::forwarder::spawn_eventbus_forwarder`，
    // 以保证 biased select 顺序不变量与 workflow_agent 调用点一致。
    {
        let tx_for_v2 = event_tx.clone();
        let bridge = langfuse_tracer.clone().map(|t| {
            crate::langfuse::bridge::LangfuseBridge::new(t, ctx.provider.display_name().to_string())
        });
        crate::event::spawn_eventbus_forwarder(
            v2_out.event_handles,
            move |exec_ev| {
                if let Some(tx) = tx_for_v2.lock().as_ref() {
                    let _ = tx.send(exec_ev);
                }
            },
            bridge,
            ctx.v2_event_tx.clone(),
        );
    }

    // Phase 5: seed transcript（history 作为 ancestor 之外的自有消息）
    {
        let transcript_arc = v2_out.session.transcript();
        let mut transcript = transcript_arc.write();
        transcript.append_batch(history);
    }

    // Phase 5.5: restore compact flags from persistence (if available)
    {
        if let (Some(store), Some(tid)) = (ctx.thread_store.as_ref(), ctx.thread_id.as_ref()) {
            match store.load_message_flags(tid).await {
                Ok(flags) if !flags.is_empty() => {
                    let transcript_arc = v2_out.session.transcript();
                    let mut transcript = transcript_arc.write();
                    transcript.set_flags_batch(flags);
                    tracing::debug!(
                        thread_id = %tid,
                        "Phase 5.5: restored compact flags from persistence"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(
                        thread_id = %tid,
                        error = %e,
                        "Phase 5.5: failed to load compact flags"
                    );
                }
            }
        }
    }

    // Phase 6: push 用户输入到 v2 queue（Receive 阶段消费）
    // [AsyncContinuation] continuation 内部续跑不 push 空 user prompt——
    // 空 human 不进入 transcript（保持 keepgoing 的"不写入空 human"约束由
    // 显式分支承担，而非复用 keepgoing 语义）；loop 仅消费已 route 的
    // Defer/Info 消息（bg 结果、workflow 完成等）。
    // [P3/D2] 记账点：通知文本随本条消息推入模型可见的 v2 MessageQueue 后，
    // 才标记"已通知该 mode"。入队前失败/取消不记账——下一 turn 重新检测仍会
    // 生成通知（可重复重试，恰好可见一次）；已入队的消息由 Receive drain_all
    // 消费进 transcript，不会重复注入也不会丢失。
    if !continuation {
        v2_out.context.session.queue.push(QueuedMessage::new(
            MessageKind::Prompt,
            V2MessageSource::UserInput,
            BaseMessage::human(agent_input.content),
        ));
        if let Some(booking) = &mode_notice_booking {
            mark_permission_mode_notified(&booking.last_notified, booking.mode);
        }
    }

    // Phase 6.5: clone recall_buffer 的 Arc，便于 Phase 8.5 在 context 被
    // run_react_loop 消费后仍可访问累积的 recall。
    let recall_buffer = Arc::clone(&v2_out.context.recall_buffer);

    // Phase 7: 运行 v2 ReAct 循环（max_iterations 与 v1 一致 = 500）
    // langfuse v2: capture turn_id before move, emit TurnStarted
    let loop_turn_id = v2_out.context.turn_id().to_string();
    {
        if let Some(tx) = event_tx.lock().as_ref() {
            let _ = tx.send(ExecutorEvent::TurnStarted {
                turn_id: loop_turn_id.clone(),
                session_id: ctx.session_id.clone(),
            });
        }
    }
    let loop_result = run_react_loop(v2_out.context, 500).await;

    // Phase 8: 从 transcript 提取最终消息列表，构造 AgentState（兼容下游 PromptResult）
    let (messages, history_replaced_by_compaction) = {
        let transcript = v2_out.session.transcript();
        let transcript = transcript.read();
        let messages: Vec<BaseMessage> =
            transcript.visible_messages().into_iter().cloned().collect();
        (messages, transcript.full_compaction_committed())
    };
    let mut agent_state = AgentState::with_messages(ctx.cwd.clone(), messages);
    agent_state.set_context("session_id", &ctx.session_id);
    agent_state.set_context("run_id", uuid::Uuid::now_v7().to_string());

    // Phase 8.5: 把 v2 recall_buffer（middleware hook 期间累积）灌入 agent_state。
    // 下游 collect_result() 调用 agent_state.drain_recall() 取出 recall_items，
    // 必须先迁移到 agent_state 才能复用 v1 的 drain 路径。
    //
    // v2 路径下 middleware hook 在临时 AgentState 上 push_recall（见
    // middleware_runner::restore_from_agent_state），restore 时 drain 到
    // StageContext.recall_buffer；循环结束后（context 已被 run_react_loop
    // 消费）从 Phase 6.5 clone 的 Arc 取回累积的 recall。
    {
        let recalls: Vec<String> = recall_buffer.write().drain(..).collect();
        for r in recalls {
            agent_state.push_recall(r);
        }
    }

    // Phase 9: 映射 LoopResult → ExecOutcome
    let (ok, stop_reason) = match loop_result {
        LoopResult::Completed => (true, PromptStopReason::EndTurn),
        LoopResult::Interrupted => (false, PromptStopReason::Cancelled),
        LoopResult::Error(ref e) => {
            error!(session_id = %ctx.session_id, error = %e, "[v2] loop failed");
            // 对非 Interrupted/MaxIterations 的致命错误，通知 TUI 显示红色错误提示
            // issue: spec/issues/2026-07-22-llm-api-error-silently-swallowed-in-tui.md
            if !matches!(e, AgentError::Interrupted)
                && !matches!(e, AgentError::MaxIterationsExceeded(_))
                && !ctx.cancel.is_cancelled()
            {
                if let Some(tx) = event_tx.lock().as_ref() {
                    let _ = tx.send(ExecutorEvent::AgentExecutionFailed {
                        message: e.user_facing_message(),
                    });
                }
            }
            let reason = if ctx.cancel.is_cancelled() || matches!(e, AgentError::Interrupted) {
                PromptStopReason::Cancelled
            } else if matches!(e, AgentError::MaxIterationsExceeded(_)) {
                PromptStopReason::MaxTurnRequests
            } else {
                PromptStopReason::EndTurn
            };
            (false, reason)
        }
    };

    // langfuse v2: emit TurnEnded
    {
        let (status, error_kind) = match loop_result {
            LoopResult::Completed => (TurnStatus::Done, None),
            LoopResult::Interrupted => (TurnStatus::Interrupted, Some(TurnErrorKind::Interrupted)),
            LoopResult::Error(ref e) => {
                let kind = if matches!(e, AgentError::Interrupted) {
                    TurnErrorKind::Interrupted
                } else if matches!(e, AgentError::MaxIterationsExceeded(_)) {
                    TurnErrorKind::MaxIterations
                } else {
                    TurnErrorKind::LlmFailure
                };
                (TurnStatus::Error, Some(kind))
            }
        };
        if let Some(tx) = event_tx.lock().as_ref() {
            let _ = tx.send(ExecutorEvent::TurnEnded {
                turn_id: loop_turn_id,
                session_id: ctx.session_id.clone(),
                status,
                error_kind,
            });
        }
    }

    // Cancel cascade children when this agent is cancelled
    if stop_reason == PromptStopReason::Cancelled {
        if let Some(ref sm) = ctx.session_manager {
            if let Some(session) = sm.get_session(&ctx.session_id) {
                session.cancel_cascade_children();
            }
        }
    }

    ExecOutcome {
        ok,
        stop_reason,
        history_replaced_by_compaction,
        agent_state,
    }
}
