//! MCP pool 的状态所有权、构造、基础查询与宿主端口。
//! 缓存、OAuth、生命周期及状态投影分别由私有子模块实现。

use std::fmt;
use std::fmt::{Debug, Formatter};
mod cache;
#[cfg(test)]
#[path = "client/cache_policy_test.rs"]
mod cache_policy_tests;
pub(crate) mod elicitation;
mod instance_face;
mod lifecycle;
mod monitor_alert;
mod monitor_identity;
mod notification_tap;
mod oauth;
pub(crate) mod output_store;
#[cfg(not(target_os = "emscripten"))]
pub(crate) mod process;
#[cfg(target_os = "emscripten")]
#[path = "client/process_wasm.rs"]
pub(crate) mod process;
mod queries;
mod readiness;
mod rewind;
mod service;
mod status;
mod subscription;
pub(crate) mod task_owner_metadata;
mod transport;
mod types;

use super::builtin::context::BuiltinContextError;
use super::builtin::context::BuiltinInstanceContext;
use super::builtin::runtime::BuiltinSpawnError;
use super::builtin::runtime::BuiltinTransport;
#[cfg(not(target_os = "emscripten"))]
use super::builtin::runtime::TickGuard;
#[cfg(not(target_os = "emscripten"))]
use super::builtin::runtime::BUILTIN_TICK_INTERVAL;
use super::config::McpCachePolicy;
use super::config::McpServerConfig;
use super::oauth_flow::OAuthFlowEvent;
use lifecycle::ServiceShutdownState;
use oauth::{OAuthFlowKey, PendingOAuthCallback};
#[cfg(not(target_os = "emscripten"))]
use peri_acp_types::builtin_mcp;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::ports::McpBuiltinWorkspaceState;
use peri_acp_types::ports::McpOAuthStartDisposition;
use peri_acp_types::ports::McpPoolShutdownReport;
use peri_acp_types::ports::McpResourceInfo;
use peri_acp_types::ports::McpServerConnectionStatus;
use peri_acp_types::ports::McpServerInfo;
use peri_acp_types::ports::McpServerOAuthStatus;
use peri_acp_types::ports::McpToolInfo;
use peri_acp_types::session::InboxHandle;
use peri_acp_types::skills::SkillMetadata;
use peri_acp_types::tasks::TaskManager;
use readiness::SystemReadinessTracker;
use rmcp::model::{Resource, ResourceContents, Tool};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

pub(crate) use cache::cache_scope_allows_persistence;
pub(crate) use cache::ConnectionResourceCache;
pub(crate) use elicitation::InteractionRoute;
pub(crate) use notification_tap::{
    pending_notification_reminder, NotificationTap, PendingMcpNotification,
};
pub use oauth::OAuthStartDisposition;
pub(crate) use readiness::{
    DiscoveryEvidence, NegotiatedSystemMcp, SystemMcpManifest, SystemReadinessError,
};
#[cfg(test)]
pub(crate) use service::ControlledMcpService;
pub(crate) use service::{
    client_capabilities_for_call, mcpp_client_info_for_profile, peer_declares_skills,
    McpServiceOwner, McpServiceWrapper, PeriClientHandler,
};
#[cfg(test)]
pub(crate) use status::status_change_text;
#[cfg(test)]
use status::{mcp_error_summary, mcp_status_label};
#[cfg(test)]
pub(crate) use subscription::build_subscription_filter;
pub(crate) use subscription::setup_subscription;
pub(crate) use transport::{
    build_authed_transport, build_http_transport, serve_client_auto, serve_client_auto_for_server,
};
pub(crate) use types::{AcpConnectionIdentity, McpConnectionKey};
pub use types::{
    ClientStatus, McpClientHandle, McpInitStatus, McpPoolError, OAuthStatus, ServerInfo,
};

mod ports;
mod session_bindings;
mod workspace_watch;

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;

#[cfg(test)]
#[path = "client_instance_face_test.rs"]
mod instance_face_tests;

#[path = "client/mcp_client_pool.rs"]
mod mcp_client_pool;
#[cfg(test)]
use mcp_client_pool::collect_with_single_recollect;
pub(crate) use mcp_client_pool::AcpConnectionDeclaration;
pub use mcp_client_pool::McpClientPool;
pub(crate) use mcp_client_pool::HTTP_CONNECT_TIMEOUT;
pub(crate) use mcp_client_pool::MCP_TASK_CANCEL_TIMEOUT;
pub(crate) use mcp_client_pool::SHUTDOWN_TIMEOUT;
pub(crate) use mcp_client_pool::STDIO_CONNECT_TIMEOUT;
pub(crate) use mcp_client_pool::SUBSCRIPTION_LISTEN_TIMEOUT;
