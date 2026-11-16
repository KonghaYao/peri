use super::{
    fmt, Arc, BaseTool, CallToolRequestParams, CallToolResponse, CronListTool, CronRegisterTool,
    CronRemoveTool, CronScheduler, CronToggleTool, Debug, Formatter, Implementation,
    ListResourcesResult, ListToolsResult, McpError, Mutex, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, RequestContext, Resource,
    ResourceContents, RoleServer, ServerCapabilities, ServerConfig, ServerHandler,
};

pub const CRON_JOBS_URI: &str = "cron://jobs";

pub const CRON_TRIGGERS_URI: &str = "cron://triggers";

/// `cron` 实例的 `ServerConfig` 名字（`Implementation::name`）；实例名仍是注册表里的
/// `"cron"`（`Implementation::name` 与注册表 key 是两件事，不要合并）。
pub(super) const CRON_SERVER_NAME: &str = "peri-cron-mcp";

/// `cron` 实例的 handler：持有 cron 工具（同一 scheduler，**不含** tick 驱动）。
pub struct CronMcpServer {
    pub(super) tools: Vec<Arc<dyn BaseTool>>,
    pub(super) scheduler: Arc<Mutex<CronScheduler>>,
}

impl Debug for CronMcpServer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CronMcpServer")
            .finish_non_exhaustive()
    }
}

impl CronMcpServer {
    /// 生产构造：工具顺序与注册表声明顺序一致（`cron_register` → `cron_list` →
    /// `cron_remove` → `cron_toggle`），各工具持同一份 scheduler 的 `Arc` 克隆（不复制注册表）。
    #[must_use]
    pub fn new(scheduler: Arc<Mutex<CronScheduler>>) -> Self {
        let tools: Vec<Arc<dyn BaseTool>> = vec![
            Arc::new(CronRegisterTool::new(Arc::clone(&scheduler))),
            Arc::new(CronListTool::new(Arc::clone(&scheduler))),
            Arc::new(CronRemoveTool::new(Arc::clone(&scheduler))),
            Arc::new(CronToggleTool::new(Arc::clone(&scheduler))),
            Arc::new(crate::tools::CronAcknowledgeTool::new(Arc::clone(
                &scheduler,
            ))),
        ];
        Self { tools, scheduler }
    }

    pub(super) fn tools(&self) -> &[Arc<dyn BaseTool>] {
        &self.tools
    }
}

impl ServerHandler for CronMcpServer {
    // 不覆写 `discover`（§10 R2）。

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            CRON_SERVER_NAME,
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(CRON_JOBS_URI, "Scheduled jobs").with_mime_type("application/json"),
            Resource::new(CRON_TRIGGERS_URI, "Pending scheduled triggers")
                .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let scheduler = self.scheduler.lock();
        let payload = match request.uri.as_str() {
            CRON_JOBS_URI => serde_json::json!({"jobs": scheduler.list_tasks()}),
            CRON_TRIGGERS_URI => serde_json::json!({"triggers": scheduler.pending_triggers()}),
            _ => return Err(McpError::resource_not_found("unknown cron resource", None)),
        };
        let text = serde_json::to_string(&payload)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![ResourceContents::text(text, request.uri).with_mime_type("application/json")],
        )))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(peri_mcp_common::list_tools_of(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        // cron 工具都不读 `ToolContext`（`cron/tools.rs` 的 `invoke` 忽略 `_ctx`）。
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => Ok(CallToolResponse::Complete(rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text("Tool execution cancelled.")]))),
            result = peri_mcp_common::invoke_tool_call(self.tools(), "", &request) => result,
        }
    }
}
