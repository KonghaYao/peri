//! Pool 任务准入、连接提交与唯一 service-close transaction。

use super::{
    ClientStatus, McpClientHandle, McpClientPool, McpServiceWrapper, OAuthStatus, SHUTDOWN_TIMEOUT,
};
use crate::mcp::builtin::runtime::{
    BuiltinServerExit, BuiltinServerTask, BUILTIN_CONVERGE_TIMEOUT,
};
use peri_acp_types::ports::McpPoolShutdownReport;
use std::sync::Arc;

pub(super) enum ServiceShutdownState {
    Idle,
    Running {
        handle: tokio::task::JoinHandle<(McpPoolShutdownReport, Vec<(String, McpServiceWrapper)>)>,
        total_services: usize,
    },
    Retry {
        report: McpPoolShutdownReport,
        services: Vec<(String, McpServiceWrapper)>,
    },
    Terminal(McpPoolShutdownReport),
}

async fn close_services(
    services: Vec<(String, McpServiceWrapper)>,
    mut settled_services: usize,
    mut failed_services: usize,
) -> (McpPoolShutdownReport, Vec<(String, McpServiceWrapper)>) {
    let mut remaining = Vec::new();
    for (server_name, mut service) in services {
        match service.close_with_timeout(SHUTDOWN_TIMEOUT).await {
            Ok(Some(_reason)) => settled_services += 1,
            Ok(None) => {
                tracing::warn!(server = %server_name, "MCP service cleanup remained unfinished");
                remaining.push((server_name, service));
            }
            Err(error) => {
                settled_services += 1;
                failed_services += 1;
                tracing::warn!(server = %server_name, %error, "MCP service cleanup task failed");
            }
        }
    }
    let unfinished_services = remaining.len();
    let report = if unfinished_services == 0 {
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
    };
    (report, remaining)
}

impl McpClientPool {
    pub(crate) fn retain_service(&self, service: McpServiceWrapper) -> McpServiceWrapper {
        let peer = service.peer().clone();
        McpServiceWrapper::shared(self.own_service(service), peer)
    }

    pub(crate) fn own_service(&self, service: McpServiceWrapper) -> Arc<super::McpServiceOwner> {
        let _admission = self.lifecycle_registration.lock();
        let owner = Arc::new(super::McpServiceOwner::new(service));
        if !self.is_open() {
            owner.begin_close();
        }
        let mut services = self.shared_services.lock();
        services.retain(|service| !service.is_stopped());
        services.push(owner.clone());
        owner
    }

    async fn close_shared_services(&self) -> usize {
        let services = self.shared_services.lock().clone();
        for service in services {
            let _ = tokio::time::timeout(
                SHUTDOWN_TIMEOUT,
                service.close_with_timeout(SHUTDOWN_TIMEOUT),
            )
            .await;
        }
        let mut services = self.shared_services.lock();
        services.retain(|service| !service.is_stopped());
        services.len()
    }

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
        // builtin 实例的 server 半边是本进程 task：随 `services` 一并收口，不留 orphan。
        self.close_builtin_task(server_name).await;
        self.configs.write().remove(server_name);
        // 句柄与配置同时消失：本代发现证据一律失效，等待方立即重读事实。
        self.system_readiness.clear_evidence(server_name);
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
        // builtin 实例：server 半边 task 必须随连接一起收口（Disabled 不保留半开链路）。
        self.close_builtin_task(server_name).await;
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
        // 禁用不是「连接中」：本代证据失效，等待方立即得到 Disabled 事实。
        self.system_readiness.clear_evidence(server_name);
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
        // 关闭事务开始：全部发现证据失效并唤醒等待方（它们在重读时看到 PoolClosed）。
        self.system_readiness.clear_all_evidence();
        self.notifier.write().take();
        self.oauth_event_callback.write().take();
        self.pending_oauth_callbacks.lock().clear();
        self.active_oauth_flows.lock().clear();
        for process in self.processes.lock().iter() {
            process.begin_close();
        }
        for service in self.shared_services.lock().iter() {
            service.begin_close();
        }
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
            let handle = tokio::spawn(close_services(services, 0, 0));
            *transaction = ServiceShutdownState::Running {
                handle,
                total_services,
            };
        }

        if let ServiceShutdownState::Retry { report, services } = &mut *transaction {
            let McpPoolShutdownReport::Incomplete {
                settled_services,
                unfinished_services,
                failed_services,
            } = *report
            else {
                unreachable!()
            };
            let handle = tokio::spawn(close_services(
                std::mem::take(services),
                settled_services,
                failed_services,
            ));
            *transaction = ServiceShutdownState::Running {
                handle,
                total_services: settled_services + unfinished_services,
            };
        }

        let (report, remaining) = match &mut *transaction {
            ServiceShutdownState::Idle => unreachable!("shutdown transaction must be installed"),
            ServiceShutdownState::Retry { .. } => {
                unreachable!("retry transaction must be installed")
            }
            ServiceShutdownState::Terminal(report) => (*report, Vec::new()),
            ServiceShutdownState::Running {
                handle,
                total_services,
            } => match handle.await {
                Ok(result) => result,
                Err(error) => {
                    tracing::error!(%error, "MCP service shutdown transaction failed");
                    (
                        McpPoolShutdownReport::Incomplete {
                            settled_services: 0,
                            unfinished_services: *total_services,
                            failed_services: *total_services,
                        },
                        Vec::new(),
                    )
                }
            },
        };
        *transaction = if remaining.is_empty() {
            ServiceShutdownState::Terminal(report)
        } else {
            ServiceShutdownState::Retry {
                report,
                services: remaining,
            }
        };
        // builtin 实例：client service 已关闭（上面的 transaction），server 半边 task
        // 随之按「有界等待 → 未收敛才 abort」收口。非 builtin pool 这里是空操作。
        self.close_builtin_tasks().await;
        let unfinished_shared = self.close_shared_services().await;
        let unfinished_processes = self.close_processes().await + unfinished_shared;
        let report = match (report, unfinished_processes) {
            (report, 0) => report,
            (
                McpPoolShutdownReport::Complete {
                    settled_services,
                    failed_services,
                },
                unfinished_services,
            ) => McpPoolShutdownReport::Incomplete {
                settled_services,
                unfinished_services,
                failed_services,
            },
            (
                McpPoolShutdownReport::Incomplete {
                    settled_services,
                    unfinished_services,
                    failed_services,
                },
                processes,
            ) => McpPoolShutdownReport::Incomplete {
                settled_services,
                unfinished_services: unfinished_services + processes,
                failed_services,
            },
        };
        if report.is_complete() {
            self.lifecycle
                .store(2, std::sync::atomic::Ordering::Release);
        }
        report
    }

    /// 登记一个 builtin server task（与 `services` 同期登记）。
    ///
    /// 返回被替换的旧 task：调用方负责按 [`Self::close_builtin_task`] 的语义有界收敛它
    /// （生产路径在重连时已先移除旧项，因此这里通常返回 `None`）。
    pub(crate) fn register_builtin_task(
        &self,
        server_name: String,
        task: BuiltinServerTask,
    ) -> Option<BuiltinServerTask> {
        self.builtin_server_tasks.lock().insert(server_name, task)
    }

    /// 取出并移除某 server 的 builtin task，按「有界等待 → 未收敛才 abort」收敛。
    ///
    /// 无该条目时为无操作（非 builtin server 走这条调用不会产生第二条关闭路径）。
    /// 未在 [`BUILTIN_CONVERGE_TIMEOUT`] 内自然收敛时告警——正常关闭必须落在 `Quit`。
    pub(crate) async fn close_builtin_task(&self, server_name: &str) -> Option<BuiltinServerExit> {
        let mut task = { self.builtin_server_tasks.lock().remove(server_name) }?;
        let exit = task.converge(BUILTIN_CONVERGE_TIMEOUT).await;
        if matches!(exit, BuiltinServerExit::AbortedAfterTimeout) {
            tracing::warn!(
                server = %server_name,
                "builtin server task 未在有界等待内收敛，已 abort"
            );
        }
        Some(exit)
    }

    /// pool 关闭：排空 builtin task 表并逐项有界收敛。
    ///
    /// 返回「需要 abort 才结束」的实例名（升序）：这是异常信号，也是「无 orphan」的
    /// 可观察证据（返回值非空说明该实例没走 EOF 自然收敛路径）。
    pub(crate) async fn close_builtin_tasks(&self) -> Vec<String> {
        let tasks: Vec<(String, BuiltinServerTask)> =
            self.builtin_server_tasks.lock().drain().collect();
        let mut aborted = Vec::new();
        for (server_name, mut task) in tasks {
            if matches!(
                task.converge(BUILTIN_CONVERGE_TIMEOUT).await,
                BuiltinServerExit::AbortedAfterTimeout
            ) {
                tracing::warn!(
                    server = %server_name,
                    "builtin server task 未在有界等待内收敛，已 abort"
                );
                aborted.push(server_name);
            }
        }
        aborted.sort();
        aborted
    }

    /// builtin task 表当前登记数（「无 orphan」断言的可观察量；仅测试可见）。
    #[cfg(test)]
    pub(crate) fn builtin_task_count(&self) -> usize {
        self.builtin_server_tasks.lock().len()
    }
}
