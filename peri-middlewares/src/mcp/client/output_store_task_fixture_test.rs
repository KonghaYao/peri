use super::*;

/// The remote source controls completion; the subscription retains its ticket.
pub(super) struct FinishingTask(pub(super) Arc<Notify>);

impl ServerHandler for FinishingTask {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        let mut config = rmcp::model::ServerConfig::default();
        config.capabilities = rmcp::model::ServerCapabilities::builder()
            .enable_tasks()
            .build();
        config
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        StartedTask.call_tool(request, context).await
    }

    async fn get_task(
        &self,
        _: rmcp::model::GetTaskParams,
        _: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::GetTaskResult, rmcp::ErrorData> {
        self.0.notified().await;
        Ok(rmcp::model::GetTaskResult::new(
            rmcp::model::DetailedTask::new(
                rmcp::model::Task::new(
                    "remote-child-task",
                    rmcp::model::TaskStatus::Completed,
                    "2026-01-01T00:00:00Z",
                    "2026-01-01T00:00:01Z",
                ),
                rmcp::model::TaskPayload::Completed {
                    result: serde_json::Map::new(),
                },
            ),
        ))
    }
}
