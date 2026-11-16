//! Workflow builtin MCP handler: model/host tools, resources, and standard subscriptions.
//!
//! 设计依据：`docs/design/workflow-mcp.md` §4。三层授权边界：
//! 1. **发现过滤**（模型面）由注册表 `BuiltinMcpTool.model_visible` 在宿主桥接点
//!    完成（`WorkflowAgentResult` 是 host-only）；本 handler 与 host 调用面无关。
//! 2. **调用授权**（服务端）：`WorkflowAgentResult` 校验调用请求 `_meta` 中的消费者
//!    令牌（[`WORKFLOW_CONSUMER_META_KEY`]），并要求目标任务已被 claim
//!    （`SettleAdmission::ExecutingOnly`）——不信任调用方参数声明的身份。
//! 3. **执行授权**：任务参数只能收缩消费者权限，由消费者侧过滤链承担（P6）。
//!
//! 工具面错误分两层（见 [`DispatchOutcome`]）：参数结构 / 未知工具 / 调用授权失败
//! 是**协议级**错误（JSON-RPC error，客户端可按错误码分类）；业务失败（run / task
//! 不存在、runtime 返回的启动校验失败、关闭与未就绪屏障）保留**工具级**
//! `is_error` 文本。
//!
//! 订阅只发标准 `notifications/resources/updated`（无 vendor notification）；
//! `get_info` 声明 `resources.subscribe` 是 SDK 受理 `subscriptions/listen` 的前提。

use std::sync::Arc;

use peri_workflow::protocol::AgentRunResult;
use peri_workflow::runtime::{
    SettleOutcome, WorkflowRuntime, WorkflowTaskState, WORKFLOW_CONSUMER_META_KEY,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, RequestMetaObject,
    Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig,
    SubscriptionFilter, Tool,
};
use rmcp::service::{RequestContext, RoleServer, SubscriptionContext};
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use serde_json::{json, Map, Value};

use crate::input::WorkflowInstanceInput;
use crate::resources::{self, ResourceReadError};

#[cfg(test)]
#[path = "server_test_fixture.rs"]
mod test_fixture;

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;

#[path = "consumer.rs"]
mod consumer;

#[path = "server/workflow_mcp_server.rs"]
mod workflow_mcp_server;

use workflow_mcp_server::required_str;
use workflow_mcp_server::DispatchOutcome;

pub use workflow_mcp_server::WorkflowMcpServer;

use workflow_mcp_server::encode_json;
use workflow_mcp_server::schema_of;
