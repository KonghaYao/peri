//! Ownership of resume status from its initial claim through background hand-off.
//!
//! A worker owns the claim's active write, its compensating write, and the withdrawal
//! of the prompt admitted by this resume. Dropping the caller requests preparation
//! rollback; it never cancels an in-flight child or resource write. Explicit rollback
//! awaits the same worker and reports its failure.
//!
//! 认领的状态写入与「恢复到认领前」由资源侧持有（`ChildResumeClaim`），调用方只提交
//! 领域结果：移交后台 / 准备失败。

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use peri_acp_types::messages::MessageId;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::session::queue::MessageQueue;
use crate::tools::{EffectiveToolError, EffectiveToolErrorCode};
use peri_acp_types::session_resources::{SessionResourceErrorKind, SessionResources};
use peri_acp_types::thread::{ThreadId, ThreadMeta};

/// 撤回重试的初始/上限间隔；只控制频率，不改变「未确认不继续」的次序。
const WITHDRAW_RETRY_INITIAL: Duration = Duration::from_millis(50);
const WITHDRAW_RETRY_MAX: Duration = Duration::from_secs(1);

/// 本次 resume 已接纳的 prompt；撤回责任随认领补偿，成功移交后台时解除。
struct AdmittedPrompt {
    queue: MessageQueue,
    id: MessageId,
    /// 入队写的所有者。写在这里存活，撤回才能等到它结清。
    enqueue: JoinHandle<()>,
}

/// 与认领 worker 共享的 prompt 责任槽；worker 是唯一撤回者。
type PromptSlot = Arc<Mutex<Option<AdmittedPrompt>>>;

/// 调用方提交给认领 worker 的领域结果。
enum ClaimDecision {
    /// 成功移交后台执行：终态状态此后由后台执行持有（仍属本次认领）。
    HandOff {
        ready: oneshot::Sender<Result<(), String>>,
        accepted: oneshot::Receiver<()>,
    },
}

pub(in crate::session::subagent) struct ResumeClaim {
    /// 决定通道；调用方消失（Drop）时直接关闭，由 worker 回滚准备阶段。
    decision: Option<oneshot::Sender<ClaimDecision>>,
    /// worker 的写入不能被调用方取消：只 detach（不 abort）。
    worker: JoinHandle<Result<(), String>>,
    /// 本次 resume 的 prompt 责任；成功移交后台后由 worker 清空。
    prompt: PromptSlot,
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
    pub(in crate::session::subagent) async fn acquire(
        store: Arc<dyn SessionResources>,
        thread_id: String,
        root_id: String,
        ownership: Option<Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>>,
    ) -> Result<(ThreadMeta, Self), Box<dyn std::error::Error + Send + Sync>> {
        let (meta_tx, meta_rx) = oneshot::channel();
        let (decision_tx, decision_rx) = oneshot::channel();
        let prompt: PromptSlot = Arc::new(Mutex::new(None));
        let worker_prompt = Arc::clone(&prompt);
        let worker = tokio::spawn(async move {
            let result = own_claim(
                store,
                thread_id,
                root_id,
                meta_tx,
                decision_rx,
                ownership,
                worker_prompt,
            )
            .await;
            if let Err(error) = &result {
                tracing::error!(%error, "resume claim worker failed");
            }
            result
        });
        // 先于第一个 await 建立：调用方在 active 写期间消失也留下明确决定。
        let claim = Self {
            decision: Some(decision_tx),
            worker,
            prompt,
        };
        let meta = meta_rx.await.map_err(|error| {
            format!("resume_subagent: status claim worker ended before reporting: {error}")
        })??;
        Ok((meta, claim))
    }

    /// 登记本次 prompt 与持有入队写的任务；worker 在后台未接纳时撤回同一行。
    ///
    /// 入队写必须在独立任务里完成（见 `resume_subagent_impl`）：调用方在
    /// `enqueue_sent` 中途被取消时，写仍会结清，worker 才能等到写结束再按 ID 撤回。
    pub(in crate::session::subagent) fn track_prompt(
        &self,
        queue: MessageQueue,
        id: MessageId,
        enqueue: JoinHandle<()>,
    ) {
        *self.prompt.lock() = Some(AdmittedPrompt { queue, id, enqueue });
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
        // 准备阶段被取消 → 直接关闭决定通道，由 worker 恢复认领前的记录。
        // 不取消资源侧正在进行的写入。
        self.decision = None;
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
    prompt: PromptSlot,
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
                // 调用方消失后仍由本 worker 收尾：后台未放行，Working 写可能已提交，
                // 只回滚 agent_status 会留下「终态 + working」，下一次进入会误判待继续。
                // 状态与工作意图的恢复统一由认领 handle 在同一门禁内提交：记录已被
                // 新任务改写时 handle 放弃补偿，调用方不再独立写工作意图。
                // 本次 prompt 也由这里撤回：调用方可能在任一 await 中被取消。
                settle_rejected(&prompt, &*handle, "resume hand-off rollback failed").await?;
            } else {
                // 后台已接纳执行：prompt 归执行侧消费，认领不再撤回。
                prompt.lock().take();
            }
        }
        // 准备阶段调用方消失：恢复到认领前的状态，不留 active 残留。
        Err(_) => {
            settle_rejected(&prompt, &*handle, "resume status rollback failed").await?;
        }
    }
    if let Some(owner) = ownership.as_mut() {
        owner.confirm_stopped();
    }
    Ok(())
}

/// 后台未接纳的收尾：先撤回本次 prompt，再回滚认领。
///
/// 两步由同一个 owned worker 顺序执行：调用方取消只丢弃自己的等待，不会把撤回
/// 与回滚拆开；撤回未确认前不回滚状态（避免重试重复投递）。
async fn settle_rejected(
    prompt: &PromptSlot,
    handle: &dyn peri_acp_types::session_resources::ChildResumeClaim,
    failure: &str,
) -> Result<(), String> {
    // 取出即唯一：即使后续路径再次进入，也不会撤回第二次。
    let admitted = prompt.lock().take();
    if let Some(admitted) = admitted {
        // 入队写的 `DetachedEnqueue` 随调用方 future 一起 drop，但存储写入可能仍在提交；
        // 在写结清前撤回会漏掉随后才落库的行。取消只可能来自调用方，本任务继续等待。
        if let Err(error) = admitted.enqueue.await {
            tracing::warn!(%error, "resume prompt enqueue task ended abnormally");
        }
        withdraw_admitted_prompt(&admitted.queue, admitted.id).await;
    }
    handle
        .mark_failed()
        .await
        .map_err(|error| format!("{failure}: {error}"))
}

/// 撤回已接纳的 prompt，直到结果可判定。
///
/// `mq_cancel` 在各后端都以单次事务删除：内存后端同步删除；SQLite 与远端在提交
/// 失败时可能只报告「未知」。因此一次 `Err` 既不能当作「行还在」，也不能当作
/// 「行已删」；重查 MQ 后行已消失才算撤回完成，仍在则按退避重试。已被 Receive
/// 消费的行返回 `AlreadyHandedOff`/`NotFound`，撤回幂等结束，不改写历史。
async fn withdraw_admitted_prompt(queue: &MessageQueue, prompt_id: MessageId) {
    // 入队先于本槽登记：先等在途提交结束，避免撤回与提交交叉后漏掉同一 ID。
    queue.await_pending_enqueues().await;
    queue.withdraw_send_inputs(&[prompt_id]);
    let mut delay = WITHDRAW_RETRY_INITIAL;
    loop {
        match queue.cancel_user_input(&prompt_id).await {
            Ok(_) => return,
            Err(error) => {
                match queue
                    .query(peri_acp_types::session::MqFilter::default())
                    .await
                {
                    Ok(rows) if !rows.iter().any(|row| row.id == prompt_id) => return,
                    Ok(_) => {}
                    Err(query_error) => {
                        tracing::warn!(
                            %query_error,
                            "resume compensation could not verify prompt withdrawal"
                        );
                    }
                }
                tracing::warn!(%error, "resume prompt withdrawal failed; retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(WITHDRAW_RETRY_MAX);
            }
        }
    }
}

async fn validate_thread(
    store: &dyn SessionResources,
    thread_id: &str,
) -> Result<ThreadMeta, Box<dyn std::error::Error + Send + Sync>> {
    crate::session::subagent::factory::validate_thread_id_format(thread_id).map_err(|error| {
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
