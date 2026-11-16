//! `BuiltinRouter` 的测试用能力 handler：按注册表声明生成工具/资源，并注入
//! 调用 panic、列表失败、任务创建与降级状态。

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use peri_acp_types::builtin_mcp::v5::capability as find_capability;
use rmcp::{
    model::Implementation,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, ContentBlock,
        CreateTaskResult, CustomRequest, CustomResult, ErrorCode, ListResourceTemplatesResult,
        ListResourcesResult, ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams,
        ReadResourceResponse, Resource, ResourceContents, ServerCapabilities, ServerConfig,
        SubscribeRequestParams, SubscriptionFilter, Task, TaskStatus, UpdateTaskParams,
    },
    service::{RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};

/// 测试探针：断言 router 是否把请求送到了正确的能力。
#[derive(Debug, Default)]
pub(crate) struct TestProbe {
    pub(crate) calls: AtomicUsize,
    pub(crate) listed: AtomicUsize,
}

impl TestProbe {
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub(crate) fn listed(&self) -> usize {
        self.listed.load(Ordering::SeqCst)
    }
}

/// 测试能力行为。
#[derive(Debug, Clone)]
pub(crate) enum TestMode {
    Normal,
    PanicOnCall,
    ErrorOnList,
    CreateTask(String),
    Degraded(String),
}

pub(crate) struct TestCapability {
    capability: &'static str,
    mode: TestMode,
    tools: Vec<rmcp::model::Tool>,
    resources: Vec<Resource>,
    probe: Arc<TestProbe>,
}

impl TestCapability {
    pub(crate) fn new(capability: &'static str, mode: TestMode) -> (Self, Arc<TestProbe>) {
        let declared = find_capability(capability).expect("test capability must be declared");
        let tools = declared
            .tools
            .iter()
            .map(|tool| {
                rmcp::model::Tool::new(
                    tool.original_name,
                    format!("test tool {}", tool.original_name),
                    rmcp::model::JsonObject::new(),
                )
            })
            .collect();
        let resources = declared
            .resources
            .static_resources
            .iter()
            .map(|uri| Resource::new(*uri, *uri))
            .collect();
        let probe = Arc::new(TestProbe::default());
        (
            Self {
                capability,
                mode,
                tools,
                resources,
                probe: Arc::clone(&probe),
            },
            probe,
        )
    }

    pub(crate) fn status(&self) -> super::CapabilityStatus {
        match &self.mode {
            TestMode::Degraded(reason) => super::CapabilityStatus::Degraded {
                reason: reason.clone(),
            },
            _ => super::CapabilityStatus::Ready,
        }
    }

    fn probe(&self) -> &TestProbe {
        &self.probe
    }
}

impl ServerHandler for TestCapability {
    fn get_info(&self) -> ServerConfig {
        let mut capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_resources_subscribe()
            .build();
        if matches!(self.mode, TestMode::CreateTask(_)) {
            capabilities
                .extensions
                .get_or_insert_with(Default::default)
                .insert(
                    rmcp::model::TASKS_EXTENSION_ID.to_owned(),
                    rmcp::model::JsonObject::new(),
                );
        }
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new(self.capability, "0.0.0"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.probe().listed.fetch_add(1, Ordering::SeqCst);
        if matches!(self.mode, TestMode::ErrorOnList) {
            return Err(McpError::internal_error("test list failure", None));
        }
        Ok(ListToolsResult::with_all_items(self.tools.clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.probe().calls.fetch_add(1, Ordering::SeqCst);
        match &self.mode {
            TestMode::PanicOnCall => panic!("test capability panic"),
            TestMode::CreateTask(task_id) => {
                let task = Task::new(
                    task_id.clone(),
                    TaskStatus::Working,
                    "2026-01-01T00:00:00Z",
                    "2026-01-01T00:00:00Z",
                );
                Ok(CallToolResponse::Task(CreateTaskResult::new(task)))
            }
            _ => Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                ContentBlock::text(format!("{}:{}", self.capability, request.name)),
            ]))),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(self.resources.clone()))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(Vec::new()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        Ok(ReadResourceResponse::Complete(
            rmcp::model::ReadResourceResult::new(vec![ResourceContents::text(
                format!("{}:{}", self.capability, request.uri),
                request.uri,
            )]),
        ))
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        _request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        Ok(())
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        _request: rmcp::model::UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        Ok(())
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        Some(requested.clone())
    }

    async fn cancel_task(
        &self,
        _request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.probe().calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn update_task(
        &self,
        _request: UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        Ok(())
    }

    async fn on_custom_request(
        &self,
        _request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "test capability has no custom requests",
            None,
        ))
    }
}
