//! 装配注入端口（3.0 批 2 波 2）。
//!
//! 资源类（`McpClientPool` / `CronScheduler` / `ToolSearchIndex` /
//! `McpClientPool`）与业务操作面（skills 扫描 / plugin 管理）在
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

use crate::acp_mcp::{AcpMcpError, AcpMcpInbound, AcpMcpRegistry, AcpMcpServerSpec};
use crate::agents::AgentCatalogEntry;
use crate::command_registry::CommandRegistry;
pub use crate::mcp_control::SessionMcpCatalogPort;
use crate::mcp_control::{
    McpControlError, McpControlRequest, McpControlResponse, McpServerControl,
};
use crate::mcp_skills::McpSkillRegistry;
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

/// `mcp/list` 暴露的单个工具投影。
///
/// 有界投影：`McpServerInfo::tools` 至多 64 条，`name`/`description` 已完成
/// 控制字符清洗与 160 字符截断；全量计数仍以
/// [`McpServerInfo::tool_count`] 为准。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
}

/// `mcp/list` 暴露的单个资源投影。
///
/// 有界投影语义同 [`McpToolInfo`]；全量计数以
/// [`McpServerInfo::resource_count`] 为准。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct McpResourceInfo {
    pub uri: String,
    pub name: Option<String>,
    pub description: Option<String>,
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
    /// 工具的有界投影（至多 64 条、文本 160 字符）；全量计数见
    /// [`Self::tool_count`]。
    pub tools: Vec<McpToolInfo>,
    /// 资源的有界投影（至多 64 条、文本 160 字符）；全量计数见
    /// [`Self::resource_count`]。
    pub resources: Vec<McpResourceInfo>,
    /// 服务器配置声明的订阅项；None = 未配置。
    pub subscriptions: Option<crate::plugin::McpSubscriptionsConfig>,
    /// server 自报版本（initialize 响应的 `serverInfo.version`）；未知为 None。
    pub version: Option<String>,
    /// 当前连接建立时刻（RFC3339 UTC）；从未成功连接为 None。
    pub connected_at: Option<String>,
    /// 与 server 协商出的 MCP 协议版本（如 `2026-07-28`）；未协商出为 None。
    pub protocol_version: Option<String>,
    /// 供 UI 显示的一行安全失败摘要（trim + 160 字符上限，由 middlewares
    /// 侧投影承载；完整错误链仅写入 tracing 日志）；非 Failed 状态为 None。
    pub error_summary: Option<String>,
    /// 服务器 URL（HTTP 传输）；stdio/builtin 传输为 None。
    pub url: Option<String>,
    pub control: McpServerControl,
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

/// 宿主面「按实例消费」的资源订阅句柄
/// （[`McpPoolPort::subscribe_instance_resource`] 的返回）。
///
/// 每次 [`Self::next`] 产出一条 `notifications/resources/updated` 携带的资源 URI；
/// `None` = 订阅已终止（连接关闭 / 实例移除 / 池关闭），此后不得再读取。
///
/// 通知只是**唤醒信号**（设计 `docs/design/workflow-mcp.md` §4.3）：不携带正文、
/// 不保证可靠投递（缓冲满即丢弃），消费方收到后经
/// [`McpPoolPort::read_instance_resource`] 按快照/游标补读。句柄是订阅的所有权：
/// `drop` 或 [`Self::cancel`] 即注销（幂等，尽力而为——取消经传输通知送达）。
#[derive(Debug)]
pub struct McpResourceSubscription {
    updates: tokio::sync::mpsc::Receiver<String>,
    cancel: CancellationToken,
}

impl McpResourceSubscription {
    /// 由实现方构造：`updates` 为通知转发通道，`cancel` 触发注销。
    pub fn new(updates: tokio::sync::mpsc::Receiver<String>, cancel: CancellationToken) -> Self {
        Self { updates, cancel }
    }

    /// 等待下一次资源更新通知；`None` = 订阅终止。
    pub async fn next(&mut self) -> Option<String> {
        self.updates.recv().await
    }

    /// 主动注销订阅（幂等）；`drop` 句柄等价。
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl Drop for McpResourceSubscription {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
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
    /// Control a server in this pool after the host has admitted the session request.
    async fn control_mcp(
        self: Arc<Self>,
        _request: McpControlRequest,
    ) -> Result<McpControlResponse, McpControlError> {
        Err(McpControlError::unsupported())
    }
    /// Current immutable MCP catalog for the given session's visibility scope.
    fn catalog_source(
        self: Arc<Self>,
        _session_id: &str,
    ) -> Option<Arc<dyn SessionMcpCatalogPort>> {
        None
    }
    /// Cancel an exact active OAuth flow in this pool.
    fn cancel_oauth_flow(&self, _flow_id: &str) -> Result<bool, String> {
        Err("MCP OAuth flow unavailable".into())
    }

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

    fn verify_shared_environment_close(&self, _root_session_id: &str) -> Result<(), String> {
        Err("Incomplete: shared environment child ownership unknown".into())
    }

    /// 资源 owner 完成消息与任务结清后释放子会话绑定；失败不得宣称已关闭。
    fn release_agent_session(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
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

    /// 池内是否存在指定实例的注册句柄（仅连接状态，不读取 MCP 业务数据）。
    ///
    /// 宿主生命周期（如 workflow 消费者）必须在实例实际装配后才启动。默认
    /// `true` 保持未声明该能力的替换实现的既有行为；`McpClientPool` 按客户端
    /// 表精确回答。
    fn has_instance(&self, _instance: &str) -> bool {
        true
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

    /// 取消静态 server 的进行中授权（`mcp/oauth_cancel`），返回是否确有取消。
    ///
    /// 默认实现返回错误（替换前类型还原失败即报错）。
    fn cancel_oauth_callback(&self, _server_name: &str) -> Result<bool, String> {
        Err("MCP OAuth flow unavailable".into())
    }

    // ── Workspace task owner ─────────────────────────────────────────────

    /// 子会话释放：取消已知 external task 并等待进程收口。默认跳过。
    async fn close_agent_session_scope(self: Arc<Self>, _session_id: &str) -> Result<(), String> {
        Err("Incomplete: agent session scope close capability unavailable".into())
    }

    /// 会话释放：取消该会话 TaskManager 已登记的 external task 并等待 settled。
    /// 默认无操作（无 runtime 投影的池没有可取消的任务）。
    async fn cancel_session_external_tasks(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }

    /// 绑定会话 TaskManager（外部任务取消/结算的投影）。默认跳过。
    fn bind_session_task_manager(&self, _session_id: &str, _manager: &Arc<dyn TaskManager>) {}

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

    // ── 按实例消费（宿主面对池内某一具体实例的工具 / 资源面） ─────────────
    //
    // 用途：宿主（ACP workflow handlers / WorkflowTaskConsumer）对池内**已连接的
    // builtin 实例**做 `tools/call`（可带 request `_meta`）、`resources/read` 与
    // `notifications/resources/updated` 订阅，不还原具体池类型。默认实现即
    // 「实现方不提供该能力」，与替换前 `downcast_ref` 失败时的分支同构：一律返回
    // 错误（调用点按错误处理，不伪造空结果）。

    /// 对池内某实例执行 `tools/call`，返回既有工具结果映射口径的文本。
    ///
    /// `request_meta`：请求 `_meta` 对象（如 workflow 消费者令牌
    /// `peri/workflowConsumer`）；`None` = 不附加元数据。
    ///
    /// 实现方语义（`McpClientPool`）：
    /// - 未知实例 / 实例未连接 ⇒ `Err`（既有 `ServerNotFound` / `NotConnected` 文本）；
    /// - 成功 ⇒ `Ok(结果文本)`（与模型面 `format_contents` 同一映射）；
    /// - 工具级失败（`isError: true`）⇒ `Err(服务端错误文本)`；
    /// - 协议 / 传输错误 / 超时 / task 形状响应 ⇒ `Err`（既有 `ToolCallError` 文本）。
    ///
    /// 默认实现返回错误（实现方不提供按实例调用能力）。
    async fn call_instance_tool(
        &self,
        _instance: &str,
        _tool: &str,
        _arguments: serde_json::Value,
        _request_meta: Option<serde_json::Value>,
    ) -> Result<String, String> {
        Err("per-instance MCP tool call face unavailable".into())
    }

    /// 读取池内某实例的资源 URI，返回资源正文文本。
    ///
    /// 实现方语义（`McpClientPool`）：未知实例 / 未连接 / 服务端拒绝
    /// （URI 不支持或资源不存在）/ 无文本内容 / 含二进制内容 ⇒ `Err`；
    /// 文本内容按序拼接 ⇒ `Ok`。读取直连实例连接，不进入跨进程资源缓存
    /// （宿主面按需读取，缓存提交属资源工具面的内容验证路径）。
    ///
    /// 默认实现返回错误（实现方不提供按实例读取能力）。
    async fn read_instance_resource(&self, _instance: &str, _uri: &str) -> Result<String, String> {
        Err("per-instance MCP resource read face unavailable".into())
    }

    /// 就池内某实例的指定资源 URI 订阅 `notifications/resources/updated`。
    ///
    /// 返回宿主消费句柄 [`McpResourceSubscription`]：`next()` 逐条产出更新 URI，
    /// `None` = 订阅终止；`drop` 句柄或 `cancel()` = 注销订阅（幂等）。
    ///
    /// 实现方语义（`McpClientPool`）：未知实例 / 未连接 / 订阅建立失败 ⇒ `Err`；
    /// 订阅流随实例连接（per-session 池）存续，实例移除 / 重连 / 池关闭即终止。
    ///
    /// 默认实现返回错误（实现方不提供按实例订阅能力）。
    async fn subscribe_instance_resource(
        &self,
        _instance: &str,
        _uri: &str,
    ) -> Result<McpResourceSubscription, String> {
        Err("per-instance MCP resource subscription face unavailable".into())
    }

    // ── builtin Workspace 冻结读取（技能 / 指令 / meta） ─────────────────
    //
    // 默认实现同「替换前 `downcast_ref` 失败 ⇒ 立即返回空」：状态为
    // `Unavailable`，读取方法返回错误（调用点先看状态，不会走到读取）。

    /// 有效 `workspace` 声明的来源身份（H3/D1：执行环境的唯一事实源）。
    ///
    /// 实现方语义（`McpClientPool`）：覆盖会话声明（含恢复/reopen 由持久 owner
    /// 装载的结果）与合并配置（部署/全局/项目/插件层 + builtin overlay）；
    /// `None` = 无 `workspace` 条目。默认 `None`（实现方不暴露来源面）。
    fn workspace_source(&self) -> Option<crate::plugin::ConfigSource> {
        None
    }

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
/// 由 host 按 ACP 连接提供（`AcpTransport` 的请求子集）；middlewares 的
/// 桥接 transport 经它把内层 MCP 请求投递为 `mcp/message`。schema 1.10 起
/// `mcp/message` 只有 agent → client 的请求方向与 client → agent 的通知方向，
/// 网关只承担前者。实现不得在失败时静默吞掉消息：投递失败必须返回错误，
/// 由桥接按连接失败处理。
#[async_trait::async_trait]
pub trait AcpMcpGatewayPort: Send + Sync {
    /// 发送 `mcp/message` 请求并等待内层 MCP 结果。
    /// cancellation 触发时发送官方 ACP 请求取消并释放本地 pending；必须有界返回。
    /// 网关保存 peri-N 与实际外层 JSON-RPC ID 的关联，不能混用两者。
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        cancellation: CancellationToken,
    ) -> Result<serde_json::Value, AcpMcpError>;
}

/// 会话级 MCP over ACP 服务端口（`peri-middlewares` 实现）。
///
/// 每个端口持有会话级池；`attach` 使用连接级共享注册表注册 client 在 setup 中
/// 声明的 server 并后台完成发现（工具经 MCP 池进入 deferred 发现）；
/// `notify` 路由 client 反向下发的 `mcp/message` 通知；`close_session`
/// 在会话结束（close / 删除 / transport 关闭 / 进程退出）时移除该会话的
/// 声明与池条目。
#[async_trait::async_trait]
pub trait AcpMcpServerPort: Send + Sync {
    /// 注册并后台发现会话声明的 acp 型 server。
    ///
    /// 同会话同 `server_id` 重复声明按幂等处理（已发现或正在发现的跳过）。
    /// 发现失败不影响调用方：失败在池状态面可见，不阻塞会话建立。
    /// 跨会话 serverId 冲突同步返回 CODE_UNAVAILABLE，不发起发现。
    fn attach(
        &self,
        registry: Arc<AcpMcpRegistry>,
        gateway: Arc<dyn AcpMcpGatewayPort>,
        servers: Vec<AcpMcpServerSpec>,
    ) -> Result<(), AcpMcpError>;

    /// 处理 client 下发的 `mcp/message` 通知。
    async fn notify(&self, inbound: AcpMcpInbound) -> Result<(), AcpMcpError>;

    /// 该服务是否存在指定 serverId 的活跃桥接（状态观测，不作为宿主路由依据）。
    /// 宿主必须通过连接级 AcpMcpRegistry 定位唯一归属会话。
    fn owns_server(&self, server_id: &str) -> bool;

    /// 移除会话的全部 ACP MCP 声明与池条目。幂等。
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
