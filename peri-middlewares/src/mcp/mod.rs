pub mod agent_registry;
pub mod auth_store;
pub mod callback_server;
pub mod channel_handler;
pub mod client;
pub mod client_oauth;
pub mod config;
pub mod discover_tool;
// ClientInitializeError 来自 rmcp crate（504 bytes），无法修改其定义
#[allow(clippy::result_large_err)]
pub mod initialize;
pub mod mcp_notify;
pub mod middleware;
pub mod oauth_flow;
pub mod reconnect;
pub mod resource_cache;
pub mod resource_tool;
pub(crate) mod skill_discovery;
pub mod task_scope;
pub mod tool_bridge;
pub mod transport;

pub use agent_registry::{ActivatedMcpAgent, McpAgentMetadata, McpAgentRegistry};
pub use auth_store::{AuthStoreError, FileCredentialStore, PerServerCredentialStore};
pub use callback_server::{parse_code_from_url, CallbackError, OAuthCallbackServer};
pub use channel_handler::ChannelHandler;
pub use client::{
    redact_mcp_error, ClientStatus, McpClientHandle, McpClientPool, McpInitStatus, McpPoolError,
    OAuthStartDisposition, OAuthStatus, ServerInfo,
};
pub(crate) use config::load_merged_config_full;
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
pub use task_scope::{McpTaskKey, McpTaskOwner, McpTaskShutdownReport, McpTaskSpawner};
pub use tool_bridge::{build_tool_bridges, McpToolBridge, ToolCallError};
