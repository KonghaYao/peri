//! Deployment retains its running host and any incomplete resource drain across cancellation.

use peri_acp_types::ports::McpTaskOwnerPort;
use peri_controller::langfuse::LangfuseShutdownReport;
use std::{collections::HashMap, sync::Arc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{
    connection::ConnectionContext,
    task_scope::{HostTaskOwner, HostTerminalShutdownReport},
    AcpServerConfig, PromptLocks, SharedSessions,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpHostShutdownReport {
    Complete,
    Incomplete,
    TelemetryFailed(LangfuseShutdownReport),
    TaskFailed { cancelled: bool },
}

impl AcpHostShutdownReport {
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// Owns the actual host task. Call shutdown after closing client admission/transport.
/// Cancelling a waiter retains the task; Incomplete retains the resource context for retry.
pub struct AcpHostHandle {
    task: Option<JoinHandle<ExitRound>>,
    retry: Option<HostExitContext>,
    terminal: Option<AcpHostShutdownReport>,
}

pub fn spawn_acp_server(
    transport: Arc<dyn crate::transport::AcpTransport>,
    cfg: AcpServerConfig,
) -> AcpHostHandle {
    spawn_with_sessions(
        transport,
        cfg,
        Arc::new(tokio::sync::Mutex::new(HashMap::new())),
    )
}

pub(super) fn spawn_with_sessions(
    transport: Arc<dyn crate::transport::AcpTransport>,
    cfg: AcpServerConfig,
    sessions: SharedSessions,
) -> AcpHostHandle {
    AcpHostHandle {
        task: Some(tokio::spawn(async move {
            super::run_acp_server_inner(transport, cfg, sessions)
                .await
                .finish()
                .await
        })),
        retry: None,
        terminal: None,
    }
}

impl AcpHostHandle {
    /// Wait for real host/resource/telemetry completion; repeated terminal observations are stable.
    pub async fn shutdown(&mut self) -> AcpHostShutdownReport {
        if let Some(report) = &self.terminal {
            return report.clone();
        }
        if self.task.is_none() {
            let context = self
                .retry
                .take()
                .expect("incomplete host must retain its context");
            self.task = Some(tokio::spawn(context.finish()));
        }
        // Await by reference. Cancellation cannot detach the host or drop its resource context.
        let result = self
            .task
            .as_mut()
            .expect("host task must be retained")
            .await;
        self.task.take();
        match result {
            Ok(ExitRound { context, report }) => {
                if report == AcpHostShutdownReport::Incomplete {
                    self.retry = Some(context);
                } else {
                    self.terminal = Some(report.clone());
                }
                report
            }
            Err(error) => {
                let report = AcpHostShutdownReport::TaskFailed {
                    cancelled: error.is_cancelled(),
                };
                self.terminal = Some(report.clone());
                report
            }
        }
    }
}

impl Drop for AcpHostHandle {
    fn drop(&mut self) {
        // Explicit shutdown owns the drain guarantee. Dropping the deployment
        // must still cancel its host loop rather than leave a detached server.
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
    }
}

pub(super) struct HostExitContext {
    pub(super) task_owner: HostTaskOwner,
    pub(super) mcp_task_owner: Box<dyn McpTaskOwnerPort>,
    pub(super) cfg: Arc<AcpServerConfig>,
    pub(super) sessions: SharedSessions,
    pub(super) prompt_locks: PromptLocks,
    pub(super) cont_tx: Option<
        Arc<tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>>,
    >,
    pub(super) connection: Arc<tokio::sync::Mutex<ConnectionContext>>,
    pub(super) connection_cancellation: CancellationToken,
    pub(super) closing_sessions: std::collections::BTreeMap<String, crate::session::AcpSession>,
}

struct ExitRound {
    context: HostExitContext,
    report: AcpHostShutdownReport,
}

impl HostExitContext {
    /// 会话任务排空之后关闭会话存储：这是唯一持有部署关闭权的时点。
    ///
    /// 返回 `Err(())` 表示关闭**未确认**（在途写入未结清、未决持久化仍在，或取消）。
    /// 事实已在实现内部判定并记录，这里只按未完成上报——不重试、不把未确认当成功；
    /// 部署保留上下文，重复关闭会重新做一遍真实检查。
    async fn shutdown_session_store(&self) -> Result<(), ()> {
        let Some(shutdown) = self.cfg.session_store_shutdown.as_ref() else {
            // 未注入关闭权（会话级装配或测试宿主）：这里没有部署存储可关闭。
            return Ok(());
        };
        shutdown.shutdown().await.map_err(|error| {
            tracing::warn!(
                kind = ?error.kind(),
                "session store close was not confirmed; deployment retained for retry"
            );
        })
    }

    async fn finish(mut self) -> ExitRound {
        let resources = super::shutdown::shutdown_host(
            &mut self.task_owner,
            self.mcp_task_owner.as_mut(),
            &self.cfg,
            &self.sessions,
            &self.prompt_locks,
            &mut self.cont_tx,
            &self.connection,
            &self.connection_cancellation,
            &mut self.closing_sessions,
        )
        .await;
        let report = match resources {
            HostTerminalShutdownReport::Incomplete { .. } => AcpHostShutdownReport::Incomplete,
            // 排空完成才轮得到部署关闭：还有任何未完成的任务时保留存储原样。
            HostTerminalShutdownReport::Complete { .. } => {
                if self.shutdown_session_store().await.is_err() {
                    AcpHostShutdownReport::Incomplete
                } else if let Some(owner) = self.cfg.langfuse_shutdown_owner.as_ref() {
                    match owner.shutdown().await {
                        LangfuseShutdownReport::Complete => AcpHostShutdownReport::Complete,
                        report => AcpHostShutdownReport::TelemetryFailed(report),
                    }
                } else {
                    AcpHostShutdownReport::Complete
                }
            }
        };
        if !report.is_complete() {
            tracing::warn!(?report, "ACP deployment shutdown incomplete or failed");
        }
        ExitRound {
            context: self,
            report,
        }
    }
}
