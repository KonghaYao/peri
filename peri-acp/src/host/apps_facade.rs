//! `SessionAppsFacade`：七个 `peri/mcp/apps/*` 方法的唯一实现（设计 §2.1、§4.4、§8）。
//!
//! 权威归属（本层只做**组合**，不铸造任何身份）：
//! - **附件 / namespace / 纪元**：`SessionInteractionRouter` 分配，本层把返回值
//!   **原样**写进 `AttachmentBinding`（自铸纪元会让 record 与 router 的纪元不一致，
//!   审批路由随即不匹配）；
//! - **序号推进与去重**：[`RequestLedger`]（同 key 只登记一次）；
//! - **执行**：`SessionInvocationPort`（session-owned canonical 入口，与 App 回调
//!   同一管线）；本层不自行调用 MCP peer；
//! - **记录事实**：`McpAppsRelayPort` 的 App facts 面（descriptors / attach 快照 /
//!   资源读取 / rerun 目标）。
//!
//! 错误分工（裁决 D3，收敛成一处）：
//! - **请求本身不合法**（方法不存在、缺 / 错 `sessionId`、会话不存在、连接不是该
//!   会话的在线 endpoint、连接不是该附件的承载连接、非本连接的 Host 域）⇒
//!   [`AcpError`]，kind 稳定、可按键区分；
//! - **业务判决**（§8 六种情况，含 `UnknownNamespace`）⇒ in-band
//!   `RequestVerdict`（同一个 error 不得承载多种事实）；
//! - **执行事实** ⇒ `AppsRequestOutcome`。
//!
//! fail-closed：缺归属 / 无在线 endpoint / 未附着 / 未批准一律**显式拒绝或显式
//! 未执行**；不伪造 turn、不自动批准、不静默降级。「未获批准」与「用户拒绝」由
//! service 的 typed outcome 区分，本层不改写它的结论。
//!
//! entitlement（`SessionAuthorizationPort`）在本层**按能力强制**（R4，方案 b）：六个受控
//! 方法先定位（会话 / 在线 endpoint）再问授权，`grant` + `covers` 任一不成立即拒绝
//! （`EntitlementDenied`）。本仓 stdio 装配**不注入**端口 ⇒ 该面默认拒绝，这是既定
//! 代价（见 `implementation-log.md` 的「已知缺口与移除条件」）。`EntitlementState::Unenforced`
//! 描述的是 record 层（签发点早于任何 App 回调），与本层已强制不矛盾（§8.2）。

use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::interaction::{AttachmentId, ConnectionEpoch};
use peri_acp_types::mcp_apps::{
    InvocationTarget, McpAppsErrorKind, McpAppsRelayPort, SessionInvocationOutcome,
    SessionInvocationPort, SessionInvocationRequest,
};
use peri_acp_types::mcp_apps_facade::{
    AppAttachRequest, AppAttachSnapshot, AppsAttachRequest, AppsAttachResponse, AppsCancelRequest,
    AppsCancelResponse, AppsCloseRequest, AppsCloseResponse, AppsDetachRequest, AppsDetachResponse,
    AppsListRequest, AppsListResponse, AppsOperation, AppsRequestOutcome, AppsRequestRequest,
    AppsRequestResponse, AppsRerunRequest, AttachmentBinding, AttachmentDetachReason,
    DetachVerdict, HostNamespace, McpAppsFeatures, McpAppsLimits, RequestNamespaceId, RequestSeq,
    RequestVerdict, ResourceListRequest, ResourceReadRequest, REQUEST_WAIT_TIMEOUT_MS,
};
use peri_acp_types::session_authorization::{SessionAuthorizationPort, SessionCapability};
use peri_acp_types::tool_invocation::InvocationOrigin;
use serde_json::Value;

use super::apps_ledger::{
    AdmissionVerdict, LedgerBudget, NamespaceKind, NamespaceOpenError, PendingRequest,
    RequestCancelReason, RequestFingerprint, RequestLedger, RequestOperation,
};
use super::interaction_router::{
    AttachRejection, DetachVerdict as RouterDetachVerdict, SessionAttachment,
    SessionInteractionRouter, ViewAttachRequest,
};
use crate::transport::types::AcpError;
use entitlement::SessionEntitlementGate;

pub(crate) use session_apps_facade::apps_error;

pub(crate) use session_apps_facade::SessionAppsFacade;

#[path = "apps_facade_alias.rs"]
mod alias;

/// 结果投影（准入判决 / 未执行 / 资源失败 → 响应形状）：从本文件拆出（`STD-SIZE-001`）。
#[path = "apps_projection.rs"]
mod projection;

/// entitlement 门（R4 消费点）：本文件的子模块，可访问私有字段而不放宽可见性。
#[path = "apps_facade/session_entitlement_gate.rs"]
pub(super) mod session_entitlement_gate;
pub(super) use session_entitlement_gate as entitlement;

#[cfg(test)]
#[path = "apps_facade_test.rs"]
mod tests;

#[path = "apps_facade/session_apps_facade.rs"]
mod session_apps_facade;
