//! `CapabilityHandler`：router 内部的能力 handler 枚举与 `ServerHandler` 转发。
//!
//! 三种来源共用一套转发面：in-process builtin、workspace 上游代理与测试 handler。
//! 转发只透传调用，不复制能力语义；错误与 panic 隔离在 [`super::BuiltinRouter`]。

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, CancelledNotificationParam,
        CustomNotification, CustomRequest, CustomResult, GetSkillRequestParams, GetSkillResult,
        GetTaskParams, GetTaskResult, ListResourceTemplatesResult, ListResourcesResult,
        ListSkillsResult, ListToolsResult, PaginatedRequestParams, ProgressNotificationParam,
        ReadResourceDirectoryRequestParams, ReadResourceDirectoryResult, ReadResourceRequestParams,
        ReadResourceResponse, ServerConfig, SubscribeRequestParams, SubscriptionFilter,
        UnsubscribeRequestParams, UpdateTaskParams,
    },
    service::{NotificationContext, RequestContext, RoleServer, SubscriptionContext},
    ErrorData as McpError, ServerHandler,
};

use super::super::dispatch::BuiltinServerHandler;
use super::CapabilityStatus;
use crate::mcp::upstream_proxy::{CapabilityHealth, UpstreamProxy};

/// 一个能力实际持有的 handler 来源。
pub(super) enum CapabilityHandler {
    InProcess(Box<BuiltinServerHandler>),
    Upstream(UpstreamProxy),
    #[cfg(test)]
    Test(super::router_test_support::TestCapability),
}

impl CapabilityHandler {
    /// 能力级健康：in-process handler 构造成功即就绪；代理读取上游连接状态。
    pub(super) fn health(&self) -> CapabilityStatus {
        match self {
            Self::InProcess(_) => CapabilityStatus::Ready,
            Self::Upstream(proxy) => match proxy.health() {
                CapabilityHealth::Ready => CapabilityStatus::Ready,
                CapabilityHealth::Connecting => CapabilityStatus::Degraded {
                    reason: "upstream is connecting".into(),
                },
                CapabilityHealth::Degraded { reason } => CapabilityStatus::Degraded {
                    reason: format!("upstream degraded: {reason:?}"),
                },
            },
            #[cfg(test)]
            Self::Test(capability) => capability.status(),
        }
    }
}

impl ServerHandler for CapabilityHandler {
    fn get_info(&self) -> ServerConfig {
        match self {
            Self::InProcess(handler) => handler.get_info(),
            Self::Upstream(proxy) => proxy.get_info(),
            #[cfg(test)]
            Self::Test(capability) => capability.get_info(),
        }
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        match self {
            Self::InProcess(handler) => handler.on_initialized(context).await,
            Self::Upstream(proxy) => proxy.on_initialized(context).await,
            #[cfg(test)]
            Self::Test(_) => {}
        }
    }

    async fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        context: NotificationContext<RoleServer>,
    ) {
        match self {
            Self::InProcess(handler) => handler.on_cancelled(notification, context).await,
            Self::Upstream(proxy) => proxy.on_cancelled(notification, context).await,
            #[cfg(test)]
            Self::Test(_) => {}
        }
    }

    async fn on_progress(
        &self,
        notification: ProgressNotificationParam,
        context: NotificationContext<RoleServer>,
    ) {
        match self {
            Self::InProcess(handler) => handler.on_progress(notification, context).await,
            Self::Upstream(proxy) => proxy.on_progress(notification, context).await,
            #[cfg(test)]
            Self::Test(_) => {}
        }
    }

    async fn on_roots_list_changed(&self, context: NotificationContext<RoleServer>) {
        match self {
            Self::InProcess(handler) => handler.on_roots_list_changed(context).await,
            Self::Upstream(proxy) => proxy.on_roots_list_changed(context).await,
            #[cfg(test)]
            Self::Test(_) => {}
        }
    }

    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        context: NotificationContext<RoleServer>,
    ) {
        match self {
            Self::InProcess(handler) => handler.on_custom_notification(notification, context).await,
            Self::Upstream(proxy) => proxy.on_custom_notification(notification, context).await,
            #[cfg(test)]
            Self::Test(_) => {}
        }
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        match self {
            Self::InProcess(handler) => handler.list_tools(request, context).await,
            Self::Upstream(proxy) => proxy.list_tools(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.list_tools(request, context).await,
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        match self {
            Self::InProcess(handler) => handler.call_tool(request, context).await,
            Self::Upstream(proxy) => proxy.call_tool(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.call_tool(request, context).await,
        }
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        match self {
            Self::InProcess(handler) => handler.list_resources(request, context).await,
            Self::Upstream(proxy) => proxy.list_resources(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.list_resources(request, context).await,
        }
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        match self {
            Self::InProcess(handler) => handler.list_resource_templates(request, context).await,
            Self::Upstream(proxy) => proxy.list_resource_templates(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.list_resource_templates(request, context).await,
        }
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        match self {
            Self::InProcess(handler) => handler.read_resource(request, context).await,
            Self::Upstream(proxy) => proxy.read_resource(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.read_resource(request, context).await,
        }
    }

    async fn read_resource_directory(
        &self,
        request: ReadResourceDirectoryRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceDirectoryResult, McpError> {
        match self {
            Self::InProcess(handler) => handler.read_resource_directory(request, context).await,
            Self::Upstream(proxy) => proxy.read_resource_directory(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.read_resource_directory(request, context).await,
        }
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        match self {
            Self::InProcess(handler) => handler.subscribe(request, context).await,
            Self::Upstream(proxy) => proxy.subscribe(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.subscribe(request, context).await,
        }
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        match self {
            Self::InProcess(handler) => handler.unsubscribe(request, context).await,
            Self::Upstream(proxy) => proxy.unsubscribe(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.unsubscribe(request, context).await,
        }
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        match self {
            Self::InProcess(handler) => handler.accepted_subscription_filter(requested),
            Self::Upstream(proxy) => proxy.accepted_subscription_filter(requested),
            #[cfg(test)]
            Self::Test(capability) => capability.accepted_subscription_filter(requested),
        }
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        match self {
            Self::InProcess(handler) => handler.listen(context).await,
            Self::Upstream(proxy) => proxy.listen(context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.listen(context).await,
        }
    }

    async fn list_skills(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListSkillsResult, McpError> {
        match self {
            Self::InProcess(handler) => handler.list_skills(request, context).await,
            Self::Upstream(proxy) => proxy.list_skills(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.list_skills(request, context).await,
        }
    }

    async fn get_skill(
        &self,
        request: GetSkillRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetSkillResult, McpError> {
        match self {
            Self::InProcess(handler) => handler.get_skill(request, context).await,
            Self::Upstream(proxy) => proxy.get_skill(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.get_skill(request, context).await,
        }
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        match self {
            Self::InProcess(handler) => handler.on_custom_request(request, context).await,
            Self::Upstream(proxy) => proxy.on_custom_request(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.on_custom_request(request, context).await,
        }
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        match self {
            Self::InProcess(handler) => handler.get_task(request, context).await,
            Self::Upstream(proxy) => proxy.get_task(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.get_task(request, context).await,
        }
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        match self {
            Self::InProcess(handler) => handler.update_task(request, context).await,
            Self::Upstream(proxy) => proxy.update_task(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.update_task(request, context).await,
        }
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        match self {
            Self::InProcess(handler) => handler.cancel_task(request, context).await,
            Self::Upstream(proxy) => proxy.cancel_task(request, context).await,
            #[cfg(test)]
            Self::Test(capability) => capability.cancel_task(request, context).await,
        }
    }
}
