use std::sync::Arc;

use peri_acp_types::{
    command::PromptStopReason,
    error::AgentError,
    event::{
        AgentEventHandler, BackgroundTaskResult, EventPublisher, ExecutorEvent, TurnErrorKind,
        TurnStatus,
    },
    event_v2::EventHandles,
    frozen::{ChildHandlerFactory, ThreadPersistence},
    goal::GoalController,
    identity::EventDeliveryClass,
    messages::BaseMessage,
    runtime::UnstampedEvent,
    session::ExecutionFailure,
    store::ThreadStore,
    tasks::{BgTaskKind, TaskManager},
};
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::agent::{
    agent_context::AgentContext,
    async_tasks::TaskManager as AgentTaskManager,
    react::AgentInput,
    stages::{run_react_loop, LoopResult},
    state::AgentState,
};
use crate::session::exec::executor::FrozenSessionData;
use crate::session::exec::stage_builder::{CachedLlmInstances, V2AgentOutput};
use crate::session::MessageTranscript;

use super::ExecOutcome;

/// EventBus forwarder 启动器闭包（ACP 侧持有 Langfuse bridge 构造；
/// 参数 = event_handles / 主 agent_id / 事件消费闭包）。
pub type ForwarderLauncherFn = Arc<
    dyn Fn(
            EventHandles,
            String,
            Box<dyn Fn(UnstampedEvent, ExecutorEvent) + Send + Sync>,
        ) -> tokio::task::JoinHandle<()>
        + Send
        + Sync,
>;

// ── v2 stages 装配与 ReAct 循环驱动 ────────────────────────────────────────

/// stage 装配请求（注入 `StageBuildFn` 的输入；L5：自原
/// `build_and_execute_agent_v2` 的 stage 相关参数打包）。
///
/// `langfuse_tracer` 不进入本结构——stage 构建的 Langfuse bridge 由 ACP
/// 装配面闭包捕获（`StageBuildInput::langfuse_bridge_factory` 注入点）。
#[allow(clippy::type_complexity)]
pub struct StageBuildRequest {
    pub cached_llm: Option<CachedLlmInstances>,
    pub frozen_session: FrozenSessionData,
    pub event_handler: Arc<dyn AgentEventHandler>,
    pub agent_overrides: Option<peri_acp_types::agents::AgentOverrides>,
    pub preload_skills: Vec<String>,
    pub child_handler_factory: Option<ChildHandlerFactory>,
    pub auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    pub thread_persistence: ThreadPersistence,
    pub goal_controller: Option<Arc<dyn GoalController>>,
    pub task_manager: Option<Arc<AgentTaskManager>>,
    pub on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
}

/// stage 装配注入面（ACP 侧从 `SessionContext` 投影 `StageBuildInput` 并补齐
/// LLM 构造 / 渲染 / 观测注入后调用 stage 装配本体）。
pub type StageBuildFn = Arc<
    dyn Fn(
            StageBuildRequest,
        ) -> Result<
            (V2AgentOutput, Option<CachedLlmInstances>),
            crate::session::exec::stage_builder::StageBuildError,
        > + Send
        + Sync,
>;

/// v2 执行请求（L5：原 `build_and_execute_agent_v2` 22 参数对象化；
/// ACP 特有构造全部经注入面接入，本模块只消费契约化输入）。
#[allow(clippy::type_complexity)]
pub struct V2ExecuteRequest {
    // ── 会话数据 ──
    pub session_id: String,
    pub cwd: String,
    pub cancel: CancellationToken,
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    pub thread_id: Option<String>,
    pub agent_input: AgentInput,
    pub history: Vec<BaseMessage>,
    pub cached_llm: Option<CachedLlmInstances>,
    pub task_manager: Option<Arc<dyn TaskManager>>,
    pub continuation: bool,
    // ── stage 装配输入（透传 StageBuildRequest）──
    pub frozen_session: FrozenSessionData,
    pub event_handler: Arc<dyn AgentEventHandler>,
    pub agent_overrides: Option<peri_acp_types::agents::AgentOverrides>,
    pub preload_skills: Vec<String>,
    pub child_handler_factory: Option<ChildHandlerFactory>,
    pub auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    pub thread_persistence: ThreadPersistence,
    pub goal_controller: Option<Arc<dyn GoalController>>,
    pub on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    // ── 注入面（L5 依赖反转）──
    /// 事件发射端口（ACP/Controller 适配层；Phase 2/3/4/7/9 统一发射点）。
    pub publisher: Arc<dyn EventPublisher>,
    /// stage 装配注入面（ACP 侧投影 `StageBuildInput` + 补齐注入）。
    pub stage_build: StageBuildFn,
    /// LLM 缓存回写（ACP 侧 AgentPool；Phase 1 装配产物入池）。
    pub store_llm: Arc<dyn Fn(CachedLlmInstances) + Send + Sync>,
    /// cancel cascade 子 agent（ACP 侧 SessionManager；循环失败后触发）。
    pub cancel_cascade: Arc<dyn Fn(&str) + Send + Sync>,
    /// EventBus forwarder 启动器（ACP 侧持有 Langfuse bridge 构造）。
    pub forwarder_launcher: ForwarderLauncherFn,
}

/// 通过注入的 stage 装配面构造 StageContext，再由 [`run_react_loop`] 驱动循环
/// （P5 后的单一执行路径）。
///
/// 关键设计：
/// - LLM/middleware 装配由 ACP 注入面完成（构造 `AgentComponents` → `StageContext`）
/// - 工具执行由 `stages/tool_dispatch` 完成（每轮从 `shared_tools` 取）
/// - 事件出口：v2 stages 通过 EventBus emit 三层事件（Render/State/Observe），
///   本函数经注入的 forwarder 启动器将其映射为 `ExecutorEvent` 并统一发射，
///   复用 event_tx / pump 管线
/// - 历史消息：seed 到 transcript；用户输入：作为 Prompt push 到 v2 queue
///
/// 调用前已完成副作用（register/deregister、event_handler、
/// workflow 消费者 spawn、goal_controller）。所有副作用与 v1 一致。
pub async fn build_and_execute_agent_v2(req: V2ExecuteRequest) -> ExecOutcome {
    use peri_acp_types::session::{MessageKind, MessageSource as V2MessageSource, QueuedMessage};

    // Phase 1: build StageContext（内部消费 AgentComponents）
    let concrete_tm: Option<Arc<AgentTaskManager>> = req.task_manager.clone().and_then(|tm| {
        let tm_any: Arc<dyn std::any::Any + Send + Sync> =
            tm as Arc<dyn std::any::Any + Send + Sync>;
        tm_any.downcast::<AgentTaskManager>().ok()
    });
    let (v2_out, new_cache) = match (req.stage_build)(StageBuildRequest {
        cached_llm: req.cached_llm,
        frozen_session: req.frozen_session,
        event_handler: req.event_handler,
        agent_overrides: req.agent_overrides,
        preload_skills: req.preload_skills,
        child_handler_factory: req.child_handler_factory,
        auxiliary_model: req.auxiliary_model,
        thread_persistence: req.thread_persistence,
        goal_controller: req.goal_controller,
        task_manager: concrete_tm,
        on_bg_complete: req.on_bg_complete,
    }) {
        Ok(output) => output,
        Err(error) => {
            error!(session_id = %req.session_id, error = %error, "[v2] stage build failed");
            let failure = ExecutionFailure::internal("Agent stage initialization failed");
            let source = UnstampedEvent::new(
                String::new(),
                String::new(),
                None,
                EventDeliveryClass::Critical,
            );
            req.publisher.publish_event(
                &req.session_id,
                &source,
                ExecutorEvent::AgentExecutionFailed {
                    message: failure.public_message.clone(),
                },
            );
            return ExecOutcome {
                ok: false,
                stop_reason: PromptStopReason::EndTurn,
                failure: Some(failure),
                history_replaced_by_compaction: false,
                agent_state: AgentState::new(&req.cwd),
            };
        }
    };
    if let Some(cache) = new_cache {
        (req.store_llm)(cache);
    }

    // Phase 2: bg event pump（复用 V2AgentOutput.bg_event_rx）
    // 事件三层化：发射点统一经 `EventPublisher`（身份：v2 循环 turn_id / 主
    // agent_id——bg 子 agent 事件归属当前 turn），消费端为主 pump（已在
    // run_session_loop 中订阅，按 session_id 过滤推送）。
    {
        let mut bg_event_rx = v2_out.bg_event_rx;
        let publisher = Arc::clone(&req.publisher);
        let bg_session_id = req.session_id.clone();
        let bg_turn_id = v2_out.context.turn_id().to_string();
        let bg_agent_id = v2_out.context.session.agent_id.to_string();
        tokio::spawn(async move {
            let mut bg_event_count: u64 = 0;
            while let Some(bg_event) = bg_event_rx.recv().await {
                bg_event_count += 1;
                let source = UnstampedEvent::new(
                    bg_turn_id.clone(),
                    bg_agent_id.clone(),
                    None,
                    EventDeliveryClass::Critical,
                );
                publisher.publish_event(&bg_session_id, &source, bg_event);
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
        let publisher = Arc::clone(&req.publisher);
        let sid = req.session_id.clone();
        tokio::spawn(async move {
            while let Some(todos) = todo_rx.recv().await {
                let entries: Vec<peri_acp_types::event::TodoEntry> = todos
                    .into_iter()
                    .map(|t| peri_acp_types::event::TodoEntry {
                        content: t.content,
                        active_form: t.active_form,
                        status: match t.status {
                            peri_acp_types::tools::TodoStatus::Pending => {
                                peri_acp_types::event::TodoStatus::Pending
                            }
                            peri_acp_types::tools::TodoStatus::InProgress => {
                                peri_acp_types::event::TodoStatus::InProgress
                            }
                            peri_acp_types::tools::TodoStatus::Completed => {
                                peri_acp_types::event::TodoStatus::Completed
                            }
                        },
                    })
                    .collect();
                // todo 更新是事件流的一部分，发射点统一经 EventPublisher
                let source = UnstampedEvent::new(
                    String::new(),
                    String::new(),
                    None,
                    EventDeliveryClass::Critical,
                );
                publisher.publish_event(&sid, &source, ExecutorEvent::TodoUpdate(entries));
            }
        });
    }

    // Phase 4: EventBus forwarder（v2 → ExecutorEvent）
    // 通过 tokio::select! 同时排空 render / state / observe 三层通道，
    // 将 v2 事件经 mapper_v2 映射为 ExecutorEvent，经注入的 launcher 启动
    // 转发任务（biased select 顺序不变量单点保持在 ACP 侧 forwarder）。
    // 注意：不直接 push 到 event_sink —— spawn_event_pump 已订阅事件流并
    // 负责推送 sink（含 push_done 同步）。直推会造成 TUI 双重渲染。
    //
    // [TRAP] TurnCompleted 在 render_tx 通道（与同迭代 TextChunk/ToolStarted/
    // ToolEnded 共享 FIFO），不能放回 state_tx：跨通道 biased select! 只保证
    // 单次迭代内的优先级，不保证跨迭代——iter2 的 TextChunk 会先于 iter1 的
    // TurnCompleted 被消费，污染 partial，渲染出"新文本在旧工具之前"的错乱。
    let forwarder_handle = {
        let publisher = Arc::clone(&req.publisher);
        let sid = req.session_id.clone();
        let agent_id = v2_out.context.session.agent_id.to_string();
        (req.forwarder_launcher)(
            v2_out.event_handles,
            agent_id,
            Box::new(move |source, exec_ev| {
                publisher.publish_event(&sid, &source, exec_ev);
            }),
        )
    };

    // Phase 5: seed transcript（history 作为 ancestor 之外的自有消息）
    // 首轮用户 turn 判定需在 history move 前捕获（Phase 5.9 使用）。
    let is_first_user_turn = !req.continuation && req.history.is_empty();
    {
        let transcript_arc = v2_out.session.transcript();
        let mut transcript = transcript_arc.write();
        transcript.append_batch(req.history);
    }

    // Phase 5.5: restore compact flags from persistence (if available)
    {
        if let (Some(store), Some(tid)) = (req.thread_store.as_ref(), req.thread_id.as_ref()) {
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
    if !req.continuation {
        v2_out.context.session.queue.push(QueuedMessage::new(
            MessageKind::Prompt,
            V2MessageSource::UserInput,
            BaseMessage::human(req.agent_input.content),
        ));

        // Phase 6.2: 首轮用户 turn 的一次性受控通知（MCP 概览等）。
        // 仅在首个模型可见 turn（history 为空且非 continuation）触发：收集
        // middleware chain 的 `first_turn_reminder` 非空贡献，作为 Info 消息
        // （`<system-reminder>` 包裹，见 append_messages_to_transcript）在用户
        // Prompt **之后**入队——Receive drain 顺序为 user 输入在前、reminder
        // 在后（"加入到 user prompt"语义，不抢在用户输入前）。
        // 纯生成无记账：入队前失败/取消无副作用，下个首 turn 重新生成。
        if is_first_user_turn {
            let mut cx = AgentContext::from_stage(&v2_out.context);
            match v2_out
                .context
                .runtime
                .middleware_chain
                .run_first_turn_reminders(&mut cx)
                .await
            {
                Ok(reminders) if !reminders.is_empty() => {
                    for text in reminders {
                        v2_out.context.session.queue.push(QueuedMessage::new(
                            MessageKind::Info,
                            V2MessageSource::SystemInjected,
                            BaseMessage::human(text),
                        ));
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "[v2] first_turn_reminder hooks failed");
                }
            }
        }
    }

    // Phase 6.5: clone recall_buffer 的 Arc，便于 Phase 8.5 在 context 被
    // run_react_loop 消费后仍可访问累积的 recall。
    let recall_buffer = Arc::clone(&v2_out.context.recall_buffer);

    // Phase 7: 运行 v2 ReAct 循环（max_iterations 与 v1 一致 = 500）
    // langfuse v2: capture turn_id before move, emit TurnStarted
    let loop_turn_id = v2_out.context.turn_id().to_string();
    {
        // TurnStarted 是事件流的一部分，发射点统一经 EventPublisher
        // （v1 直发路径的身份：turn_id 取自 v2 循环，agent_id 为空降级）。
        let source = UnstampedEvent::new(
            loop_turn_id.clone(),
            String::new(),
            None,
            EventDeliveryClass::Critical,
        );
        req.publisher.publish_event(
            &req.session_id,
            &source,
            ExecutorEvent::TurnStarted {
                turn_id: loop_turn_id.clone(),
                session_id: req.session_id.clone(),
            },
        );
    }
    let loop_result = run_react_loop(v2_out.context, 500).await;

    // `run_react_loop` consumed StageContext, so all root EventBus senders have
    // been dropped. Wait for the forwarder to drain Observe/Render/State before
    // terminal classification can make AgentDone/TurnDone visible downstream.
    let forwarder_failure = match forwarder_handle.await {
        Ok(()) => None,
        Err(join_error) => {
            error!(session_id = %req.session_id, error = %join_error, "[v2] event forwarder failed");
            Some(ExecutionFailure::internal("Agent event forwarding failed"))
        }
    };

    // Phase 8: 从 transcript 提取最终消息列表，构造 AgentState（兼容下游 PromptResult）
    // 前置：显式 flush 剩余积压，确保最终回答已落库。Drop 层 Shutdown 优雅关闭是
    // 根因兜底（覆盖全部 6 个 run_react_loop 调用方），此处是主路径双保险——
    // 让会话恢复方在 turn 结束即可读到完整历史，不依赖 drop 时序。失败不阻断
    // 内存路径（后续 Drop 仍会尝试 flush）。
    // [SAFE] 先在 guard 作用域内同步提取 Send 的 writer 通道句柄（guard 语句结束即
    // drop，不跨 await），再经关联函数 `flush_via_tx` 异步等待 barrier——调用链
    // future 不持有 parking_lot guard，保持 Send（peri-tui 在 tokio::spawn 中调用本链）。
    let flush_tx = v2_out.session.transcript().read().persist_tx_handle();
    if let Some(tx) = flush_tx {
        if let Err(e) = MessageTranscript::flush_via_tx(&tx).await {
            tracing::warn!(session_id = %req.session_id, error = %e, "[v2] phase 8 transcript flush failed");
        }
    }
    let (messages, history_replaced_by_compaction) = {
        let transcript = v2_out.session.transcript();
        let transcript = transcript.read();
        let messages: Vec<BaseMessage> =
            transcript.visible_messages().into_iter().cloned().collect();
        (messages, transcript.full_compaction_committed())
    };
    let mut agent_state = AgentState::with_messages(req.cwd.clone(), messages);
    agent_state.set_context("session_id", &req.session_id);
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

    // Phase 9: 映射 LoopResult → ExecOutcome。cancel 在 transcript flush 之后
    // 只采样一次，后续 failure / TurnEnded / cascade / outcome 共用同一分类。
    let sampled_cancel = req.cancel.is_cancelled();
    let terminal = match forwarder_failure {
        Some(failure) => internal_failure_terminal(failure),
        None => classify_loop_terminal(&loop_result, sampled_cancel),
    };
    // 诊断日志与 wire 使用同一安全投影：保留错误原意和 HTTP status，但不得把
    // provider body 中的凭据或完整 cause chain 写入日志。
    if let Some(failure) = &terminal.failure {
        error!(
            session_id = %req.session_id,
            kind = failure.kind.wire_name(),
            http_status = failure.http_status,
            message = %failure.public_message,
            "[v2] execution failed"
        );
    }
    // 对非 Interrupted/MaxIterations/cancel 的致命错误，通知 TUI 显示红色错误提示
    // issue: spec/issues/2026-07-22-llm-api-error-silently-swallowed-in-tui.md
    // （与 failure 共享同一 fatal 判定：fatal ↔ 发射；message 同一来源）
    if let Some(f) = &terminal.failure {
        // 发射点统一经 EventPublisher
        let source = UnstampedEvent::new(
            String::new(),
            String::new(),
            None,
            EventDeliveryClass::Critical,
        );
        req.publisher.publish_event(
            &req.session_id,
            &source,
            ExecutorEvent::AgentExecutionFailed {
                message: f.public_message.clone(),
            },
        );
    }

    // langfuse v2: emit TurnEnded
    {
        // 发射点统一经 EventPublisher（TurnEnded 是事件流终末事件）
        let source = UnstampedEvent::new(
            loop_turn_id.clone(),
            String::new(),
            None,
            EventDeliveryClass::Critical,
        );
        req.publisher.publish_event(
            &req.session_id,
            &source,
            ExecutorEvent::TurnEnded {
                turn_id: loop_turn_id,
                session_id: req.session_id.clone(),
                status: terminal.turn_status,
                error_kind: terminal.turn_error_kind,
            },
        );
    }

    // Cancel cascade children when this agent is cancelled
    if terminal.stop_reason == PromptStopReason::Cancelled {
        (req.cancel_cascade)(&req.session_id);
    }

    ExecOutcome {
        ok: terminal.ok,
        stop_reason: terminal.stop_reason,
        failure: terminal.failure,
        history_replaced_by_compaction,
        agent_state,
    }
}

/// Phase 9 的单一终态分类结果。
pub(crate) struct LoopTerminal {
    pub(crate) ok: bool,
    pub(crate) stop_reason: PromptStopReason,
    pub(crate) failure: Option<ExecutionFailure>,
    pub(crate) turn_status: TurnStatus,
    pub(crate) turn_error_kind: Option<TurnErrorKind>,
}

fn internal_failure_terminal(failure: ExecutionFailure) -> LoopTerminal {
    LoopTerminal {
        ok: false,
        stop_reason: PromptStopReason::EndTurn,
        failure: Some(failure),
        turn_status: TurnStatus::Error,
        turn_error_kind: Some(TurnErrorKind::Internal),
    }
}

/// Phase 9 纯分类器：一次同时决定 Prompt、Turn 和 fatal failure。
///
/// 分类契约（spec/issues/2026-08-18-acp-error-handler.md Commit 1）：
/// - `Completed` / 用户 cancel / `Interrupted` / `MaxIterationsExceeded` →
///   failure 为 `None`（它们已有标准 `StopReason` 表达，不应升级为请求失败）；
/// - 其他 `LoopResult::Error` → failure 为安全的窄投影：LLM 错误保留脱敏、
///   限长后的原始含义和可选 HTTP status，其他错误使用安全用户文案。
///
/// fatal 判定与 `AgentExecutionFailed` 发射条件共享（fatal ↔ 发射私有事件），
/// public message 由 [`ExecutionFailure::from_agent_error`] 统一生成。
pub(crate) fn classify_loop_terminal(
    loop_result: &LoopResult,
    sampled_cancel: bool,
) -> LoopTerminal {
    if matches!(loop_result, LoopResult::Completed) {
        return LoopTerminal {
            ok: true,
            stop_reason: PromptStopReason::EndTurn,
            failure: None,
            turn_status: TurnStatus::Done,
            turn_error_kind: None,
        };
    }
    if sampled_cancel
        || matches!(
            loop_result,
            LoopResult::Interrupted | LoopResult::Error(AgentError::Interrupted)
        )
    {
        return LoopTerminal {
            ok: false,
            stop_reason: PromptStopReason::Cancelled,
            failure: None,
            turn_status: TurnStatus::Interrupted,
            turn_error_kind: Some(TurnErrorKind::Interrupted),
        };
    }
    if matches!(
        loop_result,
        LoopResult::Error(AgentError::MaxIterationsExceeded(_))
    ) {
        return LoopTerminal {
            ok: false,
            stop_reason: PromptStopReason::MaxTurnRequests,
            failure: None,
            turn_status: TurnStatus::Error,
            turn_error_kind: Some(TurnErrorKind::MaxIterations),
        };
    }
    let LoopResult::Error(error) = loop_result else {
        unreachable!("Completed and Interrupted were classified above")
    };
    LoopTerminal {
        ok: false,
        stop_reason: PromptStopReason::EndTurn,
        failure: Some(ExecutionFailure::from_agent_error(error)),
        turn_status: TurnStatus::Error,
        turn_error_kind: Some(TurnErrorKind::LlmFailure),
    }
}
