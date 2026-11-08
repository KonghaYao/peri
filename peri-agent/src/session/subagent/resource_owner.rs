use std::sync::Arc;

use peri_acp_types::session::ExecutionBinding;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::system_reminder::{ReminderCategory, ReminderDelivery, ReminderSeverity};
use peri_acp_types::tasks::TaskManager as _;

use crate::agent::async_tasks::TaskManager;
use crate::session::async_result_mailbox::AsyncResultMailbox;
use crate::session::transcript::PersistOp;
use crate::session::{MessageQueue, MessageTranscript};

use super::ChildHandoffReceipt;

#[cfg(test)]
#[path = "resource_owner_test.rs"]
mod tests;

/// 资源宿主只保留任务、消息与可靠写入入口，不保留计算 session、模型或旧事件总线。
pub struct ParkedChildOwner {
    pub child_thread_id: String,
    pub parent_thread_id: String,
    pub manager: Arc<TaskManager>,
    pub queue: MessageQueue,
    pub(super) mailbox: Arc<AsyncResultMailbox>,
    pub resources: Option<Arc<dyn SessionResources>>,
    pub(super) pool: Option<std::sync::Weak<dyn peri_acp_types::ports::McpPoolPort>>,
    parent: parking_lot::Mutex<Option<std::sync::Weak<TaskManager>>>,
    writer: parking_lot::Mutex<Option<Arc<tokio::sync::mpsc::UnboundedSender<PersistOp>>>>,
    state: parking_lot::Mutex<OwnerState>,
    /// 仅测试：暂停 close 的结算前快照，复现「关闸后被迟到入队」窗口。
    #[cfg(test)]
    settle_gate: parking_lot::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}

#[derive(Default)]
struct OwnerState {
    execution: Option<ExecutionBinding>,
    closing: bool,
    preparing: bool,
    failure: Option<String>,
    receipt: Option<ChildHandoffReceipt>,
    pending_receipt: Option<ChildHandoffReceipt>,
}

impl OwnerState {
    fn check_admission(&self) -> Result<(), String> {
        if self.closing {
            return Err("child resource owner is closing".into());
        }
        if self.preparing {
            return Err("child resource owner is preparing another run".into());
        }
        if self.execution.is_some() {
            return Err("child execution or terminal event drain is still active".into());
        }
        if self.pending_receipt.is_some() {
            return Err(
                "child handoff is unconfirmed; resume and receipt clearing are refused".into(),
            );
        }
        if self.failure.is_some() {
            return Err("child resource settlement is unconfirmed".into());
        }
        Ok(())
    }
}

pub struct ChildPreparationPermit(Arc<ParkedChildOwner>);

impl Drop for ChildPreparationPermit {
    fn drop(&mut self) {
        self.0.state.lock().preparing = false;
    }
}

pub struct ChildExecutionPermit {
    owner: Arc<ParkedChildOwner>,
    execution: ExecutionBinding,
    finished: bool,
}

impl ParkedChildOwner {
    pub(super) fn new(
        child_thread_id: String,
        parent_thread_id: String,
        manager: Arc<TaskManager>,
        queue: MessageQueue,
        resources: Option<Arc<dyn SessionResources>>,
        pool: Option<std::sync::Weak<dyn peri_acp_types::ports::McpPoolPort>>,
    ) -> Arc<Self> {
        let mailbox = Arc::new(AsyncResultMailbox::new(
            child_thread_id.clone(),
            child_thread_id.clone(),
        ));
        Arc::new(Self {
            child_thread_id,
            parent_thread_id,
            manager,
            queue,
            mailbox,
            resources,
            pool,
            parent: parking_lot::Mutex::new(None),
            writer: parking_lot::Mutex::new(None),
            state: parking_lot::Mutex::new(OwnerState::default()),
            #[cfg(test)]
            settle_gate: parking_lot::Mutex::new(None),
        })
    }

    /// 计算 session 关闭前登记 flush 句柄；close 不再启动 RCRA，只做可靠落盘。
    pub fn set_writer(&self, writer: Option<Arc<tokio::sync::mpsc::UnboundedSender<PersistOp>>>) {
        *self.writer.lock() = writer;
    }

    pub(crate) fn attach_parent(&self, parent: &Arc<TaskManager>) {
        *self.parent.lock() = Some(Arc::downgrade(parent));
    }

    /// 结算完成后从 owning 父任务目录移除；失败时保留可查询记录。
    fn release_from_parent(&self) {
        if let Some(parent) = self
            .parent
            .lock()
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        {
            parent.release_child_owner(&self.child_thread_id);
        }
    }

    pub fn acquire(
        self: &Arc<Self>,
        execution: ExecutionBinding,
    ) -> Result<ChildExecutionPermit, String> {
        let mut state = self.state.lock();
        state.check_admission()?;
        // 新执行拥有新的交接身份；旧执行的 receipt 不跨 run 复用。
        state.receipt = None;
        state.pending_receipt = None;
        state.execution = Some(execution.clone());
        Ok(ChildExecutionPermit {
            owner: self.clone(),
            execution,
            finished: false,
        })
    }

    pub fn prepare_resume(self: &Arc<Self>) -> Result<ChildPreparationPermit, String> {
        let mut state = self.state.lock();
        state.check_admission()?;
        state.preparing = true;
        Ok(ChildPreparationPermit(self.clone()))
    }

    pub fn check_resume(&self) -> Result<(), String> {
        self.state.lock().check_admission()
    }

    pub fn receipt(&self) -> Option<ChildHandoffReceipt> {
        self.state.lock().receipt.clone()
    }

    /// 已冻结但尚未确认 canonical 提交的交接身份；用于失败报告。
    pub fn pending_receipt(&self) -> Option<ChildHandoffReceipt> {
        self.state.lock().pending_receipt.clone()
    }

    pub fn failure(&self) -> Option<String> {
        self.state.lock().failure.clone()
    }

    /// 用于状态提交后判断 owner 是否仍有未确认的委托义务。
    pub async fn has_unsettled_obligations(&self) -> bool {
        let unconfirmed = {
            let state = self.state.lock();
            state.failure.is_some() || state.pending_receipt.is_some()
        };
        unconfirmed
            || self.manager.has_unsettled_tasks()
            || self.queue.has_required().await.unwrap_or(true)
    }

    /// 仅测试：同步适配同一 async 判定。
    #[cfg(test)]
    pub fn has_unsettled_obligations_sync(&self) -> bool {
        futures::executor::block_on(self.has_unsettled_obligations())
    }

    /// 仅测试：下一次 close 在关闸后、结算读取前暂停；返回（进入通知，放行信号）。
    #[cfg(test)]
    pub fn gate_next_close_settlement(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self.settle_gate.lock() = Some((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    #[cfg(test)]
    async fn wait_close_settlement_gate(&self) {
        let gate = self.settle_gate.lock().take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            let _ = release.await;
        }
    }

    pub fn freeze_receipt(
        &self,
        receipt: ChildHandoffReceipt,
    ) -> Result<ChildHandoffReceipt, String> {
        let mut state = self.state.lock();
        if state.execution.as_ref() != Some(&receipt.execution) {
            return Err("child handoff execution does not own admission".into());
        }
        if let Some(existing) = &state.receipt {
            if existing.execution == receipt.execution {
                return Ok(existing.clone());
            }
        }
        if let Some(existing) = &state.pending_receipt {
            if existing.execution == receipt.execution {
                return Ok(existing.clone());
            }
        }
        state.pending_receipt = Some(receipt.clone());
        Ok(receipt)
    }

    /// canonical 提交成功后才记录；失败时 pending 保留同一身份供重试。
    pub fn retain_receipt(&self, receipt: ChildHandoffReceipt) -> Result<(), String> {
        let mut state = self.state.lock();
        if state.execution.as_ref() != Some(&receipt.execution) {
            return Err("child handoff execution does not own admission".into());
        }
        state.pending_receipt = None;
        state.receipt = Some(receipt);
        Ok(())
    }

    /// 登记关闭意图；执行或准备仍活跃时保留资源并报告 Incomplete。
    pub fn begin_close(&self) -> Result<(), String> {
        let mut state = self.state.lock();
        state.closing = true;
        if state.preparing || state.execution.is_some() {
            return Err(
                "Incomplete: child preparation, execution or terminal event drain is active".into(),
            );
        }
        Ok(())
    }

    /// 当前执行自己请求关闭：允许在 permit 仍持有时登记关闭意图。
    /// 外部关闭继续使用严格的 [`Self::begin_close`]。
    pub fn begin_close_from_execution(&self) -> Result<(), String> {
        let mut state = self.state.lock();
        state.closing = true;
        if state.preparing {
            return Err("Incomplete: child preparation is active".into());
        }
        Ok(())
    }

    /// 非模型关闭：结算保留消息、取消真实资源，确认后才释放 binding。
    pub async fn close(&self) -> Result<(), String> {
        self.begin_close()?;
        let unconfirmed = {
            let state = self.state.lock();
            state
                .pending_receipt
                .as_ref()
                .map(|_| "child handoff is unconfirmed".to_string())
                .or_else(|| state.failure.clone())
        };
        for child in self.manager.child_owners() {
            if child.child_thread_id == self.child_thread_id {
                continue;
            }
            Box::pin(child.close()).await?;
        }
        self.manager.begin_session_close();
        if let Some(pool) = &self.pool {
            let pool = pool
                .upgrade()
                .ok_or("Incomplete: child MCP resource owner unavailable")?;
            pool.close_agent_session_scope(&self.child_thread_id)
                .await?;
        }
        let external = self.manager.external_task_ids();
        for task in peri_acp_types::tasks::TaskManager::snapshot(self.manager.as_ref()).tasks {
            if task.kind == peri_acp_types::tasks::BgTaskKind::Shell
                && task.status == "running"
                && !external.contains(&task.task_id)
            {
                self.cancel_task(&task.task_id).await?;
            }
        }
        if !self.manager.wait_session_close().await {
            return Err("Incomplete: child resources remain unsettled".into());
        }
        if let Some(reason) = unconfirmed {
            return Err(format!(
                "Incomplete: child settlement is unconfirmed ({reason}); resources closed but owner and frozen state retained"
            ));
        }
        // 关闭结算开始：先拒绝新入队并等已通过闸门的写入提交，随后读取的 MQ
        // 快照才覆盖关闭前的全部外部输入。结算自身的存储写入直接经资源门面，
        // 不经过该闸门；闸门只存在于本门面，resume 新建 bind 会重新开放。
        self.queue.close_admission();
        self.queue.await_pending_enqueues().await;
        #[cfg(test)]
        self.wait_close_settlement_gate().await;
        self.settle_retained_messages().await?;
        if self.queue.has_required().await.unwrap_or(true) {
            return Err("Incomplete: retained child messages remain unprocessed".into());
        }
        if let Some(pool) = &self.pool {
            let pool = pool
                .upgrade()
                .ok_or("Incomplete: child MCP resource owner unavailable")?;
            pool.release_agent_session(&self.child_thread_id)?;
        }
        self.release_from_parent();
        Ok(())
    }

    async fn settle_retained_messages(&self) -> Result<(), String> {
        let Some(resources) = &self.resources else {
            if self.queue.has_required().await.unwrap_or(true) {
                return Err(
                    "Incomplete: canonical child history unavailable for retained messages".into(),
                );
            }
            return Ok(());
        };
        let writer = self.writer.lock().clone();
        if let Some(writer) = writer {
            match MessageTranscript::flush_via_tx_typed(writer.as_ref()).await {
                Ok(()) => {}
                Err(crate::session::transcript::FlushError::WriterClosed) => {
                    // session 已释放，writer 正常结束；终态路径已执行过 flush，
                    // close 自己的 canonical append 仍是可靠提交事实。
                    tracing::info!(
                        child_thread_id = %self.child_thread_id,
                        "child history writer already closed before close settlement"
                    );
                }
                Err(crate::session::transcript::FlushError::Persistence(error)) => {
                    let message =
                        format!("Incomplete: child history persistence unconfirmed: {error}");
                    self.state.lock().failure = Some(message.clone());
                    tracing::error!(child_thread_id = %self.child_thread_id, %error, "child close history flush failed");
                    return Err(message);
                }
            }
        }
        // 非模型结算：提醒与 MQ 行由存储一次提交。分开写会留下「提醒已写、待发行还在」
        // 的半完成状态；汇总使用稳定 delivery ID，失败后下一次 close 可原样重试。
        let entries = self
            .queue
            .query(peri_acp_types::session::MqFilter::default())
            .await
            .map_err(|error| format!("Incomplete: child MQ query failed: {error}"))?;
        if entries.is_empty() {
            return Ok(());
        }
        let (reminders, rows) = self.retained_settlement(&entries)?;
        resources
            .settle_retained_mq(
                &peri_acp_types::thread::ThreadId::from(self.child_thread_id.as_str()),
                &reminders,
                &rows,
            )
            .await
            .map_err(|error| format!("Incomplete: child MQ settlement failed: {error}"))?;
        Ok(())
    }

    /// 结算计划：哪些提醒要写、哪些 MQ 行要被删除。
    ///
    /// 纯计算，不触存储：同一份计划交给存储的原子提交，失败时历史与 MQ 都不变。
    fn retained_settlement(
        &self,
        entries: &[peri_acp_types::session::MqEntry],
    ) -> Result<
        (
            Vec<peri_acp_types::session::RetainedReminder>,
            Vec<peri_acp_types::session::MqEntry>,
        ),
        String,
    > {
        use peri_acp_types::session::{QueuedPayload, RetainedReminder};
        let queued: Vec<peri_acp_types::session::QueuedMessage> = entries
            .iter()
            .cloned()
            .map(peri_acp_types::session::QueuedMessage::from_mq_entry)
            .collect();
        let mut reminders = Vec::new();
        let mut unprocessed = Vec::new();
        for message in &queued {
            match (&message.delivery_id, &message.payload) {
                (Some(delivery_id), QueuedPayload::SystemReminder(reminder)) => {
                    reminders.push(RetainedReminder {
                        id: *delivery_id,
                        reminder: reminder.clone(),
                    });
                }
                _ => unprocessed.push(message.clone()),
            }
        }
        if !unprocessed.is_empty() {
            let delivery_id = close_settlement_delivery_id(&self.child_thread_id, &unprocessed);
            reminders.push(RetainedReminder {
                id: delivery_id,
                reminder: retained_input_record(&self.child_thread_id, delivery_id, &unprocessed)?,
            });
        }
        Ok((reminders, entries.to_vec()))
    }

    pub async fn cancel_task(&self, task_id: &str) -> Result<(), String> {
        self.manager
            .cancel_async(task_id)
            .await
            .map_err(|error| error.to_string())
    }
}

fn retained_input_record(
    child_thread_id: &str,
    delivery_id: peri_acp_types::messages::MessageId,
    unprocessed: &[peri_acp_types::session::QueuedMessage],
) -> Result<peri_acp_types::system_reminder::TrustedSystemReminder, String> {
    let facts = unprocessed
        .iter()
        .map(|message| {
            let identity = message
                .delivery_id
                .map(|id| id.as_uuid().to_string())
                .or_else(|| {
                    message
                        .message()
                        .map(|body| body.id().as_uuid().to_string())
                })
                .unwrap_or_else(|| "reminder-without-id".into());
            let preview: String = message
                .message()
                .map(|body| body.content().chars().take(200).collect())
                .unwrap_or_default();
            format!(
                "{identity} (kind: {:?}, source: {:?}, model_visible: {}, preview: {preview})",
                message.kind, message.source, message.policy.model_visible
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    crate::session::producer_reminders::try_trusted_reminder(
        ReminderCategory::Task,
        "subagent_task_lifecycle",
        "child_retained_input_unprocessed",
        ReminderSeverity::Error,
        ReminderDelivery::Required,
        format!(
            "子会话关闭前保留了 {} 条未处理的输入/提醒。它们没有被模型消费，已记录在此；不启动新推理。明细: {facts}",
            unprocessed.len()
        ),
        Some("关闭时仍有未处理输入".into()),
        serde_json::json!({
            "child_thread_id": child_thread_id,
            "delivery_id": delivery_id,
            "unprocessed": unprocessed.iter().map(|message| serde_json::json!({
                "kind": format!("{:?}", message.kind),
                "source": format!("{:?}", message.source),
                "message_id": message.message().map(|body| body.id()),
                "delivery_id": message.delivery_id,
            })).collect::<Vec<_>>(),
        }),
    )
    .map_err(|error| format!("Incomplete: retained input record rejected: {error}"))
}

fn close_settlement_delivery_id(
    child_thread_id: &str,
    messages: &[peri_acp_types::session::QueuedMessage],
) -> peri_acp_types::messages::MessageId {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"child-close-retained-input");
    hasher.update((child_thread_id.len() as u64).to_be_bytes());
    hasher.update(child_thread_id.as_bytes());
    let mut identities: Vec<String> = messages
        .iter()
        .map(|message| {
            message
                .delivery_id
                .map(|id| id.as_uuid().to_string())
                .or_else(|| {
                    message
                        .message()
                        .map(|body| body.id().as_uuid().to_string())
                })
                .unwrap_or_else(|| format!("{:?}:{:?}", message.kind, message.source))
        })
        .collect();
    identities.sort();
    for identity in identities {
        hasher.update((identity.len() as u64).to_be_bytes());
        hasher.update(identity.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    peri_acp_types::messages::MessageId::from(uuid::Uuid::from_bytes(bytes))
}

impl ChildExecutionPermit {
    /// 必须在状态提交和旧事件排空之后调用；失败阻止新执行抢占未结清的资源。
    pub fn finish(mut self, result: Result<(), String>) {
        let mut state = self.owner.state.lock();
        if state.execution.as_ref() == Some(&self.execution) {
            state.execution = None;
            state.failure = result.err();
        }
        self.finished = true;
    }
}

impl Drop for ChildExecutionPermit {
    fn drop(&mut self) {
        if !self.finished {
            let mut state = self.owner.state.lock();
            if state.execution.as_ref() == Some(&self.execution) {
                state.execution = None;
                state.failure = Some("child execution ended without terminal settlement".into());
                tracing::error!(child_thread_id = %self.owner.child_thread_id, "child resource owner retained unconfirmed execution");
            }
        }
    }
}
