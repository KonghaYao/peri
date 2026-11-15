//! `UpstreamProxy` 的内部状态：代次、任务映射、订阅计数与健康广播。
//!
//! 与代理转发分离，避免单文件越过模块上限；本模块不做协议映射。

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use parking_lot::{Mutex, RwLock};
use rmcp::{
    model::{
        CreateTaskResult, ErrorCode, ErrorData as McpError, GetTaskResult,
        ProgressNotificationParam, ProgressToken, ServerNotification, ServerPeerInfo,
    },
    service::{Peer, RoleClient, RoleServer},
};
use tokio::sync::watch;

use super::{CapabilityHealth, UpstreamAttachError, UpstreamGeneration, UpstreamLossReason};

/// 上游 task id 在下游的分配序号；跨 proxy 实例单调递增，配合 capability 前缀
/// 保证单出口内的 task id 不会被两个上游复用。
static NEXT_TASK_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
pub(super) struct Lifecycle {
    latest: u64,
    current: Option<CurrentUpstream>,
}

struct CurrentUpstream {
    generation: UpstreamGeneration,
    peer: Peer<RoleClient>,
    info: Option<ServerPeerInfo>,
}

#[derive(Default)]
struct TaskRegistry {
    by_downstream: HashMap<String, TaskRoute>,
    by_upstream: HashMap<(u64, String), String>,
}

struct TaskRoute {
    generation: UpstreamGeneration,
    upstream_task_id: String,
}

impl TaskRegistry {
    fn clear(&mut self) {
        self.by_downstream.clear();
        self.by_upstream.clear();
    }
}

#[derive(Default)]
pub(super) struct SubscriptionCounts {
    by_uri: HashMap<String, usize>,
    generation: Option<UpstreamGeneration>,
}

impl SubscriptionCounts {
    fn reset(&mut self, generation: Option<UpstreamGeneration>) {
        self.by_uri.clear();
        self.generation = generation;
    }
}

/// 订阅操作按代次串行化；每代独立 mutex，旧代次未完成的请求不会阻塞新代次。
#[derive(Default)]
pub(super) struct SubscriptionGate {
    generation: Option<UpstreamGeneration>,
    mutex: Arc<tokio::sync::Mutex<()>>,
}

impl SubscriptionGate {
    fn reset(&mut self, generation: Option<UpstreamGeneration>) {
        self.generation = generation;
        self.mutex = Arc::new(tokio::sync::Mutex::new(()));
    }
}

/// progress token 映射与早到通知缓冲。
///
/// 上游 token 由 rmcp 在发送时生成，通知可能早于 `RequestHandle` 返回；
/// 未命中映射的通知先缓冲，注册映射时按序补发，避免丢进度。
/// 键含 generation：重连后新 peer 可能复用同一 token。
#[derive(Default)]
pub(super) struct ProgressRegistry {
    mappings: HashMap<(u64, ProgressToken), ProgressToken>,
    pending: HashMap<(u64, ProgressToken), Vec<ProgressNotificationParam>>,
    pending_total: usize,
}

const PENDING_PROGRESS_LIMIT: usize = 64;

/// 取消通知只做有界投递；上游写阻塞不能延长请求超时/取消的返回。
const UPSTREAM_CANCEL_DELIVERY_TIMEOUT: Duration = Duration::from_millis(250);

pub(super) struct ProxyInner {
    pub(super) capability: String,
    pub(super) lifecycle: Mutex<Lifecycle>,
    pub(super) health: watch::Sender<CapabilityHealth>,
    pub(super) downstream: RwLock<Option<Peer<RoleServer>>>,
    tasks: Mutex<TaskRegistry>,
    pub(super) progress: Mutex<ProgressRegistry>,
    subscriptions: Mutex<SubscriptionCounts>,
    subscription_gate: Mutex<SubscriptionGate>,
    pub(super) request_timeout: Option<Duration>,
}

impl ProxyInner {
    pub(super) fn new(capability: String) -> Self {
        let (health, _) = watch::channel(CapabilityHealth::Connecting);
        Self {
            capability,
            lifecycle: Mutex::new(Lifecycle::default()),
            health,
            downstream: RwLock::new(None),
            tasks: Mutex::new(TaskRegistry::default()),
            progress: Mutex::new(ProgressRegistry::default()),
            subscriptions: Mutex::new(SubscriptionCounts::default()),
            subscription_gate: Mutex::new(SubscriptionGate::default()),
            request_timeout: None,
        }
    }

    pub(super) fn allocate_generation(&self) -> UpstreamGeneration {
        let mut lifecycle = self.lifecycle.lock();
        lifecycle.latest += 1;
        UpstreamGeneration(lifecycle.latest)
    }

    pub(super) fn attach(
        &self,
        generation: UpstreamGeneration,
        peer: Peer<RoleClient>,
    ) -> Result<(), UpstreamAttachError> {
        let mut lifecycle = self.lifecycle.lock();
        let latest = UpstreamGeneration(lifecycle.latest);
        if generation != latest {
            return Err(UpstreamAttachError::StaleGeneration { generation, latest });
        }
        let info = peer.peer_info().map(|info| info.as_ref().clone());
        lifecycle.current = Some(CurrentUpstream {
            generation,
            peer,
            info,
        });
        // 保持 lifecycle 锁到注册表清空与 health 发布完成：旧代次的 mark_lost
        // 不能在新代次 attach 后清掉新注册表或覆盖 Ready。
        self.tasks.lock().clear();
        *self.progress.lock() = ProgressRegistry::default();
        self.subscriptions.lock().reset(Some(generation));
        self.subscription_gate.lock().reset(Some(generation));
        self.health.send_replace(CapabilityHealth::Ready);
        drop(lifecycle);
        tracing::info!(
            capability = %self.capability,
            generation = generation.value(),
            "MCP upstream attached"
        );
        Ok(())
    }

    pub(super) fn mark_lost(&self, generation: UpstreamGeneration, reason: UpstreamLossReason) {
        let mut lifecycle = self.lifecycle.lock();
        let was_current = lifecycle
            .current
            .as_ref()
            .is_some_and(|current| current.generation == generation);
        if was_current {
            lifecycle.current = None;
        } else if UpstreamGeneration(lifecycle.latest) != generation || lifecycle.current.is_some()
        {
            return;
        }
        self.tasks.lock().clear();
        *self.progress.lock() = ProgressRegistry::default();
        self.subscriptions.lock().reset(None);
        self.subscription_gate.lock().reset(None);
        self.health
            .send_replace(CapabilityHealth::Degraded { reason });
        drop(lifecycle);
        tracing::warn!(
            capability = %self.capability,
            generation = generation.value(),
            ?reason,
            "MCP upstream degraded"
        );
    }

    pub(super) fn upstream(&self) -> Result<(UpstreamGeneration, Peer<RoleClient>), McpError> {
        let lifecycle = self.lifecycle.lock();
        match &lifecycle.current {
            Some(current) => Ok((current.generation, current.peer.clone())),
            None => Err(McpError::internal_error(
                format!("{} upstream is not connected", self.capability),
                None,
            )),
        }
    }

    pub(super) fn current_info(&self) -> Option<ServerPeerInfo> {
        self.lifecycle
            .lock()
            .current
            .as_ref()
            .and_then(|current| current.info.clone())
    }

    pub(super) fn is_current(&self, generation: UpstreamGeneration) -> bool {
        self.lifecycle
            .lock()
            .current
            .as_ref()
            .is_some_and(|current| current.generation == generation)
    }

    pub(super) fn remember_downstream(&self, peer: &Peer<RoleServer>) {
        let mut downstream = self.downstream.write();
        if downstream.is_none() {
            *downstream = Some(peer.clone());
        }
    }

    pub(super) async fn send_downstream(
        &self,
        generation: UpstreamGeneration,
        notification: ServerNotification,
    ) {
        if !self.is_current(generation) {
            return;
        }
        let peer = self.downstream.read().clone();
        let Some(peer) = peer else {
            tracing::debug!(
                capability = %self.capability,
                "dropping upstream notification before a downstream peer is known"
            );
            return;
        };
        if let Err(error) = peer.send_notification(notification).await {
            tracing::debug!(
                capability = %self.capability,
                %error,
                "failed to forward upstream notification to downstream"
            );
        }
    }

    pub(super) fn map_service_error(
        &self,
        generation: UpstreamGeneration,
        error: rmcp::ServiceError,
    ) -> McpError {
        match error {
            rmcp::ServiceError::McpError(error) => error,
            rmcp::ServiceError::TransportClosed | rmcp::ServiceError::TransportSend(_) => {
                self.mark_lost(generation, UpstreamLossReason::TransportClosed);
                McpError::internal_error(
                    format!("{} upstream is unavailable: {error}", self.capability),
                    None,
                )
            }
            error => McpError::internal_error(
                format!("{} upstream request failed: {error}", self.capability),
                None,
            ),
        }
    }

    pub(super) fn register_progress(
        &self,
        generation: UpstreamGeneration,
        upstream_token: ProgressToken,
        downstream_token: ProgressToken,
    ) -> Vec<ProgressNotificationParam> {
        let key = (generation.value(), upstream_token);
        let mut progress = self.progress.lock();
        progress
            .mappings
            .insert(key.clone(), downstream_token.clone());
        let mut pending = progress.pending.remove(&key).unwrap_or_default();
        progress.pending_total = progress.pending_total.saturating_sub(pending.len());
        for params in &mut pending {
            params.progress_token = downstream_token.clone();
        }
        pending
    }

    pub(super) fn unregister_progress(
        &self,
        generation: UpstreamGeneration,
        upstream_token: &ProgressToken,
    ) {
        let key = (generation.value(), upstream_token.clone());
        let mut progress = self.progress.lock();
        progress.mappings.remove(&key);
        if let Some(pending) = progress.pending.remove(&key) {
            progress.pending_total = progress.pending_total.saturating_sub(pending.len());
        }
    }

    /// 已注册映射时替换为下游 token；未知 token 先缓冲，超限则丢弃。
    pub(super) fn route_progress(
        &self,
        generation: UpstreamGeneration,
        mut params: ProgressNotificationParam,
    ) -> Option<ProgressNotificationParam> {
        let key = (generation.value(), params.progress_token.clone());
        let mut progress = self.progress.lock();
        if let Some(downstream) = progress.mappings.get(&key) {
            params.progress_token = downstream.clone();
            return Some(params);
        }
        if progress.pending_total >= PENDING_PROGRESS_LIMIT {
            tracing::debug!(
                capability = %self.capability,
                "dropping progress notification beyond the pre-registration buffer limit"
            );
            return None;
        }
        progress.pending_total += 1;
        progress.pending.entry(key).or_default().push(params);
        None
    }

    pub(super) async fn notify_upstream_cancelled(
        &self,
        peer: &Peer<RoleClient>,
        request_id: rmcp::model::RequestId,
        reason: &str,
    ) {
        let delivery = peer.notify_cancelled(rmcp::model::CancelledNotificationParam::new(
            Some(request_id),
            Some(reason.to_owned()),
        ));
        // 取消是 best-effort：transport 写阻塞不得拖住超时/取消路径的返回。
        let _ = tokio::time::timeout(UPSTREAM_CANCEL_DELIVERY_TIMEOUT, delivery).await;
    }

    fn task_id_for(&self, generation: UpstreamGeneration, upstream_task_id: &str) -> String {
        let mut tasks = self.tasks.lock();
        if let Some(existing) = tasks
            .by_upstream
            .get(&(generation.value(), upstream_task_id.to_owned()))
        {
            return existing.clone();
        }
        let downstream = format!(
            "{}:task:{}",
            self.capability,
            NEXT_TASK_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        tasks.by_upstream.insert(
            (generation.value(), upstream_task_id.to_owned()),
            downstream.clone(),
        );
        tasks.by_downstream.insert(
            downstream.clone(),
            TaskRoute {
                generation,
                upstream_task_id: upstream_task_id.to_owned(),
            },
        );
        downstream
    }

    pub(super) fn resolve_task_id(
        &self,
        generation: UpstreamGeneration,
        downstream_task_id: &str,
    ) -> Result<String, McpError> {
        let tasks = self.tasks.lock();
        match tasks.by_downstream.get(downstream_task_id) {
            Some(route) if route.generation == generation => Ok(route.upstream_task_id.clone()),
            _ => Err(McpError::new(
                ErrorCode::INVALID_PARAMS,
                format!(
                    "unknown or stale task id for {} (capability prefix required)",
                    self.capability
                ),
                None,
            )),
        }
    }

    pub(super) fn map_create_task(
        &self,
        generation: UpstreamGeneration,
        result: &mut CreateTaskResult,
    ) {
        let downstream = self.task_id_for(generation, &result.task.task_id);
        result.task.task_id = downstream;
    }

    pub(super) fn map_get_task(&self, generation: UpstreamGeneration, result: &mut GetTaskResult) {
        let downstream = self.task_id_for(generation, &result.task.task.task_id);
        result.task.task.task_id = downstream;
    }

    /// 任务状态通知的 id 需要映射到下游 id；非任务通知原样返回 `true`。
    pub(super) fn map_task_notification(
        &self,
        generation: UpstreamGeneration,
        notification: &mut ServerNotification,
    ) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        match notification {
            ServerNotification::TaskStatusNotification(task) => {
                let downstream = self.task_id_for(generation, &task.params.task.task.task_id);
                task.params.task.task.task_id = downstream;
                true
            }
            _ => true,
        }
    }

    fn stale_subscription_error(&self) -> McpError {
        McpError::internal_error(
            format!(
                "{} upstream generation changed during subscription request",
                self.capability
            ),
            None,
        )
    }

    /// 取得当前代次的订阅串行锁；代次不匹配立即拒绝，不等待旧代次请求。
    pub(super) async fn subscription_lock(
        &self,
        generation: UpstreamGeneration,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, McpError> {
        let gate = {
            let gate = self.subscription_gate.lock();
            if gate.generation != Some(generation) {
                return Err(self.stale_subscription_error());
            }
            Arc::clone(&gate.mutex)
        };
        Ok(gate.lock_owned().await)
    }

    /// 资源订阅按 URI 引用计数；只有首个订阅与最后一个退订上行。
    pub(super) fn acquire_subscription(
        &self,
        generation: UpstreamGeneration,
        uri: &str,
    ) -> Result<bool, McpError> {
        let mut subscriptions = self.subscriptions.lock();
        if subscriptions.generation != Some(generation) {
            return Err(self.stale_subscription_error());
        }
        let count = subscriptions.by_uri.entry(uri.to_owned()).or_insert(0);
        *count += 1;
        Ok(*count == 1)
    }

    pub(super) fn rollback_subscription(&self, generation: UpstreamGeneration, uri: &str) {
        let mut subscriptions = self.subscriptions.lock();
        if subscriptions.generation != Some(generation) {
            return;
        }
        if let Some(count) = subscriptions.by_uri.get_mut(uri) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                subscriptions.by_uri.remove(uri);
            }
        }
    }

    pub(super) fn release_subscription(
        &self,
        generation: UpstreamGeneration,
        uri: &str,
    ) -> Result<bool, McpError> {
        let mut subscriptions = self.subscriptions.lock();
        if subscriptions.generation != Some(generation) {
            return Err(self.stale_subscription_error());
        }
        let Some(count) = subscriptions.by_uri.get_mut(uri) else {
            return Ok(false);
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            subscriptions.by_uri.remove(uri);
            return Ok(true);
        }
        Ok(false)
    }

    /// 上游退订失败时恢复最后一个引用，保证下游重试仍会真正上行。
    pub(super) fn restore_subscription(&self, generation: UpstreamGeneration, uri: &str) {
        let mut subscriptions = self.subscriptions.lock();
        if subscriptions.generation != Some(generation) {
            return;
        }
        *subscriptions.by_uri.entry(uri.to_owned()).or_insert(0) += 1;
    }
}
