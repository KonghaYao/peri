use super::{
    fmt, result_mapping, task_owner, tools, Arc, BaseTool, CallToolRequestParams, CallToolResponse,
    CallToolResult, CancelTaskParams, ContentBlock, CreateTaskResult, Debug, DetailedTask,
    Formatter, GetTaskParams, GetTaskResult, Implementation, JsonObject, ListToolsResult, McpError,
    MetaObject, MonitorAlertMeta, MonitorState, MonitorStatus, PaginatedRequestParams,
    RequestContext, RoleServer, ServerCapabilities, ServerConfig, ServerHandler, Task, TaskPayload,
    TaskSnapshot, TaskStatus, MONITOR_META_KEY, TASKS_EXTENSION_ID,
};

/// `Implementation::name` for the monitor server.
pub(super) const MONITOR_SERVER_NAME: &str = "peri-monitor-mcp";

/// `monitor` builtin handler: owns the tool declarations and the in-memory
/// task/event registry for one instance.
///
/// Clones share the same [`MonitorState`], so standalone HTTP sessions observe
/// one instance-level registry (still owner-filtered per session).
#[derive(Clone)]
pub struct MonitorMcpServer {
    pub(super) state: Arc<MonitorState>,
    pub(super) tools: Vec<Arc<dyn BaseTool>>,
}

impl Debug for MonitorMcpServer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MonitorMcpServer")
            .finish_non_exhaustive()
    }
}

impl MonitorMcpServer {
    #[must_use]
    pub fn new(cwd: impl Into<String>) -> Self {
        let state = Arc::new(MonitorState::new(cwd));
        let tools = tools::tools(Arc::clone(&state));
        Self { state, tools }
    }
}

impl ServerHandler for MonitorMcpServer {
    fn get_info(&self) -> ServerConfig {
        let mut extensions = rmcp::model::ExtensionCapabilities::new();
        extensions.insert(TASKS_EXTENSION_ID.to_string(), JsonObject::new());
        let mut capabilities = ServerCapabilities::builder().enable_tools().build();
        capabilities.extensions = Some(extensions);
        ServerConfig::new(capabilities).with_server_info(Implementation::new(
            MONITOR_SERVER_NAME,
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(peri_mcp_common::list_tools_of(&self.tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let owner = task_owner::task_owner_from_meta(&context.meta)?;
        let arguments = request.arguments.clone().unwrap_or_default();
        match request.name.as_ref() {
            "monitor_start" => match tools::parse_start(&arguments) {
                Ok(start) => match self.state.start(start, owner).await {
                    Ok(started) => {
                        let ttl_ms = self
                            .state
                            .task_ttl_ms(&started.task_id)
                            .map_err(unknown_task)?;
                        Ok(CallToolResponse::Task(seed_task(started, ttl_ms)))
                    }
                    Err(error) => Ok(tool_error("monitor_start", &error)),
                },
                Err(error) => Ok(tool_error("monitor_start", &error)),
            },
            "monitor_read" => match tools::parse_read(&arguments).and_then(|read| {
                self.state.read(
                    &read.task_id,
                    owner.as_deref(),
                    read.after_seq,
                    read.limit,
                    read.max_bytes,
                )
            }) {
                Ok(page) => json_response(page),
                Err(error) => Ok(tool_error("monitor_read", &error)),
            },
            "monitor_stop" => match tools::parse_stop(&arguments) {
                Ok(stop) => match self
                    .state
                    .stop(&stop.task_id, owner.as_deref(), stop.reason)
                    .await
                {
                    Ok(snapshot) => json_response(snapshot),
                    Err(error) => Ok(tool_error("monitor_stop", &error)),
                },
                Err(error) => Ok(tool_error("monitor_stop", &error)),
            },
            "monitor_list" => match tools::parse_list(&arguments) {
                Ok(list) => {
                    let tasks = self
                        .state
                        .list(owner.as_deref(), list.include_ended, list.limit);
                    Ok(success_json(serde_json::json!({ "tasks": tasks })))
                }
                Err(error) => Ok(tool_error("monitor_list", &error)),
            },
            other => Err(McpError::invalid_params(
                format!("unknown tool: {other}"),
                None,
            )),
        }
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        let owner = task_owner::task_owner_from_meta(&context.meta)?;
        let snapshot = self
            .state
            .snapshot(&request.task_id, owner.as_deref())
            .map_err(unknown_task)?;
        let ttl_ms = self
            .state
            .task_ttl_ms(&request.task_id)
            .map_err(unknown_task)?;
        let mut result = GetTaskResult::new(detailed_task(snapshot.clone(), ttl_ms));
        // The Tasks projection has no room for custom task fields; the host
        // task-subscription poller reads the cumulative alert projection from
        // result metadata and owns rate limiting / delivery.
        let alert = MonitorAlertMeta {
            alert_matched: snapshot.alert_matched,
            last_alert_seq: snapshot.last_alert_seq,
            label: snapshot.label.clone(),
        };
        let mut meta = JsonObject::new();
        meta.insert(
            MONITOR_META_KEY.into(),
            serde_json::to_value(alert)
                .expect("monitor alert metadata is always JSON-serializable"),
        );
        result.meta = Some(MetaObject(meta));
        Ok(result)
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let owner = task_owner::task_owner_from_meta(&context.meta)?;
        self.state
            .request_cancel(
                &request.task_id,
                owner.as_deref(),
                "MCP task cancellation requested.",
            )
            .map_err(unknown_task)
    }
}

pub(super) fn unknown_task(_error: crate::state::MonitorError) -> McpError {
    // Ownership mismatch and missing ids share one diagnostic: never reveal
    // that another session's task exists.
    McpError::invalid_params("unknown task", None)
}

pub(super) fn seed_task(started: crate::state::StartedMonitor, ttl_ms: u64) -> CreateTaskResult {
    let now = started.started_at.clone();
    let task = Task::new(
        started.task_id.clone(),
        TaskStatus::Working,
        now.clone(),
        now,
    )
    .with_status_message("Monitor command started. Read incremental events with monitor_read.")
    .with_ttl_ms(ttl_ms)
    .with_poll_interval_ms(1_000);
    let mut meta = JsonObject::new();
    meta.insert(
        "peri/monitor".into(),
        serde_json::json!({
            "startedAt": started.started_at,
            "deadlineAt": started.deadline_at,
            "label": started.label,
        }),
    );
    CreateTaskResult::new(task).with_meta(MetaObject(meta))
}

pub(super) fn detailed_task(snapshot: TaskSnapshot, ttl_ms: u64) -> DetailedTask {
    let mut task = Task::new(
        snapshot.task_id.clone(),
        rmcp_status(snapshot.status),
        snapshot.started_at.clone(),
        snapshot.last_updated_at.clone(),
    )
    .with_poll_interval_ms(1_000)
    .with_ttl_ms(ttl_ms);
    if let Some(message) = snapshot.status_message.clone() {
        task = task.with_status_message(message);
    }
    let payload = match snapshot.status {
        MonitorStatus::Working => TaskPayload::Working,
        MonitorStatus::Completed => TaskPayload::Completed {
            result: completed_result(&snapshot)
                .as_object()
                .cloned()
                .unwrap_or_default(),
        },
        MonitorStatus::Failed => TaskPayload::Failed {
            error: serde_json::json!({
                "code": -32603,
                "message": snapshot
                    .status_message
                    .clone()
                    .unwrap_or_else(|| "Monitor task failed.".to_string()),
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        },
        MonitorStatus::Cancelled => TaskPayload::Cancelled,
    };
    DetailedTask::new(task, payload)
}

pub(super) fn completed_result(snapshot: &TaskSnapshot) -> serde_json::Value {
    let text = snapshot
        .status_message
        .clone()
        .unwrap_or_else(|| "Monitor task completed.".to_string());
    serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
    })
}

pub(super) fn rmcp_status(status: MonitorStatus) -> TaskStatus {
    match status {
        MonitorStatus::Working => TaskStatus::Working,
        MonitorStatus::Completed => TaskStatus::Completed,
        MonitorStatus::Failed => TaskStatus::Failed,
        MonitorStatus::Cancelled => TaskStatus::Cancelled,
    }
}

pub(super) fn tool_error(tool: &str, error: &crate::state::MonitorError) -> CallToolResponse {
    CallToolResponse::Complete(CallToolResult::error(vec![ContentBlock::text(
        result_mapping::failure_text(tool, error),
    )]))
}

pub(super) fn success_json(value: serde_json::Value) -> CallToolResponse {
    let mut result = CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
    result.structured_content = Some(value);
    CallToolResponse::Complete(result)
}

pub(super) fn json_response(value: impl serde::Serialize) -> Result<CallToolResponse, McpError> {
    let value = serde_json::to_value(value)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;
    Ok(success_json(value))
}
