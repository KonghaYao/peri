//! Pool-owned server control. Operations share the ordinary transport and task owner.
use super::client::McpConnectionKey;
use super::client::McpPoolError;
use super::client::McpServiceOwner;
use super::client::McpServiceWrapper;
use super::client::SHUTDOWN_TIMEOUT;
use super::reconnect::ConnectOutcome;
use super::task_scope::McpTaskKey;
use super::ClientStatus;
use super::McpClientPool;
use super::OAuthStatus;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_control::{
    McpControlAction, McpControlError, McpControlRequest, McpControlResponse, McpControlStatus,
    McpRegistrationConfig, McpServerControl, SessionMcpCatalogPort, SessionMcpCatalogSnapshot,
};
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::tools::BaseTool;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Weak;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(crate) struct PoolControl {
    slots: parking_lot::Mutex<HashMap<String, Arc<ServerSlot>>>,
    projections: parking_lot::Mutex<Vec<ProjectionBinding>>,
}
#[derive(Default)]
pub(crate) struct ServerSlot {
    pub(crate) operation: Arc<tokio::sync::Mutex<()>>,
    pub(crate) control_operation: tokio::sync::Mutex<()>,
    pub(crate) stopped: AtomicBool,
    pub(crate) transient: AtomicBool,
    pub(crate) services: parking_lot::Mutex<Vec<Arc<McpServiceOwner>>>,
    #[cfg(not(target_os = "emscripten"))]
    pub(crate) processes: parking_lot::Mutex<Vec<Arc<super::client::process::McpProcessOwner>>>,
    /// 当前进行中的首次加载 attempt；`None` 表示可发起下一次显式加载。
    pub(crate) load_attempt: parking_lot::Mutex<Option<Arc<LoadAttempt>>>,
    /// 本 slot 内 attempt 序号，仅用于任务键唯一化。
    next_load_attempt: AtomicU64,
    /// 配置版本：配置被登记/移除时推进，attempt 提交前必须仍匹配。
    config_revision: AtomicU64,
}

impl ServerSlot {
    pub(crate) fn note_config_revision(&self) {
        self.config_revision.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn config_revision(&self) -> u64 {
        self.config_revision.load(Ordering::Acquire)
    }

    fn next_load_attempt(&self) -> u64 {
        self.next_load_attempt.fetch_add(1, Ordering::AcqRel)
    }
}

/// 一次首次加载 attempt 的共享结果。
///
/// 等待者只观察同一个结果；单个等待者退出只减少计数，全部退出才取消连接。
/// attempt 被更晚操作取代或服务器停止时，由 `finish` 幂等收口，迟到结果不能覆盖。
pub(crate) struct LoadAttempt {
    server: String,
    config: super::McpServerConfig,
    config_revision: u64,
    old_status: Option<ClientStatus>,
    cancel: CancellationToken,
    waiters: AtomicUsize,
    /// 单值广播：set-once，等待者在订阅后重读，不存在丢失唤醒窗口。
    outcome: tokio::sync::watch::Sender<Option<Arc<LoadOutcome>>>,
}

pub(crate) enum LoadOutcome {
    Ready,
    Failed(McpPoolError),
    Cancelled,
}

impl LoadAttempt {
    fn new(
        server: String,
        config: super::McpServerConfig,
        config_revision: u64,
        old_status: Option<ClientStatus>,
    ) -> Self {
        Self {
            server,
            config,
            config_revision,
            old_status,
            cancel: CancellationToken::new(),
            waiters: AtomicUsize::new(0),
            outcome: tokio::sync::watch::channel(None).0,
        }
    }

    fn add_waiter(&self) {
        self.waiters.fetch_add(1, Ordering::AcqRel);
    }

    /// 只保留第一个结果：stop / 关闭 / 任务收尾可能竞争同一个 attempt。
    fn finish(&self, outcome: LoadOutcome) {
        self.outcome.send_if_modified(|slot| {
            if slot.is_none() {
                *slot = Some(Arc::new(outcome));
                true
            } else {
                false
            }
        });
    }

    fn current(&self) -> Option<Arc<LoadOutcome>> {
        self.outcome.borrow().clone()
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn cancelled_error(&self) -> McpPoolError {
        McpPoolError::ConnectionFailed {
            server: self.server.clone(),
            reason: "MCP load attempt was cancelled".into(),
        }
    }

    async fn wait(&self) -> Result<(), McpPoolError> {
        let mut receiver = self.outcome.subscribe();
        loop {
            if let Some(outcome) = receiver.borrow_and_update().clone() {
                return match &*outcome {
                    LoadOutcome::Ready => Ok(()),
                    LoadOutcome::Failed(error) => Err(error.clone()),
                    LoadOutcome::Cancelled => Err(self.cancelled_error()),
                };
            }
            if receiver.changed().await.is_err() {
                return Err(self.cancelled_error());
            }
        }
    }

    fn leave(&self) {
        if self.waiters.fetch_sub(1, Ordering::AcqRel) == 1 && self.current().is_none() {
            self.cancel.cancel();
        }
    }
}

pub(crate) struct LoadWaiter {
    attempt: Arc<LoadAttempt>,
}

impl LoadWaiter {
    pub(crate) async fn wait(self) -> Result<(), McpPoolError> {
        self.attempt.wait().await
    }
}

impl Drop for LoadWaiter {
    fn drop(&mut self) {
        self.attempt.leave();
    }
}
struct ProjectionBinding {
    registry: Weak<McpSkillRegistry>,
    commands: Option<Weak<CommandRegistry>>,
    session: Option<String>,
}
impl PoolControl {
    /// 关闭 owner / pool 时取消全部未完成的首次加载，并立即唤醒等待方。
    pub(crate) fn cancel_load_attempts(&self) {
        let slots: Vec<_> = self.slots.lock().values().cloned().collect();
        for slot in slots {
            let attempt = slot.load_attempt.lock().take();
            if let Some(attempt) = attempt {
                attempt.cancel.cancel();
                attempt.finish(LoadOutcome::Cancelled);
            }
        }
    }
}
impl McpClientPool {
    pub(crate) fn server_slot(&self, name: &str) -> Arc<ServerSlot> {
        self.control
            .slots
            .lock()
            .entry(name.into())
            .or_default()
            .clone()
    }

    /// Read-only slot lookup for catalog projection: snapshots must not create
    /// lifecycle state for servers that were never touched.
    pub(crate) fn existing_server_slot(&self, name: &str) -> Option<Arc<ServerSlot>> {
        self.control.slots.lock().get(name).cloned()
    }
    /// 取消该 server 当前的首次加载 attempt；返回是否有 attempt 需要任务收口。
    pub(crate) fn cancel_load_attempt(&self, name: &str) -> bool {
        let Some(slot) = self.existing_server_slot(name) else {
            return false;
        };
        let attempt = slot.load_attempt.lock().take();
        if let Some(attempt) = attempt {
            attempt.cancel.cancel();
            attempt.finish(LoadOutcome::Cancelled);
            true
        } else {
            false
        }
    }
    /// 没有就绪句柄时收口该 slot 已登记但未提交的连接。
    ///
    /// attempt 任务被 abort 后不会执行自身清理；卸载/取消路径用它释放迟到连接。
    pub(crate) async fn discard_uncommitted_resources(&self, name: &str) {
        if self
            .client(name)
            .is_some_and(|handle| handle.status == ClientStatus::Connected)
        {
            return;
        }
        let Some(slot) = self.existing_server_slot(name) else {
            return;
        };
        let services = slot.services.lock().clone();
        for service in services {
            let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        #[cfg(not(target_os = "emscripten"))]
        {
            let processes = slot.processes.lock().clone();
            for process in processes {
                let _ = peri_time::timeout(SHUTDOWN_TIMEOUT, process.close()).await;
            }
        }
        self.release_stopped_resources(name);
    }
    pub(crate) fn track_server_service(
        &self,
        name: &str,
        service: McpServiceWrapper,
    ) -> McpServiceWrapper {
        let peer = service.peer().clone();
        let owner = self.own_service(service);
        self.server_slot(name).services.lock().push(owner.clone());
        McpServiceWrapper::shared(owner, peer)
    }

    /// 首次加载（plan 6.2）：已就绪直接复用；否则加入或发起该 server 的唯一 attempt。
    ///
    /// 等待者共享同一结果；单个等待者退出不影响其他等待者，全部退出才取消连接。
    pub(crate) async fn ensure_loaded(
        self: &Arc<Self>,
        server_name: &str,
    ) -> Result<(), McpPoolError> {
        let slot = self.server_slot(server_name);
        if slot.stopped.load(Ordering::Acquire) || !self.is_open() {
            return Err(not_connected(server_name));
        }
        if self
            .client(server_name)
            .is_some_and(|handle| handle.status == ClientStatus::Connected)
        {
            return Ok(());
        }
        let config = self
            .configs
            .read()
            .get(server_name)
            .cloned()
            .ok_or_else(|| not_connected(server_name))?;
        if config.disabled == Some(true) {
            return Err(not_connected(server_name));
        }
        self.acquire_load_attempt(&slot, server_name, config)?
            .wait()
            .await
    }

    fn acquire_load_attempt(
        self: &Arc<Self>,
        slot: &Arc<ServerSlot>,
        server_name: &str,
        config: super::McpServerConfig,
    ) -> Result<LoadWaiter, McpPoolError> {
        let mut current = slot.load_attempt.lock();
        if let Some(attempt) = current.as_ref() {
            if !attempt.is_cancelled() {
                attempt.add_waiter();
                return Ok(LoadWaiter {
                    attempt: attempt.clone(),
                });
            }
            // 已取消但任务尚未收尾：本次显式调用立即替换，不等待迟到的收尾。
            let stale = current.take().expect("current attempt was just observed");
            stale.finish(LoadOutcome::Cancelled);
        }
        // stop 与 attempt 创建共用本锁，避免 close 之后插入新的加载。
        if slot.stopped.load(Ordering::Acquire) || !self.is_open() {
            return Err(not_connected(server_name));
        }
        let attempt = Arc::new(LoadAttempt::new(
            server_name.to_owned(),
            config,
            slot.config_revision(),
            self.client(server_name).map(|handle| handle.status.clone()),
        ));
        attempt.add_waiter();
        *current = Some(attempt.clone());
        let key = McpTaskKey::LoadAttempt {
            server: server_name.to_owned(),
            attempt: slot.next_load_attempt(),
        };
        let spawned = self.task_spawner.spawn(key, {
            let pool = self.clone();
            let slot = slot.clone();
            let attempt = attempt.clone();
            async move { pool.run_load_attempt(slot, attempt).await }
        });
        drop(current);
        if let Err(error) = spawned {
            // 没有受管任务会收口时立即给等待者一个稳定结果，避免悬挂。
            let mut current = slot.load_attempt.lock();
            if current
                .as_ref()
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &attempt))
            {
                *current = None;
            }
            drop(current);
            attempt.finish(LoadOutcome::Failed(McpPoolError::ConnectionFailed {
                server: server_name.to_owned(),
                reason: format!("MCP load attempt was not admitted: {error}"),
            }));
        }
        Ok(LoadWaiter { attempt })
    }

    /// attempt 仍是当前值、配置版本未变且未被取消，才允许发布结果。
    fn load_attempt_admitted(&self, slot: &ServerSlot, attempt: &LoadAttempt) -> bool {
        self.is_open()
            && !slot.stopped.load(Ordering::Acquire)
            && !attempt.is_cancelled()
            && slot.config_revision() == attempt.config_revision
            && slot
                .load_attempt
                .lock()
                .as_ref()
                .is_some_and(|current| std::ptr::eq(Arc::as_ptr(current), attempt))
    }

    fn finish_load_attempt(&self, slot: &ServerSlot, attempt: &LoadAttempt, outcome: LoadOutcome) {
        {
            let mut current = slot.load_attempt.lock();
            if current
                .as_ref()
                .is_some_and(|candidate| std::ptr::eq(Arc::as_ptr(candidate), attempt))
            {
                *current = None;
            }
        }
        attempt.finish(outcome);
    }

    async fn run_load_attempt(self: Arc<Self>, slot: Arc<ServerSlot>, attempt: Arc<LoadAttempt>) {
        // 与 initialize / reconnect 共用同一异步串行化：同 server 的连接操作不并发。
        let operation = tokio::select! {
            _ = attempt.cancel.cancelled() => {
                self.finish_load_attempt(&slot, &attempt, LoadOutcome::Cancelled);
                return;
            }
            operation = slot.operation.clone().lock_owned() => operation,
        };
        let server_name = attempt.server.clone();
        // 串行化后再确认一次：等待期间可能已经由 reconnect 就绪，或已被 stop 取消。
        if !self.load_attempt_admitted(&slot, &attempt) {
            drop(operation);
            self.finish_load_attempt(&slot, &attempt, LoadOutcome::Cancelled);
            return;
        }
        if self
            .client(&server_name)
            .is_some_and(|handle| handle.status == ClientStatus::Connected)
        {
            drop(operation);
            self.finish_load_attempt(&slot, &attempt, LoadOutcome::Ready);
            return;
        }
        let admit = || self.load_attempt_admitted(&slot, &attempt);
        let result = tokio::select! {
            _ = attempt.cancel.cancelled() => None,
            result = self.connect_managed(
                &server_name,
                &attempt.config,
                None,
                false,
                &admit,
            ) => Some(result),
        };
        let outcome = match result {
            None => LoadOutcome::Cancelled,
            Some(Ok(ConnectOutcome::NeedsAuthorization)) => {
                LoadOutcome::Failed(McpPoolError::ConnectionFailed {
                    server: server_name.clone(),
                    reason: "OAuth authorization required".into(),
                })
            }
            Some(Ok(ConnectOutcome::Connected(ready))) => {
                match self
                    .commit_ready(&server_name, ready, attempt.old_status.clone(), || {
                        self.load_attempt_admitted(&slot, &attempt)
                    })
                    .await
                {
                    Ok(()) => LoadOutcome::Ready,
                    Err(error) => LoadOutcome::Failed(error),
                }
            }
            Some(Err(error)) => LoadOutcome::Failed(error),
        };
        // 取消可能丢弃了已建立但未提交的连接：在释放 operation 前收口，
        // 避免取消后的重试与旧 attempt 的 service 同时存活。
        if matches!(outcome, LoadOutcome::Cancelled) {
            self.discard_uncommitted_resources(&server_name).await;
        }
        drop(operation);
        self.finish_load_attempt(&slot, &attempt, outcome);
    }
    pub(crate) fn bind_discovery_projection(
        &self,
        registry: &Arc<McpSkillRegistry>,
        commands: Option<&Arc<CommandRegistry>>,
        session: Option<&str>,
    ) {
        let mut projections = self.control.projections.lock();
        projections.retain(|p| p.registry.strong_count() > 0);
        if !projections.iter().any(|p| {
            p.registry.ptr_eq(&Arc::downgrade(registry)) && p.session.as_deref() == session
        }) {
            projections.push(ProjectionBinding {
                registry: Arc::downgrade(registry),
                commands: commands.map(Arc::downgrade),
                session: session.map(str::to_owned),
            });
        }
    }
    fn withdraw_discovery(&self) {
        let projections: Vec<_> = self
            .control
            .projections
            .lock()
            .iter()
            .filter_map(|p| {
                p.registry.upgrade().map(|r| {
                    (
                        r,
                        p.commands.as_ref().and_then(Weak::upgrade),
                        p.session.clone(),
                    )
                })
            })
            .collect();
        for (registry, commands, session) in projections {
            let connected: Vec<_> = self
                .clients_visible_to(session.as_deref())
                .into_iter()
                .map(|h| (h.name.clone(), h as peri_acp_types::mcp_skills::HandleToken))
                .collect();
            registry.project_connected(&connected);
            if let Some(commands) = commands {
                let sources: Vec<_> = connected
                    .iter()
                    .map(|(name, h)| (super::skill_discovery::mcp_source_key(name), h.clone()))
                    .collect();
                commands.project_sources(&sources);
            }
        }
    }
    pub(crate) fn control_descriptor(&self, name: &str) -> McpServerControl {
        let configs = self.configs.read();
        let config = configs.get(name);
        let source = config
            .and_then(|c| c.source.clone())
            .or_else(|| self.client(name).and_then(|handle| handle.source.clone()));
        let reason = match source.as_ref() {
            Some(super::config::ConfigSource::Builtin { .. }) => Some("builtin"),
            Some(super::config::ConfigSource::WorkspaceRemote) => Some("workspace_remote"),
            Some(super::config::ConfigSource::Acp) => Some("acp"),
            _ if config.is_some_and(|c| c.system_mcp == Some(true)) => Some("system_mcp"),
            _ if config.is_none() => Some("unavailable"),
            _ => None,
        };
        reason
            .map(McpServerControl::unavailable)
            .unwrap_or_else(McpServerControl::available)
    }
    pub(crate) async fn close_server_connection(
        self: &Arc<Self>,
        name: &str,
        stop_oauth: bool,
    ) -> Result<(), McpControlError> {
        let slot = self.server_slot(name);
        let cancelled_attempt = {
            let _admission = self.lifecycle_registration.lock();
            slot.stopped.store(true, Ordering::Release);
            let attempt = slot.load_attempt.lock().take();
            if let Some(attempt) = &attempt {
                attempt.cancel.cancel();
                attempt.finish(LoadOutcome::Cancelled);
            }
            if let Some(handle) = self.client(name) {
                handle.call_gate.revoke();
            }
            attempt
        };
        if cancelled_attempt.is_some() {
            // 受管任务先收口（abort + 等待），迟到连接不能提交。
            self.task_spawner.stop_load_attempts(name).await;
        }
        let handle = self.client(name);
        if let Some(handle) = &handle {
            handle.call_gate.revoke();
        }
        // Withdraw before awaiting: late discovery is rejected by registry token checks.
        if let Some(old) = handle.as_ref() {
            let mut stopped = (**old).clone();
            stopped.peer = None;
            stopped.tools.clear();
            stopped.resources.clear();
            stopped.status = ClientStatus::Disconnected;
            self.clients.write().insert(name.into(), Arc::new(stopped));
        }
        self.system_readiness.clear_evidence(name);
        self.bump_catalog_generation();
        self.withdraw_discovery();
        if let Some(flow) = self.active_oauth_flow(name).filter(|_| stop_oauth) {
            self.revoke_oauth_connection(&McpConnectionKey::static_server(name));
            self.stop_background(&McpTaskKey::OAuth(flow)).await;
        }
        self.stop_background(&McpTaskKey::Subscription(name.into()))
            .await;
        self.task_spawner.stop_server_discovery(name).await;
        self.task_spawner
            .stop_instance_resource_subscriptions(name)
            .await;
        if let Some(handle) = handle {
            if peri_time::timeout(SHUTDOWN_TIMEOUT, handle.call_gate.drain())
                .await
                .is_err()
            {
                return Err(incomplete());
            }
        }
        // Move resources into their pool-retained owner before any cancellation point.
        if let Some(owner) = self.take_service_for_close(name) {
            slot.services.lock().push(owner);
        }
        let resources = slot.services.lock().clone();
        let mut failed = false;
        for service in resources {
            match service.close_with_timeout(SHUTDOWN_TIMEOUT).await {
                Ok(Some(_)) => {}
                Ok(None) => failed = true,
                Err(error) => {
                    tracing::error!(server=name,%error,"MCP service cleanup failed");
                    failed = true;
                }
            }
        }
        slot.services.lock().retain(|s| !s.is_stopped());
        self.close_builtin_task(name).await;
        #[cfg(not(target_os = "emscripten"))]
        {
            let processes = slot.processes.lock().clone();
            for process in processes {
                if !matches!(
                    peri_time::timeout(SHUTDOWN_TIMEOUT, process.close()).await,
                    Ok(Ok(()))
                ) {
                    failed = true;
                }
            }
            slot.processes.lock().retain(|p| !p.is_stopped());
        }
        // 已收口的 owner 不再留在共享保留表；重复 load/close 不随轮次增长。
        self.release_stopped_resources(name);
        if failed {
            return Err(incomplete());
        }
        Ok(())
    }
    pub(crate) fn bump_catalog_generation(&self) {
        self.catalog_generation.fetch_add(1, Ordering::AcqRel);
    }
    pub async fn control_mcp(
        self: Arc<Self>,
        request: McpControlRequest,
    ) -> Result<McpControlResponse, McpControlError> {
        request.validate()?;
        if let McpControlRequest::Register {
            config: McpRegistrationConfig::Http { headers, .. },
            ..
        } = &request
        {
            for (name, value) in headers {
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| McpControlError::invalid("invalid HTTP header name"))?;
                reqwest::header::HeaderValue::from_str(value)
                    .map_err(|_| McpControlError::invalid("invalid HTTP header value"))?;
            }
        }
        let session_id = request.session_id().to_owned();
        let name = request.server_name().to_owned();
        let action = request.action();
        // Configuration loading must establish the namespace before temporary registration.
        if matches!(*self.init_status.read(), super::McpInitStatus::Pending) {
            return Err(McpControlError::new(
                -32000,
                "server_busy",
                "MCP configuration is initializing",
            ));
        }
        let slot = self.server_slot(&name);
        let _control = slot.control_operation.try_lock().map_err(|_| {
            McpControlError::new(-32000, "server_busy", "MCP server operation is in progress")
        })?;
        let register_operation = if action == McpControlAction::Register {
            Some(slot.operation.clone().try_lock_owned().map_err(|_| {
                McpControlError::new(-32000, "server_busy", "MCP server operation is in progress")
            })?)
        } else {
            None
        };
        {
            let _admission = self.lifecycle_registration.lock();
            if !self.is_open() {
                return Err(McpControlError::new(
                    -32000,
                    "pool_closed",
                    "MCP pool is closing",
                ));
            }
            match &request {
                McpControlRequest::Register { config, .. } => {
                    if matches!(config, McpRegistrationConfig::Stdio { .. })
                        && !self.stdio_available.load(Ordering::Acquire)
                    {
                        return Err(McpControlError::unsupported());
                    }
                    let normalized =
                        super::tool_bridge::sanitize_name_component(&name).to_ascii_lowercase();
                    if self
                        .configs
                        .read()
                        .keys()
                        .chain(self.clients.read().keys())
                        .any(|existing| {
                            super::tool_bridge::sanitize_name_component(existing)
                                .to_ascii_lowercase()
                                == normalized
                        })
                    {
                        return Err(McpControlError::new(
                            -32000,
                            "name_conflict",
                            "MCP server name is already registered",
                        ));
                    }
                    self.configs
                        .write()
                        .insert(name.clone(), config.clone().into_server_config());
                    slot.note_config_revision();
                    slot.transient.store(true, Ordering::Release);
                }
                _ => {
                    if !self.configs.read().contains_key(&name) {
                        return Err(McpControlError::new(
                            -32001,
                            "server_not_found",
                            "MCP server was not found",
                        ));
                    }
                    if !self.is_visible_to_session(&name, &session_id) {
                        return Err(McpControlError::new(
                            -32001,
                            "server_not_found",
                            "MCP server was not found",
                        ));
                    }
                    if !self.control_descriptor(&name).restart {
                        return Err(McpControlError::unsupported());
                    }
                }
            }
        }
        // Control has exclusive admission before cancelling the exact owned flows.
        // New reconnect/OAuth admission checks this same gate, so it cannot revive
        // the stopped server between cancellation and acquiring the connection lock.
        if let Some(flow) = self.active_oauth_flow(&name) {
            self.revoke_oauth_connection(&McpConnectionKey::static_server(&name));
            self.stop_background(&McpTaskKey::OAuth(flow)).await;
            self.revoke_oauth_connection(&McpConnectionKey::static_server(&name));
        }
        self.stop_background(&McpTaskKey::Reconnect(name.clone()))
            .await;
        let _operation = match register_operation {
            Some(operation) => operation,
            None => slot.operation.clone().try_lock_owned().map_err(|_| {
                McpControlError::new(-32000, "server_busy", "MCP server operation is in progress")
            })?,
        };
        let status = if action == McpControlAction::Stop {
            self.close_server_connection(&name, true).await?;
            McpControlStatus::Stopped
        } else {
            self.close_server_connection(&name, true).await?;
            slot.stopped.store(false, Ordering::Release);
            let result = self.reconnect_managed(&name, None, false).await;
            match self.client(&name) {
                Some(h) if h.oauth_status == OAuthStatus::NeedsAuthorization => {
                    McpControlStatus::NeedsAuthorization
                }
                Some(h) if h.status == ClientStatus::Connected && result.is_ok() => {
                    McpControlStatus::Connected
                }
                _ => {
                    let error = result
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "MCP connection did not become ready".into());
                    tracing::error!(server=%name,%error,"MCP control failed");
                    if self.is_open() && self.client(&name).is_none() {
                        McpClientPool::insert_failed(&self, &name, error.clone());
                    }
                    return Err(McpControlError::new(-32603, "connection_failed", error));
                }
            }
        };
        self.withdraw_discovery();
        Ok(McpControlResponse {
            session_id,
            server_name: name,
            action,
            status,
        })
    }
}
fn incomplete() -> McpControlError {
    McpControlError::new(
        -32603,
        "shutdown_incomplete",
        "MCP connection cleanup is incomplete; retry stop",
    )
}

fn not_connected(server: &str) -> McpPoolError {
    McpPoolError::NotConnected {
        server: server.to_owned(),
        status: ClientStatus::Disconnected,
    }
}

struct PoolCatalog {
    pool: Arc<McpClientPool>,
    session: String,
}
impl SessionMcpCatalogPort for PoolCatalog {
    fn snapshot(&self) -> Arc<SessionMcpCatalogSnapshot> {
        loop {
            let generation = self.pool.catalog_generation.load(Ordering::Acquire);
            let mut bridges = super::tool_bridge::build_typed_tool_bridges_visible_to(
                &self.pool,
                Some(&self.session),
            );
            let required: std::collections::BTreeMap<_, _> = self
                .pool
                .system_requirements()
                .into_iter()
                .filter(|r| {
                    self.pool
                        .client(&r.server)
                        .is_some_and(|h| h.status == ClientStatus::Connected)
                })
                .map(|r| (r.server, r.required_tools))
                .collect();
            // Readiness validated these immutable system declarations before the first Reason.
            for bridge in &mut bridges {
                if required
                    .get(bridge.mcp_server_name().unwrap_or_default())
                    .is_some_and(|names| {
                        names.iter().any(|name| name == bridge.original_tool_name())
                    })
                {
                    *bridge = bridge.clone().with_system_direct();
                }
            }
            if generation != self.pool.catalog_generation.load(Ordering::Acquire) {
                continue;
            }
            let mut tools: Vec<Arc<dyn BaseTool>> = bridges
                .into_iter()
                .map(|bridge| Arc::new(bridge) as Arc<dyn BaseTool>)
                .collect();
            // Dynamic Loader 与远端工具同属池内目录：来源关闭/禁用即消失（plan 5.2）。
            tools.extend(super::loader::dynamic_loader_tools(
                &self.pool,
                Some(&self.session),
            ));
            return Arc::new(SessionMcpCatalogSnapshot { generation, tools });
        }
    }
}
pub(crate) fn catalog_source(
    pool: Arc<McpClientPool>,
    session: &str,
) -> Arc<dyn SessionMcpCatalogPort> {
    Arc::new(PoolCatalog {
        pool,
        session: session.into(),
    })
}

#[cfg(test)]
#[path = "control_test.rs"]
mod tests;

#[cfg(test)]
#[path = "load_attempt_test.rs"]
mod load_attempt_tests;

#[cfg(test)]
#[path = "dynamic_cleanup_test.rs"]
mod dynamic_cleanup_tests;
