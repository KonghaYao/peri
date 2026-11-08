//! 响应发出后的执行放行：`Prepared → Launched`、续跑请求与失败回滚。

use std::sync::Arc;

use peri_acp_types::recovery::RecoveryReport;
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::thread::ThreadId;

use super::{AdmissionPhase, AdmittedRecovery, HostSessionRecovery};
use crate::host::activation;
use crate::session::executor::ContinuationRequest;

impl HostSessionRecovery {
    /// 会话响应发出后放行执行：逐 thread `allow + ensure_listener + continuation`。
    ///
    /// 根与冷 child 各自启动自己的 turn；child 的续跑请求携带父 thread，turn 结束后
    /// 由 continuation scheduler 把结果通知投递回父级持久队列。
    pub(crate) async fn launch(&self, report: &RecoveryReport) {
        for thread_id in &report.recovered {
            // Prepared → Launched 原子推进：二次 launch 看到 Launched 就不再发 continuation。
            let prepared = {
                let Some(mut entry) = self.admitted.get_mut(thread_id) else {
                    continue;
                };
                if entry.phase != AdmissionPhase::Prepared {
                    continue;
                }
                entry.phase = AdmissionPhase::Launched;
                AdmittedRecovery {
                    generation: entry.generation,
                    session_id: entry.session_id.clone(),
                    phase: AdmissionPhase::Launched,
                }
            };
            // 只有 continuation 真正进入调度器才保留 Launched；任何跳过都撤销本次
            // 准入，否则下一次显式进入会把它误判成永久 AlreadyRunning。
            if !self.launch_prepared(&prepared).await {
                self.rollback_launch(thread_id, &prepared);
            }
        }
    }

    /// 启动没有真正交出执行：按同代次快照撤销 Launched 占位。
    fn rollback_launch(&self, thread_id: &ThreadId, admission: &AdmittedRecovery) {
        let removed = self
            .admitted
            .remove_if(thread_id, |_, current| {
                current.generation == admission.generation
                    && current.session_id == admission.session_id
                    && current.phase == AdmissionPhase::Launched
            })
            .is_some();
        if removed {
            // 本次启动门不再交出 recovery 续跑：解除普通激活抑制，下一次进入重试。
            if let Some(runtime) = self.manager.get_session(&admission.session_id) {
                runtime.activation.finish_recovery_launch();
            }
        }
    }

    /// 返回是否已把执行交给 continuation 调度器。
    async fn launch_prepared(&self, prepared: &AdmittedRecovery) -> bool {
        let Some(state) = self.lifecycle(&prepared.session_id).await else {
            tracing::warn!(
                session_id = %prepared.session_id,
                "recovery launch skipped: session state disappeared"
            );
            return false;
        };
        if state.generation != prepared.generation {
            tracing::warn!(
                session_id = %prepared.session_id,
                "recovery launch skipped: session generation changed"
            );
            return false;
        }
        if state.closing || state.turn_active {
            tracing::warn!(
                session_id = %prepared.session_id,
                closing = state.closing,
                turn_active = state.turn_active,
                "recovery launch skipped: lifecycle moved on"
            );
            return false;
        }
        let Some(runtime) = self.manager.get_session(&prepared.session_id) else {
            return false;
        };
        let activation = Arc::clone(&runtime.activation);
        drop(runtime);
        // Stop 之后不再启动：提醒留在 MQ，由后续显式继续消费。
        if activation.user_stopped() || !activation.is_allowed() {
            tracing::debug!(
                session_id = %prepared.session_id,
                "recovery launch skipped: activation is not allowed"
            );
            return false;
        }
        if let Err(error) = activation::ensure_listener(
            &prepared.session_id,
            &self.sessions,
            self.cfg.as_ref(),
            self.cont_tx.as_ref(),
        ) {
            tracing::warn!(
                session_id = %prepared.session_id,
                code = error.code,
                error = %error.message,
                "recovery launch skipped: listener could not start"
            );
            return false;
        }
        if self
            .cont_tx
            .send(ContinuationRequest {
                session_id: prepared.session_id.clone(),
                kind: BgTaskKind::Agent,
                mq_steering: true,
                pending_result: false,
                recovery: true,
            })
            .is_err()
        {
            tracing::warn!(
                session_id = %prepared.session_id,
                "recovery launch skipped: continuation scheduler is gone"
            );
            return false;
        }
        // 早失败（Receive 之前）由 dispatch 写进程内标记；下一次显式进入读取它并重试。
        true
    }

    /// 仅测试：该 thread 是否仍有 Launched 占位（跳过路径不得永久占位）。
    #[cfg(test)]
    pub(crate) fn has_launched_admission(&self, thread_id: &ThreadId) -> bool {
        self.admitted
            .get(thread_id)
            .is_some_and(|entry| entry.phase == AdmissionPhase::Launched)
    }
}
