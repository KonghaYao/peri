//! SubAgent 后台非 Fork 路径：v2 stages 实现
//!
//! `run_in_background: true` 且非 fork 的执行路径：
//! 1. 通过 `build_agent_from_def` 装配 v2 字段（cancel_policy=Independent）
//! 2. 构造 v2 StageContext（与同步路径相同，区别仅在执行时机）
//! 3. tokio::spawn 内运行 `run_react_loop`，主流程立即返回
//! 4. 任务完成时通过 bg_event_sender 通知主 agent + lifecycle hook + thread_store 更新

use std::sync::Arc;

use peri_agent::{
    agent::{
        events::{ExecutorEvent, FnEventHandler},
        stages::run_react_loop,
    },
    messages::BaseMessage,
    tools::BaseTool,
};

use super::build_agent::CancelPolicy;
use super::lifecycle::{emit_subagent_stop_bg, BgCleanupGuard, BgStopEmit, BgStopEmitV2};
use crate::{
    hooks::types::{HookEvent, RegisteredHook},
    subagent::{
        background::{
            BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus, BgCancelHandle,
            BgTaskKind,
        },
        v2_bridge::build_v2_subagent_context,
    },
};

impl super::SubAgentTool {
    pub(crate) async fn invoke_background(
        &self,
        prompt: String,
        subagent_type: Option<String>,
        cwd: String,
        is_fork: bool,
        parent_messages: Vec<BaseMessage>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let registry = self
            .background_registry
            .as_ref()
            .ok_or("Background tasks not available: no registry configured")?;

        if registry.active_count() >= 3 {
            return Err("Error: maximum 3 concurrent background tasks reached. \
                 Wait for a running task to complete before starting a new one."
                .into());
        }

        let task_id = format!("bg-{}", uuid::Uuid::new_v4());

        if is_fork {
            return self
                .invoke_background_fork(prompt, cwd, task_id, registry, parent_messages)
                .await;
        }

        let agent_id =
            match &subagent_type {
                Some(id) => id.clone(),
                None => return Err(
                    "Error: background mode requires subagent_type parameter (or use fork: true)"
                        .into(),
                ),
            };

        let agent_def = match self.load_agent_def(&agent_id, &cwd) {
            Ok(a) => a,
            Err(e) => return Err(e.into()),
        };

        let build_result = self
            .build_agent_from_def(
                &agent_def,
                &agent_id,
                &cwd,
                CancelPolicy::Independent,
                true,  // skip_events
                false, // don't setup event handler
            )
            .await?;

        let child_thread_id = build_result.child_thread_id.clone();
        let max_iterations = build_result.max_iterations;
        let prompt_summary: String = prompt.chars().take(100).collect();

        // Independent cancel token（父 cancel 不传播）。
        // clone 一份用于 BackgroundTask.cancel_token 与 register_runtime（注册成功后调用）；
        // 原 token move 进 v2_ctx。
        let cancel_token = build_result.cancel_token.clone().unwrap_or_default();

        // 组装 MiddlewareChain
        let mut chain = peri_agent::middleware::chain::MiddlewareChain::new();
        for mw in build_result.middlewares {
            chain.add(mw);
        }

        // tools: Vec<Box<dyn BaseTool>> → Vec<Arc<dyn BaseTool>>
        let tools: Vec<Arc<dyn BaseTool>> = build_result
            .tools
            .into_iter()
            .map(|t| Arc::from(t) as Arc<dyn BaseTool>)
            .collect();

        // 构造 v2 StageContext（非 fork 路径不注入 parent_messages）
        // 注入 event_handler，使 SubAgent 内 LLM 的重试事件能被父级 Langfuse 追踪
        let mut llm = build_result.llm;
        llm.inject_event_handler(self.event_handler.clone());
        let v2_ctx = build_v2_subagent_context(
            llm,
            chain,
            tools,
            &cwd,
            cancel_token.clone(),
            Vec::new(),
            build_result.system_prompt,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(crate::subagent::v2_bridge::agent_id_from_child_thread(
                &child_thread_id,
            )),
        );

        // push prompt 到 queue
        v2_ctx
            .context
            .session
            .queue
            .push(peri_agent::session::queue::QueuedMessage::new(
                peri_agent::session::queue::MessageKind::Prompt,
                peri_agent::session::queue::MessageSource::UserInput,
                BaseMessage::human(prompt.clone()),
            ));

        // SubagentStarted 事件 + lifecycle hook（is_background=true）+ v2 SubagentStart
        // S3.1 起移入 spawn 闭包内、注册成功之后 emit——注册失败时零事件，
        // 不产生"已 emit SubagentStarted 无配对 Stop"的 depth 错乱。
        // [arch-align] 通过 bg_event_sender（BG pump，独立通道）发送，与 fork 路径
        // （spawner.rs）对齐。避免主 pump（event_handler → event_tx）在主 agent 结束后
        // 被 close_channel 关闭导致的时序风险——BG pump 独立运行，直到所有
        // bg_event_tx clones drop（即所有 bg task 完成）。

        // 捕获 spawn 所需资源
        let registered_hooks = self.registered_hooks.clone();
        let thread_store = self.thread_store.clone();
        let deregister_runtime = self.deregister_runtime.clone();
        let bg_event_sender = self.bg_event_sender.clone();
        let on_bg_complete = self.on_bg_complete.clone();
        let langfuse_bridge = self.langfuse_bridge.clone();
        let registry_spawn = Arc::clone(registry);
        let task_id_clone = task_id.clone();
        let child_thread_id_clone = child_thread_id.clone();
        let agent_name_clone = agent_id.clone();
        let prompt_summary_clone = prompt_summary.clone();
        let cwd_clone = cwd.clone();
        // C3: Stop emit 的父 agent 身份（闭包外读取一次）
        let parent_agent_id_for_stop = *self.parent_agent_id.read();

        // S3.1 注册门控：spawn 包装任务，闭包第一步 await 注册结果 oneshot。
        // 注册失败（并发撞 kind 上限）时包装任务直接 return——不跑 run_react_loop、
        // 不 emit 任何事件；注册成功才继续。规避 tokio 无"先注册后 spawn"API
        // （AbortHandle 只能来自 JoinHandle）的限制。
        let (reg_tx, reg_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

        // tokio::spawn 执行 v2 ReAct 循环
        let join_handle = tokio::spawn(async move {
            // S3.1 门控：注册结果（失败时调用方已发 Err；sender 被 drop 同样返回）
            match reg_rx.await {
                Ok(Ok(())) => {}
                _ => return,
            }

            let started_at = std::time::Instant::now();
            // context 将被 move 进 run_react_loop，turn_id 提前提取（Stop emit 用）
            let subagent_turn_id = v2_ctx.context.turn_id();
            let context = v2_ctx.context;
            let session = v2_ctx.session;
            // Start/Stop emit 需要 event_bus（partial move 后仍可用）+ 统一身份键
            let event_bus_for_emit = v2_ctx.event_bus.clone();
            let subagent_agent_id = v2_ctx.agent_id;
            let parent_agent_id = parent_agent_id_for_stop;

            // S3.2 同步收尾 guard：abort/panic 时 deregister_runtime + 补发
            // SubagentStopped（与上方 SubagentStarted 配对）。必须在本段事件 emit
            // 之前构造——abort 兜底路径（cancel 超时）触发 Drop 时收尾仍执行。
            let mut cleanup_guard = BgCleanupGuard {
                thread_id: child_thread_id_clone.clone(),
                deregister: deregister_runtime.clone(),
                stop: bg_event_sender.clone().map(|sender| BgStopEmit {
                    sender,
                    agent_name: agent_name_clone.clone(),
                    instance_id: child_thread_id_clone.clone(),
                }),
                // P1：v2 Stop 补发参数与下方 emit_subagent_stop_v2 调用参数同构
                // （C3 配对契约）。guard 在 v2 Start emit 之前构造，abort 兜底路径
                // 触发 Drop 时经 child EventBus 补发 v2 Stop，闭合 Langfuse AGENT span。
                stop_v2: Some(BgStopEmitV2 {
                    event_bus: event_bus_for_emit.clone(),
                    turn_id: subagent_turn_id,
                    parent_agent_id,
                    child_agent_id: subagent_agent_id,
                    agent_name: agent_name_clone.clone(),
                }),
            };

            // SubagentStarted 事件 + lifecycle hook（is_background=true）
            // [arch-align] 通过 bg_event_sender（BG pump，独立通道）发送，与 fork 路径
            // （spawner.rs）对齐。S3.1 起在注册成功后才 emit（注册失败零事件，
            // 无"已 emit Started 无配对 Stop"的 depth 错乱）。避免主 pump
            // （event_handler → event_tx）在主 agent 结束后被 close_channel 关闭的
            // 时序风险——BG pump 独立运行，直到所有 bg_event_tx clones drop。
            if let Some(ref sender) = bg_event_sender {
                let _ = sender.send(ExecutorEvent::SubagentStarted {
                    agent_name: agent_name_clone.clone(),
                    instance_id: child_thread_id_clone.clone(),
                    is_background: true,
                });
            } else {
                tracing::warn!(
                    agent = %agent_name_clone,
                    instance_id = %child_thread_id_clone,
                    "bg_event_sender unavailable, SubagentStarted event dropped"
                );
            }
            super::fire_subagent_lifecycle_hooks_static(
                &registered_hooks,
                HookEvent::SubagentStart,
                &cwd_clone,
                &agent_name_clone,
                None,
            )
            .await;

            // 补发 v2 SubagentStart（C2）：与 v1 SubagentStarted 同点、同通道（child EventBus）。
            // parent_agent_id 未注入（测试/嵌套异常）时静默跳过（helper 内 warn）。
            crate::subagent::v2_bridge::emit_subagent_start_v2(
                &event_bus_for_emit,
                subagent_turn_id,
                parent_agent_id,
                subagent_agent_id,
                &agent_name_clone,
                true,
            );

            // 启动 v2 事件转发器：消费 SubAgent EventBus 的事件，注入 source_agent_id
            // 后转发到父 Agent 的事件处理器。让 TUI 能看到 SubAgent 内的工具调用 / AI 文本。
            // 必须在 run_react_loop 之前取出 event_handles（之后 v2_ctx 已被 move）。
            //
            // [CORRECT] bg SubAgent 的 forwarder 必须使用 bg_event_sender（独立泵），
            // 而不是主 event_handler。主 event_handler → event_tx 在主 agent 的
            // TurnSuspended/TurnDone 后被 close_channel 关闭，导致 bg SubAgent 在
            // 主 turn 结束后执行的 ToolStart/ToolEnd 事件被丢弃。
            let bg_forwarder_handler: Option<
                Arc<dyn peri_agent::agent::events::AgentEventHandler>,
            > = bg_event_sender.clone().map(|tx| {
                Arc::new(FnEventHandler(move |ev: ExecutorEvent| {
                    let _ = tx.send(ev);
                })) as Arc<dyn peri_agent::agent::events::AgentEventHandler>
            });
            let _forwarder_handle =
                peri_agent::agent::subagent_event_forwarder::spawn_subagent_event_forwarder(
                    v2_ctx.event_handles,
                    bg_forwarder_handler,
                    langfuse_bridge,
                    child_thread_id_clone.clone(),
                );

            let loop_result = run_react_loop(context, max_iterations).await;

            // 补发 v2 SubagentStop（C3）：run_react_loop 返回后立即 emit，
            // 一个 emit 点覆盖 Completed / Interrupted / Error 三路且恰好一次。
            let (stop_result, stop_is_error) = match &loop_result {
                peri_agent::agent::stages::LoopResult::Completed => (
                    extract_last_ai_text(&session)
                        .chars()
                        .take(500)
                        .collect::<String>(),
                    false,
                ),
                peri_agent::agent::stages::LoopResult::Interrupted => {
                    ("interrupted".to_string(), true)
                }
                peri_agent::agent::stages::LoopResult::Error(e) => (
                    format!("Background sub-agent failed: {}", e)
                        .chars()
                        .take(500)
                        .collect::<String>(),
                    true,
                ),
            };
            crate::subagent::v2_bridge::emit_subagent_stop_v2(
                &event_bus_for_emit,
                subagent_turn_id,
                parent_agent_id,
                subagent_agent_id,
                &agent_name_clone,
                &stop_result,
                stop_is_error,
            );
            // 已显式 emit v2 SubagentStop：guard drop 时不得重复发射（P1 防双发）
            cleanup_guard.disarm_stop_v2();

            let (final_text, interrupted) = match loop_result {
                peri_agent::agent::stages::LoopResult::Completed => {
                    let text = extract_last_ai_text(&session);
                    (text, false)
                }
                peri_agent::agent::stages::LoopResult::Interrupted => (String::new(), true),
                peri_agent::agent::stages::LoopResult::Error(e) => {
                    let output = format!("Background sub-agent failed: {}", e);
                    // 错误路径：lifecycle hook + thread_store + registry notification
                    fire_subagent_stop_hooks(
                        &registered_hooks,
                        &cwd_clone,
                        &agent_name_clone,
                        &output,
                        true,
                    )
                    .await;
                    if let Some(ref store) = thread_store {
                        let _ = store
                            .update_thread_status(&child_thread_id_clone, "error")
                            .await;
                    }
                    // [arch-align] 错误分支也必须发射 SubagentStopped（is_error=true），
                    // 保证 subagent_depth 配对减 1（参考 fork 路径 spawner.rs:251-256）。
                    // 必须在 BackgroundTaskResult 构造之前发射——后者会 move output。
                    if let Some(ref sender) = bg_event_sender {
                        emit_subagent_stop_bg(
                            sender,
                            &agent_name_clone,
                            output.clone(),
                            true,
                            &child_thread_id_clone,
                        );
                    }
                    // 已显式 emit SubagentStopped：guard drop 时不得重复发射
                    cleanup_guard.disarm_stop();
                    let result = peri_agent::agent::events::BackgroundTaskResult {
                        task_id: task_id_clone.clone(),
                        agent_name: agent_name_clone.clone(),
                        prompt_summary: prompt_summary_clone.clone(),
                        success: false,
                        output,
                        tool_calls_count: crate::subagent::count_tool_calls_from_session(&session),
                        duration_ms: started_at.elapsed().as_millis() as u64,
                        child_thread_id: Some(child_thread_id_clone.clone()),
                        timed_out: false,
                    };
                    // 同步推送 Defer 到 MQ——必须在 registry.complete() 之前
                    if let Some(ref on_complete) = on_bg_complete {
                        on_complete(&result, BgTaskKind::Agent);
                    }
                    registry_spawn.complete(&task_id_clone, result);
                    // deregister 由 cleanup_guard drop 统一执行（正常/abort/panic 三路）
                    return;
                }
            };

            let output_summary: String = if interrupted {
                "interrupted".to_string()
            } else {
                final_text.chars().take(500).collect()
            };

            // SubagentStopped 事件 + lifecycle hook
            // [arch-align] 通过 bg_event_sender（BG pump）发送，与 fork 路径（spawner.rs:315）对齐。
            // 保证 subagent_depth 配对递减（handle_subagent_stop 处理）。
            if let Some(ref sender) = bg_event_sender {
                emit_subagent_stop_bg(
                    sender,
                    &agent_name_clone,
                    output_summary.clone(),
                    interrupted,
                    &child_thread_id_clone,
                );
            }
            // 已显式 emit SubagentStopped：guard drop 时不得重复发射
            cleanup_guard.disarm_stop();
            fire_subagent_stop_hooks(
                &registered_hooks,
                &cwd_clone,
                &agent_name_clone,
                &output_summary,
                interrupted,
            )
            .await;

            // thread_store 状态
            if let Some(ref store) = thread_store {
                let status = if interrupted { "cancelled" } else { "done" };
                let _ = store
                    .update_thread_status(&child_thread_id_clone, status)
                    .await;
            }

            // 后台任务完成通知（注入到主 agent 消息流）
            let result = peri_agent::agent::events::BackgroundTaskResult {
                task_id: task_id_clone.clone(),
                agent_name: agent_name_clone.clone(),
                prompt_summary: prompt_summary_clone.clone(),
                success: !interrupted,
                output: if interrupted {
                    "Background sub-agent was interrupted".to_string()
                } else {
                    final_text
                },
                tool_calls_count: crate::subagent::count_tool_calls_from_session(&session),
                duration_ms: started_at.elapsed().as_millis() as u64,
                child_thread_id: Some(child_thread_id_clone.clone()),
                timed_out: false,
            };
            if let Some(ref sender) = bg_event_sender {
                let _ = sender.send(ExecutorEvent::BackgroundTaskCompleted(result.clone()));
            } else {
                tracing::warn!(
                    task_id = %task_id_clone,
                    "bg_event_sender unavailable, BackgroundTaskCompleted event dropped"
                );
            }
            // 同步推送 Defer 到 MQ——必须在 registry.complete() 之前
            // 确保 active_count 归零时 Defer 已在 MQ 中
            if let Some(ref on_complete) = on_bg_complete {
                on_complete(&result, BgTaskKind::Agent);
            }
            registry_spawn.complete(&task_id_clone, result);
            // deregister 由 cleanup_guard drop 统一执行（正常/abort/panic 三路）
        });

        // 注册到 BackgroundTaskRegistry
        let bg_task = BackgroundTask {
            id: task_id.clone(),
            agent_name: agent_id.clone(),
            prompt_summary,
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            chrono_started_at: chrono::Utc::now(),
            kind: BgTaskKind::Agent,
            cancel_handle: BgCancelHandle::Abort(join_handle),
            cancel_token: Some(cancel_token.clone()),
            pid: None,
            output_preview: None,
        };
        if let Err(e) = registry.register_with_kind(bg_task) {
            // S3.1：注册失败（并发撞 kind 上限）——通知包装任务直接 return（不执行
            // run_react_loop、不 emit 任何事件），再如实返回错误。任务零事件零注册，
            // 无幽灵执行 / 无泄漏。
            let _ = reg_tx.send(Err(e.to_string()));
            return Err(format!("Failed to register background task: {}", e).into());
        }
        // 注册成功：先注册运行时（active_agents，与任务内 guard 的 deregister 配对），
        // 再放行包装任务继续执行。
        if let Some(register) = &self.register_runtime {
            register(child_thread_id.clone(), cancel_token, "independent".into());
        }
        let _ = reg_tx.send(Ok(()));

        if self.thread_store.is_some() {
            Ok(format!(
                "Background task {} started (thread: {}). You will be notified when it completes. \
                 You can continue with other tasks in the meantime.",
                task_id, child_thread_id
            ))
        } else {
            Ok(format!(
                "Background task {} started. You will be notified when it completes. \
                 You can continue with other tasks in the meantime.",
                task_id
            ))
        }
    }

    pub(crate) async fn invoke_background_fork(
        &self,
        prompt: String,
        cwd: String,
        _task_id: String,
        registry: &Arc<BackgroundTaskRegistry>,
        parent_messages: Vec<BaseMessage>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let parent_msgs = parent_messages;

        let mut llm = (self.llm_factory)(None);
        llm.inject_event_handler(self.event_handler.clone());
        let bg_sender = self
            .bg_event_sender
            .clone()
            .ok_or("Error: bg_event_sender not set for background fork")?;

        let config = crate::subagent::spawner::BgForkConfig {
            prompt: prompt.clone(),
            parent_messages: parent_msgs,
            cwd: std::path::PathBuf::from(&cwd),
            llm,
            max_iterations: 200,
            parent_tools: self.parent_tools.clone(),
            registered_hooks: Arc::clone(&self.registered_hooks),
            thread_store: self.thread_store.clone(),
            parent_thread_id: self.parent_thread_id.clone(),
            register_runtime: self.register_runtime.clone(),
            deregister_runtime: self.deregister_runtime.clone(),
            bg_event_sender: bg_sender,
            bg_registry: Arc::clone(registry),
            fork_directive_kind: crate::subagent::spawner::BgForkDirectiveKind::Fork,
            on_bg_complete: self.on_bg_complete.clone(),
            frozen_claude_md: self.frozen_claude_md.clone(),
            frozen_claude_local_md: self.frozen_claude_local_md.clone(),
            frozen_skill_summary: self.frozen_skill_summary.clone(),
            frozen_system_prompt: self.frozen_system_prompt.clone(),
            langfuse_bridge: self.langfuse_bridge.clone(),
            parent_agent_id: *self.parent_agent_id.read(),
        };

        let spawned = crate::subagent::spawner::spawn_background_fork(config).await?;

        // P2：v1 SubagentStarted 已移入 spawner 任务内（gate 放行后）emit，
        // 与 execute_bg 路径对齐——消除"任务快速完成/被 cancel 时 Stop 先于
        // Start 到达"（subagent_depth 先减后加）的窗口。此处不再发送。

        if self.thread_store.is_some() {
            Ok(format!(
                "Background task {} started (thread: {}). You will be notified when it completes.                  You can continue with other tasks in the meantime.",
                spawned.task_id, spawned.child_thread_id
            ))
        } else {
            Ok(format!(
                "Background task {} started. You will be notified when it completes.                  You can continue with other tasks in the meantime.",
                spawned.task_id
            ))
        }
    }
}

/// 触发 SubagentStop 生命周期 hook 的 helper
async fn fire_subagent_stop_hooks(
    registered_hooks: &Arc<Vec<RegisteredHook>>,
    cwd: &str,
    agent_name: &str,
    result: &str,
    is_error: bool,
) {
    use super::fire_subagent_lifecycle_hooks_static;
    let _ = is_error; // SubagentStop hook 不区分 error/正常
    fire_subagent_lifecycle_hooks_static(
        registered_hooks,
        HookEvent::SubagentStop,
        cwd,
        agent_name,
        Some(result),
    )
    .await;
}

/// 从 session transcript 提取最后一条非空 AI 消息文本
fn extract_last_ai_text(session: &Arc<peri_agent::session::Session>) -> String {
    // P1-11: 委托给 super::extract_last_ai_text（tool/mod.rs 共用实现）
    super::extract_last_ai_text(session)
}
