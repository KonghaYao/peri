//! Workflow Agent factory for a deployment without a local workflow runner.

use std::sync::Arc;

use peri_acp_types::{
    ports::WorkflowMiddlewarePort,
    workflow::{AgentExecutor, ProgressEvent, WorkflowTaskResult},
};
use peri_agent::{
    agent::workflow::{WorkflowAgentContext, WorkflowAgentDefinition, WorkflowMiddlewareFactory},
    middleware::r#trait::Middleware,
    tools::{BaseTool, ToolInvocationResolver},
};

use crate::{mcp::McpClientPool, workflow::WorkflowMiddleware};

pub struct WorkflowAgentMiddlewareFactory;

pub fn default_workflow_middleware_factory() -> Arc<dyn WorkflowMiddlewareFactory> {
    Arc::new(WorkflowAgentMiddlewareFactory)
}

pub fn default_workflow_middleware_factory_with_pool(
    _pool: Option<Arc<McpClientPool>>,
) -> Arc<dyn WorkflowMiddlewareFactory> {
    default_workflow_middleware_factory()
}

#[async_trait::async_trait]
impl WorkflowMiddlewareFactory for WorkflowAgentMiddlewareFactory {
    async fn resolve_agent_definition(
        &self,
        _agent_type: &str,
        _cwd: &str,
    ) -> Result<WorkflowAgentDefinition, String> {
        Err("Workflow execution is unavailable on Emscripten".into())
    }

    fn build_tools(
        &self,
        _cwd: &str,
        _disabled: &std::collections::HashSet<String>,
        _execution_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
        _mcp_skill_registry: Option<Arc<peri_acp_types::mcp_skills::McpSkillRegistry>>,
    ) -> Vec<Box<dyn BaseTool>> {
        Vec::new()
    }

    fn build_sandbox_write_tool(
        &self,
        _cwd: &str,
        _allowed_dirs: &[String],
    ) -> Option<Box<dyn BaseTool>> {
        None
    }

    fn build_middlewares(
        &self,
        _ctx: &WorkflowAgentContext,
        _model_name: &str,
        _skill_names: &[String],
        _execution_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
    ) -> Vec<Box<dyn Middleware>> {
        Vec::new()
    }

    fn build_tool_resolver(&self) -> Arc<dyn ToolInvocationResolver> {
        Arc::new(crate::tool_search::ExecuteExtraToolResolver::default())
    }

    fn build_workflow_middleware(
        &self,
        executor: Arc<dyn AgentExecutor>,
        cwd: &str,
        notification_tx: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
        progress_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>>,
    ) -> Arc<dyn WorkflowMiddlewarePort> {
        Arc::new(WorkflowMiddleware::new(
            executor,
            cwd,
            notification_tx,
            progress_rx,
        ))
    }
}
