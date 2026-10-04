//! Workflow execution is unavailable without a persistent workflow runner.

use std::sync::Arc;

use peri_acp_types::workflow::{AgentExecutor, ProgressEvent, WorkflowTaskResult};
use peri_agent::middleware::r#trait::Middleware;

/// Retains the Host port shape while exposing no local workflow execution.
pub struct WorkflowMiddleware {
    notifications: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
}

impl WorkflowMiddleware {
    pub fn new(
        _executor: Arc<dyn AgentExecutor>,
        _cwd: &str,
        notifications: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
        _progress: Option<tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>>,
    ) -> Self {
        Self { notifications }
    }
}

#[async_trait::async_trait]
impl peri_acp_types::ports::WorkflowMiddlewarePort for WorkflowMiddleware {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn runs_snapshot(&self) -> serde_json::Value {
        serde_json::json!([])
    }

    async fn kill_agent(&self, _run_id: &str, _agent_id: u64) -> bool {
        false
    }

    fn kill_run(&self, _run_id: &str) -> bool {
        false
    }

    async fn resume(&self, _run_id: &str) -> Result<String, String> {
        Err("Workflow execution is unavailable on Emscripten".into())
    }

    fn subscribe_notifications(&self) -> tokio::sync::broadcast::Receiver<WorkflowTaskResult> {
        self.notifications.subscribe()
    }

    fn set_bg_registry(&self, _manager: Arc<dyn peri_acp_types::tasks::TaskManager>) {}

    fn init_notification_buffer(&self) -> bool {
        false
    }
}

pub struct WorkflowMiddlewareAdaptor;

impl WorkflowMiddlewareAdaptor {
    pub fn new(_inner: Arc<WorkflowMiddleware>) -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Middleware for WorkflowMiddlewareAdaptor {
    fn name(&self) -> &str {
        "WorkflowMiddleware"
    }
}
