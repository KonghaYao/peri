//! `UpstreamProxy` 的内部状态：代次、任务映射、订阅计数与健康广播。
//!
//! 与代理转发分离，避免单文件越过模块上限；本模块不做协议映射。

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
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
    generation: UpstreamGeneration,
}

/// progress token 映射与早到通知缓冲。
///
/// 上游 token 由 rmcp 在发送时生成，通知可能早于 `RequestHandle` 返回；
/// 未命中映射的通知先缓冲，注册映射时按序补发，避免丢进度。
#[derive(Default)]
pub(super) struct ProgressRegistry {
    mappings: HashMap<ProgressToken, ProgressToken>,
    pending: HashMap<ProgressToken, Vec<ProgressNotificationParam>>,
    pending_total: usize,
}

const PENDING_PROGRESS_LIMIT: usize = 64;

pub(super) struct ProxyInner {
    pub(super) capability: String,
    pub(super) lifecycle: Mutex<Lifecycle>,
    pub(super) health: watch::Sender<CapabilityHealth>,
    pub(super) downstream: RwLock<Option<Peer<RoleServer>>>,
    tasks: Mutex<TaskRegistry>,
    pub(super) progress: Mutex<ProgressRegistry>,
    subscriptions: Mutex<SubscriptionCounts>,
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
        drop(lifecycle);
        self.tasks.lock().clear();
        *self.progress.lock() = ProgressRegistry::default();
        self.subscriptions.lock().clear();
        self.health.send_replace(CapabilityHealth::Ready);
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
        drop(lifecycle);
        self.tasks.lock().clear();
        *self.progress.lock() = ProgressRegistry::default();
        self.subscriptions.lock().clear();
        self.health
            .send_replace(CapabilityHealth::Degraded { reason });
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

    pub(super) async fn send_downstream(&self, notification: ServerNotification) {
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
        upstream_token: ProgressToken,
        downstream_token: ProgressToken,
    ) -> Vec<ProgressNotificationParam> {
        let mut progress = self.progress.lock();
        progress
            .mappings
            .insert(upstream_token.clone(), downstream_token.clone());
        let mut pending = progress.pending.remove(&upstream_token).unwrap_or_default();
        progress.pending_total = progress.pending_total.saturating_sub(pending.len());
        for params in &mut pending {
            params.progress_token = downstream_token.clone();
        }
        pending
    }

    pub(super) fn unregister_progress(&self, upstream_token: &ProgressToken) {
        let mut progress = self.progress.lock();
        progress.mappings.remove(upstream_token);
        if let Some(pending) = progress.pending.remove(upstream_token) {
            progress.pending_total = progress.pending_total.saturating_sub(pending.len());
        }
    }

    /// 已注册映射时替换为下游 token；未知 token 先缓冲，超限则丢弃。
    pub(super) fn route_progress(
        &self,
        mut params: ProgressNotificationParam,
    ) -> Option<ProgressNotificationParam> {
        let mut progress = self.progress.lock();
        if let Some(downstream) = progress.mappings.get(&params.progress_token) {
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
        progress
            .pending
            .entry(params.progress_token.clone())
            .or_default()
            .push(params);
        None
    }

    pub(super) async fn notify_upstream_cancelled(
        &self,
        peer: &Peer<RoleClient>,
        request_id: rmcp::model::RequestId,
        reason: &str,
    ) {
        let _ = peer
            .notify_cancelled(rmcp::model::CancelledNotificationParam::new(
                Some(request_id),
                Some(reason.to_owned()),
            ))
            .await;
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

    /// 资源订阅按 URI 引用计数；只有首个订阅与最后一个退订上行。
    pub(super) fn acquire_subscription(&self, generation: UpstreamGeneration, uri: &str) -> bool {
        let mut subscriptions = self.subscriptions.lock();
        if subscriptions.generation != generation {
            subscriptions.by_uri.clear();
            subscriptions.generation = generation;
        }
        let count = subscriptions.by_uri.entry(uri.to_owned()).or_insert(0);
        *count += 1;
        *count == 1
    }

    pub(super) fn rollback_subscription(&self, generation: UpstreamGeneration, uri: &str) {
        let mut subscriptions = self.subscriptions.lock();
        if subscriptions.generation != generation {
            return;
        }
        if let Some(count) = subscriptions.by_uri.get_mut(uri) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                subscriptions.by_uri.remove(uri);
            }
        }
    }

    pub(super) fn release_subscription(&self, generation: UpstreamGeneration, uri: &str) -> bool {
        let mut subscriptions = self.subscriptions.lock();
        if subscriptions.generation != generation {
            return false;
        }
        let Some(count) = subscriptions.by_uri.get_mut(uri) else {
            return false;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            subscriptions.by_uri.remove(uri);
            return true;
        }
        false
    }
}

impl SubscriptionCounts {
    fn clear(&mut self) {
        self.by_uri.clear();
    }
}
