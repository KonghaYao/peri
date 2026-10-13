//! MCP Apps ACP facade 契约（阶段 4a）：七个 `peri/mcp/apps/*` 方法的 wire 形状、
//! 身份类型、判决与能力声明。
//!
//! 分工（设计 §6 与 4a 设计 §1.2）：
//! - 本模块只放**跨层契约**：`peri-acp`（facade / router / ledger）与
//!   `peri-middlewares`（App facts 实现）共用同一份类型；
//! - **序号与 namespace 的分配权威**是 `peri-acp` 的 `SessionAppsFacade`，
//!   **推进权威**是 `RequestLedger`（两者都只经本模块的类型通信）；
//! - **判决权威**：设计 §8 的六种请求情况一律走 in-band [`RequestVerdict`]
//!   （裁决 D3）；[`crate::mcp_apps::McpAppsErrorKind`] 只承载「请求本身不合法」
//!   （方法不存在、缺 `sessionId`、信封/版本不符、会话不存在、连接不是该会话的
//!   在线 endpoint），两者不得互为兜底。
//!
//! 版本分工（三者不得互相推导，裁决 D2）：
//! - [`crate::mcp_apps::MCP_APPS_FACADE_VERSION`] —— ACP 侧 facade 的**方法集与判决形状**版本；
//! - [`crate::mcp_apps::MCP_APPS_PROTOCOL_VERSION`] —— 官方 MCP Apps 协议基线日期；
//! - [`crate::mcp_apps::MCP_APPS_ENVELOPE_VERSION`] —— 旧 4 方法的信封版本。

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::interaction::{AttachmentId, ConnectionEpoch};
use crate::mcp_apps::{RawCallToolResult, RawResource, SessionInvocationOutcome};
use crate::tool_invocation::{InvocationId, ToolCallId};

pub use crate::mcp_apps::MCP_APPS_FACADE_VERSION;

// ─── §4.3 预算常量（单一权威）────────────────────────────────────────────────
//
// **4a 保守值，4b 依实测调参**：调参只改本节的常量，不得在调用点重复字面量。
// 所有新增内存结构都必须由本节的上限约束（设计 §9：有界，不新增数据库 / 文件缓存）。

/// 单会话最多同时存活的 App record 数。
pub const MAX_APPS_PER_SESSION: u64 = 64;
/// 单 record 最多附件数（多 tab / 多 View）。
pub const MAX_ATTACHMENTS_PER_APP: u64 = 8;
/// 单会话最多附件数（跨 record 合计）。
pub const MAX_ATTACHMENTS_PER_SESSION: u64 = 64;
/// 单 namespace 最多未决请求数（pending 不淘汰，但有上限）。
pub const MAX_PENDING_PER_NAMESPACE: u64 = 32;
/// 单会话最多 namespace 数（Attachment 与 Host 各自一个）。
pub const MAX_NAMESPACES_PER_SESSION: u64 = 64;
/// 已完成结果保留条数上限（按条数回收）。
pub const MAX_RETAINED_RESULTS: u64 = 256;
/// 单条结果字节上限（超过即显式不可用，不做 UI 内容缓存）。
pub const MAX_RESULT_BYTES: u64 = 1024 * 1024;
/// 已完成结果retained 总字节上限。
pub const MAX_RETAINED_BYTES: u64 = 8 * 1024 * 1024;
/// detach 后的重挂接窗口（毫秒）。到期后该附件不可再挂接。
pub const DETACH_REATTACH_WINDOW_MS: u64 = 300_000;
/// 单次 `apps/request` 等待终态的上限（毫秒）。
pub const REQUEST_WAIT_TIMEOUT_MS: u64 = 30_000;
/// App record 的保留期（毫秒）；到期即终态撤销。
pub const APP_RETENTION_MS: u64 = 1_800_000;

// ─── §1.1 #6：请求域身份 ────────────────────────────────────────────────────

/// 请求 namespace：Attachment 请求域或可信 Host 操作域的有界去重键。
///
/// - **只定位，不是授权凭证**（设计 §5）：知道 ID 不能获得执行资格；
/// - 与 `{session_id, incarnation, kind}` 绑定且只增不减；销毁后**不得**以同 ID
///   重建（重建后同 ID 请求 ⇒ [`RequestVerdict::UnknownNamespace`]）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestNamespaceId(String);

impl RequestNamespaceId {
    /// 分配一个**永不复用**的新 namespace（UUID v7；分配权威是 `SessionAppsFacade`）。
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RequestNamespaceId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<String> for RequestNamespaceId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for RequestNamespaceId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for RequestNamespaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 请求序号：同一 namespace 内单调、不复用的业务身份。
///
/// 合法性由 [`RequestLedger`] 裁定：只有 `seq == last_accepted + 1` 才登记，
/// 其余按下表判决（陈旧 / 冲突 / 已回收 / 跳跃 / 容量）。传输重试必须复用原序号。
///
/// [`RequestLedger`]: ../../peri_acp/host/apps_ledger/struct.RequestLedger.html
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestSeq(u64);

impl RequestSeq {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// 下一个序号（`last_accepted + 1`）。饱和递增，不回绕。
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl std::fmt::Display for RequestSeq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 客户端页面（View）身份：只在**已认证的会话/客户端作用域内**定位页面，
/// 不是恢复权限，也不是授权凭证（设计 §6）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientViewId(String);

impl ClientViewId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ClientViewId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ─── §1.1 #2/#7：App 资格、执行状态与 descriptor ─────────────────────────────

/// App record 的**资格**：它能不能回调工具。
///
/// 展示资格、工具调用资格、资源读取资格**分别判断**（设计 §3.1 [P4]）：
/// - `Interactive` 只承诺「本 App 可经 canonical 入口回调这些工具」，不承诺任何
///   目标仍然有效（每次调用仍要做会话目录 / 撤销 / 世代复核）；
/// - `DisplayOnly` 只承诺「可显示」，**零 callback 资格**（裁决 D5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AppQualification {
    /// 可交互：`allowed_tools` 是 App 面工具名 → effective 工具名的**有界**映射。
    Interactive {
        /// App 面工具名 → effective 工具名。构造点必须同时保证它**有界**。
        allowed_tools: HashMap<String, String>,
    },
    /// 只可显示：模型可见且带 UI URI 的工具所签发的记录（不可回调）。
    DisplayOnly,
}

impl AppQualification {
    /// 可回调的工具白名单（`DisplayOnly` 恒为空）。
    pub fn allowed_tools(&self) -> Option<&HashMap<String, String>> {
        match self {
            Self::Interactive { allowed_tools } => Some(allowed_tools),
            Self::DisplayOnly => None,
        }
    }

    /// App 面工具名 → effective 工具名；`DisplayOnly` 恒 `None`。
    pub fn allowed_effective_name(&self, wire_name: &str) -> Option<&String> {
        self.allowed_tools()
            .and_then(|allowed| allowed.get(wire_name))
    }
}

/// descriptor 的**执行状态**（当前 incarnation 内的可执行性投影）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum AppExecutionState {
    /// 当前 incarnation 内可执行。
    Live,
    /// 绑定的 server 世代已不是当前世代（`bound` = 记录的值，`observed` = 池的当前值）。
    GenerationUnavailable {
        /// 记录绑定的世代。
        bound: u64,
        /// 池当前观察到的世代。
        observed: u64,
    },
    /// 已终态撤销（`apps/close` / session close / incarnation 换代 / 保留到期）。
    Closed,
}

/// 模型关联：初始展示事实与模型侧调用（卡片）的对应关系。
///
/// `invocation_id` 是**签发时**那一次实例化执行的身份（与
/// [`AppDescriptor`] 之外的执行身份同源），`tool_call_id` 只用于模型卡片关联，
/// 不承担 App callback 的请求索引（设计 §5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAssociation {
    pub invocation_id: InvocationId,
    /// 模型 tool-call ID；`None` = 非模型来源（嵌套 / Host 发起的实例化）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
}

/// 可信 UI 资源引用（**引用**，不是内容；4a 不做 UI 内容缓存）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiResourceRef {
    pub uri: String,
    pub mime_type: String,
}

/// descriptor 里的附件摘要。
///
/// **不含 `nextSeq`**：序号推进权威是 ledger，摘要只提供归属定位与纪元。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentSummary {
    pub attachment_id: AttachmentId,
    pub namespace: RequestNamespaceId,
    pub connection_epoch: ConnectionEpoch,
    /// 当前承载连接；`None` = 已 detach（有界窗口内可重挂接）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    pub client_view_id: ClientViewId,
}

/// App 的轻量描述（`apps/list` 的列表项）。
///
/// 只在**完整结果后**发布（设计 §6）：没有 running snapshot / partial input 面。
/// 不携带 raw 结果（raw 只经 `apps/attach` 给出）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppDescriptor {
    /// record 身份（= App 侧 `appId`）。
    pub app_id: String,
    pub server_id: String,
    pub server_generation: u64,
    pub resource_uri: String,
    pub instantiating_tool: String,
    pub qualification: AppQualification,
    pub model: ModelAssociation,
    pub execution_state: AppExecutionState,
    pub attachments: Vec<AttachmentSummary>,
    /// 保留期剩余毫秒（到期即终态撤销）。
    pub retention_remaining_ms: u64,
}

// ─── §1.1 #8：附件绑定与 attach 快照 ────────────────────────────────────────

/// 附件绑定：一个 View 的稳定请求域身份 + 当前连接纪元 + **下一个可用序号**。
///
/// `next_seq` 的推进权威是 ledger（构造本值时读取 ledger 的 `last_accepted + 1`）；
/// 附着时保存的快照不构成第二权威。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentBinding {
    pub attachment_id: AttachmentId,
    pub namespace: RequestNamespaceId,
    pub connection_epoch: ConnectionEpoch,
    pub next_seq: RequestSeq,
}

impl AttachmentBinding {
    /// 用**当前** ledger 序号重建绑定（附着快照与 attach 响应用同一权威）。
    pub fn with_next_seq(&self, next_seq: RequestSeq) -> Self {
        Self {
            attachment_id: self.attachment_id.clone(),
            namespace: self.namespace.clone(),
            connection_epoch: self.connection_epoch,
            next_seq,
        }
    }
}

/// `apps/attach` 的完整初始快照。
///
/// 只在**完整结果之后**给出：`initial_result` 含 `isError:true` 的 Completed 事实，
/// 不做「先展示再补齐」。attach **不调用任何业务工具**（设计 §6）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppAttachSnapshot {
    pub descriptor: AppDescriptor,
    pub attachment: AttachmentBinding,
    pub ui_resource: UiResourceRef,
    /// 实际（审批后 effective）参数，不是 submitted 参数。
    pub initial_input: Map<String, Value>,
    /// 完整初始 raw 结果（保真：content 多模态 / structuredContent / `_meta` / isError）。
    pub initial_result: RawCallToolResult,
    pub features: McpAppsFeatures,
    pub limits: McpAppsLimits,
    /// 本次是重挂接（该 record 此前已有附件）。
    pub reattached: bool,
}

// ─── §1.1 #9：能力声明 ──────────────────────────────────────────────────────

/// 后端**已实现**能力集。
///
/// 「不得声称未实现能力」在类型上强制：四项未实现能力是**私有字段**，任何构造点
/// 都无法把它们置真，只能经 [`Self::implemented`] 取值（裁决 D2 附加要求）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppsFeatures {
    /// `tools/call`（App 回调经 canonical 入口执行）。
    pub tools_call: bool,
    /// `resources/read`。
    pub resources_read: bool,
    /// `resources/list`（仅绑定 server；membership 不是唯一准入依据）。
    pub resources_list: bool,
    /// `apps/cancel`。
    pub cancel: bool,
    /// `apps/detach` + 有界窗口内重挂接。
    pub detach_reattach: bool,
    /// `apps/rerun`（Host 发起的用户明确新意图；同 key 重传不重复执行）。
    pub rerun: bool,
    /// 工具列表变更通知：通知链未接线，4a 恒 false。
    tool_list_changed: bool,
    /// 资源订阅：无实现，恒 false。
    resource_subscriptions: bool,
    /// sampling：无实现，恒 false。
    sampling: bool,
    /// logging：无实现，恒 false。
    logging: bool,
}

impl McpAppsFeatures {
    /// 已实现能力的**唯一权威**。
    pub const fn implemented() -> Self {
        Self {
            tools_call: true,
            resources_read: true,
            resources_list: true,
            cancel: true,
            detach_reattach: true,
            rerun: true,
            tool_list_changed: false,
            resource_subscriptions: false,
            sampling: false,
            logging: false,
        }
    }

    pub const fn tool_list_changed(&self) -> bool {
        self.tool_list_changed
    }

    pub const fn resource_subscriptions(&self) -> bool {
        self.resource_subscriptions
    }

    pub const fn sampling(&self) -> bool {
        self.sampling
    }

    pub const fn logging(&self) -> bool {
        self.logging
    }
}

impl Default for McpAppsFeatures {
    fn default() -> Self {
        Self::implemented()
    }
}

/// 会话侧实际生效的**预算**（与 §4.3 常量同源；客户端据此自我约束）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppsLimits {
    pub max_apps_per_session: u64,
    pub max_attachments_per_app: u64,
    pub max_attachments_per_session: u64,
    pub max_pending_per_namespace: u64,
    pub max_namespaces_per_session: u64,
    pub max_retained_results: u64,
    pub max_result_bytes: u64,
    pub max_retained_bytes: u64,
    pub detach_reattach_window_ms: u64,
    pub request_wait_timeout_ms: u64,
    pub app_retention_ms: u64,
}

impl McpAppsLimits {
    /// 实际值：直接取 §4.3 常量（**4a 保守值，4b 依实测调参**）。
    pub const fn conservative() -> Self {
        Self {
            max_apps_per_session: MAX_APPS_PER_SESSION,
            max_attachments_per_app: MAX_ATTACHMENTS_PER_APP,
            max_attachments_per_session: MAX_ATTACHMENTS_PER_SESSION,
            max_pending_per_namespace: MAX_PENDING_PER_NAMESPACE,
            max_namespaces_per_session: MAX_NAMESPACES_PER_SESSION,
            max_retained_results: MAX_RETAINED_RESULTS,
            max_result_bytes: MAX_RESULT_BYTES,
            max_retained_bytes: MAX_RETAINED_BYTES,
            detach_reattach_window_ms: DETACH_REATTACH_WINDOW_MS,
            request_wait_timeout_ms: REQUEST_WAIT_TIMEOUT_MS,
            app_retention_ms: APP_RETENTION_MS,
        }
    }
}

impl Default for McpAppsLimits {
    fn default() -> Self {
        Self::conservative()
    }
}

/// 后端能力声明（ACP 初始化 `_meta` / `apps/list` / `apps/attach` 共用一份形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppsCapability {
    pub facade_version: String,
    pub apps_protocol_version: String,
    pub mime_types: Vec<String>,
    pub backend_features: McpAppsFeatures,
    pub limits: McpAppsLimits,
}

impl McpAppsCapability {
    /// 后端已授予能力的**唯一构造入口**：features 恒为 [`McpAppsFeatures::implemented`]。
    pub fn backend(mime_types: Vec<String>) -> Self {
        Self {
            facade_version: MCP_APPS_FACADE_VERSION.to_string(),
            apps_protocol_version: crate::mcp_apps::MCP_APPS_PROTOCOL_VERSION.to_string(),
            mime_types,
            backend_features: McpAppsFeatures::implemented(),
            limits: McpAppsLimits::conservative(),
        }
    }
}

// ─── §1.1 #11：判决与请求结果 ───────────────────────────────────────────────

/// 一次 `apps/request` / `apps/rerun` 的业务判决。
///
/// **本类型是设计 §8 六种情况的唯一权威**（裁决 D3）：`UnknownNamespace` 只出现在
/// 这里，不得同时作为 `AcpError` 的独立分支（否则一处事实有两处权威）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RequestVerdict {
    /// 新请求已原子登记（指纹已存），可以执行。
    Accepted,
    /// 同 key 的请求已在执行：加入同一等待，取得同一结果（不重复执行 / 审批）。
    JoinedPending,
    /// 同 key 已有终态结果：返回同一结果。
    Duplicate,
    /// 同 key 但指纹不同（参数 / 目标 / facade 版本不同）：拒绝，不执行。
    Conflict,
    /// 同 key 的结果已被回收：即使无法再比较指纹也**绝不**当作新执行。
    ResultExpired,
    /// 序号跳跃：要求客户端改用 `expected`。
    SequenceGap {
        /// 期望的下一个序号。
        expected: RequestSeq,
    },
    /// 容量不足且尚未登记：**不消耗序号**。
    CapacityRejected,
    /// namespace 不存在 / 已销毁：不得当作新执行，也不得重建同 ID namespace。
    UnknownNamespace,
}

impl RequestVerdict {
    /// 判决是否允许执行（`Accepted` 之外一律不执行）。
    pub const fn is_admitted(&self) -> bool {
        matches!(self, Self::Accepted)
    }

    /// in-band 判决 → 错误面的**投影**（只在一处判决无法 in-band 送达时使用，
    /// 例如 `apps/attach` 的纪元/namespace 拒绝）。
    ///
    /// 这是投影而不是第二权威：判决本身仍以 [`RequestVerdict`] 为准。
    pub const fn error_kind(&self) -> Option<crate::mcp_apps::McpAppsErrorKind> {
        use crate::mcp_apps::McpAppsErrorKind as Kind;
        match self {
            Self::Accepted | Self::JoinedPending | Self::Duplicate => None,
            Self::Conflict => Some(Kind::Conflict),
            Self::ResultExpired => Some(Kind::ResultExpired),
            Self::SequenceGap { .. } => Some(Kind::SequenceGap),
            Self::CapacityRejected => Some(Kind::CapacityExceeded),
            Self::UnknownNamespace => Some(Kind::UnknownNamespace),
        }
    }
}

/// 请求的**执行事实**（判决之外的业务结果）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppsRequestOutcome {
    /// 已登记但尚未产出终态（客户端可用同 key 有界等待，不再新增 status 接口）。
    Pending,
    /// `tools/call` 的调用事实（含 `isError:true` 的 Completed）。
    ToolsCall(SessionInvocationOutcome),
    /// `resources/read` 的全部 contents（保真，不做 UI 文档以外的过滤）。
    ResourcesRead { resources: Vec<RawResource> },
    /// `resources/list` 的结果与游标。
    ResourcesList {
        resources: Vec<RawResourceDescriptor>,
        #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
        next_cursor: Option<String>,
    },
}

// ─── §2.1：七个 `peri/mcp/apps/*` 方法 ──────────────────────────────────────
//
// wire 约定与既有 DTO 一致：**请求** `deny_unknown_fields`（不吞客户端拼错的字段），
// **响应**不 deny（可追加字段，客户端不得据此推断能力）。

/// 客户端可用的 Host 操作域（`apps/rerun` 的序号权威在本 namespace）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostNamespace {
    pub namespace: RequestNamespaceId,
    pub next_seq: RequestSeq,
}

/// `peri/mcp/apps/list` 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppsListRequest {
    /// 会话定位符（**只定位**：连接还必须在该会话的在线 endpoint 表中）。
    pub session_id: String,
}

/// `peri/mcp/apps/list` 响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppsListResponse {
    pub descriptors: Vec<AppDescriptor>,
    pub features: McpAppsFeatures,
    pub limits: McpAppsLimits,
    /// 本连接的 Host 操作域（`apps/rerun` 用它 + `seq`）。
    pub host_namespace: HostNamespace,
}

/// 重挂接时客户端**观察到**的旧附件事实（用于校验，不作为授权凭证）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppsReattachRef {
    pub attachment_id: AttachmentId,
    pub epoch: ConnectionEpoch,
}

/// `peri/mcp/apps/attach` 请求（首附 = 领取 record；重挂 = 纪元递增）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppsAttachRequest {
    pub session_id: String,
    pub app_id: String,
    /// 只在已认证的会话/客户端作用域内定位页面。
    pub client_view_id: ClientViewId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reattach: Option<AppsReattachRef>,
}

/// `peri/mcp/apps/attach` 响应。
///
/// `attachment` 与 `snapshot.attachment` 是**同一个值**（facade 构造一次），
/// 二者同源同权。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppsAttachResponse {
    pub attachment: AttachmentBinding,
    pub snapshot: AppAttachSnapshot,
    pub features: McpAppsFeatures,
    pub limits: McpAppsLimits,
}

/// `apps/request` 的 typed 操作：只接受声明过的三种（不内嵌 JSON-RPC）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum AppsOperation {
    /// 工具回调（App 面工具名，不是 effective 名）。
    #[serde(rename = "tools/call")]
    ToolsCall {
        name: String,
        #[serde(default)]
        arguments: Map<String, Value>,
    },
    /// 资源读取（UI 文档校验 `ui://` + HTML MIME 在实现侧保留）。
    #[serde(rename = "resources/read")]
    ResourcesRead { uri: String },
    /// 资源列表。
    #[serde(rename = "resources/list")]
    ResourcesList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
}

/// `peri/mcp/apps/request` 请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppsRequestRequest {
    pub session_id: String,
    pub attachment_id: AttachmentId,
    pub epoch: ConnectionEpoch,
    pub seq: RequestSeq,
    pub operation: AppsOperation,
}

/// `peri/mcp/apps/request` / `peri/mcp/apps/rerun` 响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppsRequestResponse {
    pub verdict: RequestVerdict,
    /// 是否已登记（`Accepted`）。执行可在受限并发下继续。
    pub admitted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AppsRequestOutcome>,
    /// 客户端下一个可用序号（ledger 的当前推进值）。
    pub next_seq: RequestSeq,
}

/// `apps/cancel` 的判决（按键区分三种事实；`accepted` 不代表远端已回滚）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CancelVerdict {
    /// 命中了一个未决请求并登记了取消（准入前放弃信号）。
    Accepted,
    /// 该次操作已经终结（结果照常可取）。
    AlreadyFinished,
    /// 该 namespace + seq 无记录（从未登记或已被回收）。
    Unknown,
}

/// `peri/mcp/apps/cancel` 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppsCancelRequest {
    pub session_id: String,
    pub attachment_id: AttachmentId,
    pub epoch: ConnectionEpoch,
    pub seq: RequestSeq,
}

/// `peri/mcp/apps/cancel` 响应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppsCancelResponse {
    pub verdict: CancelVerdict,
}

/// `apps/detach` 的判决（幂等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DetachVerdict {
    /// 本次调用撤销了该附件的投递资格（有界窗口内可重挂接）。
    Detached,
    /// 该附件此前已 detach（幂等）。
    AlreadyDetached,
    /// 该附件不属于本会话 / 不可寻址。
    UnknownAttachment,
}

/// `peri/mcp/apps/detach` 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppsDetachRequest {
    pub session_id: String,
    pub attachment_id: AttachmentId,
    pub epoch: ConnectionEpoch,
}

/// `peri/mcp/apps/detach` 响应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppsDetachResponse {
    pub verdict: DetachVerdict,
}

/// `apps/close` 的判决。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloseVerdict {
    /// 本次调用结束了该 App 的交互资格（不删除模型历史）。
    Closed,
    /// 该 App 此前已关闭（幂等）。
    AlreadyClosed,
}

/// `peri/mcp/apps/close` 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppsCloseRequest {
    pub session_id: String,
    pub app_id: String,
}

/// `peri/mcp/apps/close` 响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppsCloseResponse {
    pub verdict: CloseVerdict,
    /// 随本次 close 一并结束的附件（可观测的收尾事实）。
    pub detached_attachments: Vec<AttachmentId>,
}

/// `peri/mcp/apps/rerun` 请求（新用户意图：新 key 必须重新准入 + 审批）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppsRerunRequest {
    pub session_id: String,
    /// Host 操作域（`apps/list` / `apps/attach` 下发）。
    pub host_namespace: RequestNamespaceId,
    pub seq: RequestSeq,
    /// 目标权威来源：服务端**记录**里的那一次调用；客户端不提供 server/tool。
    pub source_invocation_ref: InvocationId,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

// ─── §4.1：App facts 面（`McpAppsRelayPort` 的入参 / 出参）───────────────────

/// `attach_app` 入参。
///
/// - 附件绑定由**调用方（router/ledger）**给出（纪元权威在 router）；
/// - `owner_connection_id` 是承载该 View 的**可信连接**（facade 从 ACP 请求得到，
///   不是客户端字段）：没有它就无法建立附件→连接的交付归属（§4.1 签名草稿漏列，
///   实现时必须显式）；
/// - 目标字段（server / tool / URI）不来自客户端，也不来自本结构：实现侧从
///   record 导出（设计 §6）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppAttachRequest {
    pub session_id: String,
    pub app_id: String,
    /// 承载该 View 的可信连接身份。
    pub owner_connection_id: String,
    pub attachment: AttachmentBinding,
    pub client_view_id: ClientViewId,
}

/// 附件 detach 的原因（只用于日志与归属，不改变执行事实）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentDetachReason {
    /// 页面卸载 / `apps/detach`。
    ViewDetached,
    /// 重挂接：旧附件被新纪元替换。
    Reattached,
    /// 承载连接被 retire。
    ConnectionRetired,
    /// `apps/close`。
    AppClosed,
}

/// `close_app` 的结果（幂等：已关闭同样返回事实）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseAppOutcome {
    pub verdict: CloseVerdict,
    pub detached_attachments: Vec<AttachmentId>,
}

/// `read_resources` 入参（目标 server / URI 由实现侧从 record 导出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceReadRequest {
    pub session_id: String,
    pub app_id: String,
    pub attachment_id: AttachmentId,
    pub epoch: ConnectionEpoch,
    pub uri: String,
}

/// `list_resources` 入参（`cursor` 只对 list 有意义，故与 read 分型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceListRequest {
    pub session_id: String,
    pub app_id: String,
    pub attachment_id: AttachmentId,
    pub epoch: ConnectionEpoch,
    pub cursor: Option<String>,
}

/// 资源**目录项**（`resources/list` 的成员）：保真承载 server 声明。
///
/// 与 [`RawResource`]（`resources/read` 返回的 contents）不是同一事实：目录项可以
/// 没有 `mimeType`，并带有 name / description 等描述字段。未知扩展字段经
/// `extra` 保真（不得声称无损而实际丢弃）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawResourceDescriptor {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "_meta", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// `list_resources` 结果（保真目录项 + 游标）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceList {
    pub resources: Vec<RawResourceDescriptor>,
    pub next_cursor: Option<String>,
}

/// rerun / attach 的**目标身份**：服务端记录导出的权威值。
///
/// 客户端字段只用于**校验**（不匹配即拒）；记录不在保留范围 ⇒ 显式拒绝、无回落。
/// 随 outcome 记录一同保留（rerun 目标来源），因此可序列化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetIdentity {
    pub server_id: String,
    /// 服务器声明的原始工具名。
    pub wire_tool_name: String,
    /// 目录准入后的 effective 工具名。
    pub effective_tool_name: String,
}

#[cfg(test)]
#[path = "mcp_apps_facade_test.rs"]
mod tests;
