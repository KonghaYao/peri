use std::sync::Arc;

use peri_acp_types::session::ExecutionBinding;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::system_reminder::{ReminderCategory, ReminderDelivery, ReminderSeverity};
use peri_acp_types::tasks::TaskManager as _;

use crate::agent::async_tasks::TaskManager;
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
    pub resources: Option<Arc<dyn SessionResources>>,
    pub(super) pool: Option<std::sync::Weak<dyn peri_acp_types::ports::McpPoolPort>>,
    parent: parking_lot::Mutex<Option<std::sync::Weak<TaskManager>>>,
    writer: parking_lot::Mutex<Option<Arc<tokio::sync::mpsc::UnboundedSender<PersistOp>>>>,
    state: parking_lot::Mutex<OwnerState>,
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
        Arc::new(Self {
            child_thread_id,
            parent_thread_id,
            manager,
            queue,
            resources,
            pool,
            parent: parking_lot::Mutex::new(None),
            writer: parking_lot::Mutex::new(None),
            state: parking_lot::Mutex::new(OwnerState::default()),
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
        if state.closing {
            return Err("child resource owner is closing".into());
        }
        if state.preparing {
            return Err("child resource owner is preparing another run".into());
        }
        if let Some(error) = &state.failure {
            return Err(format!("child resource settlement is unconfirmed: {error}"));
        }
        if state.execution.is_some() {
            return Err("child execution or terminal event drain is still active".into());
        }
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
        if state.closing || state.preparing || state.execution.is_some() || state.failure.is_some()
        {
            return Err("child resource owner is not available for resume".into());
        }
        state.preparing = true;
        Ok(ChildPreparationPermit(self.clone()))
    }

    pub fn check_resume(&self) -> Result<(), String> {
        let state = self.state.lock();
        if state.closing || state.preparing || state.execution.is_some() || state.failure.is_some()
        {
            return Err(format!(
                "child resource owner cannot resume: closing={}, preparing={}, active={}, failure={:?}",
                state.closing,
                state.preparing,
                state.execution.is_some(),
                state.failure
            ));
        }
        Ok(())
    }

    pub fn receipt(&self) -> Option<ChildHandoffReceipt> {
        self.state.lock().receipt.clone()
    }

    pub fn failure(&self) -> Option<String> {
        self.state.lock().failure.clone()
    }

    /// 用于状态提交后判断 owner 是否仍有未确认的委托义务。
    pub fn has_unsettled_obligations(&self) -> bool {
        self.manager.active_count() > 0
            || self.manager.has_pending_deliveries()
            || self.queue.has_required()
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
        self.settle_retained_messages().await?;
        if self.queue.has_required() {
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
            if self.queue.has_required() {
                return Err(
                    "Incomplete: canonical child history unavailable for retained messages".into(),
                );
            }
            return Ok(());
        };
        let writer = self.writer.lock().clone();
        if let Some(writer) = writer {
            if let Err(error) = MessageTranscript::flush_via_tx(writer.as_ref()).await {
                // 计算 session 已释放时 writer 可能已关闭；终态路径已执行过
                // flush，close 自己的 canonical append 仍是可靠提交事实。
                tracing::error!(
                    child_thread_id = %self.child_thread_id,
                    %error,
                    "child history flush unavailable before close settlement"
                );
            }
        }
        loop {
            let drained = self.queue.drain_all();
            if drained.is_empty() {
                break;
            }
            if let Err(error) = self.commit_retained(resources, &drained).await {
                self.queue.push_batch(drained);
                return Err(error);
            }
        }
        Ok(())
    }

    async fn commit_retained(
        &self,
        resources: &Arc<dyn SessionResources>,
        messages: &[peri_acp_types::session::QueuedMessage],
    ) -> Result<(), String> {
        use peri_acp_types::session::QueuedPayload;
        let mut unprocessed = Vec::new();
        for message in messages {
            match (&message.delivery_id, &message.payload) {
                (Some(delivery_id), QueuedPayload::SystemReminder(reminder)) => {
                    resources
                        .append_reminder_if_absent(&self.child_thread_id, *delivery_id, reminder)
                        .await
                        .map_err(|error| {
                            format!("Incomplete: retained terminal settlement failed: {error}")
                        })?;
                }
                _ => unprocessed.push(message.clone()),
            }
        }
        if unprocessed.is_empty() {
            return Ok(());
        }
        let delivery_id = close_settlement_delivery_id(&self.child_thread_id, &unprocessed);
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
        let reminder = crate::session::producer_reminders::try_trusted_reminder(
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
                "child_thread_id": self.child_thread_id,
                "delivery_id": delivery_id,
                "unprocessed": unprocessed.iter().map(|message| serde_json::json!({
                    "kind": format!("{:?}", message.kind),
                    "source": format!("{:?}", message.source),
                    "message_id": message.message().map(|body| body.id()),
                    "delivery_id": message.delivery_id,
                })).collect::<Vec<_>>(),
            }),
        )
        .map_err(|error| format!("Incomplete: retained input record rejected: {error}"))?;
        resources
            .append_reminder_if_absent(&self.child_thread_id, delivery_id, &reminder)
            .await
            .map_err(|error| format!("Incomplete: retained input record failed: {error}"))?;
        Ok(())
    }

    pub async fn cancel_task(&self, task_id: &str) -> Result<(), String> {
        self.manager
            .cancel_async(task_id)
            .await
            .map_err(|error| error.to_string())
    }
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
