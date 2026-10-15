//! Service snapshot 的周期协调器。
//!
//! 这个协调器只负责按既有节奏采样源、调用各责任单元并把观察结果交给统一
//! 提交门；它不在渲染期产生副作用。文件、memory、会话列表、会话元数据和会话
//! 服务各自维护 last-good/节流状态；提交门负责一次身份重验和类型明确的部分发布。
//! 本批只拆职责，仍串行完成单次 tick，不引入独立调度或新增查询；慢任务不会
//! 叠加相同 key 的刷新，但仍会等待本 tick 其它观察值。

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use peri_acp_types::permission::SharedPermissionMode;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::acp_client::AcpTuiClient;
use crate::app::service_registry::{ProcessResourceMonitor, SharedPeriConfig};
use crate::kit::atoms::{
    ACTIVE_EXECUTION_CWD, ACTIVE_SESSION_ID, THREAD_BROWSER_ARCHIVED, THREAD_BROWSER_SCOPE,
    THREAD_LIST_ERROR, THREAD_LIST_HAS_MORE, THREAD_LIST_PAGE_COUNT,
};

mod commit;
mod file_scan;
mod memory_scan;
mod session_metadata;
mod session_services;
mod thread_list_refresh;

use commit::{
    ProcessSnapshotPatch, ProjectionAtoms, ProjectionIdentity, ServiceSnapshotPatch,
    SessionSnapshotPatch, SnapshotPatch, commit_projection,
};
pub(crate) use commit::{SessionFieldUpdate, publish_session_fields};
use file_scan::FileScan;
use memory_scan::MemoryScan;
#[cfg(test)]
use session_metadata::derive_provider_and_model;
use session_metadata::{SessionMetadata, SessionMetadataRefresh, derive_providers};
use session_services::SessionServices;
use thread_list_refresh::ThreadListRefresh;

/// 快照源——从 `ServiceRegistry` 抽出的可跨 task 共享句柄。
#[derive(Clone)]
pub struct SnapshotSource {
    pub cwd: String,
    pub client: Option<AcpTuiClient>,
    pub peri_config: SharedPeriConfig,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub resource_monitor: Arc<Mutex<ProcessResourceMonitor>>,
    pub hooks: Vec<crate::kit::atoms::HookSummary>,
    pub plugins: Vec<crate::kit::atoms::PluginSummary>,
    pub providers: Vec<crate::kit::atoms::ProviderSummary>,
}

/// 启动 service snapshot 后台任务：每 2 秒轮询，并响应执行目录发布通知。
pub fn spawn_service_snapshot(
    src: SnapshotSource,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_service_snapshot(
        src,
        shutdown,
        SlowSnapshotRefresh::default(),
    ))
}

async fn run_service_snapshot(
    src: SnapshotSource,
    shutdown: CancellationToken,
    mut slow: SlowSnapshotRefresh,
) {
    let mut execution_cwd = src
        .client
        .as_ref()
        .map(AcpTuiClient::subscribe_execution_cwd);
    let mut interval = peri_time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(peri_time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                debug!("service_snapshot: shutdown signal received, exiting");
                break;
            }
            Ok(()) = async {
                match execution_cwd.as_mut() {
                    Some(changes) => changes.changed().await,
                    None => std::future::pending().await,
                }
            } => {}
            _ = interval.tick() => {}
        }
        if let Some(changes) = execution_cwd.as_mut() {
            changes.borrow_and_update();
        }
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            result = tick_once(&src, &mut slow) => {
                if let Err(error) = result {
                    warn!(%error, "service_snapshot: tick failed");
                }
            }
        }
    }
}

#[derive(Default)]
struct SlowSnapshotRefresh {
    services_identity: Option<(String, u64)>,
    services: SessionServices,
    files: FileScan,
    threads: ThreadListRefresh,
    memory: MemoryScan,
    metadata: SessionMetadataRefresh,
}

impl SlowSnapshotRefresh {
    fn reset_services(&mut self, identity: &Option<(String, u64)>) {
        if *identity != self.services_identity {
            self.services = SessionServices::default();
            self.services_identity = identity.clone();
        }
    }
}

/// 执行一次采样。每个责任单元完成自己的刷新后，只通过 `commit_projection` 发布。
async fn tick_once(
    src: &SnapshotSource,
    slow: &mut SlowSnapshotRefresh,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (memory_mb, cpu_percent) = {
        let mut monitor = src.resource_monitor.lock();
        monitor.refresh_if_needed();
        (monitor.memory_mb(), monitor.cpu_percent())
    };
    let base_metadata = SessionMetadata::from_config(&src.peri_config, src.permission_mode.load());
    let now = peri_time::monotonic_now();
    let active_cwd = ACTIVE_EXECUTION_CWD.state().read().clone();
    let cwd = active_cwd.clone().unwrap_or_else(|| src.cwd.clone());
    let scope = THREAD_BROWSER_SCOPE.get();
    let archived = THREAD_BROWSER_ARCHIVED.get();
    let mut thread_error = THREAD_LIST_ERROR.state().read().clone();
    let mut has_more = THREAD_LIST_HAS_MORE.get();
    let page_count = THREAD_LIST_PAGE_COUNT.get();

    let thread_observation = slow
        .threads
        .refresh(now, src.client.as_ref(), &cwd, scope, archived, page_count)
        .await;
    if thread_observation.refreshed {
        thread_error = thread_observation.error;
        has_more = thread_observation.has_more;
    }
    slow.files.refresh(now, &cwd).await;
    slow.memory.refresh(now).await;

    let session_id = ACTIVE_SESSION_ID.state().read().clone();
    let identity = src
        .client
        .as_ref()
        .and_then(AcpTuiClient::stable_session_identity);
    slow.reset_services(&identity);
    let services = slow.services.refresh(src, &session_id, &identity).await;

    let metadata = slow
        .metadata
        .refresh(src.client.as_ref(), &session_id, base_metadata)
        .await;
    let cron_jobs = slow.services.cron_jobs.clone();
    let services_patch = ServiceSnapshotPatch {
        mcp: slow.services.mcp.clone(),
        cron_total: cron_jobs.len(),
        cron_enabled: cron_jobs.iter().filter(|job| job.enabled).count(),
    };

    let expected = ProjectionIdentity {
        active_cwd,
        session_id,
        session_generation: identity,
        scope,
        archived,
        page_count,
    };
    let patches = [
        SnapshotPatch::Cwd(cwd),
        SnapshotPatch::Process(ProcessSnapshotPatch {
            memory_mb,
            cpu_percent,
        }),
        SnapshotPatch::Session(SessionSnapshotPatch {
            provider_name: metadata.provider_name,
            model_alias: metadata.model_alias,
            model_name: metadata.model_name,
            effort: metadata.effort,
            permission_mode: metadata.permission_mode,
        }),
        SnapshotPatch::Services(services_patch),
    ];
    let committed = commit_projection(
        src.client.as_ref(),
        &expected,
        &patches,
        ProjectionAtoms {
            title: metadata.title,
            projection_error: services.error,
            thread_error,
            has_more,
            threads: thread_observation.entries,
            cron_jobs,
            files: slow.files.entries.clone(),
            hooks: services.hooks,
            plugins: services.plugins,
            providers: derive_providers(&src.peri_config),
            mcp_servers: slow.services.mcp_servers.clone(),
            memory_entries: slow.memory.entries.clone(),
        },
    );
    if !committed {
        return Ok(());
    }
    Ok(())
}

#[cfg(test)]
#[path = "service_snapshot_test.rs"]
mod tests;
