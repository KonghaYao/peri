//! Workflow MCP data → Host task projection. No runtime or owner registry references.
use peri_acp_types::{
    ports::McpPoolPort,
    tasks::{ExternalTaskRegistration, TaskManager, TaskSnapshot},
    workflow::WorkflowTaskResult,
};
use serde::Deserialize;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const URI: &str = "workflow://background-tasks";
const CONSUMER_META: &str = "peri/workflowConsumer";
type Deliver = Arc<dyn Fn(&WorkflowTaskResult) -> Result<(), String> + Send + Sync>;

#[derive(Deserialize)]
struct Page {
    snapshot: TaskSnapshot,
    completions: Vec<WorkflowTaskResult>,
}

pub(crate) struct WorkflowProjection {
    pub(crate) client: Arc<Client>,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

pub(crate) struct Client {
    pool: Arc<dyn McpPoolPort>,
    manager: Arc<dyn TaskManager>,
    session_id: String,
    token: String,
    deliver: Deliver,
    scan: tokio::sync::Mutex<()>,
}

impl WorkflowProjection {
    pub(crate) fn start(
        pool: Arc<dyn McpPoolPort>,
        manager: Arc<dyn TaskManager>,
        session_id: String,
        token: String,
        deliver: Deliver,
    ) -> Self {
        let client = Arc::new(Client {
            pool,
            manager,
            session_id,
            token,
            deliver,
            scan: tokio::sync::Mutex::new(()),
        });
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_client = client.clone();
        let handle = tokio::spawn(async move {
            let mut failed = false;
            loop {
                let result = tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    result = task_client.reconcile() => result,
                };
                if let Err(error) = &result {
                    if !failed {
                        tracing::warn!(%error, "workflow MCP projection unavailable; retaining unsettled tasks");
                    }
                }
                failed = result.is_err();
                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    _ = tokio::time::sleep(if failed { Duration::from_secs(1) } else { Duration::from_millis(250) }) => {},
                }
            }
        });
        Self {
            client,
            cancel,
            handle,
        }
    }
}
impl Drop for WorkflowProjection {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.handle.abort();
    }
}

impl Client {
    pub(crate) async fn close(&self) -> Result<(), String> {
        self.pool
            .call_instance_tool(
                "workflow",
                "WorkflowClose",
                json!({}),
                Some(json!({CONSUMER_META:self.token})),
            )
            .await?;
        self.reconcile().await
    }

    async fn reconcile(&self) -> Result<(), String> {
        let _scan = self.scan.lock().await;
        let page: Page =
            serde_json::from_str(&self.pool.read_instance_resource("workflow", URI).await?)
                .map_err(|error| format!("invalid {URI}: {error}"))?;
        // Deliver retained terminals before admitting active projections; the owner's
        // concurrency slot may already have been reused by another run.
        for completion in &page.completions {
            let record = page
                .snapshot
                .tasks
                .iter()
                .find(|record| record.task_id == completion.run_id)
                .ok_or_else(|| "workflow completion has no owner task record".to_string())?;
            let registration = self.registration(record, Some(completion.clone()));
            self.manager
                .restore_external_terminal(
                    registration,
                    &format!("{}:terminal", completion.run_id),
                    completion.background_result(),
                )
                .await?;
            self.pool
                .call_instance_tool(
                    "workflow",
                    "WorkflowCompletionAck",
                    json!({"runId":completion.run_id}),
                    Some(json!({CONSUMER_META:self.token})),
                )
                .await?;
        }
        for record in &page.snapshot.tasks {
            if record.status == "running"
                && !page
                    .completions
                    .iter()
                    .any(|result| result.run_id == record.task_id)
            {
                self.manager
                    .register_external(self.registration(record, None))?;
                self.pool
                    .call_instance_tool(
                        "workflow",
                        "WorkflowTaskObserved",
                        json!({"runId":record.task_id}),
                        Some(json!({CONSUMER_META:self.token})),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    fn registration(
        &self,
        record: &peri_acp_types::tasks::TaskRecord,
        completion: Option<WorkflowTaskResult>,
    ) -> ExternalTaskRegistration {
        let pool = Arc::downgrade(&self.pool);
        let run_id = record.task_id.clone();
        let cancel = Arc::new(move || {
            let pool = pool.clone();
            let run_id = run_id.clone();
            Box::pin(async move {
                let pool = pool
                    .upgrade()
                    .ok_or_else(|| "workflow MCP pool unavailable".to_string())?;
                pool.call_instance_tool("workflow", "WorkflowKill", json!({"runId":run_id}), None)
                    .await
                    .map(|_| ())
            })
                as std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        });
        let deliver = self.deliver.clone();
        let on_terminal = Arc::new(
            move |_: &peri_acp_types::event::BackgroundTaskResult,
                  _: peri_acp_types::messages::MessageId| {
                let completion = completion.clone();
                let deliver = deliver.clone();
                Box::pin(async move {
                    let result = completion.ok_or_else(|| {
                        "workflow terminal has not been read through MCP".to_string()
                    })?;
                    deliver(&result)
                })
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<(), String>> + Send>,
                    >
            },
        );
        ExternalTaskRegistration {
            session_id: self.session_id.clone(),
            initiator_session_id: Some(self.session_id.clone()),
            owner_identity: "workflow".into(),
            owner_task_id: record.task_id.clone(),
            kind: peri_acp_types::tasks::BgTaskKind::Workflow,
            summary: record.summary.clone(),
            started_at: Some(record.started_at.clone()),
            cancel,
            on_terminal,
        }
    }
}

#[cfg(test)]
#[path = "workflow_projection_test.rs"]
mod tests;
