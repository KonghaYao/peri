//! session entitlement 的**消费**端口（MCP Apps 重设计 §4.3）。
//!
//! 设计契约：`session entitlement` 由 SDK / 部署的**可信身份层**提供；Peri
//! 通过本端口（或等价注入点）消费 grant。它回答的是「这个调用者是否有这个
//! session 的这项授权」，而不是「按 id 能取到哪个运行时对象」。
//!
//! ## 与 [`crate::session::SessionAccessPort`] 的区别（**不得互相替代**）
//!
//! - [`crate::session::SessionAccessPort`]：**运行时数据访问**端口。输入只有
//!   `session_id` 字符串，作用是把执行体与 ACP `SessionManager` 解耦（取
//!   v2 队列 / inbox / goal / cron bridge 等）；默认实现返回 `None`/`false`，
//!   只证明「按 id 找得到对象」，**不构成任何鉴权证明**——知道 id 的人也能让
//!   它返回对象。
//! - 本端口：**授权消费**端口。输入是调用者身份 + 目标 session + 能力，输出是
//!   授权事实（grant）或缺省拒绝。它不提供任何运行时对象句柄。
//!
//! 因此「用 `SessionAccessPort` 查得到 session」永远不能当作 entitlement 通过；
//! 凡是要把某次操作判为「已授权」（App attach、App 操作、Attachment 控制等），
//! 必须经本端口。
//!
//! ## 注入与 fail-closed
//!
//! - 注入点：部署 / SDK 装配层提供 `Arc<dyn SessionAuthorizationPort>` 实现
//!   （如把已认证的连接映射为 [`TrustedCallerId`]，再按 session 定位授权记录）；
//!   Peri 侧只持 trait object，不解释也不可能伪造调用者身份。
//! - **默认拒绝**：[`SessionAuthorizationPort::grant`] 的默认实现返回 `None`，
//!   即「没有可信身份层 → 不授权」。实现方漏实现只会更严格，不会更宽松。
//!   未注入端口（`Option<Arc<dyn SessionAuthorizationPort>>` 为 `None`）同样
//!   视为不可用，消费方必须按拒绝处理，不得回落到「只看 id 是否存在」。
//!
//! 详见 `spec/issues/2026-10-12-mcp-apps-redesign/implementation-log.md`
//! （契约来源 §4.3）。

/// 可信身份层给出的调用者身份（已认证连接 / 已认证客户端会话）。
///
/// 由部署侧身份层铸造，**不得**取自请求参数自述的字符串（App JSON-RPC id、
/// 客户端自报的 view id、session locator 都不算身份）。
///
/// # Examples
///
/// ```
/// use peri_acp_types::session_authorization::TrustedCallerId;
///
/// let caller = TrustedCallerId::new("conn-7f3a");
/// assert_eq!(caller.as_str(), "conn-7f3a");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TrustedCallerId(String);

impl TrustedCallerId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TrustedCallerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for TrustedCallerId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for TrustedCallerId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// 需要 entitlement 的能力面（对应 §6 facade 操作族，按能力而非按方法名划分）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCapability {
    /// 建立 / 重新建立 Attachment（`apps/attach`）。
    AttachApps,
    /// 经 Attachment 发起 App 操作（`apps/request`：tools/call、resources/read|list）。
    AppOperations,
    /// 撤销 Attachment 与 App 交互资格（`apps/cancel` / `apps/detach` / `apps/close`）。
    ControlApps,
}

/// 一次授权查询：谁，在哪个 session，要做什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntitlementRequest {
    /// 目标 session（定位事实，不构成授权理由）。
    pub session_id: String,
    /// 可信调用者身份。
    pub caller: TrustedCallerId,
    /// 请求的能力。
    pub capability: SessionCapability,
}

impl SessionEntitlementRequest {
    pub fn new(
        session_id: impl Into<String>,
        caller: impl Into<TrustedCallerId>,
        capability: SessionCapability,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            caller: caller.into(),
            capability,
        }
    }
}

/// 授权事实：可信身份层确认 (session, caller, capability) 成立。
///
/// 消费方必须用 [`SessionGrant::covers`] 校验 grant 与当前请求一致——端口返回
/// 别的 session / 别的调用者 / 别的能力的 grant 不构成本次授权。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGrant {
    session_id: String,
    caller: TrustedCallerId,
    capability: SessionCapability,
}

impl SessionGrant {
    /// 由可信身份层铸造（仅实现 `SessionAuthorizationPort` 的部署侧调用）。
    pub fn new(
        session_id: impl Into<String>,
        caller: impl Into<TrustedCallerId>,
        capability: SessionCapability,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            caller: caller.into(),
            capability,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn caller(&self) -> &TrustedCallerId {
        &self.caller
    }

    pub fn capability(&self) -> SessionCapability {
        self.capability
    }

    /// 该 grant 是否恰好覆盖本次请求（三元组全等）。
    pub fn covers(&self, request: &SessionEntitlementRequest) -> bool {
        self.session_id == request.session_id
            && self.caller == request.caller
            && self.capability == request.capability
    }
}

/// entitlement 消费端口：把「是否有授权」的裁决交给可信身份层。
///
/// 默认实现（不覆写 [`Self::grant`]）恒返回 `None` = **不授权**：没有接入可信
/// 身份层时，任何需要 entitlement 的操作都必须被拒。实现方必须只依据部署侧
/// 身份事实作答，不得回落到「session 存在即授权」「
/// [`crate::session::SessionAccessPort`] 查得到即授权」。
///
/// # Examples
///
/// ```
/// use peri_acp_types::session_authorization::{
///     SessionAuthorizationPort, SessionCapability, SessionEntitlementRequest, SessionGrant,
/// };
///
/// /// 未接入身份层：默认实现即拒绝。
/// struct NotWired;
/// impl SessionAuthorizationPort for NotWired {}
///
/// /// 部署侧身份层：只授权 conn-1 对 session-1 的 attach。
/// struct DeploymentIdentity;
/// impl SessionAuthorizationPort for DeploymentIdentity {
///     fn grant(&self, request: &SessionEntitlementRequest) -> Option<SessionGrant> {
///         (request.caller.as_str() == "conn-1"
///             && request.session_id == "session-1"
///             && request.capability == SessionCapability::AttachApps)
///             .then(|| SessionGrant::new("session-1", "conn-1", SessionCapability::AttachApps))
///     }
/// }
///
/// let request = SessionEntitlementRequest::new(
///     "session-1",
///     "conn-1",
///     SessionCapability::AttachApps,
/// );
/// let port: &dyn SessionAuthorizationPort = &NotWired;
/// assert_eq!(port.grant(&request), None, "未接入身份层必须拒绝");
///
/// let port: &dyn SessionAuthorizationPort = &DeploymentIdentity;
/// let grant = port.grant(&request).expect("部署身份层授权");
/// assert!(grant.covers(&request));
/// ```
pub trait SessionAuthorizationPort: Send + Sync {
    /// 返回 `Some(grant)` = 可信身份层明确授权；`None` = 不授权（含未接入身份层）。
    ///
    /// 消费方必须再经 [`SessionGrant::covers`] 校验 grant 覆盖本次请求。
    fn grant(&self, _request: &SessionEntitlementRequest) -> Option<SessionGrant> {
        None
    }
}

#[cfg(test)]
#[path = "session_authorization_test.rs"]
mod tests;
