//! 装配注入端口（3.0 批 2 波 2）。
//!
//! 资源类（`McpClientPool` / `CronScheduler` / `ToolSearchIndex` /
//! `WorkflowMiddleware`）与业务操作面（skills 扫描 / plugin 管理）在
//! peri-acp 协议面不再直接引用具体实现；宿主装配点构造具体实例后
//! upcast 为端口注入，ACP 侧只持端口接口（`docs/top-level.md` §0 依赖方向）。
//!
//! 具体实现位于 `peri-middlewares`（端口 impl 归实现方）。
//! `downcast_arc` 为还原点：middlewares 装配面（`assembly.rs:127-152`）与
//! 装配面宿主（`host/workflow_agent.rs` / `host/stage_builder.rs`）经 `as_any`
//! 还原具体类型调用业务方法（与 `TaskManager` downcast 先例一致）。

use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::acp_mcp::{AcpMcpError, AcpMcpInbound, AcpMcpServerSpec};
use crate::agents::AgentCatalogEntry;
use crate::command_registry::CommandRegistry;
use crate::dynamic_mcp::{
    CanonicalDynamicMcpAction, DynamicMcpCatalogTool, DynamicMcpFailure, DynamicMcpInstanceKey,
    DynamicMcpNotification, DynamicMcpResponse, DynamicMcpShutdownReport, ResolvedSecret,
    SecretRef, SessionMcpCapabilitySnapshot,
};
use crate::mcp_skills::{HandleToken, McpSkillRegistry};
use crate::skills::SkillMetadata;
use crate::tasks::TaskManager;
use tokio_util::sync::CancellationToken;

/// Terminal evidence for one MCP pool service-close transaction.
///
/// `Incomplete` means at least one rmcp cleanup timed out or the owner task
/// itself failed. The pool must remain `Closing`; repeated callers observe the
/// same report and must not claim a fully closed service graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpPoolShutdownReport {
    Complete {
        settled_services: usize,
        failed_services: usize,
    },
    Incomplete {
        settled_services: usize,
        unfinished_services: usize,
        failed_services: usize,
    },
}

impl McpPoolShutdownReport {
    pub fn is_complete(self) -> bool {
        matches!(self, Self::Complete { .. })
    }
}

/// Terminal evidence for the externally owned MCP background-task scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTaskShutdownReport {
    Complete,
}

/// Non-Clone deployment owner for MCP initialization/OAuth/reconnect/
/// subscription work.
///
/// ACP owns this capability only through the contract crate. Concrete task
/// registration and keyed completion remain in `peri-middlewares`.
#[async_trait::async_trait]
pub trait McpTaskOwnerPort: Send + Sync {
    /// Close task admission synchronously before any pool/resource drain.
    fn begin_shutdown(&self);

    /// Abort and join every task admitted before `begin_shutdown`.
    async fn shutdown(&mut self) -> McpTaskShutdownReport;
}

/// MCP 服务器连接状态（[`McpPoolPort::server_infos`] 投影；与
/// `peri-middlewares` 的 `ClientStatus` 同构，屏蔽其错误正文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerConnectionStatus {
    Connected,
    Failed,
    Disconnected,
    Disabled,
    Uninitialized,
}

/// MCP 服务器 OAuth 授权状态（[`McpPoolPort::server_infos`] 投影）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerOAuthStatus {
    None,
    Authorized,
    NeedsAuthorization,
}

/// 单个 MCP 服务器的状态投影（`mcp/list` 命令面数据源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerInfo {
    pub name: String,
    pub transport: String,
    pub status: McpServerConnectionStatus,
    pub oauth_status: McpServerOAuthStatus,
    pub tool_count: usize,
    pub resource_count: usize,
}

/// `mcp/oauth_start` 的启动结果（与 middlewares 内部
/// `OAuthStartDisposition` 同构）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpOAuthStartDisposition {
    Started,
    AlreadyActive,
    Conflict { active_flow_id: String },
}

/// builtin `workspace` 实例在当前池中的连接态（冻结期技能/指令/meta 读取的
/// 等待与短路判定；与替换前 `get_client("workspace")` + `ClientStatus` 同构）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpBuiltinWorkspaceState {
    /// 实现方不提供 builtin Workspace 实例（替换前类型还原失败 ⇒ 立即短路）。
    Unavailable,
    /// 实例尚未装配进池（按 initPhase 决定是否继续有界等待）。
    Absent,
    /// 句柄已连接。
    Connected,
    /// 句柄存在但非 Connected（Failed / Disconnected / Disabled）⇒ 立即短路。
    NotConnected,
}

/// MCP 客户端池端口（`peri-middlewares::mcp::McpClientPool` 实现）。
///
/// 宿主装配点构造 `McpClientPool` 后 upcast 注入；ACP 协议面只传递
/// 句柄，工具桥接/服务器管理在装配面宿主（`host/workflow_agent.rs` /
/// `host/stage_builder.rs`）。`shutdown` / `snapshot` 为 M-TUI 收口新增
/// 数据端口（`host/shutdown` 与 `mcp/list` 命令面经此访问，TUI 不再
/// 直持具体句柄）。
///
/// W3 端口补全（C3 前置）：ACP 侧的服务器目录 / OAuth / 会话执行 owner /
/// Workspace task scope / 发现预热改经本端口，不再 `downcast_arc` 还原
/// 具体类型。默认实现即「实现方不提供该能力」，与替换前类型还原失败时的
/// 分支同构：目录与 OAuth 类返回错误、清理与跳过类空操作；生产装配只注入
/// `McpClientPool`，路径不变。
#[async_trait::async_trait]
pub trait McpPoolPort: Send + Sync {
    /// 还原具体实现（downcast 还原点，供 middlewares 装配面与装配面宿主使用）。
    fn as_any(&self) -> &dyn Any;

    fn bind_agent_session(
        &self,
        _session_id: &str,
        _inbox: crate::session::InboxHandle,
        _manager: Arc<dyn crate::tasks::TaskManager>,
    ) {
    }

    fn agent_session_binding(
        &self,
        _session_id: &str,
    ) -> Option<(
        crate::session::InboxHandle,
        Arc<dyn crate::tasks::TaskManager>,
    )> {
        None
    }

    /// Revert recorded file changes in the session's trusted Workspace owner.
    /// Implementations must reject unavailable owners and report every failed change.
    async fn rewind_files(
        &self,
        _session_id: &str,
        _changes: serde_json::Value,
    ) -> Result<(), String> {
        Err("trusted Workspace rewind capability unavailable".into())
    }

    /// Whether this session has MCP Tasks whose terminal notification is pending.
    fn has_active_tasks(&self, _session_id: &str) -> bool {
        false
    }

    /// Synchronously close task/callback/commit admission before external task
    /// owners are joined. Idempotent.
    fn begin_shutdown(&self) {}

    /// 关闭连接池（`host/shutdown` 命令面调用；与 `McpClientPool::shutdown`
    /// 语义一致）。调用者取消/并发/重试观察同一 service-close transaction；
    /// cleanup timeout 返回 `Incomplete`，实现保持 `Closing`。
    async fn shutdown(&self) -> McpPoolShutdownReport;

    /// 池状态快照（`mcp/list` 命令面数据源）：`{"initPhase": ..., "servers":
    /// [...]}`，字段语义与 TUI 面板投影一致（序列化格式由实现方保证，
    /// 契约层不透传具体类型）。
    fn snapshot(&self) -> serde_json::Value;

    // ── 服务器目录 / OAuth（`mcp/list` 与 `mcp/oauth_*` 命令面） ──────────

    /// 全量服务器目录（`mcp/list` 数据源；语义与 `McpClientPool::all_server_infos`
    /// 一致：clients 表优先，configs 表补 `Uninitialized`）。
    ///
    /// 默认实现返回错误（实现方无服务器目录能力；替换前类型还原失败即报错）。
    fn server_infos(&self) -> Result<Vec<McpServerInfo>, String> {
        Err("MCP server catalog unavailable".into())
    }

    /// 指定 server 的进行中 OAuth flow id（`mcp/list` 逐项投影）。
    ///
    /// 默认 `None`（实现方无该能力；调用方仅在 [`Self::server_infos`] 成功后读取）。
    fn active_oauth_flow(&self, _server_name: &str) -> Option<String> {
        None
    }

    /// 以稳定 flow identity 启动 OAuth 授权（`mcp/oauth_start`；reservation
    /// 在 spawn 前完成，并发/重试不得启动第二个 provider flow）。
    ///
    /// 默认实现返回错误（替换前类型还原失败即报错）。
    fn spawn_oauth_flow_with_id(
        self: Arc<Self>,
        _server_name: &str,
        _flow_id: &str,
    ) -> Result<McpOAuthStartDisposition, String> {
        Err("MCP OAuth flow unavailable".into())
    }

    /// 投递授权码回传（`mcp/oauth_callback`，静态 server 面）。
    ///
    /// 默认实现返回错误（替换前类型还原失败即报错）。
    fn deliver_oauth_callback(
        &self,
        _server_name: &str,
        _code: String,
        _state: String,
    ) -> Result<(), String> {
        Err("MCP OAuth flow unavailable".into())
    }

    /// 以完整 dynamic identity 投递授权码；实现不得退化为裸 server name。
    ///
    /// 默认实现返回错误（替换前类型还原失败即报错）。
    fn deliver_dynamic_oauth_callback(
        &self,
        _instance: DynamicMcpInstanceKey,
        _flow_id: &str,
        _code: String,
        _state: String,
    ) -> Result<(), String> {
        Err("MCP OAuth flow unavailable".into())
    }

    /// 取消静态 server 的进行中授权（`mcp/oauth_cancel`），返回是否确有取消。
    ///
    /// 默认实现返回错误（替换前类型还原失败即报错）。
    fn cancel_oauth_callback(&self, _server_name: &str) -> Result<bool, String> {
        Err("MCP OAuth flow unavailable".into())
    }

    /// 取消指定 dynamic instance 的进行中授权，返回是否确有取消。
    ///
    /// 默认实现返回错误（替换前类型还原失败即报错）。
    fn cancel_dynamic_oauth_flow(
        &self,
        _instance: DynamicMcpInstanceKey,
        _flow_id: &str,
    ) -> Result<bool, String> {
        Err("MCP OAuth flow unavailable".into())
    }

    // ── Workspace task scope ─────────────────────────────────────────────
    //
    // 默认实现同「替换前类型还原失败 ⇒ 跳过」：不参与 scope 生命周期。

    /// 在 Store 清除持久 close intent 后重新开启已关闭的 scope。默认跳过。
    async fn open_workspace_task_scope(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }

    /// 会话关闭：栅栏新工作、发现既有任务、经 TaskManager 路由取消。默认跳过。
    async fn close_workspace_task_scope(self: Arc<Self>, _session_id: &str) -> Result<(), String> {
        Ok(())
    }

    /// 会话已不在内存（重入 close）时对账既有 scope 并收口。默认跳过。
    async fn reconcile_closing_workspace_scope(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }

    /// 绑定会话 TaskManager（外部任务取消/结算的投影）。默认跳过。
    fn bind_session_task_manager(&self, _session_id: &str, _manager: &Arc<dyn TaskManager>) {}

    /// 从 Workspace owner 重建可丢弃的 Agent 任务投影。默认跳过。
    async fn recover_workspace_tasks(self: Arc<Self>, _session_id: &str) -> Result<(), String> {
        Ok(())
    }

    /// 在初始快照后保持 scope 游标（长驻任务；`cancel` 触发即退出）。默认立即返回。
    async fn watch_workspace_tasks(self: Arc<Self>, _session_id: &str, _cancel: CancellationToken) {
    }

    // ── 会话 MCP 发现接线（session/new 预热路径） ─────────────────────────

    /// 挂接连接完成事件 → 幂等发现。端口面不含 ExecutorEvent 通知通道
    /// （ACP 路径为 `None`；含通知的完整版仍由 middlewares 装配面内部调用）。
    /// 默认跳过。
    fn attach_connection_notifier(
        self: Arc<Self>,
        _registry: Option<&Arc<McpSkillRegistry>>,
        _command_registry: Option<&Arc<CommandRegistry>>,
        _cancel: &CancellationToken,
    ) {
    }

    /// 会话新建即触发幂等发现（不装配 chain）。默认跳过。
    fn prewarm_discovery(
        self: Arc<Self>,
        _registry: &Arc<McpSkillRegistry>,
        _command_registry: &Arc<CommandRegistry>,
        _session_id: &str,
        _cancel: &CancellationToken,
    ) {
    }

    // ── builtin Workspace 冻结读取（技能 / 指令 / meta） ─────────────────
    //
    // 默认实现同「替换前 `downcast_ref` 失败 ⇒ 立即返回空」：状态为
    // `Unavailable`，读取方法返回错误（调用点先看状态，不会走到读取）。

    /// builtin `workspace` 实例的连接态。默认 `Unavailable`（实现方不提供）。
    fn builtin_workspace_state(&self) -> McpBuiltinWorkspaceState {
        McpBuiltinWorkspaceState::Unavailable
    }

    /// 冻结期技能清单快照（元数据快照，不读正文）。默认错误。
    ///
    /// 实现方语义（`McpClientPool`）：实例未装配 / 未连接 / 未声明 skills
    /// 能力 ⇒ `Ok(空)`（不是失败）；能力已声明且实例健康但读取失败 ⇒ `Err`
    /// （调用方 fail-closed，不谎称「无技能」）。
    async fn read_builtin_workspace_skills(&self) -> Result<Vec<SkillMetadata>, String> {
        Err("builtin workspace skills face unavailable".into())
    }

    /// 冻结期项目指令读取：`(main, local)`，main 为 import 展开后的正文，
    /// local 为原文；两者都可为 `None`（未列出 ≠ 失败）。默认错误。
    async fn read_builtin_workspace_instructions(
        &self,
    ) -> Result<(Option<String>, Option<String>), String> {
        Err("builtin workspace instruction face unavailable".into())
    }

    /// 冻结期 MetaHarness 覆盖文档读取（仅启用 section；不回落磁盘）。默认错误。
    async fn read_builtin_workspace_meta(
        &self,
        _enabled_sections: &HashSet<String>,
    ) -> Result<HashMap<String, String>, String> {
        Err("builtin workspace meta face unavailable".into())
    }
}

impl dyn McpPoolPort {
    /// 将 `Arc<dyn McpPoolPort>` 还原为具体实现 `Arc<T>`（类型不符返回原 `Arc`）。
    pub fn downcast_arc<T: McpPoolPort + 'static>(self: Arc<Self>) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            // 经 `as_any()` 取具体类型的 TypeId：直接对 trait object 调
            // `type_id()` 会命中 `Any` 的 blanket impl，返回
            // `TypeId::of::<dyn McpPoolPort>()`（trait object 自身），
            // 恒不等于 `TypeId::of::<T>()` → downcast 恒失败 → 装配面回退
            // 临时实例，注入的连接池与装配产物分离（同构
            // 2026-08-06-e2e-workflow-not-completing 遗留项）。
            if (*ptr).as_any().type_id() == TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

/// ACP 侧 agent→client 发送网关（MCP over ACP 的传输切片）。
///
/// 由 host 按 ACP 连接提供（`AcpTransport` 的请求/通知子集）；middlewares 的
/// 桥接 transport 经它把内层 MCP 消息投递为 `mcp/message`。实现不得在失败时
/// 静默吞掉消息：投递失败必须返回错误，由桥接按连接失败处理。
#[async_trait::async_trait]
pub trait AcpMcpGatewayPort: Send + Sync {
    /// 发送 `mcp/message` 请求并等待内层 MCP 结果。
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AcpMcpError>;

    /// 发送 `mcp/message` 通知（无响应）。
    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), AcpMcpError>;
}

/// 会话级 MCP over ACP 服务端口（`peri-middlewares` 实现）。
///
/// 一个端口服务一个 ACP 连接的多个会话：`attach` 注册 client 在会话 setup 中
/// 声明的 server 并后台建连（连接就绪后工具经 MCP 池进入 deferred 发现）；
/// `request` / `notify` 路由 client 反向下发的 `mcp/message`；`close_session`
/// 在会话结束（close / 删除 / transport 关闭 / 进程退出）时断开该会话全部连接。
#[async_trait::async_trait]
pub trait AcpMcpServerPort: Send + Sync {
    /// 注册并后台连接会话声明的 acp 型 server。
    ///
    /// 同会话同 `server_id` 重复声明按幂等处理（已连接或正在连接的跳过）。
    /// 建连失败不影响调用方：失败在池状态面可见，不阻塞会话建立。
    fn attach(&self, gateway: Arc<dyn AcpMcpGatewayPort>, servers: Vec<AcpMcpServerSpec>);

    /// 处理 client 下发的 `mcp/message` 请求，返回内层 MCP 结果。
    async fn request(&self, inbound: AcpMcpInbound) -> Result<serde_json::Value, AcpMcpError>;

    /// 处理 client 下发的 `mcp/message` 通知。
    async fn notify(&self, inbound: AcpMcpInbound) -> Result<(), AcpMcpError>;

    /// 该服务是否承载指定连接（入站 `mcp/message` 的定位依据）。
    ///
    /// 入站消息只带 `connectionId`、不带 `sessionId`，而 MCP 池是会话级的
    /// （宿主装配每个会话各持一份连接事实），因此宿主需要在各会话服务间
    /// 定位承载者：只有 `true` 的那个才能路由该消息。
    fn owns_connection(&self, connection_id: &str) -> bool;

    /// 关闭会话的全部 ACP MCP 连接（发送 `mcp/disconnect`、移除池条目）。幂等。
    async fn close_session(&self, session_id: &str);
}

/// 工具检索索引端口（`peri-middlewares::tool_search::ToolSearchIndex` 实现）。
pub trait ToolSearchPort: Send + Sync {
    /// 还原具体实现（downcast 还原点，供 middlewares 装配面与装配面宿主使用）。
    fn as_any(&self) -> &dyn Any;
}

impl dyn ToolSearchPort {
    /// 将 `Arc<dyn ToolSearchPort>` 还原为具体实现 `Arc<T>`（类型不符返回原 `Arc`）。
    pub fn downcast_arc<T: ToolSearchPort + 'static>(self: Arc<Self>) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            // 经 `as_any()` 取具体类型的 TypeId：直接对 trait object 调
            // `type_id()` 会命中 `Any` 的 blanket impl，返回
            // `TypeId::of::<dyn ToolSearchPort>()`（trait object 自身），
            // 恒不等于 `TypeId::of::<T>()` → downcast 恒失败 → 装配面回退
            // 默认实例，注入的搜索索引与装配产物分离（同构
            // 2026-08-06-e2e-workflow-not-completing 遗留项）。
            if (*ptr).as_any().type_id() == TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

/// Deployment-scoped Dynamic MCP operation and shutdown port.
#[async_trait::async_trait]
pub trait DynamicMcpDeploymentPort: Send + Sync {
    async fn execute(
        &self,
        session_id: &str,
        action: CanonicalDynamicMcpAction,
    ) -> Result<DynamicMcpResponse, DynamicMcpFailure>;

    /// Register the canonical session catalog used for pre-commit name collision
    /// rejection. Registration must happen before any load can be admitted.
    fn register_catalog(
        &self,
        session_id: &str,
        tools: Vec<DynamicMcpCatalogTool>,
    ) -> Result<(), DynamicMcpFailure>;

    fn capability(&self, session_id: &str) -> Arc<dyn SessionMcpCapabilityPort>;

    fn close_registration(&self, session_id: &str) -> Arc<dyn SessionCloseRegistration>;

    /// Validate that an opaque Dynamic MCP identity still names the current live
    /// incarnation. OAuth RPCs must call this before touching scoped flow APIs.
    fn accepts_instance(&self, _instance: &DynamicMcpInstanceKey) -> bool {
        false
    }

    /// Bind a weak, checked notification target for one live session. Rebinding
    /// replaces the previous lease; dropping the returned sink disables delivery.
    fn bind_notification_sink(
        &self,
        _session_id: &str,
        _sink: std::sync::Weak<dyn DynamicMcpNotificationSinkPort>,
    ) -> bool {
        false
    }

    /// Deliver an OAuth authorization URL only through the checked sink bound to
    /// the originating session. Implementations must reject stale instances and
    /// must never fall back to a deployment-global transport.
    fn notify_authorization_needed(
        &self,
        _instance: &DynamicMcpInstanceKey,
        _flow_id: &str,
        _authorization_url: &str,
    ) -> bool {
        false
    }

    fn begin_shutdown(&self);

    async fn close_session(&self, session_id: &str) -> DynamicMcpShutdownReport;

    async fn shutdown(&self) -> DynamicMcpShutdownReport;
}

/// Session-local immutable capability source shared by the main agent and all
/// of its subagents.
pub trait SessionMcpCapabilityPort: Send + Sync {
    fn snapshot(&self) -> Arc<SessionMcpCapabilitySnapshot>;

    /// Bind the existing session MCP read/discovery registries to this checked
    /// capability source. The returned lease owns no parallel capability
    /// registry: it only projects the effective handles into the existing MCP
    /// pool, skill registry and command registry.
    fn bind_projection(
        &self,
        _static_handles: Vec<(String, HandleToken)>,
        _skill_registry: Arc<crate::mcp_skills::McpSkillRegistry>,
        _command_registry: Arc<crate::command_registry::CommandRegistry>,
    ) -> Arc<dyn SessionMcpProjectionLease> {
        panic!("session MCP capability does not support checked projection")
    }
}

/// Incarnation-checked adapter from a session capability view to the existing
/// MCP tools/resources/skills/commands production registries.
pub trait SessionMcpProjectionLease: Send + Sync {
    fn as_any(&self) -> &dyn Any;

    /// Refresh the effective server view. Returns false after session close.
    fn refresh(&self) -> bool;

    /// Close projection admission and remove every dynamic projection. Idempotent.
    fn close(&self);
}

/// Idempotent session-close lease. ACP may call this without understanding the
/// Dynamic MCP state machine.
#[async_trait::async_trait]
pub trait SessionCloseRegistration: Send + Sync {
    async fn revoke_and_cleanup(&self) -> DynamicMcpShutdownReport;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SecretResolveError {
    #[error("secret reference was not found")]
    NotFound,
    #[error("secret reference access was denied")]
    Denied,
    #[error("secret resolver is unavailable")]
    Unavailable,
}

/// Production implementations must resolve opaque references only after HITL.
#[async_trait::async_trait]
pub trait SecretResolverPort: Send + Sync {
    async fn resolve(&self, reference: &SecretRef) -> Result<ResolvedSecret, SecretResolveError>;
}

/// Checked session-specific Dynamic MCP notification sink. Implementations must
/// reject stale incarnation writes and must never broadcast as a fallback.
pub trait DynamicMcpNotificationSinkPort: Send + Sync {
    fn notify(&self, notification: DynamicMcpNotification) -> bool;

    fn notify_authorization_needed(
        &self,
        _instance: &DynamicMcpInstanceKey,
        _flow_id: &str,
        _authorization_url: &str,
    ) -> bool {
        false
    }

    fn accepts(&self, instance: &DynamicMcpInstanceKey) -> bool;
}

/// Workflow 中间件端口（`peri-middlewares::workflow::WorkflowMiddleware` 实现）。
///
/// per-session 实例；构造点（装配面宿主 `host/workflow_agent.rs` 的
/// `create_session_workflow_middleware`）持有具体实现，协议面只持端口句柄。
/// 命令面（workflow/list_runs / kill_agent / kill_run / resume）与执行装配
/// （bg registry 注入 / 完成通知订阅）均经本端口；装配面宿主可经
/// `downcast_arc` 还原具体类型。
#[async_trait::async_trait]
pub trait WorkflowMiddlewarePort: Send + Sync {
    /// 还原具体实现（downcast 还原点，供 middlewares 装配面与装配面宿主使用）。
    fn as_any(&self) -> &dyn Any;

    /// 全部 run 快照（JSON 透传：`RunProgress` 保留在 peri-workflow，
    /// 契约层不引入 indexmap 依赖）。
    fn runs_snapshot(&self) -> serde_json::Value;

    /// 终止单个 workflow agent（返回是否命中）。
    async fn kill_agent(&self, run_id: &str, agent_id: u64) -> bool;

    /// 终止整个 run（返回是否命中）。
    fn kill_run(&self, run_id: &str) -> bool;

    /// 从 journal 恢复 run。
    async fn resume(&self, run_id: &str) -> Result<String, String>;

    /// 订阅 run 完成通知（每 run 一条 `WorkflowTaskResult`）。
    fn subscribe_notifications(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::workflow::WorkflowTaskResult>;

    /// 注入统一后台任务注册表（session 级 TaskManager）。
    fn set_bg_registry(&self, bg_registry: std::sync::Arc<dyn crate::tasks::TaskManager>);

    /// 通知消费者单次 spawn 门（首次调用返回 true）。
    fn init_notification_buffer(&self) -> bool;
}

impl dyn WorkflowMiddlewarePort {
    /// 将 `Arc<dyn WorkflowMiddlewarePort>` 还原为具体实现 `Arc<T>`（类型不符返回原 `Arc`）。
    pub fn downcast_arc<T: WorkflowMiddlewarePort + 'static>(
        self: Arc<Self>,
    ) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            // 经 `as_any()` 取具体类型的 TypeId：直接对 trait object 调
            // `type_id()` 会命中 `Any` 的 blanket impl，返回
            // `TypeId::of::<dyn WorkflowMiddlewarePort>()`（trait object 自身），
            // 恒不等于 `TypeId::of::<T>()` → downcast 恒失败 → 装配面回退
            // 临时实例，WorkflowTool 注册的 registry 与 executor 完成通知
            // 消费者订阅的 registry 分离，workflow 完成通知丢失
            // （e2e workflow 超时，2026-08-06）。
            if (*ptr).as_any().type_id() == TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

/// Agent 候选目录端口：主提示词 `{{available_agents}}` 渲染经此取候选
/// （W5：唯一来源是会话级 MCP Agent registry 对 builtin `workspace` 实例
/// `resources/list` 的投影）。
///
/// 历史沿革：W4b 删除 `available_skills`（技能命令面改由 MCP 发现异步投影）；
/// W5 把本端口从「本地扫盘（`scan_agents_detailed`）」改为「registry 投影」，
/// 端口实现留在 `peri-middlewares`（`host_ports::AgentCatalogProvider`），
/// ACP 侧不直调业务 crate（§0 依赖方向）。
pub trait AgentCatalogPort: Send + Sync {
    /// 还原具体实现（downcast 还原点，供 middlewares 装配面绑定会话级 registry）。
    fn as_any(&self) -> &dyn std::any::Any;

    /// 本地来源候选（E13 优先级去重）。
    ///
    /// `include_builtin=false`（父会话冻结的 built-in policy 关闭）时返回的列表
    /// 不含 builtin 来源项；面未装配 / 实例被关闭 ⇒ 空列表（X4：不回落磁盘）。
    fn catalog(&self, include_builtin: bool) -> Vec<AgentCatalogEntry>;
}

impl dyn AgentCatalogPort {
    /// 将 `Arc<dyn AgentCatalogPort>` 还原为具体实现 `Arc<T>`（类型不符返回原 `Arc`）。
    pub fn downcast_arc<T: AgentCatalogPort + 'static>(
        self: Arc<Self>,
    ) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            if (*ptr).as_any().type_id() == TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

#[cfg(test)]
#[path = "ports_test.rs"]
mod tests;
