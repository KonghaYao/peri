pub mod acp;
pub mod agent_registry;
pub mod apps;
pub mod apps_invoke;
pub mod apps_relay;
pub mod auth_store;
// builtin MCP 行为层（注册表解析 / 默认层 overlay / 关闭集 / 直连性声明）。
pub(crate) mod builtin;
pub mod callback_server;
pub mod client;
pub mod client_oauth;
pub mod config;
pub mod discover_tool;
pub mod dynamic;
// ClientInitializeError 来自 rmcp crate（504 bytes），无法修改其定义
#[allow(clippy::result_large_err)]
pub mod initialize;
pub mod middleware;
pub mod oauth_flow;
pub mod reconnect;
pub mod resource_cache;
pub mod resource_tool;
pub(crate) mod skill_activation;
pub(crate) mod skill_discovery;
pub(crate) mod system_tools;
pub mod task_scope;
pub mod tool_bridge;
mod tool_request;
pub mod transport;

pub use acp::AcpMcpService;
pub use agent_registry::{ActivatedMcpAgent, McpAgentMetadata, McpAgentRegistry};
pub use apps::{
    canonical_resource_uri, raw_resource, raw_tool, tool_resource_uri, tool_visibility,
    McpAppsInvocationError, McpAppsInvocationSeam, McpAppsInvoker, McpCapabilityProfile,
    RawCallToolResult, RawMcpResource, RawMcpTool, ToolVisibility, MCP_APPS_VERSION,
    MCP_APP_MIME_TYPE, MCP_UI_EXTENSION,
};
pub use auth_store::{OAuthCredentialClient, PerServerCredentialStore};
pub use callback_server::{parse_code_from_url, CallbackError, OAuthCallbackServer};
pub use client::{
    redact_mcp_error, ClientStatus, McpClientHandle, McpClientPool, McpInitStatus, McpPoolError,
    OAuthStartDisposition, OAuthStatus, ServerInfo,
};
pub use config::{
    load_merged_config, remove_server_from_config, set_server_disabled, ConfigSource,
    McpConfigError, McpConfigFile, McpServerConfig, OAuthConfig,
};
pub use middleware::McpMiddleware;
pub use oauth_flow::{
    OAuthCallbackResult, OAuthFailureKind, OAuthFlowError, OAuthFlowEvent, OAuthFlowManager,
};
pub use resource_tool::McpResourceTool;
pub use rmcp::model::{Resource, Tool};
pub use task_scope::{
    DynamicMcpTaskKind, McpTaskKey, McpTaskOwner, McpTaskShutdownReport, McpTaskSpawner,
    TaskAdmissionError,
};
pub use tool_bridge::{build_tool_bridges, McpToolBridge, ToolCallError};
// D-02：crate 内 seam 测试（主 plan §6 W5）。模块名参与 `cargo test` 过滤，
// 故不沿用 `mod tests`（过滤 `mcp::mcp_v4_seam` 必须命中本模块）。
#[cfg(test)]
#[path = "mcp_v4_seam_test.rs"]
mod mcp_v4_seam_tests;

// Git Watch 下沉（builtin workspace 订阅回传）的线路级验收：T4–T7b 经生产链路装配的
// 端到端回传证据（T7b 另复用本文件的订阅探针 handler）。模块名参与 `cargo test`
// 过滤（`builtin_subscription_wire`）。
#[cfg(test)]
#[path = "builtin_subscription_wire_test.rs"]
mod builtin_subscription_wire_tests;

// W5：Agent 资源面真实线路夹具（供 subagent/tool 与 catalog 用例使用）。
#[cfg(test)]
#[path = "agent_face_fixture.rs"]
pub(crate) mod agent_face_fixture;

// builtin 默认配置层（loader step 6.5 overlay）的 crate 内验收（owner：I-02）。
// 两个 builtin 测试模块由 I-02 在 W3 一次挂载，避免 builtin_apply / builtin_runtime
// 出现两个 owner。
#[cfg(test)]
#[path = "builtin_apply_test.rs"]
mod builtin_apply_tests;

// builtin 运行时与关闭面的 crate 内验收（owner：V-01，W4 扩展为启动路径 / 审批 /
// wire 计数 / 无 orphan / 大 payload；W3 由 I-02 建挂载点并落关闭面断言）。
#[cfg(test)]
#[path = "builtin_runtime_test.rs"]
mod builtin_runtime_tests;

// Workspace handler integration tests remain host-side because they also exercise the host
// transport, client bridge, and recovery lifecycle. The handler and tools themselves live in
// `peri-mcp-workspace`.
#[cfg(test)]
#[path = "builtin/workspace_test.rs"]
mod workspace_builtin_tests;
#[cfg(test)]
#[path = "builtin/workspace_recovery_test.rs"]
mod workspace_recovery_tests;

// Scheduler tick ownership is a host lifecycle concern; cron server/tool wire behavior lives in
// `peri-mcp-cron`.
#[cfg(test)]
#[path = "builtin_cron_runtime_test.rs"]
mod builtin_cron_runtime_tests;
