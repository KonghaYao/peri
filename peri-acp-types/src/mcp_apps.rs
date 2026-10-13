//! MCP Apps Stable ACP relay contracts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::interaction::{AttachmentId, UnansweredCause};
use crate::mcp_apps_facade::{
    AppAttachRequest, AppAttachSnapshot, AppDescriptor, AttachmentDetachReason, CloseAppOutcome,
    ResourceList, ResourceListRequest, ResourceReadRequest, TargetIdentity,
};

/// raw 工具结果的权威定义在 `crate::tool_outcome`（保真承载 + typed accessors）；
/// 本模块的 relay envelope 复用同一类型。
pub use crate::tool_outcome::JsonRpcError;
/// 旧名保留，避免同一 wire 概念出现两份定义（权威定义见 `crate::tool_outcome`）。
pub use crate::tool_outcome::RawToolResult as RawCallToolResult;

pub const MCP_APPS_PROTOCOL_VERSION: &str = "2026-01-26";
pub const MCP_APPS_ENVELOPE_VERSION: &str = "1";
/// ACP 侧 Apps facade 的**方法集与判决形状**版本（裁决 D2）。
///
/// 与另外两个数字分工不同，**不得互相推导**：
/// - 本常量：Peri 与 Host 之间的 `peri/mcp/apps/*` facade 契约版本（方法、判决、
///   快照字段）。加方法 / 改判决形状才递增。
/// - [`MCP_APPS_PROTOCOL_VERSION`]：官方 MCP Apps 协议基线日期（`ui/*` 语义）。
/// - [`MCP_APPS_ENVELOPE_VERSION`]：旧 4 方法的请求信封版本（`envelopeVersion`）；
///   属**对外兼容**面，随别名删除一并消失（裁决 D1 的删除条件）。
pub const MCP_APPS_FACADE_VERSION: &str = "2";
pub const MCP_APPS_HTML_MIME: &str = "text/html;profile=mcp-app";
pub const MCP_APPS_ENV: &str = "PERI_MCP_APPS";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAppOpenRequest {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub server_id: String,
    pub tool_name: String,
    pub owner_session_id: String,
    pub invocation_token: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppOpenResponse {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub mcp_protocol_version: String,
    pub server_id: String,
    pub app_session_id: String,
    pub resource_uri: String,
}

/// Host-initiated App tool rerun (`peri/mcp/invoke`).
///
/// Issues a **new** model-equivalent MCP Apps lease; does not consume a
/// historical `invocationToken`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAppInvokeRequest {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub server_id: String,
    pub tool_name: String,
    pub owner_session_id: String,
    #[serde(default)]
    pub arguments: serde_json::Map<String, Value>,
}

// Host 顺序身份**不在本契约上**：进程级静态计数器（旧 `McpAppInvokeRequest::sequence()`）
// 已删除（阶段 4a-S3）。归属事实 `Host{namespace, seq}` 的 `seq` 权威是请求级
// ledger 校验后的 `RequestSeq`（设计 §1.2）；旧 `peri/mcp/invoke` 别名在 ledger
// 接线前由服务侧局部计数，且不得成为跨会话共享的第二权威。

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppInvokeResponse {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub mcp_protocol_version: String,
    pub server_id: String,
    pub tool_call_id: String,
}

/// Host 程序化调用的归属值（`{namespace, seq}`；序号权威在调用方的请求账本）。
///
/// 归属 → 审批 endpoint 的映射见 `TrustedInteractionOwner::Host`：同一对值既描述
/// 执行身份，也描述交互归属（设计 §1.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInvocationRef {
    /// Host 操作域（`apps/list` / `apps/attach` 下发的 namespace）。
    pub namespace: String,
    /// 该域内单调递增的请求序号（本层不解释语义，只转述）。
    pub seq: u64,
}

/// Successful host invoke: ACP `toolCallId` plus projection fields.
#[derive(Debug, Clone, PartialEq)]
pub struct McpAppInvokeOutcome {
    pub mcp_protocol_version: String,
    pub tool_call_id: String,
    pub effective_tool_name: String,
    pub arguments: serde_json::Map<String, Value>,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAppRequest {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub server_id: String,
    pub app_session_id: String,
    pub resource_uri: String,
    pub payload: JsonRpcRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpResourceRequest {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub server_id: String,
    pub app_session_id: String,
    pub resource_uri: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppResponse {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub mcp_protocol_version: String,
    pub server_id: String,
    pub app_session_id: String,
    pub resource_uri: String,
    pub payload: JsonRpcResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceResponse {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub mcp_protocol_version: String,
    pub server_id: String,
    pub resources: Vec<RawResource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: JsonRpcVersion,
    pub id: JsonRpcId,
    pub method: String,
    pub params: CallToolParams,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallToolParams {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    String(String),
    Number(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JsonRpcVersion {
    #[serde(rename = "2.0")]
    V2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcResponse {
    Result(JsonRpcResultResponse),
    Error(JsonRpcErrorResponse),
}

impl JsonRpcResponse {
    pub fn id(&self) -> &JsonRpcId {
        match self {
            Self::Result(response) => &response.id,
            Self::Error(response) => &response.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonRpcResultResponse {
    pub jsonrpc: JsonRpcVersion,
    pub id: JsonRpcId,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: JsonRpcVersion,
    pub id: JsonRpcId,
    pub error: JsonRpcError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawResource {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(rename = "_meta", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl RawResource {
    pub fn is_valid_app_resource(&self, requested_uri: &str) -> bool {
        self.uri == requested_uri
            && self.uri.starts_with("ui://")
            && self.mime_type == MCP_APPS_HTML_MIME
            && self.text.is_some() != self.blob.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAppsErrorKind {
    InvalidEnvelope,
    UnsupportedEnvelopeVersion,
    UnsupportedAppsVersion,
    CapabilityDisabled,
    UnknownServer,
    ServerDisconnected,
    StaleServerGeneration,
    InvalidSession,
    Forbidden,
    ToolNotFound,
    ToolNotAppVisible,
    ResourceNotFound,
    InvalidResource,
    PolicyDenied,
    /// 审批不可用（没有在线 endpoint / 无法征询用户）：与「用户拒绝」不同，
    /// 结果保持**未执行**（设计 §3.4 的 fail-closed 表）。
    ApprovalUnavailable,
    Cancelled,
    UnsupportedMethod,
    UpstreamProtocolError,
    // ── 阶段 4a facade 方法级错误面（裁决 D3 的收敛点）─────────────────────
    /// namespace 不存在 / 已销毁（不得重建同 ID namespace）。
    ///
    /// **收敛点**：`apps/request` / `apps/rerun` 的 §8 判决**不**经本 kind，
    /// 一律走 in-band [`crate::mcp_apps_facade::RequestVerdict::UnknownNamespace`]；
    /// 本 kind 只用于**请求本身不合法**、判决无法 in-band 送达的方法
    /// （如 `apps/attach` 的 namespace 拒绝）。
    UnknownNamespace,
    /// 附件纪元（`connectionEpoch`）或会话运行时世代（incarnation）不再是当前值：
    /// 调用方必须重新 `attach` 取得新纪元。旧纪元的请求 / 旧批准一律失效。
    StaleAttachmentEpoch,
    /// 同一 namespace + seq 的指纹与已登记记录不同（参数 / 目标 / facade 版本不同）。
    Conflict,
    /// 同 key 的结果已按预算回收：**即使无法再比较指纹也绝不重新执行**。
    ResultExpired,
    /// 序号跳跃（跳过了一个或多个未登记的序号）：客户端必须改用期望序号。
    SequenceGap,
    /// 容量不足（尚未登记）：**不消耗序号**，登记未发生。
    CapacityExceeded,
}

impl McpAppsErrorKind {
    /// 稳定的 wire 键（客户端按键区分判决；不得让一个 kind 承载多种事实）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEnvelope => "invalid_envelope",
            Self::UnsupportedEnvelopeVersion => "unsupported_envelope_version",
            Self::UnsupportedAppsVersion => "unsupported_apps_version",
            Self::CapabilityDisabled => "capability_disabled",
            Self::UnknownServer => "unknown_server",
            Self::ServerDisconnected => "server_disconnected",
            Self::StaleServerGeneration => "stale_server_generation",
            Self::InvalidSession => "invalid_session",
            Self::Forbidden => "forbidden",
            Self::ToolNotFound => "tool_not_found",
            Self::ToolNotAppVisible => "tool_not_app_visible",
            Self::ResourceNotFound => "resource_not_found",
            Self::InvalidResource => "invalid_resource",
            Self::PolicyDenied => "policy_denied",
            Self::ApprovalUnavailable => "approval_unavailable",
            Self::Cancelled => "cancelled",
            Self::UnsupportedMethod => "unsupported_method",
            Self::UpstreamProtocolError => "upstream_protocol_error",
            Self::UnknownNamespace => "unknown_namespace",
            Self::StaleAttachmentEpoch => "stale_attachment_epoch",
            Self::Conflict => "conflict",
            Self::ResultExpired => "result_expired",
            Self::SequenceGap => "sequence_gap",
            Self::CapacityExceeded => "capacity_exceeded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("MCP Apps relay request failed")]
pub struct McpAppsRelayError {
    pub kind: McpAppsErrorKind,
}

#[async_trait::async_trait]
pub trait McpAppsRelayPort: Send + Sync {
    fn close_connection(&self, owner_connection_id: &str);
    fn close_session(&self, owner_session_id: &str);

    // ── App facts 面（阶段 4a 设计 §4.1；生产实现 = `PoolMcpAppsRelay`）──────
    //
    // 这里的方法是 façade 的**数据面**：只回答「当前 incarnation 内的 record
    // 事实」，不构成第二条执行链——工具调用一律经 `SessionInvocationPort`。

    /// 本会话环境的 runtime epoch（ledger / 提交复核的构造输入）。
    fn runtime_epoch(&self) -> u64;

    /// 当前 incarnation 内可读的轻量 descriptors。
    ///
    /// 只在**完整结果之后**发布；不读取或恢复旧执行账本（设计 §6）。
    async fn list_apps(&self, session_id: &str) -> Result<Vec<AppDescriptor>, McpAppsRelayError>;

    /// 领取 record 并附着附件：首附 = 领取，重挂 = 纪元递增。
    ///
    /// **不调用任何业务工具**；目标字段由实现侧从 record 导出（设计 §6）。
    /// 纪元取**调用方值**（`request.attachment.connection_epoch` 由 router 分配）。
    async fn attach_app(
        &self,
        request: AppAttachRequest,
    ) -> Result<AppAttachSnapshot, McpAppsRelayError>;

    /// 撤销一个附件的投递资格（`apps/detach` / 连接 retire / `apps/close`）。
    ///
    /// **不取消**已 dispatch 的调用（由 invocation owner 有界收尾）。
    fn detach_attachment(&self, attachment_id: &AttachmentId, reason: AttachmentDetachReason);

    /// 显式结束该 App 的交互资格：撤销 record 与其全部附件。
    ///
    /// 不删除模型历史，也不取消已提交的执行（设计 §5）。
    fn close_app(&self, session_id: &str, app_id: &str) -> CloseAppOutcome;

    /// 资源读取：始终发给 record 绑定的 server（Peri 不自行 fetch URL / 读本机文件）。
    async fn read_resources(
        &self,
        request: ResourceReadRequest,
    ) -> Result<Vec<RawResource>, McpAppsRelayError>;

    /// 资源列表：membership 不是唯一准入依据（按同一资源策略过滤）。
    async fn list_resources(
        &self,
        request: ResourceListRequest,
    ) -> Result<ResourceList, McpAppsRelayError>;

    /// rerun 的目标权威：由 `sourceInvocationRef` 对应的**服务端记录**导出。
    ///
    /// 记录不在保留范围 ⇒ 显式拒绝，**无回落**（绝不按客户端字段执行）。
    fn source_target(
        &self,
        session_id: &str,
        source: &crate::tool_invocation::InvocationId,
    ) -> Result<TargetIdentity, McpAppsRelayError>;

    /// record 身份（`app_id`）对应的 MCP 协议版本，同时复核该 record 的 server
    /// 世代仍在线上。
    ///
    /// 取代旧的 `validate_binding(&AppSessionBinding)`（S8）：绑定复核按**记录身份**
    /// 查询，不再由调用方拼装一份 lease 形状。语义保持：记录不存在 / 已撤销 ⇒
    /// `PolicyDenied`，server 世代不符 ⇒ `StaleServerGeneration`，无连接 ⇒
    /// `ServerDisconnected`，成功返回该连接协商出的 MCP 协议版本字符串。
    async fn binding_protocol_version(&self, app_id: &str) -> Result<String, McpAppsRelayError>;

    /// Host-initiated App tool call: MCP `tools/call` 经 canonical 入口执行。
    ///
    /// 生产路径是旧 `peri/mcp/invoke` 别名（`handle_invoke` → 本方法）。**保留**：
    /// 该 wire 名的对外兼容义务未到期（裁决 D1）；删除条件是
    /// `side-projects/mcp-apps/check-peri.ts` 等客户端切到 `peri/mcp/apps/rerun`
    /// 之后，随旧 4 名一并删除（S8 的第二段）。
    ///
    /// The returned `tool_call_id` is the ACP `toolCallId` / later `invocationToken`.
    ///
    /// `attribution` 是**调用方**给出的 Host 归属值（`{namespace, seq}`）：序号权威是
    /// 调用方的请求账本（`SessionAppsFacade` 的 Host 操作域），本层不得自持计数器
    /// （旧的 `PoolMcpAppsRelay::host_sequence` 已删除：它是服务侧局部第二权威）。
    async fn invoke_app(
        &self,
        request: &McpAppInvokeRequest,
        attribution: HostInvocationRef,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<McpAppInvokeOutcome, McpAppsRelayError>;
}

// ─── SessionInvocationPort：session-owned canonical 调用入口 ────────────────────

/// session 级 canonical 调用入口：App 回调与 Host 程序化调用**共同**的准入面。
///
/// 语义边界（设计 §4.1）：
/// - 实现方持有 session 级事实（pool / record registry / 规则链 / 准入 owner），
///   不持有任何 per-turn 对象（`StageContext`、`MiddlewareChain` 快照、EventBus、
///   pinned catalog）；
/// - 调用经**同一条** canonical 管线（解析 → before-tool 规则 → 批准 → 提交 →
///   执行），不伪造 turn 事实（无 assistant 消息、无 `step`）；
/// - 一切缺失（record 撤销、目标不绑定、无归属、无在线 endpoint）都是
///   **显式未执行**（[`SessionInvocationOutcome::NotExecuted`]），不是静默成功、
///   不是自动批准、也不伪装成「用户拒绝」。
///
/// `entitlement`（[`crate::session_authorization::SessionAuthorizationPort`]）在
/// 本阶段**不被消费**：凭据层属 SDK 部署方（设计 §4.3 裁决 R4）。本类型不因此
/// 弱化既有的连接绑定 / record 撤销校验。
#[async_trait::async_trait]
pub trait SessionInvocationPort: Send + Sync {
    /// 执行一次 App / Host 发起的 canonical 工具调用。
    ///
    /// 返回 `Ok(NotExecuted{..})` 表示**显式未执行**（准入拒绝，零 wire 调用）；
    /// `Err` 仅用于请求无法作为一次调用被受理的情形（会话 / 信封不匹配）。
    async fn invoke(
        &self,
        request: SessionInvocationRequest,
    ) -> Result<SessionInvocationOutcome, McpAppsRelayError>;

    /// 会话关闭：撤销本会话全部 record 与交付令牌，并做有界收尾。
    fn session_closing(&self);

    /// 撤销一个 App record（`apps/close`）；已 dispatch 的调用不受影响。
    fn revoke_app(&self, app_id: &str);

    /// 撤销一个连接附件（连接关闭 / detach）；已 dispatch 的调用不受影响。
    fn revoke_attachment(&self, attachment_id: &AttachmentId);

    /// rerun 的目标权威：由 `sourceInvocationRef` 对应的**服务端 outcome 记录**导出。
    ///
    /// - 会话不符 / 记录不在保留范围 / 该次调用没有绑定目标 ⇒ `None`：调用方必须
    ///   显式拒绝（`apps/rerun` 的判决），**绝不**回落客户端字段（裁决 D4）；
    /// - 只导出**目标身份**，不授权执行：rerun 仍要经 [`Self::invoke`] 重新准入。
    fn source_target(
        &self,
        session_id: &str,
        source: &crate::tool_invocation::InvocationId,
    ) -> Option<TargetIdentity>;
}

/// 一次 session-owned canonical 调用的请求。
#[derive(Debug, Clone)]
pub struct SessionInvocationRequest {
    /// 调用所属会话（实现方据此拒绝跨会话冒用）。
    pub session_id: String,
    /// 调用方观察到的 runtime epoch：与服务实例纪元不一致即拒绝（环境已换代）。
    pub runtime_epoch: u64,
    /// typed 执行来源：`App{..}` 或 `Host{..}`。模型 / 嵌套来源不属本入口。
    pub origin: crate::tool_invocation::InvocationOrigin,
    /// 调用目标（App record / Host server+tool）。
    pub target: InvocationTarget,
    /// 请求作用域的取消令牌。实现方只把它当作**准入前的放弃信号**：不得把它接线
    /// 成已提交调用的取消源（关闭 App / 连接不得中断已 dispatch 的调用）。
    pub cancel: tokio_util::sync::CancellationToken,
    /// 调用方希望用于回执关联的执行身份（Host 面 `toolCallId` 需要稳定值）。
    ///
    /// 只作回执关联：**不参与任何安全裁决**（准入、撤销、世代判定一律走服务端事实）。
    /// `None` = 服务端生成（App 面回调不需要回执身份）。
    pub invocation_id: Option<crate::tool_invocation::InvocationId>,
}

/// session-owned canonical 调用的目标。
#[derive(Debug, Clone, PartialEq)]
pub enum InvocationTarget {
    /// App 回调：record 身份 + App 面工具名（经 record 的 `allowed_tools` 映射）+ 参数。
    App {
        /// record 身份（= App 侧看到的 `appSessionId`）。
        record_id: String,
        /// App 面工具名（JSON-RPC `params.name`，不是 effective 名）。
        tool_name: String,
        /// 调用参数。
        arguments: serde_json::Map<String, Value>,
    },
    /// Host 程序化调用：server + wire 工具名 + 参数。
    Host {
        /// MCP server 身份。
        server_id: String,
        /// wire 工具名（服务器声明的原始名，不是 effective 名）。
        tool_name: String,
        /// 调用参数。
        arguments: serde_json::Map<String, Value>,
    },
}

/// 一次调用的结果事实（对齐设计 §5 状态表）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SessionInvocationOutcome {
    /// 远端已报告结果（`isError: true` 同样是结果事实，不折叠成错误）。
    Completed(RawCallToolResult),
    /// 远端 JSON-RPC 协议错误（不是工具结果）。
    RemoteProtocolError(JsonRpcError),
    /// 服务端接受为异步任务，本次未产出最终结果。
    TaskAccepted(TaskReceipt),
    /// 已发出调用但结果不可知（如成功返回却无可承载的 raw 载荷）：不得当成成功。
    OutcomeUnknown {
        /// 不可知原因的简短说明（不含外部自由文本）。
        reason: String,
    },
    /// **显式未执行**：准入在 wire 调用之前拒绝，零副作用。
    NotExecuted {
        /// 未执行的原因（typed）。
        cause: NotExecutedCause,
    },
}

impl SessionInvocationOutcome {
    /// typed outcome → 错误种类投影（**单一权威**：App 面回调、Host 调用与旧别名
    /// 的 wire 投影都经它，不伪造结果，也不把「未获批准」伪装成「用户拒绝」）。
    ///
    /// `Completed` 也是结果事实（`isError:true` 同样如此）：本投影只在结果**无法**
    /// 用结果形状承载时使用（旧别名 / 错误面），不改变 outcome 记录本身。
    pub fn error_kind(&self) -> McpAppsErrorKind {
        match self {
            Self::Completed(_)
            | Self::RemoteProtocolError(_)
            | Self::TaskAccepted(_)
            | Self::OutcomeUnknown { .. } => McpAppsErrorKind::UpstreamProtocolError,
            Self::NotExecuted { cause } => match cause {
                NotExecutedCause::CancelledBeforeCommit => McpAppsErrorKind::Cancelled,
                // 审批不可用（含「无人可答」的全部原因）：与「用户拒绝」不同，
                // 结果保持未执行（不伪装成拒绝）。
                NotExecutedCause::ApprovalUnavailable(_) => McpAppsErrorKind::ApprovalUnavailable,
                NotExecutedCause::UserRejected | NotExecutedCause::PolicyDenied => {
                    McpAppsErrorKind::PolicyDenied
                }
                NotExecutedCause::TargetRevoked
                | NotExecutedCause::TargetUnbound
                | NotExecutedCause::UntrustedOwner
                | NotExecutedCause::CommitConsumed => McpAppsErrorKind::PolicyDenied,
            },
        }
    }
}

/// 未执行的原因（fail-closed 表，设计 §3.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "cause")]
pub enum NotExecutedCause {
    /// record 不存在 / 已撤销 / 服务端换代。
    TargetRevoked,
    /// 工具不在 record 的 `allowed_tools` 或不在本会话 app-visible 视图。
    TargetUnbound,
    /// 无可信归属（缺 session / turn / 附件事实）。
    UntrustedOwner,
    /// 审批不可用（无在线 endpoint / epoch 过期 / 客户端不能作答）。
    ///
    /// **不是**用户拒绝：实现方不得把它当作已授权，也不得投影成用户拒绝。
    ApprovalUnavailable(UnansweredCause),
    /// 用户在准入前明确拒绝执行该调用。
    ///
    /// 与 [`Self::ApprovalUnavailable`] 严格区分：用户拒绝是**已作出的决定**，
    /// 审批不可用是**没有人可以决定**。
    UserRejected,
    /// 策略层面显式拒绝（审批中间件在**不允许请求审批**的模式下拒绝、或目标
    /// 在策略上不可投递）。
    ///
    /// 与 [`Self::TargetRevoked`] 严格区分：目标仍然存在且未撤销，是**当前策略**
    /// 不许执行；也与 [`Self::UserRejected`] 不同——没有用户决定发生。
    PolicyDenied,
    /// 该次提交的一次性 wire 资格已被消耗（同一冻结目标被重复投递）。
    ///
    /// 正常管线不会产生本原因：提交资格由 `AdmittedPeerTool` 消费一次，
    /// 重复调用是内部一致性错误，必须显式暴露而不是再次发出 wire 调用。
    CommitConsumed,
    /// 提交前被取消 / 放弃（请求令牌已取消）。
    CancelledBeforeCommit,
}

/// MCP 任务回执（`tools/call` 返回任务而非结果）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskReceipt {
    /// 服务端任务 ID。
    pub task_id: String,
    /// 服务端报告的任务状态（原样转述，不做解释）。
    pub status: String,
}

/// 无活跃 turn 时被**显式跳过**的、需要 turn 的事后钩子（裁决 R2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkippedPostHook {
    /// `after_tool`：需要真实 turn 事实，idle 执行下不可用。
    AfterToolNotAvailableWithoutTurn,
    /// `on_error`：同上。
    OnErrorNotAvailableWithoutTurn,
}

impl SkippedPostHook {
    /// 稳定标签（日志与 outcome 记录用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AfterToolNotAvailableWithoutTurn => "after_tool:not_available_without_turn",
            Self::OnErrorNotAvailableWithoutTurn => "on_error:not_available_without_turn",
        }
    }
}

/// outcome 存储中的一条记录：结果事实 + 事后钩子跳过事实（R2）+ 目标身份（D4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInvocationOutcomeRecord {
    /// 本次调用的执行身份。
    pub invocation_id: crate::tool_invocation::InvocationId,
    /// 本次调用**已绑定**的目标身份（rerun 的唯一权威来源）。
    ///
    /// `None` = 该次调用在绑定目标之前就被拒（未执行，也没有可导出的目标）；
    /// 绝不回落到客户端自述的 server / tool（裁决 D4）。
    pub target: Option<TargetIdentity>,
    /// 结果事实。
    pub outcome: SessionInvocationOutcome,
    /// 无 turn 时被显式跳过的事后钩子（不静默、不改写已完成结果）。
    pub skipped_post_hooks: Vec<SkippedPostHook>,
}

#[cfg(test)]
#[path = "mcp_apps_test.rs"]
mod tests;
