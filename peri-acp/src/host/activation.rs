use std::sync::{atomic::Ordering, Arc};

use peri_acp_types::session::{MessageQueue, MessageSource};
use peri_acp_types::tasks::BgTaskKind;
use peri_agent::session::async_result_mailbox::AsyncResultMailbox;
use tokio::sync::mpsc;

use super::{task_scope, AcpServerConfig, SessionState, SharedSessions};
use crate::session::executor::ContinuationRequest;
use crate::session::SessionActivation;
use crate::transport::types::AcpError;

/// 查询失败后的重查退避：有界、可观察，不忙等。
const ACTIVATION_RETRY_INITIAL: std::time::Duration = std::time::Duration::from_millis(50);
const ACTIVATION_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(1);

/// 调度二次资格检查可占用 prompt lock 等待查询恢复的总时限。
///
/// 只吸收瞬时存储故障；超出后交还 listener，避免长时间阻塞同会话的用户输入。
pub(super) const DISPATCH_RECHECK_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);

/// 调度二次资格查询的可失败重查：与激活 listener 共用同一退避节奏。
///
/// `Err` 表示存储无法回答，不能折叠成「没有工作」；同一轮 dispatch 的多次查询
/// 共享 `deadline`，预算耗尽后返回最后一次错误，由调用方
/// [`MessageQueue::request_recheck`] 保留重查责任。
pub(super) async fn checked_dispatch_query<T, E, F, Fut>(
    session_id: &str,
    query: &'static str,
    deadline: tokio::time::Instant,
    mut run: F,
) -> Result<T, String>
where
    E: std::fmt::Display,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut retry_delay = ACTIVATION_RETRY_INITIAL;
    loop {
        match run().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let error = error.to_string();
                let giving_up = tokio::time::Instant::now() + retry_delay > deadline;
                tracing::warn!(
                    %session_id,
                    query,
                    %error,
                    retry_after_ms = retry_delay.as_millis() as u64,
                    exhausted = giving_up,
                    "continuation dispatch query failed; bounded recheck"
                );
                if giving_up {
                    return Err(error);
                }
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(ACTIVATION_RETRY_MAX);
            }
        }
    }
}

/// 调度准入只需要这几个布尔事实；复制后必须释放 sessions 锁，再查询 MQ。
pub(super) struct PendingState {
    pub closing: bool,
    pub cancel_active: bool,
    pub continuation_in_flight: bool,
    pub continuation_armed: bool,
}

impl PendingState {
    pub(super) fn from_state(state: &SessionState) -> Self {
        Self {
            closing: state.closing,
            cancel_active: state.cancel_token.is_some(),
            continuation_in_flight: state.continuation_in_flight,
            continuation_armed: state.continuation_armed,
        }
    }
}

/// 激活检查的结论：`Retry` 表示存储暂时无法回答，唤醒没有被判定为「无工作」。
pub(super) enum PendingRequest {
    Ready(ContinuationRequest),
    Idle,
    Retry(String),
}

pub(super) async fn pending_request(
    session_id: &str,
    state: &PendingState,
    queue: &MessageQueue,
    activation: &SessionActivation,
    mailbox: &AsyncResultMailbox,
) -> PendingRequest {
    if state.closing || state.cancel_active || state.continuation_in_flight {
        return PendingRequest::Idle;
    }
    let activation_decision = match activation.can_activate(queue).await {
        Ok(true) => Some(true),
        Ok(false) if state.continuation_armed => {
            match queue
                .has_pending_defer(&MessageSource::SubAgentComplete)
                .await
            {
                Ok(true) => Some(false),
                Ok(false) => None,
                Err(error) => return PendingRequest::Retry(error.to_string()),
            }
        }
        Ok(false) => None,
        Err(error) => return PendingRequest::Retry(error.to_string()),
    };
    if let Some(mq_steering) = activation_decision {
        return PendingRequest::Ready(ContinuationRequest {
            session_id: session_id.to_owned(),
            kind: BgTaskKind::Agent,
            mq_steering,
            pending_result: false,
            recovery: false,
        });
    }
    if activation.can_activate_result(mailbox) {
        // 通知已出队并写入历史，但主模型尚未处理：即使队列没有新工作，也构成
        // 一次允许的自动尝试。Blocked / Suspended 不在候选内。
        return PendingRequest::Ready(ContinuationRequest {
            session_id: session_id.to_owned(),
            kind: BgTaskKind::Agent,
            mq_steering: false,
            pending_result: true,
            recovery: false,
        });
    }
    PendingRequest::Idle
}

pub(super) fn ensure_listener(
    session_id: &str,
    sessions: &SharedSessions,
    cfg: &AcpServerConfig,
    sender: &mpsc::UnboundedSender<ContinuationRequest>,
) -> Result<(), AcpError> {
    let runtime = cfg
        .session_manager
        .get_session(session_id)
        .ok_or_else(|| AcpError::new(-32602, "session runtime not found"))?;
    let activation = Arc::clone(&runtime.activation);
    if activation.listener_started.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let queue = runtime.v2_message_queue.clone();
    let cancellation = runtime.cancel_token.clone();
    let mailbox = Arc::clone(&runtime.async_result_mailbox);
    drop(runtime);
    let mut wake = queue.subscribe_wake();
    let mut result_changed = mailbox.subscribe_state();
    let shutdown = cfg.host_task_spawner.shutdown_token();
    let manager = cfg.session_manager.clone();
    let sessions = Arc::clone(sessions);
    let session_id = session_id.to_owned();
    let sender = sender.clone();
    let listener_activation = Arc::clone(&activation);
    let result = cfg.host_task_spawner.spawn(
        task_scope::HostTaskOwnerKind::Session,
        task_scope::HostTaskKind::SessionActivation,
        async move {
            let mut retry_delay = ACTIVATION_RETRY_INITIAL;
            loop {
                if cancellation.is_cancelled() || shutdown.is_cancelled() {
                    return;
                }
                wake.borrow_and_update();
                result_changed.borrow_and_update();
                let pending = {
                    let sessions = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        _ = shutdown.cancelled() => return,
                        sessions = sessions.lock() => sessions,
                    };
                    let Some(state) = sessions.get(&session_id) else {
                        return;
                    };
                    let Some(runtime) = manager.get_session(&session_id) else {
                        return;
                    };
                    if !Arc::ptr_eq(&runtime.activation, &listener_activation) {
                        return;
                    }
                    PendingState::from_state(state)
                };
                match pending_request(
                    &session_id,
                    &pending,
                    &queue,
                    &listener_activation,
                    &mailbox,
                )
                .await
                {
                    PendingRequest::Ready(request) => {
                        retry_delay = ACTIVATION_RETRY_INITIAL;
                        if sender.send(request).is_err() {
                            tracing::error!(%session_id, "session activation channel closed");
                            return;
                        }
                    }
                    PendingRequest::Idle => retry_delay = ACTIVATION_RETRY_INITIAL,
                    PendingRequest::Retry(error) => {
                        // 查询失败不是「没有工作」：保留重查责任，指数退避且封顶，
                        // 不忙等也不把已消费的唤醒丢掉。
                        tracing::warn!(
                            %session_id,
                            %error,
                            retry_after_ms = retry_delay.as_millis(),
                            "session activation query failed; retrying"
                        );
                        let stopped = tokio::select! {
                            biased;
                            _ = cancellation.cancelled() => true,
                            _ = shutdown.cancelled() => true,
                            _ = tokio::time::sleep(retry_delay) => false,
                        };
                        if stopped {
                            return;
                        }
                        retry_delay = (retry_delay * 2).min(ACTIVATION_RETRY_MAX);
                        continue;
                    }
                }
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return,
                    _ = shutdown.cancelled() => return,
                    changed = wake.changed() => {
                        if changed.is_err() { return; }
                    }
                    changed = result_changed.changed() => {
                        if changed.is_err() { return; }
                    }
                }
            }
        },
    );
    if let Err(error) = result {
        activation.listener_started.store(false, Ordering::Release);
        return Err(AcpError::new(
            -32010,
            format!("session activation unavailable: {error:?}"),
        ));
    }
    Ok(())
}

/// 测试面便捷断言；生产路径使用可区分查询错误的 [`mq_allowed_checked`]。
#[cfg(test)]
pub(super) async fn mq_allowed(cfg: &AcpServerConfig, session_id: &str) -> bool {
    mq_allowed_checked(cfg, session_id).await.unwrap_or(false)
}

/// MQ 唤醒门禁的可失败形态：`Err` 表示无法判断，调用方必须保留重查责任。
pub(super) async fn mq_allowed_checked(
    cfg: &AcpServerConfig,
    session_id: &str,
) -> Result<bool, String> {
    let Some(runtime) = cfg.session_manager.get_session(session_id) else {
        return Ok(false);
    };
    if runtime.cancel_token.is_cancelled() {
        return Ok(false);
    }
    let activation = Arc::clone(&runtime.activation);
    let queue = runtime.v2_message_queue.clone();
    drop(runtime);
    activation
        .can_activate(&queue)
        .await
        .map_err(|error| error.to_string())
}

/// mq steering 的放行判定（可失败形态）：正常激活之外，Host 注入且仍在队列里的
/// 恢复提醒也必须能推进——失败水位记录的是「尝试开始时的 MQ 水位」，一个在 Receive
/// 之前就失败的尝试会把刚注入的提醒误算成已处理。提醒仍在队列是队列事实，epoch 与
/// Stop 判定不变，因此这里只补回「已进入但尚未接管」的提醒，不放宽 superseded 保护。
///
/// `Err` 表示查询无法回答，不等于明确拒绝；调用方必须保留重查责任。
pub(super) async fn mq_steering_allowed_checked(
    cfg: &AcpServerConfig,
    session_id: &str,
) -> Result<bool, String> {
    if mq_allowed_checked(cfg, session_id).await? {
        return Ok(true);
    }
    let Some(runtime) = cfg.session_manager.get_session(session_id) else {
        return Ok(false);
    };
    if runtime.cancel_token.is_cancelled() || !runtime.activation.is_allowed() {
        return Ok(false);
    }
    let queue = runtime.v2_message_queue.clone();
    drop(runtime);
    queue
        .has_pending_defer(&MessageSource::SessionRecovery)
        .await
        .map_err(|error| error.to_string())
}

/// Ready 结果是否仍允许自动尝试（Stop 抑制与失败水位在 owner 内统一判定）。
pub(super) async fn result_allowed(cfg: &AcpServerConfig, session_id: &str) -> bool {
    let Some(runtime) = cfg.session_manager.get_session(session_id) else {
        return false;
    };
    if runtime.cancel_token.is_cancelled() {
        return false;
    }
    runtime
        .activation
        .can_activate_result(&runtime.async_result_mailbox)
}
