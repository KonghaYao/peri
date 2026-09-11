//! A registered run owns execution and completion publication; callers only observe it.
use crate::{
    journal::WorkflowJournalStore,
    progress::WorkflowProgressStore,
    registry::{WorkflowRunStatus, WorkflowTaskRegistry, WorkflowTaskResult},
    runner::{WorkflowInput, WorkflowResult, WorkflowRunner},
};
use futures::FutureExt;
use std::sync::{Arc, Weak};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
};

pub(super) struct RunCompletion {
    pub registry: Weak<WorkflowTaskRegistry>,
    pub progress: Arc<WorkflowProgressStore>,
    pub journal: Arc<WorkflowJournalStore>,
    pub run_id: String,
    pub name: String,
    pub started_at: std::time::Instant,
}

#[derive(Clone)]
pub(super) struct CompletedRun {
    pub result: WorkflowTaskResult,
    pub stderr_tail: Option<String>,
}

impl RunCompletion {
    pub(super) fn spawn(
        self,
        runner: Arc<WorkflowRunner>,
        input: WorkflowInput,
        kill: oneshot::Receiver<()>,
    ) -> (JoinHandle<()>, watch::Receiver<Option<CompletedRun>>) {
        let (done_tx, done_rx) = watch::channel(None);
        let (completed_tx, completed_rx) = watch::channel(None);
        let task = tokio::spawn(async move {
            // Unwinding must still settle the registered run. Panic content is never published.
            let execution = std::panic::AssertUnwindSafe(runner.run(
                self.run_id.clone(),
                input,
                self.progress.clone(),
                self.journal.clone(),
                done_tx,
                kill,
            ))
            .catch_unwind()
            .await;
            match execution {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(error = %error, "Workflow failed to start"),
                Err(_) => tracing::warn!("Workflow runner panicked before completion"),
            }
            // Runner cleanup is complete; no second task waits on a raw done channel.
            let raw = done_rx.borrow().clone().unwrap_or_else(|| WorkflowResult {
                run_id: self.run_id.clone(),
                status: "failed".into(),
                return_value: None,
                error: Some("workflow process exited before reporting result".into()),
                post_processing_status: peri_acp_types::workflow::PostProcessingStatus::Failed,
                delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
                stderr_tail: None,
            });
            let stderr_tail = raw.stderr_tail.clone();
            let result = self.project(raw);
            // The task cannot keep the registry alive by holding its own owner.
            if let Some(registry) = self.registry.upgrade() {
                registry.complete(&self.run_id, result.clone());
            }
            // Defer delivery / bg active count remain the session consumer's responsibility.
            let _ = completed_tx.send(Some(CompletedRun {
                result,
                stderr_tail,
            }));
        });
        (task, completed_rx)
    }

    fn project(&self, result: WorkflowResult) -> WorkflowTaskResult {
        // 从 progress_store 获取真实 agent 数量与 tool count
        // 必须在 done_rx 之后读取——此时 workflow 已执行完毕，
        // progress_store 已被所有 progress/event RPC 填充。
        let (agent_count, tool_calls_count) =
            self.progress.get_run_stats(&self.run_id).unwrap_or((0, 0));
        let phase_summaries = self.progress.get_phase_summaries(&self.run_id);
        let acceptance_status = self
            .journal
            .read_state(&self.run_id)
            .map(|state| state.acceptance_status)
            .unwrap_or_default();
        let delivery_status = result.delivery_status;
        let success = result.status == "completed"
            && delivery_status != peri_acp_types::workflow::DeliveryStatus::Blocked;
        let status = match result.status.as_str() {
            "completed" => WorkflowRunStatus::Completed,
            "killed" => WorkflowRunStatus::Killed,
            _ => WorkflowRunStatus::Failed,
        };
        let execution_status = match result.status.as_str() {
            "completed" => peri_acp_types::workflow::ExecutionStatus::Completed,
            "killed" => peri_acp_types::workflow::ExecutionStatus::Killed,
            _ => peri_acp_types::workflow::ExecutionStatus::Failed,
        };
        WorkflowTaskResult {
            run_id: self.run_id.clone(),
            workflow_name: self.name.clone(),
            success,
            status,
            execution_status,
            acceptance_status,
            post_processing_status: result.post_processing_status,
            delivery_status: result.delivery_status,
            state_artifact_exists: self
                .journal
                .run_dir(&self.run_id)
                .join("state.json")
                .is_file(),
            duration_ms: self.started_at.elapsed().as_millis() as u64,
            agent_count,
            tool_calls_count,
            error: result.error,
            phase_summaries,
            attempts: self.journal.read_attempts(&self.run_id).unwrap_or_default(),
        }
    }
}

pub(super) async fn receive_completion(
    rx: &mut watch::Receiver<Option<CompletedRun>>,
) -> Option<CompletedRun> {
    loop {
        if let Some(result) = rx.borrow().clone() {
            return Some(result);
        }
        if rx.changed().await.is_err() {
            return rx.borrow().clone();
        }
    }
}
