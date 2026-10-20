use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::FutureExt;
use peri_acp_types::identity::AgentId;
use tokio_util::sync::CancellationToken;

use super::lifecycle::drain_subagent_events;
use super::types::{
    ChildRunOutcome, SubagentFailure, SubagentLifecycleStart, SubagentLifecycleStop,
};
use super::util::{count_tool_calls_from_session, extract_last_ai_text, format_handoff_notice};
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
use crate::agent::subagent_event_forwarder::spawn_subagent_event_forwarder_for_completion;
use crate::agent::LangfuseBridgeLike;
use crate::session::factory::{DeregisterRuntimeFn, RegisterRuntimeFn};
use peri_acp_types::session_resources::{SessionMetaPatch, SessionResources};
use peri_acp_types::thread::{AgentStatus, ThreadId};

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
    on_bg_complete: Option<peri_acp_types::tasks::OnBgCompleteFn>,
    langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    session_resources: Option<Arc<dyn SessionResources>>,
    deregister_runtime: Option<DeregisterRuntimeFn>,
    on_subagent_start: Option<SubagentLifecycleStart>,
    on_subagent_stop: Option<SubagentLifecycleStop>,
    register_runtime: Option<RegisterRuntimeFn>,
    parent_agent_id: Option<AgentId>,
    parent_tool_call_id: Option<String>,
    cancel_token: CancellationToken,
    v2_ctx: V2SubagentContext,
    resume_claim: Option<&mut super::factory::ResumeClaim>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let task_manager =
        task_manager.ok_or("Background tasks not available: no task manager configured")?;
    let permit = v2_ctx
        .session
        .subagent_host()
        .and_then(|host| host.child_owner.clone())
        .map(|owner| owner.acquire(v2_ctx.context.session.turn.execution_binding()))
        .transpose()?;
    let permit_slot = Arc::new(parking_lot::Mutex::new(permit));
    let execution_permit = Arc::clone(&permit_slot);
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
    let panic_task_id = task_id.clone();
    let panic_thread_id = child_thread_id.clone();
    let panic_agent_name = agent_name.clone();
    let panic_prompt_summary = prompt_summary.clone();
    let panic_on_complete = on_bg_complete.clone();
    let panic_manager = Arc::clone(&task_manager);
    let panic_resources = session_resources.clone();
    let runtime_started = Arc::new(AtomicBool::new(false));
    let cleanup_runtime: Option<DeregisterRuntimeFn> = deregister_runtime.map(|deregister| {
        let runtime_started = runtime_started.clone();
        Arc::new(move |thread_id: &str| {
            if runtime_started.swap(false, Ordering::AcqRel) {
                deregister(thread_id);
            }
        }) as DeregisterRuntimeFn
    });
    let cleanup_guard = BgCleanupGuard {
        thread_id: child_thread_id.clone(),
        deregister: cleanup_runtime.clone(),
        stop: None,
    };

    let execution = async move {
        let mut cleanup_guard = cleanup_guard;
        let inbox_guard = inbox_guard;
        let mut permit = execution_permit.lock().take();
        // S3.1 门控：注册结果（失败时调用方已发 Err；sender 被 drop 同样返回）
        match reg_rx.await {
            Ok(Ok(())) => {}
            _ => {
                // 未开始执行：释放准入，不留下永久 unconfirmed 状态。
                if let Some(permit) = permit.take() {
                    permit.finish(Ok(()));
                }
                return;
            }
        }

        let started_at = peri_time::monotonic_now();
        // context 将被 move 进 run_react_loop，turn_id 提前提取（Start/Stop emit 用）
        let execution_turn = v2_ctx.context.session.turn.clone();
        let context = v2_ctx.context;
        let session = v2_ctx.session;
        // Start/Stop emit 需要 event_bus（partial move 后仍可用）+ 统一身份键
        let event_bus_for_emit = v2_ctx.event_bus;
        let subagent_agent_id = v2_ctx.agent_id;

        // S3.2 同步收尾 guard：abort/panic 时 deregister_runtime + 补发
        // v2 SubagentStop（含 v1 协议化直发，与 SubagentStarted 配对）。
        // 必须在本段事件 emit 之前构造。
        cleanup_guard.stop = Some(BgStopEmitV2 {
            event_bus: Arc::downgrade(&event_bus_for_emit),
            turn: execution_turn.clone(),
            parent_agent_id,
            child_agent_id: subagent_agent_id,
            agent_name: agent_name_for_task.clone(),
            // v1 协议化直发目标（bg 泵；None = 无 bg 通道，仅 v2 补发）
            sender: bg_event_sender.clone(),
        });
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

        // 真实子会话身份：hook 载荷 agent_id 的来源（不是 agent 名）。
        if let Some(on_start) = &on_subagent_start {
            on_start(
                &child_thread_id_for_task,
                &agent_name_for_task,
                &cwd_for_task,
            );
        }
        emit_subagent_start_v2(
            &event_bus_for_emit,
            execution_turn.turn_id(),
            parent_agent_id,
            subagent_agent_id,
            &agent_name_for_task,
            true,
            parent_tool_call_id.clone(),
        );
        forward_subagent_start_v1(
            bg_forwarder_handler.as_ref(),
            build_subagent_start_v2(
                execution_turn.turn_id(),
                parent_agent_id,
                subagent_agent_id,
                &agent_name_for_task,
                true,
                parent_tool_call_id,
            ),
        );
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

        let mut outcome =
            super::child_runner::run_child_until_terminal(context, max_iterations, &session).await;
        if let Err(error) = super::lifecycle::flush_session_history(&session).await {
            outcome = match outcome {
                ChildRunOutcome::HandedOff { receipt } => ChildRunOutcome::Failed {
                    error,
                    residual: Some(receipt),
                },
                ChildRunOutcome::Failed {
                    error: original,
                    residual,
                } => {
                    tracing::error!(%error, %original, "background child history flush failed after execution failure");
                    ChildRunOutcome::Failed {
                        error: original,
                        residual,
                    }
                }
                ChildRunOutcome::HandoffFailed { cause, failure } => {
                    tracing::error!(
                        %error,
                        "background child history flush failed after unconfirmed handoff; retaining structured failure"
                    );
                    let mut failure = failure;
                    failure.flush_error = Some(error.to_string());
                    ChildRunOutcome::HandoffFailed { cause, failure }
                }
                _ => ChildRunOutcome::Failed {
                    error,
                    residual: None,
                },
            };
        }
        if let ChildRunOutcome::Failed { error, .. } = &outcome {
            if !execution_turn.cancel_token.is_cancelled()
                && !matches!(error, crate::error::AgentError::Interrupted)
            {
                tracing::error!(
                    session_id = %child_thread_id_for_task,
                    turn_id = %execution_turn.turn_id(),
                    agent_name = %agent_name_for_task,
                    category = error.category_name(),
                    error = %error.user_facing_message(),
                    causes = ?error.cause_chain(),
                    "background child execution failed"
                );
            }
        }
        let interrupted = matches!(outcome, ChildRunOutcome::Interrupted);
        let explicit_close = interrupted
            || session.config().cancel_token.is_cancelled()
            || session
                .subagent_host()
                .is_some_and(|host| host.close_state.is_closing());
        if explicit_close {
            if let Some(owner) = session
                .subagent_host()
                .and_then(|host| host.child_owner.clone())
            {
                if let Err(error) = owner.begin_close_from_execution() {
                    tracing::error!(thread_id = %child_thread_id_for_task, %error, "child close intent registration failed");
                    cleanup_guard.deregister = None;
                    cleanup_guard.disarm_stop();
                    if let Some(permit) = permit.take() {
                        permit.finish(Err(error));
                    }
                    return;
                }
            }
            if let Some(permit) = permit.take() {
                permit.finish(Ok(()));
            }
        }
        if let Err(error) = super::close::settle_explicit_close(&session, interrupted).await {
            tracing::error!(thread_id = %child_thread_id_for_task, %error, "child close incomplete; delegation terminal withheld");
            cleanup_guard.deregister = None;
            cleanup_guard.disarm_stop();
            return;
        }
        // Stop accepting messages before any async terminal work or notifications.
        drop(inbox_guard);

        // Successful settlement and cooperative cancellation publish the terminal
        // result; handoff/failure paths report through the delivery callback only.
        let mut publish_completed = matches!(
            &outcome,
            ChildRunOutcome::Settled | ChildRunOutcome::Interrupted
        );
        let mut drain_failure: Option<String> = None;
        let (text, mut output_summary, mut status, success, failure) = match &outcome {
            ChildRunOutcome::Settled => {
                let text = extract_last_ai_text(&session);
                let summary = text.chars().take(500).collect::<String>();
                (text, summary, AgentStatus::Done, true, None)
            }
            ChildRunOutcome::Interrupted => (
                "Background sub-agent was interrupted".to_string(),
                "interrupted".to_string(),
                AgentStatus::Cancelled,
                false,
                None,
            ),
            ChildRunOutcome::HandedOff { receipt } => {
                let failure = SubagentFailure::handed_off(
                    &child_thread_id_for_task,
                    &agent_name_for_task,
                    receipt.clone(),
                );
                (
                    extract_last_ai_text(&session),
                    "delegation incomplete: pending tasks handed off".to_string(),
                    AgentStatus::Error,
                    false,
                    failure.safe_failure(),
                )
            }
            ChildRunOutcome::Failed { error, residual } => {
                let failure =
                    SubagentFailure::safe_failure_from_error(&child_thread_id_for_task, error);
                let mut output = format!(
                    "Background sub-agent failed: {}",
                    error.user_facing_message()
                );
                if let Some(receipt) = residual {
                    output.push_str("\n\n");
                    output.push_str(&format_handoff_notice(receipt));
                }
                let summary = output.chars().take(500).collect::<String>();
                (output, summary, AgentStatus::Error, false, failure)
            }
            ChildRunOutcome::HandoffFailed { cause, failure } => {
                let mut output = String::new();
                if let Some(cause) = cause {
                    output.push_str(&cause.user_facing_message());
                    output.push_str("\n\n");
                }
                output.push_str(&format!(
                    "{}: {}",
                    super::types::handoff_failure_label(failure),
                    failure.error
                ));
                if let Some(flush) = &failure.flush_error {
                    output.push_str(&format!("\nhandoff history flush failed: {flush}"));
                }
                if let Some(receipt) = &failure.receipt {
                    output.push_str("\n\n");
                    output.push_str(&format_handoff_notice(receipt));
                }
                let safe = match cause {
                    Some(cause) => {
                        SubagentFailure::safe_failure_from_error(&child_thread_id_for_task, cause)
                    }
                    None => {
                        let error = super::types::handoff_failure_agent_error(failure);
                        SubagentFailure::safe_failure_from_error(&child_thread_id_for_task, &error)
                    }
                };
                let summary = output.chars().take(500).collect::<String>();
                (output, summary, AgentStatus::Error, false, safe)
            }
        };
        let output = match &outcome {
            ChildRunOutcome::HandedOff { receipt } => {
                format!("{text}\n\n{}", format_handoff_notice(receipt))
            }
            _ => text,
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
        let subagent_turn_id = execution_turn.turn_id();
        emit_subagent_stop_v2_with_failure(
            &event_bus_for_emit,
            SubagentStopV2Input {
                turn_id: subagent_turn_id,
                parent_agent_id,
                child_agent_id: subagent_agent_id,
                agent_name: &agent_name_for_task,
                result: &output_summary,
                is_error: !result.success,
                subagent_failure: result.subagent_failure.clone(),
            },
        );
        // The guard retains only a Weak producer and stays armed throughout
        // drain, so forced abort still pairs Started with exactly one Stopped.
        if let Err(error) =
            drain_subagent_events(event_bus_for_emit, forwarder_handle, langfuse_bridge).await
        {
            tracing::error!(thread_id = %child_thread_id_for_task, error = %error.user_facing_message(), causes = ?error.cause_chain(), "child event forwarding barrier incomplete");
            drain_failure = Some(error.to_string());
            if result.success {
                let failure =
                    SubagentFailure::new(&child_thread_id_for_task, &agent_name_for_task, error);
                result.output =
                    format!("Background sub-agent failed: {}", failure.public_message());
                output_summary = result.output.chars().take(500).collect();
                result.subagent_failure = failure.safe_failure();
                result.success = false;
                publish_completed = false;
                status = AgentStatus::Error;
            } else {
                result.output = format!(
                    "{}\n\nchild event drain failed: {}",
                    result.output,
                    error.user_facing_message()
                );
                output_summary = result.output.chars().take(500).collect();
            }
        }
        if let Some(ref on_stop) = on_subagent_stop {
            on_stop(
                &child_thread_id_for_task,
                &agent_name_for_task,
                &cwd_for_task,
                &output_summary,
                !result.success,
            );
        }
        let terminal_persisted = persist_background_terminal(
            session_resources.as_deref(),
            &child_thread_id_for_task,
            status,
            &mut result,
        )
        .await;
        if !terminal_persisted {
            publish_completed = false;
            output_summary = result.output.chars().take(500).collect();
        }
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
        settle_background_terminal(&task_manager_spawn, result, on_bg_complete);
        let owner = session
            .subagent_host()
            .and_then(|host| host.child_owner.clone());
        let permit_result = if !terminal_persisted {
            Err("terminal_status_persist_failed".to_string())
        } else if let Some(error) = drain_failure.as_ref() {
            Err(format!("child event drain failed: {error}"))
        } else if matches!(outcome, ChildRunOutcome::HandoffFailed { .. }) {
            Err("child handoff persistence is unconfirmed".to_string())
        } else if matches!(outcome, ChildRunOutcome::Failed { .. })
            && owner
                .as_ref()
                .is_some_and(|owner| owner.has_unsettled_obligations())
        {
            Err("child settlement is unconfirmed after execution failure".to_string())
        } else {
            Ok(())
        };
        if let Some(permit) = permit {
            permit.finish(permit_result);
        }
        // deregister 由 cleanup_guard drop 统一执行（正常/abort/panic 三路）
    };
    let join_handle = match peri_acp_types::tasks::TaskManager::spawn_owned(
        task_manager.as_ref(),
        Box::pin(async move {
            let started_at = peri_time::monotonic_now();
            if let Err(payload) = AssertUnwindSafe(execution).catch_unwind().await {
                let diagnostic = payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| payload.downcast_ref::<&str>().copied())
                    .unwrap_or("non-string panic payload");
                tracing::error!(
                    task_id = %panic_task_id,
                    thread_id = %panic_thread_id,
                    panic = %diagnostic,
                    "background subagent execution panicked"
                );
                let mut result = crate::agent::events::BackgroundTaskResult {
                    task_id: panic_task_id.clone(),
                    agent_name: panic_agent_name,
                    prompt_summary: panic_prompt_summary,
                    success: false,
                    output: format!("Background sub-agent execution panicked: {diagnostic}"),
                    tool_calls_count: 0,
                    duration_ms: started_at.elapsed().as_millis() as u64,
                    child_thread_id: Some(panic_thread_id.clone()),
                    timed_out: false,
                    subagent_failure: None,
                    shell_output: None,
                };
                persist_background_terminal(
                    panic_resources.as_deref(),
                    &panic_thread_id,
                    AgentStatus::Error,
                    &mut result,
                )
                .await;
                settle_background_terminal(&panic_manager, result, panic_on_complete);
            }
            // panic 已在边界内收敛；正常返回让 spawn_owned 确认执行已停止。
        }),
    ) {
        Ok(handle) => handle,
        Err(error) => {
            if let Some(permit) = permit_slot.lock().take() {
                permit.finish(Ok(()));
            }
            return Err(error.into());
        }
    };

    // 注册到 BackgroundTaskRegistry
    let bg_task = BackgroundTask {
        id: task_id.clone(),
        agent_name: agent_name.clone(),
        prompt_summary,
        status: BackgroundTaskStatus::Running,
        started_at: peri_time::monotonic_now(),
        chrono_started_at: peri_time::now_wall().into(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(join_handle),
        cancel_token: Some(cancel_token.clone()),
        pid: None,
        output_preview: None,
        agent_inbox: Some(agent_inbox),
        // 本地 Agent owner 任务：投递归属是父会话，但该身份不在本函数作用域内，
        // 记录为 None（MCP 外部任务的投递归属在 tool dispatch 侧记录）。
        initiator_session_id: None,
        owner_session_id: None,
        owner_identity: None,
    };
    if let Err(e) = task_manager.register_with_kind(bg_task) {
        // S3.1：注册失败（Agent 类无并发上限，失败仅剩 session execution scope
        // 关闭一类）——通知包装任务直接 return（不执行 run_react_loop、不 emit
        // 任何事件），再如实返回错误。任务零事件零注册，无幽灵执行 / 无泄漏。
        let _ = reg_tx.send(Err(e.to_string()));
        return Err(format!("Failed to register background task: {}", e).into());
    }
    let mut registration = PendingBackgroundRegistration {
        manager: task_manager.clone(),
        task_id: Some(task_id.clone()),
        runtime_cleanup: cleanup_runtime.map(|cleanup| (child_thread_id.clone(), cleanup)),
    };
    let handoff = match resume_claim {
        Some(claim) => Some(claim.release().await?),
        None => None,
    };
    // 注册成功：先注册运行时（active_agents，与任务内 guard 的 deregister 配对），
    // 再放行包装任务继续执行。
    runtime_started.store(true, Ordering::Release);
    if let Some(register) = &register_runtime {
        register(child_thread_id.clone(), cancel_token, "independent".into());
    }
    reg_tx
        .send(Ok(()))
        .map_err(|_| "background execution stopped before startup acceptance")?;
    if let Some(handoff) = handoff {
        handoff.accept()?;
    }
    registration.task_id = None;
    registration.runtime_cleanup = None;

    Ok(())
}

async fn persist_background_terminal(
    store: Option<&dyn SessionResources>,
    thread_id: &str,
    status: AgentStatus,
    result: &mut crate::agent::events::BackgroundTaskResult,
) -> bool {
    let Some(store) = store else {
        return true;
    };
    if let Err(error) = store
        .update_session_meta(
            &ThreadId::from(thread_id),
            &SessionMetaPatch {
                status: Some(status),
                ..Default::default()
            },
        )
        .await
    {
        tracing::error!(%thread_id, %error, "subagent terminal status write failed");
        result.success = false;
        result.output = format!(
            "{}; subagent terminal status write failed: {error}",
            result.output
        );
        return false;
    }
    true
}

fn settle_background_terminal(
    manager: &TaskManager,
    result: crate::agent::events::BackgroundTaskResult,
    delivery: Option<peri_acp_types::tasks::OnBgCompleteFn>,
) {
    let task_id = result.task_id.clone();
    if let Some(delivery) = delivery {
        if let Err(error) = manager.settle_completed(&task_id, result, delivery) {
            tracing::error!(%task_id, %error, "subagent terminal delivery is pending");
        }
    } else {
        manager.complete(&task_id, result);
    }
}

struct PendingBackgroundRegistration {
    manager: Arc<TaskManager>,
    task_id: Option<String>,
    runtime_cleanup: Option<(String, DeregisterRuntimeFn)>,
}

impl Drop for PendingBackgroundRegistration {
    fn drop(&mut self) {
        if let Some(task_id) = self.task_id.take() {
            if let Err(error) = self.manager.cancel(&task_id) {
                tracing::error!(%task_id, %error, "background preparation cancellation failed");
            }
        }
        if let Some((thread_id, cleanup)) = self.runtime_cleanup.take() {
            cleanup(&thread_id);
        }
    }
}
