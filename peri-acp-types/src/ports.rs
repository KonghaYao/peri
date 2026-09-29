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
use std::path::PathBuf;
use std::sync::Arc;

use crate::acp_mcp::{AcpMcpError, AcpMcpInbound, AcpMcpServerSpec};
use crate::agents::AgentCapability;
use crate::dynamic_mcp::{
    CanonicalDynamicMcpAction, DynamicMcpCatalogTool, DynamicMcpFailure, DynamicMcpInstanceKey,
    DynamicMcpNotification, DynamicMcpResponse, DynamicMcpShutdownReport, ResolvedSecret,
    SecretRef, SessionMcpCapabilitySnapshot,
};
use crate::mcp_skills::HandleToken;

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

/// MCP 客户端池端口（`peri-middlewares::mcp::McpClientPool` 实现）。
///
/// 宿主装配点构造 `McpClientPool` 后 upcast 注入；ACP 协议面只传递
/// 句柄，工具桥接/服务器管理在装配面宿主（`host/workflow_agent.rs` /
/// `host/stage_builder.rs`）。`shutdown` / `snapshot` 为 M-TUI 收口新增
/// 数据端口（`host/shutdown` 与 `mcp/list` 命令面经此访问，TUI 不再
/// 直持具体句柄）。
#[async_trait::async_trait]
pub trait McpPoolPort: Send + Sync {
    /// 还原具体实现（downcast 还原点，供 middlewares 装配面与装配面宿主使用）。
    fn as_any(&self) -> &dyn Any;

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

/// LSP 文档同步端口错误（A30 / IF-P3-09 冻结变体集）。
///
/// 变体最小集即够用：`NoServer`（无路由 server，或路由到的 server 未就绪）
/// 与 `Protocol`（协议 / JSON-RPC / 传输失败）。`reason` 由实现方写入**已脱敏**
/// 的固定规则文本：不得包含文件路径、文件内容、env 值、凭据或 token。
/// `Debug` / `Display` 只进 debug 日志，不进模型面工具文本。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LspSyncError {
    /// 无路由 server，或路由到的 server 未就绪（调用方不得补拉进程）。
    #[error("无可用 LSP 服务器")]
    NoServer,

    /// 协议 / JSON-RPC / 传输失败；`reason` 为已脱敏原因。
    #[error("LSP 文档同步失败: {reason}")]
    Protocol { reason: String },
}

/// LSP 服务器池端口（由 `peri-mcp-lsp::pool::LspServerPool` 实现）。
///
/// **host 级唯一实例**（A11/A21/A22）：构造点在宿主装配
/// （`peri-acp/src/host/assemble.rs` 经 `peri_mcp_lsp::create_host_lsp_pool`），
/// 同一 `Arc` 分两路消费——builtin `lsp` 实例的工具面（handler 构造时按
/// `has_servers()` 快照）与装配面 `AssemblyContext::lsp_pool` 投影
/// （`ChainSlot::Lsp` 只装 `LspSyncMiddleware`），**session 不创建也不销毁**。
/// 代价是已裁决的功能退化：多 cwd / 多 session 共享 host `root_uri`
/// （per-session 恢复见 wave 3 的 `ToolContext` 计划）。宿主退出
/// （`run_acp_server` / `run_acp_stdio` 返回）经 `shutdown` 优雅关闭全部服务器子进程。
///
/// [`as_any`](LspPoolPort::as_any) / [`downcast_arc`](dyn LspPoolPort::downcast_arc)
/// 是端口通用还原点（装配面已不再使用，保留给替身与诊断）；协议面只持端口句柄。
///
/// 文档同步能力（A23/A30）合流到本端口，不新增第二 LSP 端口：`ready_for` /
/// `did_change` / `did_save` 一律**不提供默认实现**——默认 no-op 会让端口替身
/// 静默通过，制造「同步已生效」的假绿；所有实现者必须显式实现。
#[async_trait::async_trait]
pub trait LspPoolPort: Send + Sync {
    /// 还原具体实现（downcast 还原点，供端口替身与诊断使用）。
    fn as_any(&self) -> &dyn Any;

    /// 优雅关闭全部服务器（发送 shutdown/exit 并终止子进程；幂等）。
    async fn shutdown(&self);

    /// 该路径当前是否有**可用且就绪**的 LSP 服务器（同步、廉价、只读）。
    ///
    /// 这是调用方**读文件之前的唯一前置判定**：返回 `false` ⇒ 调用方不得读取
    /// 文件、不得发送任何通知。实现只做扩展名路由与就绪查询（`server_for_file`
    /// + `is_ready`），**不启动 / 不拉起任何 language server**，无副作用。
    fn ready_for(&self, path: &std::path::Path) -> bool;

    /// 通知文件内容变更（`textDocument/didChange`）。
    ///
    /// `path` 是**文件系统绝对 path，不是 URI**；`path → file:// URI` 的转换
    /// 留在实现内部。端口只做协议工作：**不读文件**（内容由调用方读取后经
    /// `text` 传入）、不新增 capability root、不解析工具输入。
    /// 失败返回 typed [`LspSyncError`]，调用方只做 debug 降级。
    async fn did_change(&self, path: &std::path::Path, text: &str) -> Result<(), LspSyncError>;

    /// 通知文件已保存（`textDocument/didSave`）。path 与失败语义同
    /// [`LspPoolPort::did_change`]。
    async fn did_save(&self, path: &std::path::Path) -> Result<(), LspSyncError>;
}

impl dyn LspPoolPort {
    /// 将 `Arc<dyn LspPoolPort>` 还原为具体实现 `Arc<T>`（类型不符返回原 `Arc`）。
    pub fn downcast_arc<T: LspPoolPort + 'static>(self: Arc<Self>) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            // 经 `as_any()` 取具体类型的 TypeId：直接对 trait object 调
            // `type_id()` 会命中 `Any` 的 blanket impl，返回
            // `TypeId::of::<dyn LspPoolPort>()`（trait object 自身），
            // 恒不等于 `TypeId::of::<T>()` → downcast 恒失败 → 装配面回退
            // 临时实例，会话级 pool 与装配产物分离（同构
            // 2026-08-06-e2e-workflow-not-completing 遗留项）。
            if (*ptr).as_any().type_id() == TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

/// Agents 扫描端口：协议命令面（available-commands / agent 列表）经此访问 agents
/// 扫描业务，具体扫描逻辑留在 `peri-middlewares`（`scan_agents_detailed`）。
///
/// W4b（F6/J5）：原 `available_skills`（同步扫盘投影 `core:{skill}` 命令）已删除
/// ——技能目录的唯一来源是会话级 MCP skill registry，命令面投影随 MCP 发现异步
/// 产生（`peri-middlewares/src/mcp/skill_discovery.rs`），端口不再承担任何技能
/// 内容读取。
pub trait SkillsPort: Send + Sync {
    /// 扫描可调度 agent 目录，返回 `(agent_id, name, description, capability)`。
    fn agents(
        &self,
        cwd: &str,
        extra_dirs: &[PathBuf],
        include_built_ins: bool,
    ) -> Vec<(String, String, String, AgentCapability)>;
}

#[cfg(test)]
#[path = "ports_test.rs"]
mod tests;
