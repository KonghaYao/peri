use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::FutureExt;
use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, OnBgCompleteFn, TaskManager as _};
use tokio::sync::oneshot;

use peri_acp_types::identity::AgentId;

use super::factory::ResumeClaim;
use super::lifecycle::drain_subagent_events;
use super::sync_settlement::{
    parent_result, settle_with_retry, terminal_status, terminal_status_for_outcome,
    SyncExecutionResult,
};
use super::terminal_settlement::{
    settle_terminal_without_result, TerminalMqRoute, TerminalOwnership,
};
use super::types::{
    ChildHandoffReason, ChildHandoffReceipt, ChildRunOutcome, SubagentFailure,
    SubagentLifecycleStart, SubagentLifecycleStop,
};
use super::util::extract_last_ai_text;
use super::v2_bridge::{forward_subagent_start_v1, forward_subagent_stop_v1, V2SubagentContext};
use super::{
    build_subagent_start_v2, build_subagent_stop_v2_with_failure, emit_subagent_start_v2,
    emit_subagent_stop_v2_with_failure, on_subagent_stop_handler, DeregisterGuard,
    SubagentStopV2Input,
};
use crate::agent::async_tasks::TaskManager;
use crate::agent::events::{AgentEventHandler, ExecutorEvent, FnEventHandler};
use crate::agent::subagent_event_forwarder::spawn_subagent_event_forwarder_for_completion;
use crate::agent::LangfuseBridgeLike;
use crate::session::factory::{DeregisterRuntimeFn, RegisterRuntimeFn};
use crate::session::Session;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::thread::{ThreadId, WorkState};

#[cfg(test)]
#[path = "run_sync_test.rs"]
mod tests;

#[cfg(test)]
#[path = "run_sync_settlement_test.rs"]
mod settlement_tests;

#[cfg(test)]
#[path = "run_sync_sqlite_settlement_test.rs"]
mod sqlite_settlement_tests;

pub(super) struct SyncPending {
    pub receipt: ChildHandoffReceipt,
    pub pending_tasks: Vec<String>,
}

/// 交接信号：receipt 通道与终态归属同行，避免执行体与 worker 各写一次终态。
pub(super) struct SyncHandoffSignal {
    sender: oneshot::Sender<ChildHandoffReceipt>,
    terminal: Arc<TerminalOwnership>,
    /// wrapper 在无 detach 时也有存储可收敛；否则执行体必须自己写终态。
    wrapper_settles: bool,
}

pub(super) enum SyncCompletion {
    Finished(ChildRunOutcome),
    Pending(SyncPending),
}

type SyncExecutionTask = tokio::task::JoinHandle<SyncExecutionResult>;

/// 父 future 只消费已 spawn 的子执行结果；JoinError 只可能来自子任务 panic
/// （cancel 只 detach，不 abort），必须显式转成 typed 失败并留日志。
fn join_error(error: tokio::task::JoinError) -> Box<dyn std::error::Error + Send + Sync> {
    tracing::error!(%error, "sync child execution task failed");
    format!("sync child execution task failed: {error}").into()
}

fn flatten_execution(
    joined: Result<SyncExecutionResult, tokio::task::JoinError>,
) -> SyncExecutionResult {
    joined.map_err(join_error)?
}

async fn await_execution(execution: SyncExecutionTask) -> SyncExecutionResult {
    flatten_execution(execution.await)
}

/// 父工具调用 future 被 drop 时必须终止子执行：子任务由独立 tokio 任务持有，
/// drop JoinHandle 只会 detach。成功交接后 disarm——此后的执行由 session 资源
/// owner 持有，父调用 drop 不再代表取消。
struct SyncChildCancelGuard {
    session: Option<Arc<Session>>,
}

impl SyncChildCancelGuard {
    fn new(session: Arc<Session>) -> Self {
        Self {
            session: Some(session),
        }
    }

    fn disarm(&mut self) {
        self.session = None;
    }
}

impl Drop for SyncChildCancelGuard {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            // 生产 cancel 路径已由父 token cascade 取消，这里是幂等的二次取消；
            // 同时覆盖 caller 被 drop 而自身令牌未取消（abort / 宿主放弃）的场景。
            session.config().cancel_token.cancel();
        }
    }
}

/// wrapper 仍可能接管终态：Drop 后执行体不再让位，自己写终态。
struct SyncWrapperAlive(Arc<TerminalOwnership>);

impl Drop for SyncWrapperAlive {
    fn drop(&mut self) {
        self.0.wrapper_finished();
    }
}

pub(super) fn new_sync_task_id() -> String {
    format!("sync-{}", uuid::Uuid::now_v7())
}

pub(super) fn handoff_event_handler(
    original: Option<Arc<dyn AgentEventHandler>>,
    background: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    handed_off: Arc<AtomicBool>,
) -> Option<Arc<dyn AgentEventHandler>> {
    if original.is_none() && background.is_none() {
        return None;
    }
    Some(Arc::new(FnEventHandler(move |event| {
        if handed_off.load(Ordering::Acquire) {
            if let Some(sender) = &background {
                if let Err(error) = sender.send(event) {
                    tracing::error!(%error, "sync child handoff event receiver closed");
                }
            } else if let Some(handler) = &original {
                handler.on_event(event);
            }
        } else if let Some(handler) = &original {
            handler.on_event(event);
        }
    })))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_sync_with_registered_handoff<Run, Execution>(
    session: Arc<Session>,
    child_thread_id: String,
    agent_name: String,
    parent_manager: Option<Arc<TaskManager>>,
    task_id: Option<String>,
    on_complete: Option<OnBgCompleteFn>,
    terminal_route: Option<TerminalMqRoute>,
    run: Run,
) -> Result<SyncCompletion, Box<dyn std::error::Error + Send + Sync>>
where
    Run: FnOnce(SyncHandoffSignal, Arc<AtomicBool>) -> Execution + Send + 'static,
    Execution: Future<Output = Result<ChildRunOutcome, Box<dyn std::error::Error + Send + Sync>>>
        + Send
        + 'static,
{
    let host = session
        .subagent_host()
        .ok_or("sync child host unavailable")?;
    // 同步执行必须先清掉旧续跑意图，否则崩溃后可能被当作后台执行恢复。
    let resources = host.session_resources.clone();
    if let Some(resources) = resources.as_deref() {
        resources
            .set_work_state(&ThreadId::from(child_thread_id.clone()), WorkState::Idle)
            .await?;
    }
    let (handoff_sender, mut handoff_ready) = oneshot::channel();
    let handed_off = Arc::new(AtomicBool::new(false));
    let terminal_ownership = Arc::new(TerminalOwnership::new());
    // 只有拿到终态路由时 wrapper 才接管无 detach 的收敛；无 route 由执行体按
    // 同步路径写终态。
    let wrapper_settles = terminal_route.is_some();
    let settlement_store = terminal_route
        .as_ref()
        .map(|route| Arc::clone(&route.store));
    let _wrapper_alive = SyncWrapperAlive(Arc::clone(&terminal_ownership));
    // 子执行由独立任务持有：父工具调用 future 因 cancel 被 drop 时 JoinHandle
    // 只 detach，子任务仍能观察到已取消的 child token，走完 flush / close /
    // 终态持久化 / SubagentStop / permit 结算的既有收尾。
    let mut cancel_guard = SyncChildCancelGuard::new(Arc::clone(&session));
    let mut execution = tokio::spawn(run(
        SyncHandoffSignal {
            sender: handoff_sender,
            terminal: Arc::clone(&terminal_ownership),
            wrapper_settles,
        },
        Arc::clone(&handed_off),
    ));
    let receipt = tokio::select! {
        biased;
        // 交接 receipt 优先于执行终态：spawn 后两者独立完成，已提交的交接
        // 不能被「执行恰好先结束」误判成同步完成。
        receipt = &mut handoff_ready => match receipt {
            Ok(receipt) => receipt,
            // runner 在交接提交前失败；等待其 typed 终态，不制造瞬时后台任务。
            Err(_) => {
                let outcome = await_execution(execution).await?;
                cancel_guard.disarm();
                return Ok(SyncCompletion::Finished(outcome));
            }
        },
        outcome = &mut execution => {
            let outcome = flatten_execution(outcome)?;
            cancel_guard.disarm();
            return Ok(SyncCompletion::Finished(outcome));
        }
    };
    if receipt.reason == ChildHandoffReason::ExecutionFailed {
        // RCRA has already failed. Finish its existing lifecycle barriers so
        // the original typed error reaches the caller; a residual receipt is
        // not permission to report this execution as merely Pending. This
        // does not start another settlement window or wait for residual tasks.
        let outcome = await_execution(execution).await?;
        cancel_guard.disarm();
        return Ok(SyncCompletion::Finished(outcome));
    }
    // 没有可原子结算的目标时保持同步：先 detach 再找投递方会留下无人收尾的后台任务。
    let can_detach = terminal_route.is_some()
        && parent_manager.is_some()
        && task_id.is_some()
        && on_complete.is_some();
    if !can_detach || host.close_state.is_closing() {
        tracing::warn!(
            %child_thread_id,
            "sync child handoff has no live terminal route; finishing synchronously"
        );
        // 执行体看到交接已提交时会先让位；wrapper 有存储时必须接回终态写入。
        let owns_terminal = settlement_store.is_some() && terminal_ownership.claim_handoff();
        let outcome = await_execution(execution).await;
        if owns_terminal {
            if let Some(store) = settlement_store.as_deref() {
                let status = terminal_status(&outcome, &session);
                if let Err(error) = settle_terminal_without_result(
                    store,
                    &ThreadId::from(child_thread_id.clone()),
                    status,
                )
                .await
                {
                    tracing::error!(%child_thread_id, %error, "sync child terminal write failed without detach");
                }
            }
        }
        cancel_guard.disarm();
        return Ok(SyncCompletion::Finished(outcome?));
    }
    if !terminal_ownership.claim_handoff() {
        // 执行已先收尾（Settled），交接不能再改写终态。
        let outcome = await_execution(execution).await?;
        cancel_guard.disarm();
        return Ok(SyncCompletion::Finished(outcome));
    }
    let terminal_route = terminal_route.expect("checked above");
    let manager = parent_manager.expect("checked above");
    let worker_task_id = task_id.expect("checked above");

    let log_task_id = worker_task_id.clone();
    let worker_session = Arc::clone(&session);
    let worker_thread_id = child_thread_id.clone();
    let worker_agent_name = agent_name.clone();
    let worker_close_state = Arc::clone(&host.close_state);
    let delivery = on_complete.expect("checked above");
    let close_task_id = worker_task_id.clone();
    let worker_terminal_route = terminal_route.clone();
    let (durable_tx, durable_rx) = oneshot::channel();
    // Keep ownership until close admission has atomically installed both the
    // registration and completion worker. Rejection creates no parent receipt.
    let mut execution = Some(execution);
    let spawned = host.close_state.spawn_sync_worker(close_task_id, || {
        let registration = manager.register(BgTaskRegistration {
            task_id: worker_task_id.clone(), kind: BgTaskKind::Agent,
            summary: format!("synchronous sub-agent {agent_name}"), kill: None, pid: None,
        })?;
        let execution = execution.take().expect("worker admitted only once");
        Ok(async move {
            // worker 持有不可取消的写入；Pending 只能在持久化确认后返回。
            let durable = if worker_session.config().cancel_token.is_cancelled() {
                Err("sync child was cancelled before background handoff".to_owned())
            } else {
                worker_terminal_route
                    .store
                    .set_work_state(
                        &ThreadId::from(worker_thread_id.clone()),
                        WorkState::Working,
                    )
                    .await
                    .map_err(|error| error.to_string())
            };
            if let Err(error) = &durable {
                tracing::error!(%worker_thread_id, %error, "sync child background handoff rejected");
                worker_session.config().cancel_token.cancel();
            }
            let _ = durable_tx.send(durable);
            let outcome = await_execution(execution).await;
            // The child task has joined; only this producer can prove its resources stopped.
            manager.confirm_external_execution_stopped(&registration);
            let status = terminal_status(&outcome, &worker_session);
            let result = parent_result(
                &outcome,
                &worker_session,
                &worker_task_id,
                &worker_agent_name,
                &worker_thread_id,
            );
            let message = worker_terminal_route.message(&result);
            // A3：先建立父 mailbox 的 Queued 责任，再让结果在 finalize 中可见。
            if let Err(error) = worker_terminal_route.accept(&message) {
                tracing::error!(
                    %worker_thread_id,
                    %error,
                    "sync child terminal result acceptance failed; retaining working for retry"
                );
                worker_close_state.record_sync_delivery_failure(
                    &worker_task_id,
                    &format!("terminal result accept failed: {error}"),
                );
                return;
            }
            // finalize 失败时用同一 ID 与正文重试；改写 payload 会让重试变成 Conflict，
            // registry 交付因此永久 pending。稳定 ID 的重复提交是幂等 Existing。
            if let Err(error) =
                settle_with_retry(&worker_terminal_route, &worker_thread_id, status, &message).await
            {
                tracing::error!(
                    %worker_thread_id,
                    %error,
                    "sync child terminal settlement failed after retries; retaining working for next entry"
                );
                worker_close_state.record_sync_delivery_failure(
                    &worker_task_id,
                    &format!("terminal settlement failed after retries: {error}"),
                );
                return;
            }
            let delivery_close_state = Arc::clone(&worker_close_state);
            let delivery_task_id = worker_task_id.clone();
            let confirmed: OnBgCompleteFn = Arc::new(move |result, kind| {
                match delivery(result, kind) {
                    Ok(()) => {
                        // 成功回执（含 registry 重试后的成功）清除此前失败记录。
                        delivery_close_state.settle_sync_delivery(&delivery_task_id);
                        Ok(())
                    }
                    Err(error) => {
                        delivery_close_state
                            .record_sync_delivery_failure(&delivery_task_id, &error);
                        Err(error)
                    }
                }
            });
            match manager.settle_completed(&registration, result, confirmed) {
                Ok(true) => worker_close_state.settle_sync_delivery(&worker_task_id),
                Ok(false) => worker_close_state.record_sync_delivery_failure(
                    &worker_task_id,
                    "parent task was already terminal before this delivery",
                ),
                Err(error) => {
                    // registry 已在返回 Err 前保存待投递结果并启动单一 retry worker。
                    if manager.has_pending_delivery(&worker_task_id) {
                        worker_close_state.settle_sync_delivery(&worker_task_id);
                        tracing::warn!(task_id = %worker_task_id, %error, "sync child delivery taken over by registry retry owner");
                    } else {
                        worker_close_state
                            .record_sync_delivery_failure(&worker_task_id, &error.to_string());
                        tracing::error!(task_id = %worker_task_id, %error, "sync child delivery failed without a retry owner");
                    }
                }
            }
        })
    });
    if let Err(error) = spawned {
        tracing::warn!(%error, task_id = %log_task_id, "sync worker admission rejected; finishing synchronously without a parent receipt");
        // 准入被拒时没有 worker 接管终态；执行体与 wrapper 用同一 CAS 竞争唯一写入，
        // 任何一侧被 drop 都不会留下无人收敛的 HandedOff。
        terminal_ownership.abandon_handoff();
        let outcome =
            await_execution(execution.expect("rejected factory leaves execution owned")).await;
        if terminal_ownership.reclaim_abandoned_handoff() {
            let status = terminal_status(&outcome, &session);
            if let Err(error) = settle_terminal_without_result(
                terminal_route.store.as_ref(),
                &ThreadId::from(child_thread_id.clone()),
                status,
            )
            .await
            {
                tracing::error!(%child_thread_id, %error, "sync child terminal write failed after rejected handoff");
            }
        }
        cancel_guard.disarm();
        return Ok(SyncCompletion::Finished(outcome?));
    }

    terminal_ownership.mark_worker_admitted();
    durable_rx.await.map_err(|error| {
        format!("sync child handoff worker ended before durable acceptance: {error}")
    })??;
    handed_off.store(true, Ordering::Release);
    tracing::warn!(
        %child_thread_id,
        task_id = %log_task_id,
        pending = receipt.tasks.len(),
        "sync child execution handed off to session-owned resource owner"
    );
    let pending_tasks = receipt
        .tasks
        .iter()
        .map(|task| {
            format!(
                "{} ({:?}; owner: {})",
                task.task_id,
                task.kind,
                task.owner_session_id.as_deref().unwrap_or(&child_thread_id)
            )
        })
        .collect();
    // 交接已成功：worker 持有执行，父调用 future 的 drop 不再终止子任务。
    cancel_guard.disarm();
    Ok(SyncCompletion::Pending(SyncPending {
        receipt,
        pending_tasks,
    }))
}

// ─── 同步运行 ────────────────────────────────────────────────────────────────

/// 同步子 agent：当前调用内运行 RCRA，完成后以 typed outcome 收尾。
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_sync_subagent(
    child_thread_id: &str,
    agent_name: &str,
    cwd: &str,
    max_iterations: usize,
    event_handler: Option<Arc<dyn AgentEventHandler>>,
    on_subagent_start: Option<SubagentLifecycleStart>,
    on_subagent_stop: Option<SubagentLifecycleStop>,
    session_resources: Option<Arc<dyn SessionResources>>,
    register_runtime: Option<RegisterRuntimeFn>,
    deregister_runtime: Option<DeregisterRuntimeFn>,
    langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    parent_agent_id: Option<AgentId>,
    parent_tool_call_id: Option<String>,
    v2_ctx: V2SubagentContext,
    session: Arc<Session>,
    mut resume_claim: Option<ResumeClaim>,
    handoff_start: Option<SyncHandoffSignal>,
) -> Result<ChildRunOutcome, Box<dyn std::error::Error + Send + Sync>> {
    // 交接信号拆两半：sender 交给 child runner，终态归属留在执行体直到收尾。
    let (handoff_start, terminal_state) = match handoff_start {
        Some(signal) => (
            Some(signal.sender),
            Some((signal.terminal, signal.wrapper_settles)),
        ),
        None => (None, None),
    };
    let mut permit = session
        .subagent_host()
        .and_then(|host| host.child_owner.clone())
        .map(|owner| owner.acquire(v2_ctx.context.session.turn.execution_binding()))
        .transpose()?;
    let event_handler = crate::agent::subagent_event_forwarder::with_child_event_origin(
        event_handler,
        peri_acp_types::event::ChildEventStream::new(
            child_thread_id.to_string(),
            v2_ctx.context.session.turn.execution_binding(),
        ),
    );
    let agent_name = agent_name.to_string();
    let cwd = cwd.to_string();

    // 启动注册（active_agents，与 DeregisterGuard drop 配对）
    if let Some(register) = &register_runtime {
        register(
            child_thread_id.to_string(),
            (*session.config().cancel_token).clone(),
            "cascade".into(),
        );
    }
    let _deregister_guard = DeregisterGuard {
        thread_id: child_thread_id.to_string(),
        deregister: deregister_runtime,
    };

    // The resumed claim crosses the first execution await with us. Its Drop
    // records cancellation without duplicating lifecycle Stop/hook delivery.
    if let Some(claim) = &mut resume_claim {
        claim.mark_running(session.clone());
        // 只在新执行获得准入后恢复处理责任；准备失败不得提前解阻塞。
        session.async_result_mailbox().resume_scope();
    }
    let terminal_resources = session_resources.clone();
    let stop_resources = if resume_claim.is_some() {
        None // 认领持有终态写入（finish 内定向写状态），此处不重复写。
    } else {
        session_resources
    };

    // 真实子会话身份：hook 载荷 agent_id 的来源（不是 agent 名）。
    if let Some(on_start) = &on_subagent_start {
        on_start(child_thread_id, &agent_name, &cwd);
    }
    emit_subagent_start_v2(
        &v2_ctx.event_bus,
        v2_ctx.context.turn_id(),
        parent_agent_id,
        v2_ctx.agent_id,
        &agent_name,
        false,
        parent_tool_call_id.clone(),
    );
    forward_subagent_start_v1(
        event_handler.as_ref(),
        build_subagent_start_v2(
            v2_ctx.context.turn_id(),
            parent_agent_id,
            v2_ctx.agent_id,
            &agent_name,
            false,
            parent_tool_call_id,
        ),
    );

    // v2 事件转发器：子 EventBus → 父事件 handler（TUI 可见子 agent 工具调用/AI 文本）
    let forwarder_handle = spawn_subagent_event_forwarder_for_completion(
        v2_ctx.event_handles,
        event_handler.clone(),
        langfuse_bridge.clone(),
        child_thread_id.to_string(),
    );
    let child_agent_id = v2_ctx.agent_id;
    let event_bus = v2_ctx.event_bus;

    // 运行 v2 ReAct 循环
    let execution_turn = v2_ctx.context.session.turn.clone();
    let mut outcome = std::panic::AssertUnwindSafe(
        super::child_runner::run_child_with_handoff_signal(
            v2_ctx.context,
            max_iterations,
            &session,
            handoff_start,
        ),
    )
    .catch_unwind()
    .await
    .unwrap_or_else(|_| ChildRunOutcome::Failed {
        error: crate::error::AgentError::Other(anyhow::anyhow!("sync child execution panicked")),
        residual: None,
    });
    // 已提交的交接不能替代后续 history flush 的确认。
    let mut history_flush_failed = false;
    if let Err(error) = super::lifecycle::flush_session_history(&session).await {
        history_flush_failed = true;
        outcome = match outcome {
            ChildRunOutcome::HandedOff { receipt } => ChildRunOutcome::Failed {
                error,
                residual: Some(receipt),
            },
            ChildRunOutcome::Failed {
                error: original,
                residual,
            } => {
                tracing::error!(%error, %original, "sync child history flush failed after execution failure");
                ChildRunOutcome::Failed {
                    error: original,
                    residual,
                }
            }
            ChildRunOutcome::HandoffFailed { cause, failure } => {
                tracing::error!(
                    %error,
                    "sync child history flush failed after unconfirmed handoff; retaining structured failure"
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
                session_id = %child_thread_id,
                turn_id = %execution_turn.turn_id(),
                agent_name,
                category = error.category_name(),
                error = %error.user_facing_message(),
                causes = ?error.cause_chain(),
                "sync child execution failed"
            );
        }
    }
    let subagent_turn_id = Some(execution_turn.turn_id());
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
                if let Some(permit) = permit.take() {
                    permit.finish(Err(error.clone()));
                }
                return Err(error.into());
            }
        }
        if let Some(permit) = permit.take() {
            permit.finish(Ok(()));
        }
    }
    let close_error = super::close::settle_explicit_close(&session, interrupted)
        .await
        .err();
    if let Some(error) = &close_error {
        tracing::error!(thread_id = %child_thread_id, %error, "child close incomplete; finalizing terminal while retaining evidence");
    }
    // 真实资源关闭失败时不写终态：资源未释放的事实必须保持可见（与 background
    // 路径的 terminal withheld 一致），resume 仍由 Active 状态拒绝。
    let close_failed = close_error.is_some();
    // handoff worker 已接管终态时，执行体不再单独写状态；非 handoff 路径保持原逻辑。
    let terminal_owner = if close_failed {
        true
    } else {
        match terminal_state.as_ref() {
            Some((ownership, wrapper_settles))
                if *wrapper_settles
                    && matches!(&outcome, ChildRunOutcome::HandedOff { .. })
                    && ownership.should_yield_handoff() =>
            {
                // 交接已提交且 wrapper 仍可收敛：等它 claim HandedOff，避免抢写。
                false
            }
            Some((ownership, _)) if ownership.reclaim_abandoned_handoff() => {
                // 准入被拒或 wrapper 在交接后退出：执行体接回唯一终态写入。
                true
            }
            Some((ownership, _)) => ownership.claim_settlement(),
            None => true,
        }
    };
    let stop_resources = if close_failed || !terminal_owner {
        None
    } else {
        stop_resources
    };

    let (mut stop_result, mut stop_is_error, mut stop_failure) =
        stop_projection(&outcome, &agent_name, child_thread_id, &session);
    // 终态与工作意图共用同一个映射：真实取消 → Cancelled + Stopped；其余中断/失败
    // → Error + Idle；只有 Settled 才是 Done。写入推迟到事件排空之后：排空失败要把
    // 本次执行收口为 Error，不能先写 Done 再改写既有终态。
    let mut terminal_status_value = terminal_status_for_outcome(&outcome, &session);
    if let Some(error) = &close_error {
        let error: crate::error::AgentError =
            anyhow::anyhow!("child close incomplete: {error}").into();
        stop_result = error.to_string();
        stop_is_error = true;
        stop_failure = SubagentFailure::safe_failure_from_error(child_thread_id, &error);
    }
    // 这里只触发 SubagentStop hook；终态写入在排空结果确定后执行一次。
    let mut settlement_error = on_subagent_stop_handler(
        &on_subagent_stop,
        &None,
        &agent_name,
        child_thread_id,
        &stop_result,
        terminal_status_value,
        &cwd,
    )
    .await
    .err();
    if settlement_error.is_none() {
        if let Some(error) = close_error {
            settlement_error = Some(anyhow::anyhow!("child close incomplete: {error}").into());
        }
    }
    // 关闭失败时认领不在此结清：Drop 以 CancelRunning 重试关闭，且失败时保留
    // 原状态，避免把未释放的资源报告成可 resume。
    if let Some(error) = settlement_error.as_ref() {
        stop_result = error.to_string();
        stop_is_error = true;
        stop_failure = SubagentFailure::safe_failure_from_error(child_thread_id, error);
    }
    if let Some(subagent_turn_id) = subagent_turn_id {
        emit_subagent_stop_v2_with_failure(
            &event_bus,
            SubagentStopV2Input {
                turn_id: subagent_turn_id,
                parent_agent_id,
                child_agent_id,
                agent_name: &agent_name,
                result: &stop_result,
                is_error: stop_is_error,
                subagent_failure: stop_failure.clone(),
            },
        );
    }
    // Close the child producer after the terminal v2 event. Awaiting the
    // forwarder drains queued render/observe events (including visible deltas)
    // before the synchronous v1 stop reaches the parent handler.
    if let Err(error) = drain_subagent_events(event_bus, forwarder_handle, langfuse_bridge).await {
        tracing::error!(thread_id = %child_thread_id, error = %error.user_facing_message(), causes = ?error.cause_chain(), "sync child event forwarding barrier incomplete");
        settlement_error = Some(anyhow::anyhow!("child event drain failed: {error}").into());
        // 关闭失败时即使 drain 也失败，仍不得写终态：资源未释放的事实必须保持
        // 可见，resume 继续由 Active 状态拒绝。其余路径把终态改为 Error，
        // 由下面的唯一写入点落库，不覆盖任何已提交终态。
        terminal_status_value = peri_acp_types::thread::AgentStatus::Error;
        let residual = match &outcome {
            ChildRunOutcome::HandedOff { receipt } => Some(receipt.clone()),
            ChildRunOutcome::Failed { residual, .. } => residual.clone(),
            _ => None,
        };
        outcome = ChildRunOutcome::Failed {
            error: crate::error::AgentError::Other(anyhow::anyhow!(
                "child event drain failed: {error}"
            )),
            residual,
        };
        stop_result = format!(
            "{agent_name} execution failed: {}",
            error.user_facing_message()
        )
        .chars()
        .take(500)
        .collect();
        stop_is_error = true;
        stop_failure = SubagentFailure::safe_failure_from_error(child_thread_id, &error);
    }
    // 排空结果已确定：认领与未交接收尾都只在此时写一次最终终态。
    let claim = if close_failed {
        None
    } else {
        resume_claim.take()
    };
    let mut terminal_write_failed = false;
    if let Some(claim) = claim {
        let finished = if terminal_owner {
            // 与 handler 的写入顺序一致：先工作意图，再终态；写失败时保留 Active
            // 供恢复，而不是留下 Done + working 或写回认领前的状态。
            let work_state = WorkState::after_agent_terminal(terminal_status_value);
            let work_state_result = match terminal_resources.as_deref() {
                Some(store) => {
                    store
                        .set_work_state(&ThreadId::from(child_thread_id), work_state)
                        .await
                }
                None => Ok(()),
            };
            match work_state_result {
                Ok(()) => claim.finish(terminal_status_value).await,
                Err(error) => {
                    let released = claim.release_without_terminal().await;
                    match released {
                        Ok(()) => Err(format!("terminal work state write failed: {error}")),
                        Err(release_error) => Err(format!(
                            "terminal work state write failed: {error}; {release_error}"
                        )),
                    }
                }
            }
        } else {
            // finalize 持有终态：认领不写任何状态，child 保持 Active 直到 finalize
            // 提交；远端存储的 active 守卫因此仍能命中唯一的终态写入。
            claim.release_without_terminal().await
        };
        if let Err(error) = finished {
            terminal_write_failed = true;
            settlement_error =
                Some(anyhow::anyhow!("terminal_status_persist_failed: {error}").into());
        }
    }
    // close_failed / handoff 让位 / resume 认领都不会进入这里：`stop_resources`
    // 已按同一组条件在写终态前收敛为 None。
    if let Some(store) = stop_resources.as_deref() {
        if let Err(error) = settle_terminal_without_result(
            store,
            &ThreadId::from(child_thread_id),
            terminal_status_value,
        )
        .await
        {
            tracing::error!(thread_id = %child_thread_id, %error, "terminal_status_persist_failed");
            terminal_write_failed = true;
            settlement_error =
                Some(anyhow::anyhow!("terminal_status_persist_failed: {error}").into());
        }
    }
    if terminal_write_failed {
        if let Some(error) = settlement_error.as_ref() {
            stop_result = error.to_string();
            stop_is_error = true;
            stop_failure = SubagentFailure::safe_failure_from_error(child_thread_id, error);
        }
    }
    // v1 协议化载体直发（SubagentStopped）：与 Started 同源（v2 事件构造 +
    // observe_event_to_executor 同步映射），保证 Stopped 在 turn 收尾前到达
    // 父协议化链路（TUI 容器销毁 / depth 配对）。
    if let Some(subagent_turn_id) = subagent_turn_id {
        forward_subagent_stop_v1(
            event_handler.as_ref(),
            build_subagent_stop_v2_with_failure(
                subagent_turn_id,
                parent_agent_id,
                child_agent_id,
                &agent_name,
                &stop_result,
                stop_is_error,
                stop_failure,
            ),
        );
    }

    let owner = session
        .subagent_host()
        .and_then(|host| host.child_owner.clone());
    // residual 是已确认的交接；无 receipt 时仍须检查动态到达的义务。
    let unsettled_obligations = match owner.as_ref() {
        Some(owner) => owner.has_unsettled_obligations().await,
        None => false,
    };
    let permit_result = if let Some(error) = settlement_error.as_ref() {
        Err(error.to_string())
    } else if history_flush_failed {
        Err("child history flush is unconfirmed".into())
    } else if matches!(outcome, ChildRunOutcome::HandoffFailed { .. }) {
        Err("child handoff persistence is unconfirmed".into())
    } else if matches!(outcome, ChildRunOutcome::Failed { residual: None, .. })
        && unsettled_obligations
    {
        Err("child settlement is unconfirmed after execution failure".into())
    } else {
        Ok(())
    };
    if let Some(permit) = permit.take() {
        permit.finish(permit_result);
    }
    if let Some(error) = settlement_error {
        return Err(Box::new(SubagentFailure::new(
            child_thread_id,
            &agent_name,
            error,
        )));
    }
    match outcome {
        ChildRunOutcome::Failed { error, residual } => {
            let failure = match residual {
                Some(receipt) => SubagentFailure::failed_with_handoff(
                    child_thread_id,
                    &agent_name,
                    error,
                    receipt,
                ),
                None => SubagentFailure::new(child_thread_id, &agent_name, error),
            };
            Err(Box::new(failure))
        }
        ChildRunOutcome::HandoffFailed { cause, failure } => Err(Box::new(
            SubagentFailure::handoff_failed(child_thread_id, &agent_name, cause, &failure),
        )),
        other => Ok(other),
    }
}

fn stop_projection(
    outcome: &ChildRunOutcome,
    agent_name: &str,
    child_thread_id: &str,
    session: &Arc<Session>,
) -> (
    String,
    bool,
    Option<peri_acp_types::error::SafeSubagentFailure>,
) {
    match outcome {
        ChildRunOutcome::Settled => (
            extract_last_ai_text(session).chars().take(500).collect(),
            false,
            None,
        ),
        ChildRunOutcome::Interrupted => ("interrupted".to_string(), true, None),
        ChildRunOutcome::HandedOff { receipt } => {
            let failure =
                SubagentFailure::handed_off(&receipt.child_thread_id, agent_name, receipt.clone());
            (
                format!("delegation incomplete: {}", failure.public_message())
                    .chars()
                    .take(500)
                    .collect(),
                true,
                failure.safe_failure(),
            )
        }
        ChildRunOutcome::Failed { error, .. } => (
            format!(
                "{agent_name} execution failed: {}",
                error.user_facing_message()
            )
            .chars()
            .take(500)
            .collect(),
            true,
            SubagentFailure::safe_failure_from_error(child_thread_id, error),
        ),
        ChildRunOutcome::HandoffFailed { cause, failure } => {
            let label = super::types::handoff_failure_label(failure);
            let flush = failure
                .flush_error
                .as_ref()
                .map(|error| format!("; handoff history flush failed: {error}"))
                .unwrap_or_default();
            let message = match cause {
                Some(cause) => format!(
                    "{agent_name} execution failed: {}; {label}: {}{flush}",
                    cause.user_facing_message(),
                    failure.error
                ),
                None => format!("delegation incomplete: {label}: {}{flush}", failure.error),
            };
            let safe = match cause {
                Some(cause) => SubagentFailure::safe_failure_from_error(child_thread_id, cause),
                None => {
                    let error = super::types::handoff_failure_agent_error(failure);
                    SubagentFailure::safe_failure_from_error(child_thread_id, &error)
                }
            };
            (message.chars().take(500).collect(), true, safe)
        }
    }
}
