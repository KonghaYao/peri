use std::sync::Arc;

use peri_acp_types::session::ExecutionBinding;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::tasks::TaskManager as _;

use crate::agent::async_tasks::TaskManager;
use crate::session::MessageQueue;

use super::ChildHandoffReceipt;

#[cfg(test)]
#[path = "resource_owner_test.rs"]
mod tests;

/// 资源宿主只保留任务和消息，不保留计算 session、模型或旧事件总线。
pub struct ParkedChildOwner {
    pub child_thread_id: String,
    pub parent_thread_id: String,
    pub manager: Arc<TaskManager>,
    pub queue: MessageQueue,
    pub resources: Arc<dyn SessionResources>,
    pub(super) pool: Option<std::sync::Weak<dyn peri_acp_types::ports::McpPoolPort>>,
    state: parking_lot::Mutex<OwnerState>,
}

#[derive(Default)]
struct OwnerState {
    execution: Option<ExecutionBinding>,
    closing: bool,
    preparing: bool,
    failure: Option<String>,
    receipt: Option<ChildHandoffReceipt>,
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
        resources: Arc<dyn SessionResources>,
        pool: Option<std::sync::Weak<dyn peri_acp_types::ports::McpPoolPort>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            child_thread_id,
            parent_thread_id,
            manager,
            queue,
            resources,
            pool,
            state: parking_lot::Mutex::new(OwnerState::default()),
        })
    }

    pub fn acquire(
        self: &Arc<Self>,
        execution: ExecutionBinding,
    ) -> Result<ChildExecutionPermit, String> {
        let mut state = self.state.lock();
        if state.closing {
            return Err("child resource owner is closing".into());
        }
        if let Some(error) = &state.failure {
            return Err(format!("child resource settlement is unconfirmed: {error}"));
        }
        if state.execution.is_some() {
            return Err("child execution or terminal event drain is still active".into());
        }
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
        if state.closing || state.execution.is_some() || state.failure.is_some() {
            return Err(format!(
                "child resource owner cannot resume: closing={}, active={}, failure={:?}",
                state.closing,
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
        state.receipt = Some(receipt.clone());
        Ok(receipt)
    }

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

    pub async fn close(&self) -> Result<(), String> {
        self.begin_close()?;
        for child in self.manager.child_owners() {
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
        if self.queue.has_required() {
            return Err("Incomplete: child retained messages require canonical settlement".into());
        }
        Ok(())
    }

    pub async fn cancel_task(&self, task_id: &str) -> Result<(), String> {
        self.manager
            .cancel_async(task_id)
            .await
            .map_err(|error| error.to_string())
    }
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
