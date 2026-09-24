use std::sync::Arc;

use peri_acp_types::identity::AgentId;
use tokio_util::sync::CancellationToken;

use super::lifecycle::drain_subagent_events;
use super::types::{SubagentFailure, SubagentLifecycleStart, SubagentLifecycleStop};
use super::util::{count_tool_calls_from_session, extract_last_ai_text};
use super::v2_bridge::{forward_subagent_start_v1, forward_subagent_stop_v1, V2SubagentContext};
use super::{
    build_subagent_start_v2, build_subagent_stop_v2_with_failure, emit_subagent_start_v2,
    emit_subagent_stop_v2_with_failure, BgCleanupGuard, BgStopEmitV2, SubagentStopV2Input,
};
use crate::agent::async_tasks::{
    BackgroundAgentInbox, BackgroundAgentInboxGuard, BackgroundTask, BackgroundTaskStatus,
    BgCancelHandle, BgTaskKind, TaskManager,
};
use crate::agent::events::{AgentEventHandler, ExecutorEvent};
use crate::agent::stages::{run_react_loop, LoopResult};
use crate::agent::subagent_event_forwarder::spawn_subagent_event_forwarder_for_completion;
use crate::agent::LangfuseBridgeLike;
use crate::session::factory::{DeregisterRuntimeFn, RegisterRuntimeFn};
use crate::thread::ThreadStore;

// ─── 后台运行 ────────────────────────────────────────────────────────────────

/// 后台子 agent：tokio::spawn 包装运行 + TaskManager 注册（S3.1 gate）+ 收尾。
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) async fn spawn_background_subagent(
    task_id: String,
    child_thread_id: String,
    agent_name: String,
    prompt: String,
    cwd: String,
    max_iterations: usize,
    bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    task_manager: Option<Arc<TaskManager>>,
    on_bg_complete: Option<
        Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>,
    >,
    langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    thread_store: Option<Arc<dyn ThreadStore>>,
    deregister_runtime: Option<DeregisterRuntimeFn>,
    on_subagent_start: Option<SubagentLifecycleStart>,
    on_subagent_stop: Option<SubagentLifecycleStop>,
    register_runtime: Option<RegisterRuntimeFn>,
    parent_agent_id: Option<AgentId>,
    cancel_token: CancellationToken,
    v2_ctx: V2SubagentContext,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let task_manager =
        task_manager.ok_or("Background tasks not available: no task manager configured")?;
    let task_manager_spawn = Arc::clone(&task_manager);
    let agent_inbox = BackgroundAgentInbox::new(
        child_thread_id.clone(),
        v2_ctx.session.queue().clone(),
        cancel_token.clone(),
    );
    let inbox_guard = BackgroundAgentInboxGuard(Arc::clone(&agent_inbox));

    let prompt_summary: String = prompt.chars().take(100).collect();

    // S3.1 注册门控：spawn 包装任务，闭包第一步 await 注册结果 oneshot。
    let (reg_tx, reg_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

    let task_id_for_task = task_id.clone();
    let child_thread_id_for_task = child_thread_id.clone();
    let agent_name_for_task = agent_name.clone();
    let prompt_summary_for_task = prompt_summary.clone();
    let cwd_for_task = cwd.clone();

    let execution = async move {
        // S3.1 门控：注册结果（失败时调用方已发 Err；sender 被 drop 同样返回）
        match reg_rx.await {
            Ok(Ok(())) => {}
            _ => return,
        }

        let started_at = std::time::Instant::now();
        // context 将被 move 进 run_react_loop，turn_id 提前提取（Start/Stop emit 用）
        let subagent_turn_id = v2_ctx.context.turn_id();
        let context = v2_ctx.context;
        let session = v2_ctx.session;
        // Start/Stop emit 需要 event_bus（partial move 后仍可用）+ 统一身份键
        let event_bus_for_emit = v2_ctx.event_bus;
        let subagent_agent_id = v2_ctx.agent_id;

        // S3.2 同步收尾 guard：abort/panic 时 deregister_runtime + 补发
        // v2 SubagentStop（含 v1 协议化直发，与 SubagentStarted 配对）。
        // 必须在本段事件 emit 之前构造。
        let mut cleanup_guard = BgCleanupGuard {
            thread_id: child_thread_id_for_task.clone(),
            deregister: deregister_runtime.clone(),
            stop: Some(BgStopEmitV2 {
                event_bus: Arc::downgrade(&event_bus_for_emit),
                turn_id: subagent_turn_id,
                parent_agent_id,
                child_agent_id: subagent_agent_id,
                agent_name: agent_name_for_task.clone(),
                // v1 协议化直发目标（bg 泵；None = 无 bg 通道，仅 v2 补发）
                sender: bg_event_sender.clone(),
            }),
        };
        // Declared after cleanup_guard so panic/abort revokes admission before
        // cleanup publishes deregistration or SubagentStop (reverse drop order).
        let inbox_guard = inbox_guard;

        // v1 协议化发射目标（bg 泵）：BG pump 独立于主 pump，主 turn 结束后仍存活。
        // 构造提前到 Started 直发之前（start 借用、stop 直发 clone、forwarder move）。
        let bg_forwarder_handler: Option<Arc<dyn AgentEventHandler>> =
            bg_event_sender.clone().map(|tx| {
                Arc::new(crate::agent::events::FnEventHandler(
                    move |ev: ExecutorEvent| {
                        let _ = tx.send(ev);
                    },
                )) as Arc<dyn AgentEventHandler>
            });
        let bg_stop_handler = bg_forwarder_handler.clone();

        // lifecycle hook（SubagentStart）
        if let Some(ref on_start) = on_subagent_start {
            on_start(&agent_name_for_task, &cwd_for_task);
        }

        // v2 SubagentStart（C2）：与 lifecycle hook 同点、同通道（child EventBus）。
        emit_subagent_start_v2(
            &event_bus_for_emit,
            subagent_turn_id,
            parent_agent_id,
            subagent_agent_id,
            &agent_name_for_task,
            true,
        );
        // v1 协议化载体直发（SubagentStarted）：发射语义单一事实源为 v2 事件构造
        // （ObserveEvent 身份透传：child_agent_id → instance_id），经
        // `observe_event_to_executor` 同步映射后直发 bg_event_sender——同步保证
        // Started 恒先于任何 SubagentStopped / BackgroundTaskCompleted
        // （正常/取消/abort 三路，P2 顺序契约）。
        if bg_event_sender.is_some() {
            forward_subagent_start_v1(
                bg_forwarder_handler.as_ref(),
                build_subagent_start_v2(
                    subagent_turn_id,
                    parent_agent_id,
                    subagent_agent_id,
                    &agent_name_for_task,
                    true,
                ),
            );
        } else {
            tracing::warn!(
                agent = %agent_name_for_task,
                instance_id = %child_thread_id_for_task,
                "bg_event_sender unavailable, SubagentStarted event dropped"
            );
        }

        // 启动 v2 事件转发器：消费 SubAgent EventBus 的事件，注入 source_agent_id
        // 后转发到 bg_event_sender（BG pump 独立于主 pump，主 turn 结束后仍存活）。
        // SubagentStart/Stop 不在此转发（发射侧已同步协议化直发，防双发——
        // 见 `forward_subagent_start_v1` / `forward_subagent_stop_v1`）。
        let forwarder_handle = spawn_subagent_event_forwarder_for_completion(
            v2_ctx.event_handles,
            bg_forwarder_handler,
            langfuse_bridge.clone(),
            child_thread_id_for_task.clone(),
        );

        let loop_result = run_react_loop(context, max_iterations).await;
        // Stop accepting messages before any async terminal work or notifications.
        drop(inbox_guard);

        // Errors report their terminal result through the callback/TaskManager;
        // only successful completion and cooperative cancellation emit Completed.
        let mut publish_completed = !matches!(&loop_result, LoopResult::Error(_));
        let (output, output_summary, status, success, failure) = match loop_result {
            LoopResult::Completed => {
                let text = extract_last_ai_text(&session);
                let summary = text.chars().take(500).collect::<String>();
                (text, summary, "done", true, None)
            }
            LoopResult::Interrupted => (
                "Background sub-agent was interrupted".to_string(),
                "interrupted".to_string(),
                "cancelled",
                false,
                None,
            ),
            LoopResult::Error(error) => {
                let failure =
                    SubagentFailure::new(&child_thread_id_for_task, &agent_name_for_task, error);
                let output = format!("Background sub-agent failed: {}", failure.public_message());
                let summary = output.chars().take(500).collect::<String>();
                (output, summary, "error", false, failure.safe_failure())
            }
        };
        let mut result = crate::agent::events::BackgroundTaskResult {
            task_id: task_id_for_task.clone(),
            agent_name: agent_name_for_task.clone(),
            prompt_summary: prompt_summary_for_task.clone(),
            success,
            output,
            tool_calls_count: count_tool_calls_from_session(&session),
            duration_ms: started_at.elapsed().as_millis() as u64,
            child_thread_id: Some(child_thread_id_for_task.clone()),
            timed_out: false,
            subagent_failure: failure.clone(),
            shell_output: None,
        };
        let mut output_summary = output_summary;
        let mut status = status;
        emit_subagent_stop_v2_with_failure(
            &event_bus_for_emit,
            SubagentStopV2Input {
                turn_id: subagent_turn_id,
                parent_agent_id,
                child_agent_id: subagent_agent_id,
                agent_name: &agent_name_for_task,
                result: &output_summary,
                is_error: !result.success,
                subagent_failure: failure,
            },
        );
        // The guard retains only a Weak producer and stays armed throughout
        // drain, so forced abort still pairs Started with exactly one Stopped.
        if let Err(error) =
            drain_subagent_events(event_bus_for_emit, forwarder_handle, langfuse_bridge).await
        {
            // Cancellation and typed model failure remain authoritative, but
            // successful model completion cannot hide a broken event stream.
            if result.success {
                result.success = false;
                publish_completed = false;
                result.output = format!(
                    "Background sub-agent failed: {}",
                    error.user_facing_message()
                );
                output_summary = result.output.clone();
                status = "error";
            }
        }
        result.duration_ms = started_at.elapsed().as_millis() as u64;
        forward_subagent_stop_v1(
            bg_stop_handler.as_ref(),
            build_subagent_stop_v2_with_failure(
                subagent_turn_id,
                parent_agent_id,
                subagent_agent_id,
                &agent_name_for_task,
                &output_summary,
                !result.success,
                result.subagent_failure.clone(),
            ),
        );
        cleanup_guard.disarm_stop();
        if let Some(ref on_stop) = on_subagent_stop {
            on_stop(
                &agent_name_for_task,
                &cwd_for_task,
                &output_summary,
                !result.success,
            );
        }
        if let Some(ref store) = thread_store {
            let _ = store
                .update_thread_status(&child_thread_id_for_task, status)
                .await;
        }

        // Preserve the error-path protocol: Stopped is the last wire event,
        // while the typed result still reaches the shared completion callback.
        if publish_completed {
            if let Some(ref sender) = bg_event_sender {
                let _ = sender.send(ExecutorEvent::BackgroundTaskCompleted(result.clone()));
            } else {
                tracing::warn!(
                    task_id = %task_id_for_task,
                    "bg_event_sender unavailable, BackgroundTaskCompleted event dropped"
                );
            }
        }
        // 同步推送 Defer 到 MQ——必须在 registry.complete() 之前
        // 确保 active_count 归零时 Defer 已在 MQ 中
        if let Some(ref on_complete) = on_bg_complete {
            on_complete(&result, BgTaskKind::Agent);
        }
        task_manager_spawn.complete(&task_id_for_task, result);
        // deregister 由 cleanup_guard drop 统一执行（正常/abort/panic 三路）
    };
    let join_handle = peri_acp_types::tasks::TaskManager::spawn_owned(
        task_manager.as_ref(),
        Box::pin(execution),
    )?;

    // 注册到 BackgroundTaskRegistry
    let bg_task = BackgroundTask {
        id: task_id.clone(),
        agent_name: agent_name.clone(),
        prompt_summary,
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(join_handle),
        cancel_token: Some(cancel_token.clone()),
        pid: None,
        output_preview: None,
        agent_inbox: Some(agent_inbox),
    };
    if let Err(e) = task_manager.register_with_kind(bg_task) {
        // S3.1：注册失败（Agent 类无并发上限，失败仅剩 session execution scope
        // 关闭一类）——通知包装任务直接 return（不执行 run_react_loop、不 emit
        // 任何事件），再如实返回错误。任务零事件零注册，无幽灵执行 / 无泄漏。
        let _ = reg_tx.send(Err(e.to_string()));
        return Err(format!("Failed to register background task: {}", e).into());
    }
    // 注册成功：先注册运行时（active_agents，与任务内 guard 的 deregister 配对），
    // 再放行包装任务继续执行。
    if let Some(register) = &register_runtime {
        register(child_thread_id.clone(), cancel_token, "independent".into());
    }
    let _ = reg_tx.send(Ok(()));

    Ok(())
}
