//! session-owned canonical 调用入口（App 回调与 Host 程序化调用的**共同**准入面）。
//!
//! 设计 §3.1 六步管线：归属校验 → 目标绑定 → 既有 before-tool 规则 → 批准覆盖
//! 最终目标 → admission owner 串行裁决（已绑定 peer）→ 先写 outcome 再投影。
//!
//! 本模块的类型隔离（设计 §2）：
//! - [`AppDeliveryToken`] / [`AttachmentDeliveryToken`] / [`InvocationCancel`] 各自
//!   独立，互不转换；`McpAppBindingRegistry` **不暴露** `cancel()`；
//! - [`admission::DispatchCommit`] 不实现 `Clone`，只能被 [`AdmittedPeerTool`] 消费一次；
//! - 因此「关闭 App」与「取消执行」在类型上没有共同 API。
//!
//! 不伪造 turn 事实：idle 执行没有 assistant 消息、没有 `step`、没有 render 事件
//!（设计 §3.2 / §4.1）。

use std::fmt;
use std::fmt::{Debug, Formatter};
pub(crate) mod admission;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::identity::AgentId;
use peri_acp_types::interaction::{AttachmentId, InteractionAttribution};
use peri_acp_types::mcp_apps::{
    InvocationTarget, McpAppsErrorKind, McpAppsRelayError, NotExecutedCause,
    SessionInvocationOutcome, SessionInvocationOutcomeRecord, SessionInvocationPort,
    SessionInvocationRequest, SkippedPostHook,
};
use peri_acp_types::mcp_apps_facade::TargetIdentity;
use peri_acp_types::tool_invocation::{EffectiveToolCall, InvocationId, InvocationOrigin};
use peri_acp_types::tools::{
    BaseTool, EffectiveToolError, EffectiveToolErrorCode, ToolContext, ToolOutput,
};
use peri_agent::agent::stages;
use peri_agent::agent::stages::InvocationContext;
use peri_agent::agent::stages::PostHookSkipSink;
use peri_agent::agent::stages::SessionInvocationSource;
use peri_agent::session::factory::InvocationRuleChain;
use peri_agent::session::queue::MessageQueue;
use peri_agent::session::tool_catalog::{CatalogToolEntry, SessionToolCatalogSnapshot, ToolSource};
use peri_agent::tools::DirectToolInvocationResolver;
use tokio_util::sync::CancellationToken;

use self::admission::{PeerBinding, RevocationKey, SessionAdmissionOwner};
use super::apps::{McpAppBindingRegistry, McpAppRecord};
use super::client::{McpClientHandle, McpClientPool};
use super::tool_bridge::McpToolBridge;

#[cfg(test)]
#[path = "session_invocation_test.rs"]
mod tests;

#[path = "session_invocation/session_invocation_service.rs"]
mod session_invocation_service;
#[cfg(test)]
use session_invocation_service::map_dispatch_error;
pub use session_invocation_service::AdmittedPeerTool;
pub use session_invocation_service::AppDeliveryToken;
pub use session_invocation_service::AttachmentDeliveryToken;
pub use session_invocation_service::InvocationCancel;
pub use session_invocation_service::InvocationOutcomeStore;
pub use session_invocation_service::OutcomeStoreError;
pub use session_invocation_service::SessionInvocationInputs;
pub use session_invocation_service::SessionInvocationService;
pub use session_invocation_service::SessionQueueLookup;
