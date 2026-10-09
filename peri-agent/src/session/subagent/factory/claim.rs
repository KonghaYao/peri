//! Ownership of resume status from its initial claim through terminal reporting.
//!
//! A worker owns the claim's active write and its compensating write. Dropping the
//! caller requests preparation rollback or running cancellation; it never cancels an
//! in-flight resource write. Explicit rollback / finish await the same worker and
//! report its failure.
//!
//! 认领的状态写入与「恢复到认领前」由资源侧持有（`ChildResumeClaim`），调用方只提交
//! 领域结果：开始运行 / 移交后台 / 准备失败 / 终止。

use std::sync::Arc;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::tools::{EffectiveToolError, EffectiveToolErrorCode};
use peri_acp_types::session_resources::{
    SessionMetaPatch, SessionResourceErrorKind, SessionResources,
};
use peri_acp_types::thread::{AgentStatus, ThreadId, ThreadMeta};

/// 调用方提交给认领 worker 的领域结果。
enum ClaimDecision {
    /// 成功移交后台执行：终态状态此后由后台执行持有（仍属本次认领）。
    HandOff {
        ready: oneshot::Sender<Result<(), String>>,
        accepted: oneshot::Receiver<()>,
    },
    /// 同步收尾：先结清认领，再写领域终态。
    Finish(AgentStatus),
    CancelRunning(Arc<crate::session::Session>),
}

pub(in crate::session::subagent) struct ResumeClaim {
    /// 决定通道；调用方消失（Drop）时按当前阶段发送取消或直接关闭。
    decision: Option<oneshot::Sender<ClaimDecision>>,
    /// worker 的写入不能被调用方取消：只 detach（不 abort）。
    worker: JoinHandle<Result<(), String>>,
    running: Option<Arc<crate::session::Session>>,
}

pub(in crate::session::subagent) struct BackgroundHandoff(oneshot::Sender<()>);

impl BackgroundHandoff {
    pub(in crate::session::subagent) fn accept(self) -> Result<(), String> {
        self.0
            .send(())
            .map_err(|_| "resume hand-off worker stopped before acceptance".to_string())
    }
}

impl ResumeClaim {
    /// 校验（只读）后由资源侧串行认领 child；认领在写侧门禁内完成「读状态 + 写 active」。
    pub(super) async fn acquire(
        store: Arc<dyn SessionResources>,
        thread_id: String,
        root_id: String,
        ownership: Option<Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>>,
    ) -> Result<(ThreadMeta, Self), Box<dyn std::error::Error + Send + Sync>> {
        let (meta_tx, meta_rx) = oneshot::channel();
        let (decision_tx, decision_rx) = oneshot::channel();
        let worker = tokio::spawn(async move {
            let result =
                own_claim(store, thread_id, root_id, meta_tx, decision_rx, ownership).await;
            if let Err(error) = &result {
                tracing::error!(%error, "resume claim worker failed");
            }
            result
        });
        // 先于第一个 await 建立：调用方在 active 写期间消失也留下明确决定。
        let claim = Self {
            decision: Some(decision_tx),
            worker,
            running: None,
        };
        let meta = meta_rx.await.map_err(|error| {
            format!("resume_subagent: status claim worker ended before reporting: {error}")
        })??;
        Ok((meta, claim))
    }

    /// 成功移交给后台执行：终态状态此后由后台执行持有。
    pub(in crate::session::subagent) async fn release(
        &mut self,
    ) -> Result<BackgroundHandoff, String> {
        let (ready_tx, ready_rx) = oneshot::channel();
        let (accepted_tx, accepted_rx) = oneshot::channel();
        self.decide(ClaimDecision::HandOff {
            ready: ready_tx,
            accepted: accepted_rx,
        });
        ready_rx
            .await
            .map_err(|error| format!("resume hand-off worker failed: {error}"))??;
        Ok(BackgroundHandoff(accepted_tx))
    }

    pub(in crate::session::subagent) fn mark_running(
        &mut self,
        session: Arc<crate::session::Session>,
    ) {
        self.running = Some(session);
    }

    /// 同步 Stop 已运行 hook：把领域终态交给 worker，由它先结清认领再写入。
    pub(in crate::session::subagent) async fn finish(
        mut self,
        status: AgentStatus,
    ) -> Result<(), String> {
        self.decide(ClaimDecision::Finish(status));
        (&mut self.worker)
            .await
            .map_err(|error| format!("resume status completion worker failed: {error}"))?
    }

    /// 准备失败：关闭决定通道，worker 恢复到认领前的记录并报告其失败。
    pub(super) async fn rollback(mut self) -> Result<(), String> {
        self.decision = None;
        (&mut self.worker)
            .await
            .map_err(|error| format!("resume status rollback worker failed: {error}"))?
    }

    /// 提交领域结果；已提交过（或未及提交即被取消）时不重复。
    fn decide(&mut self, decision: ClaimDecision) {
        if let Some(decision_tx) = self.decision.take() {
            let _ = decision_tx.send(decision);
        }
    }
}

impl Drop for ResumeClaim {
    fn drop(&mut self) {
        // 运行中被取消 → 领域终态「取消」；准备阶段被取消 → 直接关闭决定通道，
        // 由 worker 恢复认领前的记录。两者都不取消资源侧正在进行的写入。
        if let Some(session) = self.running.take() {
            if let Some(decision_tx) = self.decision.take() {
                let _ = decision_tx.send(ClaimDecision::CancelRunning(session));
            }
        }
    }
}

/// 认领 worker：active 写入一旦开始，就必须由本任务驱动到完成，与调用方是否仍在无关。
async fn own_claim(
    store: Arc<dyn SessionResources>,
    thread_id: ThreadId,
    root_id: ThreadId,
    mut meta_tx: oneshot::Sender<Result<ThreadMeta, Box<dyn std::error::Error + Send + Sync>>>,
    decision: oneshot::Receiver<ClaimDecision>,
    mut ownership: Option<Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>>,
) -> Result<(), String> {
    // 校验只读且可被调用方取消；取消后不再进入写入。
    let validated = tokio::select! {
        biased;
        _ = meta_tx.closed() => {
            if let Some(owner) = ownership.as_mut() {
                owner.confirm_stopped();
            }
            return Ok(());
        },
        result = validate_thread(store.as_ref(), &thread_id) => result,
    };
    let meta = match validated {
        Ok(meta) => meta,
        Err(error) => {
            tracing::warn!(%thread_id, %error, "resume claim validation failed");
            let _ = meta_tx.send(Err(error));
            if let Some(owner) = ownership.as_mut() {
                owner.confirm_stopped();
            }
            return Ok(());
        }
    };
    // Never select cancellation against this write: the store may commit before
    // returning. Any rollback must be ordered after its completion.
    let handle = match store.claim_child_resume(&thread_id, &root_id).await {
        Ok(handle) => handle,
        Err(error) => {
            tracing::error!(%thread_id, %error, effect = ?error.effect(), "resume thread claim failed");
            if error.effect() == peri_acp_types::session_resources::MutationOutcome::NotApplied {
                if let Some(owner) = ownership.as_mut() {
                    owner.confirm_stopped();
                }
            }
            let _ = meta_tx.send(Err(error.into()));
            return Ok(());
        }
    };
    // 调用方已消失时发送失败，但认领证据已落库：下面的决定分支会给出补偿。
    let _ = meta_tx.send(Ok(meta));
    match decision.await {
        Ok(ClaimDecision::HandOff { ready, accepted }) => {
            let handoff = handle.hand_off_to_background().await;
            let accepted = match handoff {
                Ok(()) => ready.send(Ok(())).is_ok() && accepted.await.is_ok(),
                Err(error) => {
                    let error = format!("resume claim hand-off failed: {error}");
                    tracing::error!(%thread_id, %error, "background hand-off failed");
                    let _ = ready.send(Err(error));
                    false
                }
            };
            if !accepted {
                handle
                    .mark_failed()
                    .await
                    .map_err(|error| format!("resume hand-off rollback failed: {error}"))?;
            }
        }
        Ok(ClaimDecision::Finish(status)) => {
            // 顺序不可反——结清会把记录写回认领前的值，先写终态会被它覆盖。
            handle
                .mark_terminated()
                .await
                .map_err(|error| format!("resume claim settle failed: {error}"))?;
            store
                .update_session_meta(
                    &thread_id,
                    &SessionMetaPatch {
                        status: Some(status),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|error| format!("resume terminal status write failed: {error}"))?;
        }
        Ok(ClaimDecision::CancelRunning(session)) => {
            super::super::close::close_subagent_session_scope(session).await?;
            handle
                .mark_terminated()
                .await
                .map_err(|error| error.to_string())?;
            store
                .update_session_meta(
                    &thread_id,
                    &SessionMetaPatch {
                        status: Some(AgentStatus::Cancelled),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        // 准备阶段调用方消失：恢复到认领前的状态，不留 active 残留。
        Err(_) => {
            handle
                .mark_failed()
                .await
                .map_err(|error| format!("resume status rollback failed: {error}"))?;
        }
    }
    if let Some(owner) = ownership.as_mut() {
        owner.confirm_stopped();
    }
    Ok(())
}

async fn validate_thread(
    store: &dyn SessionResources,
    thread_id: &str,
) -> Result<ThreadMeta, Box<dyn std::error::Error + Send + Sync>> {
    super::validate_thread_id_format(thread_id).map_err(|error| {
        EffectiveToolError::new(EffectiveToolErrorCode::InvalidInput, error.to_string())
    })?;
    let meta = match store.load_session_meta(&thread_id.to_owned()).await {
        Ok(meta) => meta,
        Err(error) if matches!(error.kind(), SessionResourceErrorKind::NotFound) => {
            return Err(EffectiveToolError::new(
                EffectiveToolErrorCode::InvalidInput,
                format!("resume_subagent: thread not found: {thread_id}"),
            )
            .into());
        }
        Err(error) => return Err(error.into()),
    };
    if meta.agent_status.is_active() {
        return Err(EffectiveToolError::new(
            EffectiveToolErrorCode::ApplicationFailed,
            format!(
                "resume_subagent: thread {} is still active \
            (thread 仍处于运行态: 可能仍在执行, 或上次异常退出未收尾; \
            若确认无执行中任务, 可改用 Agent(subagent_type: ...) 新建)",
                thread_id
            ),
        )
        .into());
    }
    Ok(meta)
}

#[cfg(test)]
#[path = "claim_preflight_test.rs"]
mod preflight_tests;
