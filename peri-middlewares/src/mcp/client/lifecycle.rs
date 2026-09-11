//! Pool 任务准入、连接提交与唯一 service-close transaction。

use super::{
    ClientStatus, McpClientHandle, McpClientPool, McpServiceWrapper, OAuthStatus, SHUTDOWN_TIMEOUT,
};
use peri_acp_types::ports::McpPoolShutdownReport;
use std::sync::Arc;

pub(super) enum ServiceShutdownState {
    Idle,
    Running {
        handle: tokio::task::JoinHandle<McpPoolShutdownReport>,
        total_services: usize,
    },
    Terminal(McpPoolShutdownReport),
}

async fn close_services(services: Vec<(String, McpServiceWrapper)>) -> McpPoolShutdownReport {
    let mut settled_services = 0;
    let mut unfinished_services = 0;
    let mut failed_services = 0;
    for (server_name, mut service) in services {
        match service.close_with_timeout(SHUTDOWN_TIMEOUT).await {
            Ok(Some(_reason)) => settled_services += 1,
            Ok(None) => {
                unfinished_services += 1;
                tracing::warn!(server = %server_name, "MCP service cleanup remained unfinished");
            }
            Err(error) => {
                settled_services += 1;
                failed_services += 1;
                tracing::warn!(server = %server_name, %error, "MCP service cleanup task failed");
            }
        }
    }
    if unfinished_services == 0 {
        McpPoolShutdownReport::Complete {
            settled_services,
            failed_services,
        }
    } else {
        McpPoolShutdownReport::Incomplete {
            settled_services,
            unfinished_services,
            failed_services,
        }
    }
}

impl McpClientPool {
    pub(crate) fn handle_generation(&self, handle: &Arc<McpClientHandle>) -> u64 {
        self.handle_generations
            .lock()
            .get(&handle.name)
            .and_then(|entries| {
                entries.iter().find_map(|(candidate, generation)| {
                    candidate
                        .upgrade()
                        .filter(|candidate| Arc::ptr_eq(candidate, handle))
                        .map(|_| *generation)
                })
            })
            .unwrap_or(0)
    }

    pub(super) fn advance_handle_generation(&self, handle: &Arc<McpClientHandle>) -> u64 {
        let generation = self
            .next_handle_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut generations = self.handle_generations.lock();
        let entries = generations.entry(handle.name.clone()).or_default();
        entries.retain(|(candidate, _)| candidate.strong_count() > 0);
        entries.push((Arc::downgrade(handle), generation));
        generation
    }

    pub async fn remove_server(self: &Arc<Self>, server_name: &str) {
        self.stop_background(&crate::mcp::task_scope::McpTaskKey::Subscription(
            server_name.to_string(),
        ))
        .await;
        self.clients.write().remove(server_name);
        let service = { self.services.lock().remove(server_name) };
        if let Some(mut svc) = service {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        self.configs.write().remove(server_name);
    }

    /// 将服务器标记为 Disabled：关闭连接但保留 config 和 handle（用于面板展示）
    pub async fn set_disabled(self: &Arc<Self>, server_name: &str) {
        self.stop_background(&crate::mcp::task_scope::McpTaskKey::Subscription(
            server_name.to_string(),
        ))
        .await;
        // 关闭实际连接
        let service = { self.services.lock().remove(server_name) };
        if let Some(mut svc) = service {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        // 更新 handle 为 Disabled 状态（保留 config 引用）
        let (source, url) = self
            .configs
            .read()
            .get(server_name)
            .map(|c| (c.source.clone(), c.url.clone()))
            .unwrap_or((None, None));
        self.clients.write().insert(
            server_name.to_string(),
            Arc::new(McpClientHandle {
                name: server_name.to_string(),
                version: None,
                cache_version: None,
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Disabled,
                oauth_status: OAuthStatus::default(),
                source,
                url,
                skills_capable: false,
                channel_capable: false,
            }),
        );
    }

    pub(crate) fn is_open(&self) -> bool {
        self.lifecycle.load(std::sync::atomic::Ordering::Acquire) == 0
    }

    pub fn begin_shutdown(&self) {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return;
        }
        self.lifecycle
            .store(1, std::sync::atomic::Ordering::Release);
        self.notifier.write().take();
        self.oauth_event_callback.write().take();
        self.pending_oauth_callbacks.lock().clear();
        self.active_oauth_flows.lock().clear();
    }

    pub fn spawn_background<F>(
        &self,
        key: crate::mcp::task_scope::McpTaskKey,
        future: F,
    ) -> Result<(), crate::mcp::task_scope::McpTaskScopeClosed>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return Err(crate::mcp::task_scope::TaskAdmissionError::OwnerClosed);
        }
        self.task_spawner.spawn(key, future)
    }

    pub async fn stop_background(&self, key: &crate::mcp::task_scope::McpTaskKey) {
        self.task_spawner.stop_key(key).await;
    }

    pub(crate) fn try_commit_connection(
        &self,
        name: String,
        handle: Arc<McpClientHandle>,
        service: McpServiceWrapper,
    ) -> Result<(), McpServiceWrapper> {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return Err(service);
        }
        self.advance_handle_generation(&handle);
        self.services.lock().insert(name.clone(), service);
        self.clients.write().insert(name, handle);
        Ok(())
    }

    pub async fn shutdown(&self) -> McpPoolShutdownReport {
        let mut transaction = self.service_shutdown.lock().await;
        if matches!(*transaction, ServiceShutdownState::Idle) {
            self.begin_shutdown();
            let names: Vec<String> = self.clients.read().keys().cloned().collect();
            for name in &names {
                if let Some(c) = self.clients.write().get_mut(name) {
                    if matches!(c.status, ClientStatus::Connected) {
                        tracing::info!(server = %name, "关闭连接");
                    }
                    let h = Arc::make_mut(c);
                    h.status = ClientStatus::Disconnected;
                    h.peer = None;
                }
            }
            let mut services: Vec<_> = self.services.lock().drain().collect();
            services.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            let total_services = services.len();
            let handle = tokio::spawn(close_services(services));
            *transaction = ServiceShutdownState::Running {
                handle,
                total_services,
            };
        }

        let report = match &mut *transaction {
            ServiceShutdownState::Idle => unreachable!("shutdown transaction must be installed"),
            ServiceShutdownState::Terminal(report) => return *report,
            ServiceShutdownState::Running {
                handle,
                total_services,
            } => match handle.await {
                Ok(report) => report,
                Err(error) => {
                    tracing::error!(%error, "MCP service shutdown transaction failed");
                    McpPoolShutdownReport::Incomplete {
                        settled_services: 0,
                        unfinished_services: *total_services,
                        failed_services: *total_services,
                    }
                }
            },
        };
        *transaction = ServiceShutdownState::Terminal(report);
        if report.is_complete() {
            self.lifecycle
                .store(2, std::sync::atomic::Ordering::Release);
        }
        report
    }
}
