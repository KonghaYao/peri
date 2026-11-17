//! `_peri/history` 分流与执行任务所有权（FIX-04）。
//!
//! 接收循环内只做同步准入；后端读取交给受连接任务所有权与在途配额约束的后台任务。
//! `close` 走快速路径，配额饱和时立即拒绝而不是排队。

use std::sync::Arc;

use serde_json::Value;

use super::ServerLoop;
use crate::host::diagnostics::ResponseDiagnostics;
use crate::host::history_service;
use crate::host::task_scope;
use crate::transport::types::RequestId;

impl ServerLoop<'_> {
    /// `_peri/history` 分流：连接级协商与 own 适配器注入都在进入服务前完成。
    ///
    /// FIX-04：接收循环内只做同步准入（含 requestKey 账本登记），保持同连接请求的
    /// 接收顺序；后端读取交给受连接任务所有权与在途配额约束的后台任务。`close`
    /// 走快速路径；配额饱和时不等待，立即把 attempt 标记为可重试并返回领域错误。
    pub(super) async fn handle_history_request(&self, id: RequestId, params: Value) {
        let diagnostics =
            ResponseDiagnostics::new(id, peri_acp_types::history_v2::HISTORY_METHOD, &params);
        let (facts, service, override_backend) = {
            let connection = self.connection.lock().await;
            #[cfg(test)]
            let override_backend = connection.history_v2_backend_for_test();
            #[cfg(not(test))]
            let override_backend = None;
            (
                connection.facts(),
                connection.history_v2.clone(),
                override_backend,
            )
        };
        if !facts.history_v2 {
            let _ = diagnostics
                .send(
                    self.transport.as_ref(),
                    Err(history_service::protocol_not_negotiated_error()),
                )
                .await;
            return;
        }
        let backend: Arc<dyn peri_acp_types::history_v2::HistoryV2Backend> = override_backend
            .unwrap_or_else(|| {
                Arc::new(
                    crate::host::history_v2_adapter::HistoryV2BackendAdapter::new(
                        Arc::clone(&self.cfg.session_resources),
                        facts.caps.clone(),
                    ),
                )
            });
        let limits = facts
            .caps
            .history_v2
            .as_ref()
            .map(|capability| capability.limits.clone())
            .unwrap_or_default();
        let admission = service.admit_request(
            &params,
            history_service::HistoryServiceRequest::new(true, Some(backend), limits),
        );
        match admission {
            Err(error) => {
                let _ = diagnostics.send(self.transport.as_ref(), Err(error)).await;
            }
            Ok(history_service::HistoryAdmission::Replay(value)) => {
                let _ = diagnostics.send(self.transport.as_ref(), Ok(value)).await;
            }
            Ok(history_service::HistoryAdmission::Execute(execution)) => {
                let execution = *execution;
                if execution.is_close() {
                    // close 不读后端：快速路径保证饱和/慢读期间仍可处理。
                    let result = service.execute(execution).await;
                    if !self.connection_cancellation.is_cancelled() {
                        let _ = diagnostics.send(self.transport.as_ref(), result).await;
                    }
                    return;
                }
                // 不等待配额：接收循环必须继续处理 cancel/close。
                let Some(permit) = service.try_acquire_in_flight() else {
                    let error = service.reject_busy(&execution);
                    let _ = diagnostics.send(self.transport.as_ref(), Err(error)).await;
                    return;
                };
                self.spawn_history_execution(service, execution, permit, diagnostics)
                    .await;
            }
        }
    }

    /// 把已准入的读请求交给连接所有权任务；断连取消时任务被回收，迟到结果不发送。
    async fn spawn_history_execution(
        &self,
        service: Arc<history_service::HistoryV2Service>,
        execution: history_service::HistoryExecution,
        permit: tokio::sync::OwnedSemaphorePermit,
        diagnostics: ResponseDiagnostics,
    ) {
        let transport = Arc::clone(self.transport);
        let rejected_diagnostics = diagnostics.clone();
        let rejected_transport = Arc::clone(&transport);
        let rejected_service = Arc::clone(&service);
        let cancellation = self.connection_cancellation.clone();
        // spawn 失败时 future 会被原样丢弃；用共享槽位取回执行对象以释放账本记录。
        let pending = Arc::new(std::sync::Mutex::new(Some(execution)));
        let task_pending = Arc::clone(&pending);
        let spawned = self.cfg.host_task_spawner.spawn(
            task_scope::HostTaskOwnerKind::Connection,
            task_scope::HostTaskKind::History,
            async move {
                let _permit = permit;
                if cancellation.is_cancelled() {
                    if let Some(execution) = task_pending.lock().unwrap().take() {
                        let _ = service.reject_cancelled(&execution);
                    }
                    return;
                }
                let Some(execution) = task_pending.lock().unwrap().take() else {
                    return;
                };
                let result = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => Err(
                        crate::transport::types::AcpError::new(-32800, "request cancelled"),
                    ),
                    result = service.execute(execution) => result,
                };
                // 连接已取消：执行结果作废，不能向已断开的连接发送成功响应。
                if !cancellation.is_cancelled() {
                    let _ = diagnostics.send(transport.as_ref(), result).await;
                }
            },
        );
        if spawned.is_err() {
            let rejected = pending.lock().unwrap().take();
            if let Some(execution) = rejected {
                let error = rejected_service.reject_cancelled(&execution);
                let _ = rejected_diagnostics
                    .send(rejected_transport.as_ref(), Err(error))
                    .await;
            }
        }
    }
}
