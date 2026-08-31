//! Shared prompt execution logic（L5：自 `peri-acp/src/host/exec/executor.rs`
//! 物理迁入，ACP 侧 `crate::session::executor` 薄壳 re-export 保兼容）。
//!
//! Provides [`run_session_loop`] which encapsulates the common agent execution
//! pipeline used by both TUI (via [`TransportEventSink`]) and stdio (via
//! [`StdioEventSink`]) paths.
//!
//! Compact 由 v2 `stages/compact.rs`（`run_react_loop` 在每轮开头调
//! `compact_v2::run_compact`）统一处理，不再需要外层 loop + resubmit，
//! 也不再经过 CompactMiddleware。
//!
//! # 文件结构（EXECUTOR-SPLIT 选项 B + L5 迁出）
//!
//! 本文件是 orchestrator，仅保留：
//! - 共享类型：`PromptStopReason` / `PromptResult` / `FrozenSessionData`
//!   / `SessionContext` / `TurnInput` / `TurnConfig` / `LangfuseHooks`
//! - 入口：`run_session_loop`（编排）+ `build_and_execute_agent`（cfg 组装与 v2 dispatch）
//! - Prediction facade：`execute_prediction` / `extract_prediction_text`
//!
//! 子流程已随 L5 迁入本 crate `session::exec::executor_helpers`：
//! - [`intercept_immediate_command`]：slash 命令拦截
//! - [`spawn_event_pump`]：后台事件泵 + Langfuse tracer（注入闭包）
//! - [`build_and_execute_agent_v2`]：v2 stages 装配与 ReAct 循环驱动（9 个 phase）
//! - [`collect_result`] / [`close_channel`] / [`wait_for_pump`]：结果收集
//!
//! 本模块经下方 use 块把 helper 提升到本模块命名空间，使 `executor_test.rs`
//! 的 `super::{intercept_immediate_command, InterceptRequest}` 路径继续可解析。
//!
//! # 依赖反转（§0）
//!
//! 本模块只依赖 peri-acp-types / peri-model / crate 内部：
//! - `provider` / `peri_config` / `AgentPool` / `SessionManager` / `Controller`
//!   五个 ACP 特有字段端口化为投影值 + 注入闭包 + [`SessionAccessPort`] /
//!   [`EventPublisher`] / [`EventSubscriber`] 端口（ACP 宿主装配面构造）；
//! - 事件发射/订阅经契约端口（[`EventPublisher`] / [`EventSubscriber`] 适配层
//!   在 ACP 宿主侧），命令拦截 / stage 装配 / Langfuse / cancel cascade
//!   全部经注入面接入；
//! - Langfuse 遥测经 [`LangfuseHooks`]（on_turn_start / on_turn_end /
//!   bridge_factory 闭包，ACP 宿主从 `LangfuseSession` 构造）；
//! - stage 装配桥（[`StageBuildFn`]）与 EventBus forwarder 启动器
//!   （[`ForwarderLauncherFn`]）由 ACP 宿主构造后经 [`TurnInput`] 注入。
//!
//! ## Cancel 语义保持
//!
//! - `intercept_immediate_command` 内的 `tokio::select!` 分支顺序原样保留
//!   （`cmd.execute` 与 `cancel.cancelled()` 仍按原 biased 顺序，二者均触发 push_done）
//! - `build_and_execute_agent_v2` 末尾的 cancel cascade 仍在循环失败后触发，
//!   `LoopResult::Error` 分支先发 `AgentExecutionFailed` 事件再判断 stop_reason
//! - `collect_result` 严格 "close → wait_for_pump(10s timeout) → drain recall"

use std::sync::Arc;

use tokio::sync::oneshot as exec_oneshot;

use peri_acp_types::{
    event::ExecutorEvent,
    interaction::UserInteractionBroker,
    messages::{BaseMessage, ContentBlock, MessageContent},
    session::QueuedMessage,
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use peri_acp_types::tasks::{BgRegistryEvent, BgTaskKind};

use crate::agent::react::AgentInput;
use crate::session::async_router::AsyncRouter;

// 子流程 helper 同 crate 迁移（`session::exec::executor_helpers`）：
// intercept_immediate_command / InterceptRequest / spawn_event_pump /
// SpawnPumpRequest / PumpHandle / collect_result / CollectRequest /
// close_channel / wait_for_pump / build_and_execute_agent_v2 /
// V2ExecuteRequest / StageBuildFn / ExecOutcome ——
// executor_test.rs 通过 `super::` 访问的符号路径保持不变。
pub use crate::session::exec::executor_helpers::{
    build_and_execute_agent_v2, close_channel, collect_result, intercept_immediate_command,
    spawn_event_pump, wait_for_pump, CollectRequest, CommandLookupFn, ExecOutcome,
    ForwarderLauncherFn, InterceptOutcome, InterceptRequest, PumpHandle, SpawnPumpRequest,
    StageBuildFn, V2ExecuteRequest,
};

mod agent_build;
mod context;
mod prediction;

use agent_build::build_and_execute_agent;

pub use context::{
    AutoClassifierFactory, FrozenFallbackBuilder, FrozenSessionData, LangfuseBridgeFactory,
    LangfuseHooks, LangfuseTurnEndHook, SessionContext, SubagentLlmFactory, TurnInput,
};
pub use prediction::{
    execute_prediction, extract_prediction_text, parse_prediction_actions, PredictionError,
};

/// High-level reason why prompt execution stopped, used to derive ACP `StopReason`.
///
/// L5 契约化：事实源 `peri-acp-types::command::PromptStopReason`。
pub use peri_acp_types::command::PromptStopReason;

/// bg 完成 → ACP server continuation scheduler 的通知请求。
///
/// 由 executor 的 `on_bg_complete` 闭包在 [`AsyncRouter::route_bg_result`] 之后发送：
/// 先确保 deferred callback 已写入 SessionInbox，再通知 scheduler。
/// scheduler 按 session 原子 take `session/cancel` 置位的标记后运行一次内部
/// AsyncContinuation（见 peri-tui/src/acp_server/continuation.rs）。
#[derive(Debug, Clone)]
pub struct ContinuationRequest {
    pub session_id: String,
    pub kind: BgTaskKind,
}

/// Result of prompt execution.
///
/// L5 契约化：事实源 `peri-acp-types::session::PromptResult`。
pub use peri_acp_types::session::PromptResult;

/// keepgoing 判定：内容按 block 判空（`MessageContent::is_empty`）。
///
/// 这是 TUI keepgoing 按钮 ↔ ACP ↔ agent stages 的**跨层共享判定**：
/// - 此处：空 prompt → 跳过 recall 注入（`run_session_loop`）
/// - `peri-agent` stages：空 Prompt → 不写入 transcript（`append_messages_to_transcript`）
///
/// 必须与 stages 层保持同一语义。用 `is_empty()`（按 content block 判空）而非
/// `text_content().trim()`——后者会把 `Blocks([Image])` 这类纯附件消息误判为
/// keepgoing（图片接入后即触发），且畸形请求经 `extract_prompt_params` 默认值
/// （空文本）落入 keepgoing 路径时行为一致。
///
/// 协议约定（见 docs/standards/architecture-contracts.md ARC-KEEPGOING-001）：
/// 空白 user prompt = "继续跑 loop"，唯一生产者为 TUI keepgoing 按钮。
pub fn is_keepgoing(content: &peri_acp_types::messages::MessageContent) -> bool {
    content.is_empty()
}

/// Per-turn computed configuration derived from [`SessionContext`].
///
/// Built once at the top of [`run_session_loop`], passed by reference to
/// [`build_and_execute_agent`] to avoid recomputing and to keep the agent
/// builder function signature manageable.
#[allow(dead_code)]
struct TurnConfig<'a> {
    cwd: &'a str,
    frozen: Option<&'a FrozenSessionData>,
    language: Option<String>,
    cancel: &'a AgentCancellationToken,
    permission_mode: &'a Arc<peri_acp_types::permission::SharedPermissionMode>,
    broker: &'a Arc<dyn UserInteractionBroker>,
    session_start_source: Option<String>,
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    effective_context_window: u32,
}

/// BgRegistryEvent → unstable 事件（bg-task-started/completed/cancelled）映射。
///
/// TUI bg 面板协议面（`AcpEventData::BgTask*` 解码）依赖的事件名与 payload
/// 字段保持不变——事件三层化仅改发射/消费路径（发射经 Controller 补打身份、
/// 消费经 Controller 订阅），不改协议面。
fn registry_unstable_event(event: &BgRegistryEvent) -> (String, serde_json::Value) {
    match event {
        BgRegistryEvent::Started {
            task_id,
            kind,
            summary,
            started_at,
        } => (
            "bg-task-started".to_string(),
            serde_json::json!({
                "task_id": task_id,
                "kind": kind,
                "summary": summary,
                "started_at": started_at,
            }),
        ),
        BgRegistryEvent::Completed {
            task_id,
            kind,
            success,
            output_preview,
            duration_ms,
            // route_bg_result 现在在 spawner 中同步执行（在 task_manager.complete()
            // 之前），不再需要 registry 事件泵异步注入。
            result: _result,
        } => (
            "bg-task-completed".to_string(),
            serde_json::json!({
                "task_id": task_id,
                "kind": kind,
                "success": success,
                "output_preview": output_preview,
                "duration_ms": duration_ms,
            }),
        ),
        BgRegistryEvent::Cancelled { task_id, reason } => (
            "bg-task-cancelled".to_string(),
            serde_json::json!({
                "task_id": task_id,
                "reason": reason,
            }),
        ),
    }
}

/// Shared agent execution pipeline with auto-compact support.
///
/// # 调用方职责（L5 依赖反转）
///
/// - Session management (storing/retrieving cwd, history, cancel_token)
/// - Choosing the broker (HITL/AskUser handler)
/// - Providing the correct `EventSink` implementation
/// - 投影 ACP 特有构造（provider / peri_config / AgentPool / SessionManager /
///   Controller）为 [`SessionContext`] 的端口字段与注入闭包
/// - 经 [`TurnInput::stage_build`] / [`TurnInput::forwarder_launcher`] 注入
///   stage 装配桥与 EventBus forwarder 启动器（ACP 宿主侧）
pub async fn run_session_loop(ctx: SessionContext, turn: TurnInput) -> PromptResult {
    let TurnInput {
        event_sink,
        content,
        continuation,
        frozen,
        history,
        incoming_recalls,
        bg_results,
        langfuse,
        stage_build,
        forwarder_launcher,
    } = turn;

    // keepgoing：空白 user prompt 是 TUI keepgoing 按钮发起的"继续跑 loop"指令。
    // 语义：不插入 user prompt（stages/append_messages_to_transcript 跳过空 Prompt），
    // 仅让 Receive 消费计数 >0 从而驱动 ReAct loop 继续。此时不注入 recall——
    // 否则 recall 会拼进 user 消息使其非空，破坏"不插入"语义。
    // 判定与 stages 层共用同一语义：按 content block 判空（见 is_keepgoing 注释）。
    //
    // [AsyncContinuation] 内部续跑（continuation=true）不是 keepgoing：
    // 空 user prompt 不落入 keepgoing 语义，也不走空历史 keepgoing short-circuit。
    // 与 keepgoing 相同的是：**不注入 recall**——上一轮留给用户 prompt 的 recall
    // 由 run_prompt 保留在 SessionState（clone 而非 take），续跑只消费已 route 的
    // Defer/Info 消息；recall 留给后续用户 prompt 注入。
    let is_keepgoing = !continuation && is_keepgoing(&content);
    let incoming_recalls = if is_keepgoing || continuation {
        tracing::debug!(
            skip = if continuation {
                "continuation"
            } else {
                "keepgoing"
            },
            "empty user prompt, skipping recall injection"
        );
        Vec::new()
    } else {
        incoming_recalls
    };

    // 空历史 + 空 prompt：无内容可继续——直接短路返回，避免跑一轮无意义 LLM 调用。
    // （TUI 侧 handle_keepgoing_submit 已有 has_session 防御；此处防御 stdio 等
    // 其他 transport 对全新 session 发空 prompt 的场景。）
    if is_keepgoing && history.is_empty() {
        tracing::debug!("keepgoing: empty history, short-circuiting (nothing to continue)");
        // [TRAP] 短路路径绕过 agent event pump（spawn_event_pump 的 push_done
        // 不会执行），必须手动发送终止通知（ARC-EVENT-001），否则 TUI 依赖
        // AgentDone→TurnDone 退出 loading 的机制失效，界面永久卡在 loading。
        // stop_reason 与正常路径保持一致（executor_helpers push_done "end_turn"）。
        event_sink
            .push_done(&ctx.session_id, "end_turn", ctx.request_id.as_deref())
            .await;
        return PromptResult {
            messages: history,
            ok: true,
            stop_reason: PromptStopReason::EndTurn,
            history_replaced_by_compaction: false,
            recall_items: Vec::new(),
            failure: None,
        };
    }

    // Compact config — computed early for command interception and agent building.
    // （L5：env overrides 在宿主构造点应用，语义与 load_compact_config 一致）
    let disable_compact = std::env::var("DISABLE_COMPACT").is_ok()
        || std::env::var("DISABLE_AUTO_COMPACT").is_ok()
        || !ctx.compact_config.auto_compact_enabled;

    // 解析会话级共享的 v2 MessageQueue（经 SessionAccessPort）。
    // 缺失时（无 session_access / session 不存在）退化为独立 MessageQueue，
    // 保持行为可运行——但跨 turn 消息将不可见（仅降级场景）。
    //
    // 在 run_session_loop 开头解析而非 build_and_execute_agent 内部，
    // 是为了让 bg_results / workflow Path B 等会话级注入能在此处统一 push。
    let v2_message_queue = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.v2_message_queue(&ctx.session_id))
        .unwrap_or_default();

    // 解析 session-level SessionInbox（await-wake wrapper）。
    // 用于：(1) executor idle 期间 await_wake 阻塞等待异步事件，
    // (2) AsyncRouter 推送 bg_results/workflow 事件时触发 wake。
    // None 表示不支持 async wake（如 print mode），保持向后兼容。
    let session_inbox = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.session_inbox(&ctx.session_id));

    // 构建 AsyncRouter（统一异步事件路由到 inbox）。
    // 通过 InboxHandle 推送 Defer 消息并触发 wake Notify，
    // 替代 executor 的直接 v2_message_queue.push（raw，无 wake）。
    let async_router = session_inbox
        .as_ref()
        .map(|inbox| AsyncRouter::new(inbox.handle()));

    // bg_results 通过 AsyncRouter（或回退到 v2 MessageQueue）push（Defer kind）。
    //
    // Defer 是异步延迟结果的正确语义：本轮 Receive 跳过保留，End 阶段 drain
    // 唤醒新 turn，并由 `mod.rs::run_react_loop` 写入 transcript（包裹
    // `<system-reminder>`）。与 WorkflowComplete / cron 等其他异步唤醒路径
    // 走同一套机制——见 `append_messages_to_transcript`。
    if !bg_results.is_empty() {
        tracing::info!(
            count = bg_results.len(),
            "[bg-diag] ctx.bg_results is non-empty, will inject each via AsyncRouter"
        );
        if let Some(ref router) = async_router {
            // v2 路径：通过 AsyncRouter → InboxHandle → push_defer（触发 wake）
            for result in &bg_results {
                router.route_bg_result(result, BgTaskKind::Agent);
            }
        } else {
            // 回退路径：直接 push（无 wake，兼容 print mode / 无 SessionAccess）
            use peri_acp_types::session::{MessageKind as V2Kind, MessageSource as V2Src};
            for result in &bg_results {
                v2_message_queue.push(QueuedMessage::new(
                    V2Kind::Defer,
                    V2Src::SubAgentComplete,
                    BaseMessage::human(MessageContent::text(result.to_notification())),
                ));
            }
        }
    }

    // Auxiliary model — reuse AgentPool cache if available, otherwise create fresh.
    // 共享于 v2 stages/compact.rs（摘要）与 Goal 工具（完成度验证）。
    // （L5：缓存读取 / fresh 构造经注入闭包，AgentPool 留在 ACP）
    let cached_llm = ctx.get_cached_llm.as_ref().and_then(|f| f());
    let auxiliary_model: Option<Arc<dyn peri_model::Model>> = if disable_compact {
        None
    } else {
        cached_llm
            .as_ref()
            .map(|c| c.auxiliary_model.clone())
            .or_else(|| {
                // 转发器从 session 级 AgentPool 取：fresh 模型烘焙的 observer 会随
                // CachedLlmInstances 跨 turn 复用，必须指向 session 级转发器。
                ctx.fresh_auxiliary_model.as_ref().map(|f| f())
            })
    };

    // Context window（宿主构造点已按 context_1m 计算 effective 值）
    let effective_context_window = ctx.effective_context_window;

    // session 级 TaskManager（跨 prompt 存活，由 executor 从 session 获取）
    let task_manager_for_cmd = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.task_manager(&ctx.session_id))
        .unwrap_or_else(|| Arc::new(peri_acp_types::tasks::NoopTaskManager));

    // Registry → 事件链泵（事件三层化收尾）：发射经 EventPublisher
    // （BgRegistryEvent 包装为 ExecutorEvent::BgRegistryEvent 载体；身份降级为
    // 空串——registry 事件无 turn 归属），消费端从 subscribe() 工厂订阅
    // 本 session 事件并映射回 bg-task-* unstable 事件（TUI bg 面板协议面不变）。
    {
        let (registry_event_tx, mut registry_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<BgRegistryEvent>();
        task_manager_for_cmd.set_event_sender(registry_event_tx, ctx.session_id.clone());
        let mut subscription = (ctx.subscribe)();
        let registry_sink = Arc::clone(&event_sink);
        let registry_sid = ctx.session_id.clone();
        let publisher = Arc::clone(&ctx.event_publisher);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = subscription.recv() => {
                        match msg {
                            Ok(m) if m.envelope.session_id == registry_sid => {
                                if let Some(ExecutorEvent::BgRegistryEvent(event)) = m.event {
                                    let (event_name, payload) = registry_unstable_event(&event);
                                    registry_sink
                                        .push_unstable_event(&registry_sid, event_name, payload)
                                        .await;
                                }
                            }
                            Ok(_) => {}
                            Err(peri_acp_types::event::SubscriptionError::Lagged(n)) => {
                                tracing::warn!(n, "registry event subscription lagged, events dropped");
                            }
                            Err(peri_acp_types::event::SubscriptionError::Closed) => break,
                        }
                    }
                    ev = registry_event_rx.recv() => {
                        match ev {
                            Some(event) => {
                                // 发射端：registry 事件无 turn/agent 身份（身份降级为
                                // 空串；envelope 仅 ACP 内部使用）。
                                let source = peri_acp_types::runtime::UnstampedEvent::new(
                                    String::new(),
                                    String::new(),
                                    None,
                                    peri_acp_types::identity::EventDeliveryClass::Critical,
                                );
                                publisher.publish_event(
                                    &registry_sid,
                                    &source,
                                    ExecutorEvent::BgRegistryEvent(event),
                                );
                            }
                            None => {
                                // 发射点集合结束（registry_event_tx 全 drop）：drain 广播
                                // 在途事件后退出（与主 pump 同语义）。
                                loop {
                                    match subscription.try_recv() {
                                        Ok(Some(m)) if m.envelope.session_id == registry_sid => {
                                            if let Some(ExecutorEvent::BgRegistryEvent(event)) = m.event {
                                                let (event_name, payload) = registry_unstable_event(&event);
                                                registry_sink
                                                    .push_unstable_event(&registry_sid, event_name, payload)
                                                    .await;
                                            }
                                        }
                                        Ok(Some(_)) => {}
                                        Ok(None) => break,
                                        Err(peri_acp_types::event::SubscriptionError::Lagged(n)) => {
                                            tracing::warn!(n, "registry event subscription lagged, events dropped");
                                            break;
                                        }
                                        Err(peri_acp_types::event::SubscriptionError::Closed) => break,
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    // ── L5 命令拦截注入面（注册表 / compact 配置）──
    let command_lookup = Arc::clone(&ctx.command_lookup);
    let compact_config_loader = Arc::clone(&ctx.compact_config_loader);

    // Command interception — check if content is a slash command before building agent.
    // 三态分发（Phase 5 Step 6）：
    //   Handled(r)   → 命令已完成（push_done 已由拦截层调用），直接返回；
    //   Inject(text) → AgentInput::blocks(text) 进 agent 管线（不 push_done，
    //                  pump 负责）；
    //   PassThrough  → 现状 AgentInput::blocks(content)。
    let injected_content = match intercept_immediate_command(InterceptRequest {
        content: &content,
        history: &history,
        cwd: &ctx.cwd,
        session_id: &ctx.session_id,
        cancel: &ctx.cancel,
        thread_store: ctx.thread_store.clone(),
        thread_id: ctx.thread_id.clone(),
        // L5：冻结数据由调用点投影为字符串字段（原 FrozenSessionData 引用）
        frozen_claude_md: frozen
            .as_ref()
            .and_then(|f| f.claude_md().map(String::from)),
        frozen_claude_local_md: frozen
            .as_ref()
            .and_then(|f| f.claude_local_md().map(String::from)),
        frozen_skill_summary: frozen
            .as_ref()
            .and_then(|f| f.skill_summary().map(String::from)),
        // fork/bg-fork 复用的冻结 prompt（16_workflow 已删除（C2），子面向
        // 与主 prompt 字节相同，直接复用主 prompt）
        frozen_system_prompt: frozen.as_ref().map(|f| f.system_prompt().to_string()),
        event_sink: &event_sink,
        auxiliary_model: &auxiliary_model,
        task_manager: &task_manager_for_cmd,
        command_lookup,
        compact_config_loader,
    })
    .await
    {
        InterceptOutcome::Handled(result) => return result,
        InterceptOutcome::Inject(text) => MessageContent::text(text),
        InterceptOutcome::PassThrough => content,
    };

    let trace_input = injected_content.text_content();
    // 演进 1（meta-harness 波 4）：permission_mode 运行时通知注入已整体删除。
    // 模型对权限模式的感知仅经 10_hitl 段落（PermissionMiddleware 持有）
    // 的机制说明——mode 会话内切换不再注入 `<system-reminder>` 通知。
    // 此处仅保留 incoming_recalls 的受控容器注入语义。
    let agent_input = if incoming_recalls.is_empty() {
        AgentInput::blocks(injected_content)
    } else {
        let reminder_text = format!(
            "<system-reminder>\n{}\n</system-reminder>",
            incoming_recalls.join("\n")
        );
        let mut blocks = injected_content.content_blocks();
        blocks.push(ContentBlock::text(reminder_text));
        AgentInput::blocks(MessageContent::blocks(blocks))
    };

    // [v2] Context budget 由 AgentComponents 传给 StageContext，此处不再需要本地变量。

    // Event channel (lives for entire run_session_loop lifetime)
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let event_tx = Arc::new(parking_lot::Mutex::new(Some(event_tx)));

    // 将会 move 的 middleware resources（无法借用，必须 move）。
    // turn 仍以引用形式借用 cwd/cancel/permission_mode/broker。
    let turn = TurnConfig {
        cwd: &ctx.cwd,
        frozen: frozen.as_ref(),
        language: frozen
            .as_ref()
            .and_then(|f| f.language().map(|s| s.to_string()))
            .or_else(|| ctx.language.clone()),
        cancel: &ctx.cancel,
        permission_mode: &ctx.permission_mode,
        broker: &ctx.broker,
        session_start_source: ctx.session_start_source.clone(),
        auxiliary_model: auxiliary_model.clone(),
        effective_context_window,
    };

    // Langfuse 遥测经注入闭包（宿主构造的 LangfuseHooks）接入。
    let langfuse_on_turn_start: Option<Arc<dyn Fn() + Send + Sync>> = langfuse.as_ref().map(|h| {
        let on_start = Arc::clone(&h.on_turn_start);
        let trace_input = trace_input.to_string();
        Arc::new(move || {
            on_start(&trace_input);
        }) as Arc<dyn Fn() + Send + Sync>
    });
    let langfuse_on_turn_end: Option<LangfuseTurnEndHook> =
        langfuse.as_ref().map(|h| Arc::clone(&h.on_turn_end));

    // Main event pump（事件三层化：发射点 → EventPublisher →
    // 本泵订阅消费；event_rx 仅作发射点集合的关闭信号）
    let (stop_reason_tx, stop_reason_rx) = exec_oneshot::channel::<(
        PromptStopReason,
        peri_acp_types::session::TurnTelemetryOutcome,
    )>();
    let pump_handle = spawn_event_pump(SpawnPumpRequest {
        // L5：订阅经端口适配（契约层 SubscriptionError 镜像，Controller 零改动）
        subscription: (ctx.subscribe)(),
        event_rx,
        stop_reason_rx,
        sink: Arc::clone(&event_sink),
        session_id: ctx.session_id.clone(),
        effective_context_window,
        // L5：Langfuse tracer 留在 ACP——泵经闭包在任务开头触发
        // on_turn_start、pump_done 之后触发 on_turn_end（JoinHandle drop =
        // fire-and-forget，不得阻塞管线）。
        langfuse_on_turn_start,
        langfuse_on_turn_end,
        request_id: ctx.request_id.clone(),
    });

    // 把会 move/借用 的资源直接传入 build_and_execute_agent。
    // 由于 prompt builder 需要的所有资源都已提供，调用方后续不再访问这些已 move 字段
    // （session_id 在 collect_result 借用，此时 build_and_execute_agent 已完成）。
    let exec_outcome = build_and_execute_agent(
        &ctx,
        &turn,
        agent_input,
        history,
        &ctx.session_id,
        cached_llm.as_ref(),
        &v2_message_queue,
        async_router.clone(),
        task_manager_for_cmd,
        continuation,
        stage_build,
        forwarder_launcher,
    )
    .await;

    // Send canonical terminal outcome to the event pump before it pushes done.
    let telemetry_outcome = peri_acp_types::session::TurnTelemetryOutcome::from_result(
        exec_outcome.stop_reason,
        exec_outcome.failure.clone(),
    );
    let _ = stop_reason_tx.send((exec_outcome.stop_reason, telemetry_outcome));

    let result = collect_result(CollectRequest {
        event_tx: &event_tx,
        pump_handle,
        session_id: &ctx.session_id,
        exec_outcome,
    })
    .await;

    // turn 收尾：转发器是 session 级（挂 AgentPool），turn 间不清理、不重建，
    // 靠下一 turn `build_agent` 覆盖式 `set` 当前 handler。残留 handler（v1
    // 直发）经 `EventPublisher` 发射，事件泵随本轮 `event_tx` 关闭
    // 退出后到达的事件自然丢弃（与迁移前 close 后检查 None 丢弃语义一致）。

    result
}

#[cfg(test)]
#[path = "executor_test.rs"]
mod tests;

#[cfg(test)]
#[path = "executor_prediction_test.rs"]
mod prediction_tests;
