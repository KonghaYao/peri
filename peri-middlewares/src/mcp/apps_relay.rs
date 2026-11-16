//! MCP Apps relay：连接绑定校验 + 资源读取 + 把 App 回调 / Host 调用交给
//! **session-owned canonical 入口**（[`SessionInvocationPort`]）。
//!
//! 本层不再持有 dispatcher 或执行取消令牌（设计 §4 删除清单 3）：App 记录是纯数据，
//! 调用经 [`crate::mcp::session_invocation::SessionInvocationService`] 重新取用。

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::interaction::AttachmentId;
use peri_acp_types::interaction::ConnectionEpoch;
use peri_acp_types::mcp_apps::McpAppInvokeOutcome;
use peri_acp_types::mcp_apps::McpAppInvokeRequest;
use peri_acp_types::mcp_apps::McpAppsErrorKind;
use peri_acp_types::mcp_apps::McpAppsRelayError;
use peri_acp_types::mcp_apps::McpAppsRelayPort;
use peri_acp_types::mcp_apps::RawResource;
use peri_acp_types::mcp_apps::SessionInvocationPort;
use peri_acp_types::mcp_apps::MCP_APPS_HTML_MIME;
use peri_acp_types::mcp_apps_facade::AppAttachRequest;
use peri_acp_types::mcp_apps_facade::AppAttachSnapshot;
use peri_acp_types::mcp_apps_facade::AppDescriptor;
use peri_acp_types::mcp_apps_facade::AppExecutionState;
use peri_acp_types::mcp_apps_facade::AttachmentDetachReason;
use peri_acp_types::mcp_apps_facade::CloseAppOutcome;
use peri_acp_types::mcp_apps_facade::CloseVerdict;
use peri_acp_types::mcp_apps_facade::McpAppsFeatures;
use peri_acp_types::mcp_apps_facade::McpAppsLimits;
use peri_acp_types::mcp_apps_facade::RawResourceDescriptor;
use peri_acp_types::mcp_apps_facade::ResourceList;
use peri_acp_types::mcp_apps_facade::ResourceListRequest;
use peri_acp_types::mcp_apps_facade::ResourceReadRequest;
use peri_acp_types::mcp_apps_facade::TargetIdentity;
use peri_acp_types::mcp_apps_facade::UiResourceRef;

use super::apps::AttachmentRoute;
use super::apps::McpAppRecord;
use super::apps::RecordState;
use super::client::McpClientPool;

#[cfg(test)]
#[path = "apps_relay_test.rs"]
mod tests;

#[path = "apps_relay/pool_mcp_apps_relay.rs"]
mod pool_mcp_apps_relay;
pub(crate) use pool_mcp_apps_relay::relay_error;
pub use pool_mcp_apps_relay::PoolMcpAppsRelay;
