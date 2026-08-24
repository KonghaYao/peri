use std::sync::Arc;

use chrono::Local;

use peri_acp_types::{
    event::{AgentEventHandler, BackgroundTaskResult, ExecutorEvent},
    frozen::ThreadPersistence,
    messages::{BaseMessage, MessageContent},
    session::{MessageQueue, QueuedMessage},
    tasks::{BgTaskKind, TaskManager},
};

use crate::agent::react::AgentInput;
use crate::session::async_router::AsyncRouter;
use crate::session::exec::executor_helpers::{
    build_and_execute_agent_v2, ExecOutcome, ForwarderLauncherFn, StageBuildFn, V2ExecuteRequest,
};
use crate::session::exec::stage_builder::CachedLlmInstances;

use super::{ContinuationRequest, FrozenSessionData, SessionContext, TurnConfig};

/// Agent 执行后的最终输出（state + 停止原因）。
///
/// L5：定义迁入本模块（`session::exec::executor_helpers::ExecOutcome`），
/// 本处经上方 use 块 re-export。
/// 构建 + 执行 agent。包含：
/// - system prompt 解析（frozen 或 legacy 重建）
/// - SubAgentMiddleware register/deregister 闭包
/// - `build_agent` 调用 + AgentPool 缓存回写
/// - bg event pump + todo 转发 pump 启动
/// - `build_and_execute_agent_v2` 调用 + 错误事件转发
/// - cancel cascade 子 agent
#[allow(clippy::too_many_arguments)]
pub(super) async fn build_and_execute_agent(
    ctx: &SessionContext,
    turn: &TurnConfig<'_>,
    agent_input: AgentInput,
    history: Vec<BaseMessage>,
    session_id: &str,
    cached_llm: Option<&CachedLlmInstances>,
    v2_message_queue: &MessageQueue,
    async_router: Option<AsyncRouter>,
    task_manager: Arc<dyn TaskManager>,
    continuation: bool,
    stage_build: StageBuildFn,
    forwarder_launcher: ForwarderLauncherFn,
) -> ExecOutcome {
    let frozen_session = if let Some(f) = turn.frozen {
        // 使用 session 创建时冻结的完整数据，跳过重建。
        f.clone()
    } else {
        // 调用方未提供 frozen 数据时，经注入的防御性构建器在此一次性构建
        // （渲染面在 ACP 宿主；生产不可达——print mode 已迁移为提前构建
        // FrozenSessionData，此分支仅作防御性编程保留，None 时回落最小数据）。
        match ctx.frozen_fallback_builder.as_ref() {
            Some(builder) => builder(turn.cwd, turn.language.as_deref()),
            None => {
                // 最小回落：无 skills / 无 CLAUDE.md 的空冻结数据
                FrozenSessionData::from_frozen_parts(
                    crate::session::FrozenContext {
                        system_prompt: Arc::from(""),
                        claude_md: Arc::from(""),
                        skill_summary: Arc::from(""),
                        date: Arc::from(Local::now().format("%Y-%m-%d").to_string()),
                        language: turn.language.clone().map(Arc::from),
                        // 防御性回退：无冻结数据时 MetaHarness 状态为空（无覆盖、无关闭）
                        meta_harness: peri_acp_types::meta_harness::MetaHarnessState::default(),
                    },
                    None,
                )
            }
        }
    };

    // Build register/deregister closures for SubAgentMiddleware（经 SessionAccessPort
    // 端口构造——原逻辑：定位 AcpSession 并维护 active_agents 注册表）
    let register_runtime = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.register_runtime(session_id));
    let deregister_runtime = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.deregister_runtime(session_id));

    let event_handler: Arc<dyn AgentEventHandler> =
        Arc::new(peri_acp_types::event::FnEventHandler({
            // v1 协议化载体直发（subagent 发射侧同步映射 / retry observer 等）
            // 统一经 EventPublisher：无 turn_id/agent_id 身份的事件身份降级为空串
            // （envelope 仅 ACP 内部使用，TUI 协议化映射不消费空身份字段）。
            // v1 ExecutorEvent 中间态已退役（批 2「v1-retire」）：本 handler 是
            // ACP 协议序列化面的接收端，不承载 Agent 层业务发射。
            let publisher = Arc::clone(&ctx.event_publisher);
            let sid = session_id.to_string();
            move |event: ExecutorEvent| {
                let source = peri_acp_types::runtime::UnstampedEvent::new(
                    String::new(),
                    String::new(),
                    None,
                    peri_acp_types::identity::EventDeliveryClass::Critical,
                );
                publisher.publish_event(&sid, &source, event);
            }
        }));

    // Session 级 workflow 完成通知消费者（单次 spawn）。
    // 单路径：
    //   Path B (Agent): 通过 AsyncRouter → InboxHandle → push_defer（Defer kind）→ End 阶段唤醒新 turn。
    // TUI 侧通知由 registry 通道提供（registry.complete → BgRegistryEvent::Completed →
    // bg-task-completed unstable event），不再经 EventSink 直推 BackgroundTaskCompleted——
    // 该映射在 event_sink 是死路径（S5.1，issue 2026-08-05-background-task-completed-event-dead-path）。
    if let Some(wf_mw) = ctx.workflow_middleware.as_ref() {
        // 将 session 级 TaskManager 注入 WorkflowMiddleware（延迟注入，支持内部可变性）
        wf_mw.set_bg_registry(task_manager.clone());

        // init_notification_buffer() 是 set-once gate：首次返回 true，后续返回 false。
        // WorkflowMiddleware 是 session 级实例（session/new 创建），
        // 因此每个 session 的消费者只 spawn 一次，无跨 session 污染。
        if wf_mw.init_notification_buffer() {
            let wf_mw_for_notify = Arc::clone(wf_mw);
            // AsyncRouter（v2 路径：push_defer + wake Notify）
            // 或回退 v2 queue clone（无 inbox 时直接 push，无 wake）
            let wf_router = async_router.clone();
            let fallback_queue = v2_message_queue.clone();
            // task_manager 用于在 Defer 入队后递减 active_count，消除竞态窗口
            let notify_bg = task_manager.clone();
            tokio::spawn(async move {
                let mut rx = wf_mw_for_notify.subscribe_notifications();
                loop {
                    match rx.recv().await {
                        Ok(task_result) => {
                            // Path B: 通过 AsyncRouter（或回退 v2 queue）push Defer。
                            // AsyncRouter → InboxHandle → push_defer 触发 wake Notify，
                            // 替代直接 notify_queue.push（raw，无 wake）。
                            if let Some(ref router) = wf_router {
                                router.route_workflow_event(
                                    &task_result.run_id,
                                    &task_result.workflow_name,
                                    task_result.status.as_str(),
                                    task_result.duration_ms,
                                    task_result.agent_count,
                                    task_result.tool_calls_count,
                                    &task_result.phase_summaries,
                                );
                            } else {
                                // 回退：直接 push（无 wake，兼容无 inbox 场景）
                                let mut phase_lines = String::new();
                                for s in &task_result.phase_summaries {
                                    let token_info = if s.token_count > 0 {
                                        format!(", {} tokens", s.token_count)
                                    } else {
                                        String::new()
                                    };
                                    let dur_info = if let Some(d) = s.duration_ms {
                                        format!(", {}ms", d)
                                    } else {
                                        String::new()
                                    };
                                    phase_lines.push_str(&format!(
                                        "- {}: {} agents{}{}\n",
                                        s.name, s.agent_count, token_info, dur_info
                                    ));
                                }
                                // 幽灵完成事件防护（issue 2026-08-05）：killed/failed 不得显示为 "completed"
                                let status_word = match task_result.status.as_str() {
                                    "completed" => "completed",
                                    "killed" => "killed",
                                    _ => "failed",
                                };
                                // 不包裹 <system-reminder>：append_messages_to_transcript 统一包裹所有 Defer/Info
                                let notif_text = format!(
                                    "Workflow '{}' {status_word}. ({}ms, {} agents, {} tool calls)\n\
                                    {}Results saved to .claude/workflow-runs/{}/state.json",
                                    task_result.workflow_name,
                                    task_result.duration_ms,
                                    task_result.agent_count,
                                    task_result.tool_calls_count,
                                    phase_lines,
                                    task_result.run_id,
                                );
                                fallback_queue.push(QueuedMessage::new(
                                    peri_acp_types::session::MessageKind::Defer,
                                    peri_acp_types::session::MessageSource::WorkflowComplete,
                                    BaseMessage::human(MessageContent::text(notif_text)),
                                ));
                            }

                            // 构造 BackgroundTaskResult 并写入 registry：触发 BgRegistryEvent::Completed
                            // （→ bg-task-completed unstable event → TUI 通知条），这是通知的真实路径。
                            // 不再经 EventSink 直推 BackgroundTaskCompleted（S5.1：event_sink 无映射，死路径）。
                            let bg = BackgroundTaskResult {
                                task_id: task_result.run_id.clone(),
                                agent_name: format!("workflow:{}", task_result.workflow_name),
                                prompt_summary: task_result.workflow_name.clone(),
                                success: task_result.success,
                                output: format!(
                                    "Workflow '{}' finished with status {:?} ({}ms, {} agents, {} tool calls). \
                                     Results in .claude/workflow-runs/{}/state.json",
                                    task_result.workflow_name,
                                    task_result.status,
                                    task_result.duration_ms,
                                    task_result.agent_count,
                                    task_result.tool_calls_count,
                                    task_result.run_id
                                ),
                                tool_calls_count: task_result.tool_calls_count,
                                duration_ms: task_result.duration_ms,
                                child_thread_id: None,
                                timed_out: false,
                            };
                            // 在 Defer 入队后递减 active_count，消除 tool.rs 通知 task 中的竞态窗口：
                            // 原实现在 registry.complete() broadcast 后立即调用 bg.complete_workflow()，
                            // 若 broadcast consumer 尚未被调度，agent 的 idle_should_wait probe
                            // (active_count > 0) 提前归零 → agent 退出 ReAct loop → Defer 堆积在队列中。
                            // [修复] 将 complete_workflow 移至 consumer 内 Defer push 之后执行。
                            notify_bg.complete(&task_result.run_id, bg);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("WF notification consumer lagged by {} messages", n);
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break; // session 结束，自然退出
                        }
                    }
                }
            });
        }
    }

    // 从 session_access 获取 goal_state（实现 GoalController trait）
    let goal_controller: Option<Arc<dyn peri_acp_types::goal::GoalController>> = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.goal_controller(session_id));

    let thread_persistence = ThreadPersistence {
        store: ctx.thread_store.clone(),
        parent_thread_id: ctx.thread_id.clone(),
        register_runtime,
        deregister_runtime,
    };

    let task_manager_opt = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.task_manager(session_id));

    // on_bg_complete：bg 完成时**先**把结果同步 route 到 SessionInbox
    // （Defer + wake），**再**通知 ACP server 的 per-session continuation
    // scheduler。回调可能在主 prompt 结束后才发生（bg 独立运行），此时
    // callback queue 已先写入；scheduler 原子 take session/cancel 标记后
    // 通过同一 session execution path 发起内部 AsyncContinuation。
    let on_bg_complete = async_router.as_ref().map(|router| {
        let router = router.clone();
        let notify = ctx.continuation_notify.clone();
        let sid = ctx.session_id.clone();
        Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
            router.route_bg_result(result, kind);
            if let Some(ref tx) = notify {
                let _ = tx.send(ContinuationRequest {
                    session_id: sid.clone(),
                    kind,
                });
            }
        }) as Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>
    });

    // ── L5 执行体注入面（stage 构建 / 事件发射 / LLM 缓存 / cancel cascade / forwarder）──
    // 事件发射端口（Controller 适配；Phase 2/3/4/7/9 统一发射点）
    let publisher = Arc::clone(&ctx.event_publisher);

    // LLM 缓存回写（AgentPool；ACP 宿主注入）
    let store_llm: Arc<dyn Fn(CachedLlmInstances) + Send + Sync> = match &ctx.store_llm {
        Some(f) => Arc::clone(f),
        None => Arc::new(|_| {}),
    };

    // cancel cascade 子 agent（SessionAccessPort）
    let sa_for_cascade = ctx.session_access.clone();
    let cancel_cascade: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |sid: &str| {
        if let Some(ref sa) = sa_for_cascade {
            sa.cancel_cascade_children(sid);
        }
    });

    // EventBus forwarder 启动器（ACP 宿主注入；Langfuse bridge 构造在 ACP——
    // 观测旁路；biased select 顺序不变量单点保持在 ACP spawn_eventbus_forwarder）

    // v2 单一路径。
    build_and_execute_agent_v2(V2ExecuteRequest {
        session_id: ctx.session_id.clone(),
        cwd: ctx.cwd.clone(),
        cancel: ctx.cancel.clone(),
        thread_store: ctx.thread_store.clone(),
        thread_id: ctx.thread_id.clone(),
        agent_input,
        history,
        cached_llm: cached_llm.cloned(),
        task_manager: task_manager_opt,
        continuation,
        // ── stage 装配输入（透传 StageBuildRequest）──
        frozen_session,
        event_handler,
        agent_overrides: None,       // agent_overrides
        preload_skills: Vec::new(),  // preload_skills
        child_handler_factory: None, // child_handler_factory
        auxiliary_model: turn.auxiliary_model.clone(),
        thread_persistence,
        goal_controller,
        on_bg_complete,
        // ── 注入面 ──
        publisher,
        stage_build,
        store_llm,
        cancel_cascade,
        forwarder_launcher,
    })
    .await
}
