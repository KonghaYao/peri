//! ACP Host — transport-agnostic request handler（自 peri-tui 迁出归位）。
//!
//! Accepts any [`crate::transport::AcpTransport`] implementation (mpsc for TUI, stdio for IDE),
//! builds and executes ReAct agents, and pushes [`agent_client_protocol::schema::v1::SessionUpdate`] notifications
//! back through the transport. ACP Host = 部署单元（`docs/top-level.md` §7/§19）：
//! 由 cli/TUI 作为部署装配点启动，TUI 进程不再持有控制面。
//!
//! Prompt execution is owned by a background task. Sessions are shared via
//! `Arc<tokio::sync::Mutex<HashMap>>`.
//!
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

pub use crate::session::state_builders::{
    apply_profile_effort, apply_thinking_effort, build_config_options, build_mode_state,
    parse_permission_mode,
};
use peri_acp_types::command::command_route::RouteEntry;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::SettingsHooksPort;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::plugin::PluginManagerPort;
use peri_acp_types::ports::{AgentCatalogPort, McpPoolPort, McpTaskOwnerPort, ToolSearchPort};
use tokio_util::sync::CancellationToken;

use crate::provider::{LlmProvider, PeriConfig};

pub mod assemble;
pub(crate) mod compact_config;
mod connection;
mod lifecycle;
mod workspace;
/// session 级交互装配（idle 准入规则链 / 交互门取用）；见模块文档。
mod workspace_interaction;
#[cfg(not(target_os = "emscripten"))]
mod workspace_resources;
pub use lifecycle::{spawn_acp_server, AcpHostHandle, AcpHostShutdownReport};
// 关闭报告的遥测载荷：`AcpHostShutdownReport::TelemetryFailed` 携带的终态与计数快照。
// 部署面（TUI）必须能命名它们，才能把「遥测未收敛」与业务失败分开，而调用方不该为此
// 直依赖 peri-controller / langfuse-client（同 `lib.rs` 的 LangfuseSessionLike re-export）。
pub use langfuse_client::DeliveryCounts;
pub use peri_controller::langfuse::LangfuseShutdownReport;
mod activation;
mod continuation;
pub mod controller_ports;
mod diagnostics;
mod execution;
#[cfg(test)]
#[path = "executor_flow_test.rs"]
mod executor_flow_tests;
mod history_page;
/// session-owned 交互路由（按可信归属选审批 endpoint；投递失败原因的诚实产出点：
/// `NoOnlineEndpoint` / `OwnerUnmatched` / `AttachmentDetached`，见 `interaction_router.rs`）。
mod interaction_router;

/// MCP Apps facade（七个 `peri/mcp/apps/*` 方法）与请求账本（设计 4a §1.3/§4.4）。
mod apps_facade;
mod apps_ledger;
#[cfg(test)]
#[path = "executor_flow_beta_flag_test.rs"]
mod executor_flow_beta_flag_tests;
mod mcp_apps;
#[cfg(test)]
#[path = "mcp_apps_idle_test.rs"]
mod mcp_apps_idle_tests;
// V-02（W4）的 host seam 断言：首个 LLM 请求的三个冻结 effective name、能力关闭的
// 首个请求面、`PERI_MCP_BUILTIN=off` 语义、启动 fatal 投影、BLOCKED 缺口复证。
// 模块名参与 `cargo test` 过滤（`host::mcp_v4_builtin`），故不沿用 `mod tests`。
#[cfg(test)]
#[path = "mcp_v4_builtin_test.rs"]
mod mcp_v4_builtin;
#[cfg(test)]
#[path = "mcp_v4_startup_test.rs"]
mod mcp_v4_startup_tests;
// wave 1 的 host 侧 wire 夹具（主 plan A10）：node 脚本含 wire 日志与 `tools/call`
// 分支 + 复刻的工具调用 model 替身。owner 序列 V-06（W0 建 + 录迁移前基线）→
// V-02（W4 复用）。模块名参与 `cargo test` 过滤（`host::mcp_v4_wire_fixture`），
// 故不沿用 `mod tests`。文件名必须保留 `_test.rs` 后缀：本夹具经引用业务 crate
// （`peri_middlewares` / `peri_model`）验证行为，靠 `scripts/check-layer-imports.sh`
// 的测试文件豁免（`*_test.rs`）才不构成越层 import。
#[cfg(test)]
#[path = "mcp_v4_wire_fixture_test.rs"]
mod mcp_v4_wire_fixture;
// wave 2 的**终态**用例（H-04 / V-03）：自带「生产同构的 builtin host」夹具 —— pool →
// `BuiltinInstanceContext` 注入 → `run_initialize`（A33 的顺序），并驱动真实装配面
// （`assemble_server_config`）验证配置合并早于 handler 构造、多 cwd 退化登记与
// host shutdown 有界关闭。
#[cfg(test)]
#[path = "mcp_v4_wave2_test.rs"]
mod mcp_v4_wave2;
mod notify;
mod oauth_delivery;
mod prediction;
mod prediction_projection;
mod prepared;
#[cfg(test)]
#[path = "prepared_test.rs"]
mod prepared_tests;
mod prompt;
mod prompt_dispatch;
pub mod prompt_handle;
mod requests;
mod server_loop;
mod shutdown;
pub mod stage_builder;
#[cfg(not(target_os = "emscripten"))]
pub mod stdio;
mod task_scope;
/// 宿主遥测装配（暂存能力注入 + 关闭结果保有规则），见模块文档。
mod telemetry;
#[cfg(test)]
#[path = "unify_wire_baseline_test.rs"]
mod unify_wire_baseline_tests;
mod user_input;
/// Workflow agent 装配面薄壳（session 级运行时创建，见模块文档）。
/// `not(emscripten)`：本地 workflow 运行时随 `peri-mcp-workflow` 目标依赖移出
/// 构建面（wasm 产物不打包 workflow），该目标不提供 workflow 装配面。
#[cfg(not(target_os = "emscripten"))]
pub mod workflow_agent;
/// 宿主侧 workflow 任务消费者（P3 就绪与订阅接线，design `workflow-mcp` §5.5）。
/// `not(emscripten)`：依赖 builtin `workflow` 实例面（同目标段依赖 `peri-mcp-workflow`）。
#[cfg(not(target_os = "emscripten"))]
pub(crate) mod workflow_consumer;
#[cfg(all(test, not(target_os = "emscripten")))]
#[path = "workflow_consumer_test.rs"]
mod workflow_consumer_tests;
// wave 3（AW3-11 session 级 seam）**发送端**用例：会话环境装配产出的 per-session
// `TaskManager` + session 级 `on_bg_complete` 送进 builtin `workspace` 实例上下文，
// 并在会话登记处保持同一 `Arc`；回调经 inbox 把 Shell 完成投为 `Defer` 并唤醒。
// 模块名参与 `cargo test` 过滤（`host::workspace_seam`），故不沿用 `mod tests`。
#[cfg(test)]
#[path = "workspace_seam_test.rs"]
mod workspace_seam;

pub(crate) use continuation::{
    run_continuation_scheduler, run_cron_continuation_scheduler, CronContinuationContext,
};
pub(crate) use notify::{extract_session_id, handle_notification, send_session_info_update};
pub(crate) use prompt::run_prompt;
pub(crate) use prompt_dispatch::{dispatch_prompt_turn, PromptOrigin};
pub(crate) use requests::handle_request_for_connection;

// ── Session state ────────────────────────────────────────────────────────────

/// Host 程序化交互命名空间：cron / loop 调度触发的进入模型前审批。
///
/// 与 [`peri_acp_types::tool_invocation::InvocationOrigin::Host`] 同族（Host 入口
/// 命名空间）；交互归属用它 + session 级单调序号定位一次 Host 请求。
pub(crate) const SCHEDULED_TRIGGER_NAMESPACE: &str = "peri/cron/scheduled-trigger";

pub(crate) struct SessionState {
    // 以下字段 stdio 路径的 typed handler（host::stdio）需要读写，批 1 起
    // 提升为 pub(crate)；其余沿用 host 内部模块可见性。
    #[allow(dead_code)] // session 标识字段，保留供调试
    pub(crate) session_id: String,
    pub(crate) thread_id: String,
    pub(crate) cwd: String,
    pub(crate) environment: Option<Arc<workspace::SessionEnvironment>>,
    pub(crate) closing: bool,
    pub(crate) history: Vec<BaseMessage>,
    /// 当前**有效**的持久化历史（excluded 正文不在其中）；`history` 是它的 legacy 投影，
    /// 供旧命令消费。完整 canonical 历史（含 excluded 原文）只从存储读取。
    pub(crate) history_payloads: Vec<peri_acp_types::store::PersistedPayload>,
    /// 会话结构版本：**破坏历史范围**的提交（rewind / 精确删除）成功一次即 `+1`
    /// （REQ-T04 末段、验收 A19「旧 cursor 失效」）。
    ///
    /// 内存计数器，不落盘：本仓不加 DDL，且它保护的正是「本次运行内已发出的游标」。
    /// 进程重启后归零——重启后的旧游标仍要过存储那一关（锚点/范围身份查表）。
    /// **追加消息与 Full/Smart compact 不推进它**：REQ-H02 要求一次在途浏览不被它们打断。
    pub(crate) structure_version: u64,
    pub(crate) cancel_token: Option<CancellationToken>,
    /// **session-owned 交互互斥门**：同一 session 内 Approval / Questions 串行。
    ///
    /// 生命周期 = 本 session 状态（跨轮存活），不随某一轮的 broker 重建：
    /// `run_prompt` 每轮新建 `AcpTransportBroker`（ACP transport 是可替换
    /// endpoint），但都借用本句柄，跨轮互斥因此成立（设计 §4.3）。session
    /// 关闭/重建（`session/close` → 本结构 remove；`session/load` 新建）后
    /// 得到新门，旧门的等待者随旧 session 一起结束。
    pub(crate) interaction_gate: Arc<tokio::sync::Mutex<()>>,
    /// **session 级 Host 入口请求序号**：Host 程序化交互归属
    /// （[`peri_acp_types::interaction::TrustedInteractionOwner::Host`]）的
    /// `seq` 单调来源，同一 session 内只增不减。
    pub(crate) host_request_seq: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) continuation_armed: bool,
    pub(crate) continuation_epoch: u64,
    pub(crate) continuation_in_flight: bool,
    pub(crate) continuation_mq_steering_pending: bool,
    // ── Frozen session data (populated at creation, immutable thereafter) ──
    pub(crate) frozen: Option<crate::session::executor::FrozenSessionData>,
    /// Recall items from previous turn (injected as <system-reminder> in next user message).
    pub(crate) recall_items: Vec<String>,
    /// Session-scoped agent component pool for reusing heavy objects across prompts.
    pub(crate) agent_pool: crate::session::agent_pool::AgentPool,
    /// Session 级 workflow 完成通知源（P5：替代旧链 middleware 端口；
    /// 运行时归 builtin `workflow` 实例，本句柄只承载完成通知订阅）。
    pub(crate) workflow_notifications: Option<Arc<peri_acp_types::workflow::WorkflowNotifications>>,
    // ── Prediction 写入的会话元数据（MVP：仅存储，不展示）──
    /// 预测生成的会话标题（未来 /rename 与标题栏显示使用）。
    pub(crate) title: Option<String>,
    /// 预测生成的会话标签（未来按标签检索使用）。
    pub(crate) tags: Vec<String>,
}

// ── Server config ────────────────────────────────────────────────────────────

/// All cross-session configuration needed by the ACP server.
pub struct AcpServerConfig {
    pub(crate) workspace_assembly: Option<assemble::WorkspaceAssembly>,
    pub(crate) host_task_owner: Option<task_scope::HostTaskOwner>,
    pub(crate) host_task_spawner: task_scope::HostTaskSpawner,
    pub(crate) mcp_task_owner: Option<Box<dyn McpTaskOwnerPort>>,
    pub provider: Arc<parking_lot::RwLock<LlmProvider>>,
    pub peri_config: Arc<parking_lot::RwLock<PeriConfig>>,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    pub mcp_pool: Option<Arc<dyn McpPoolPort>>,
    /// MCP over ACP 服务（会话 setup 声明的 `type: "acp"` server 的连接事实）。
    ///
    /// 会话级字段：由会话工作区装配持有（每个会话各有一个 MCP 池），host 级
    /// 装配（`session_resources == false`）为 `None`——那时没有池可承载连接。
    /// 会话 setup 经 [`requests::acp_mcp`] 登记声明，入站 `mcp/message` 同样
    /// 经它在各会话间定位承载者。
    pub(crate) acp_mcp: Option<Arc<dyn peri_acp_types::ports::AcpMcpServerPort>>,
    /// Optional stdio-only MCP Apps backend. Absence keeps the capability fail closed.
    pub mcp_apps_relay: Option<Arc<dyn peri_acp_types::mcp_apps::McpAppsRelayPort>>,
    /// 会话级 MCP Apps facade（七个 `peri/mcp/apps/*` 方法的唯一实现）。
    ///
    /// **会话级字段**：由会话环境装配点（`host/workspace.rs`）与 relay 同点同源
    /// 构造（同一 router / 同一 runtime epoch）；apps 能力关闭或未装配会话环境时
    /// 为 `None` ⇒ 请求显式拒绝（fail-closed，不静默降级）。
    pub(crate) mcp_apps_facade: Option<Arc<apps_facade::SessionAppsFacade>>,
    /// session entitlement 消费端口（裁决 R4 方案 b：**只接线不注入**）。
    ///
    /// **部署级字段**：由 SDK / 服务化部署的装配层注入可信身份层实现；本仓 stdio
    /// 装配（`assemble_stdio_config` 等）恒为 `None`，会话环境装配把它原样传给
    /// facade 的 entitlement 门 ⇒ `apps/*` 六个受控方法按契约**默认拒绝**
    /// （`entitlement_denied`）。本地部署的 Apps 面因此不可用，这是既定代价
    /// （`implementation-log.md`「已知缺口与移除条件」）。
    ///
    /// 与 `peri_acp_types::session::SessionAccessPort` **不得互相替代**：它回答
    /// 「这个调用者是否有这个 session 的这项授权」，不是「按 id 取得到哪个对象」。
    pub session_authorization:
        Option<Arc<dyn peri_acp_types::session_authorization::SessionAuthorizationPort>>,
    pub dynamic_mcp: Option<Arc<dyn peri_acp_types::ports::DynamicMcpDeploymentPort>>,
    /// OAuth 授权事件通道（host 级，跨 session）：装配点创建 (tx, rx) 并注入
    /// tx（MCP 授权回调经此转发 AcpEvent），run_acp_server take rx 后 spawn
    /// 消费者 task，以 `peri/agent_event` notification（sessionId 为空串，
    /// host 级事件不做 session 过滤）送达 TUI。
    pub oauth_event_tx:
        Option<tokio::sync::mpsc::UnboundedSender<crate::event::oauth::HostOAuthEvent>>,
    pub(crate) oauth_event_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<crate::event::oauth::HostOAuthEvent>>,
    pub plugin_skill_roots: Vec<peri_acp_types::skills::SkillRoot>,
    /// 插件命令静态条目（Phase 6 B2：`plugin_data.all_commands` 经
    /// `plugin_route_entries` 预转；会话创建时 register_all，注册顺序 =
    /// 内置 → 本地 skills（C1）→ 插件（本字段）→ 动态注入（发现管线异步））。
    pub plugin_command_entries: Vec<RouteEntry>,
    /// 插件来源闭合位（M6）：由 frozen/session-local 策略派生，随装配注入。
    ///
    /// 消费面是**命令面的运行期刷新**（`requests/plugin.rs` 的 install /
    /// uninstall RPC）——关闭的会话不得经一次管理 RPC 重新拿到可执行插件命令；
    /// 与 `plugin_command_entries` 同批决定，不在 RPC 里回读配置。
    pub plugin_face_closed: bool,
    pub plugin_hooks: Vec<peri_acp_types::hooks::RegisteredHook>,
    /// 仅插件 hooks（不含 settings hooks；`plugin/list` 命令面数据源——
    /// TUI hooks 面板经 ACP 拿数据，M-TUI 收口）。
    pub plugin_hooks_only: Vec<peri_acp_types::hooks::RegisteredHook>,
    pub plugin_loaded: Vec<peri_acp_types::plugin::LoadedPlugin>,
    pub hook_groups: Vec<Vec<peri_acp_types::hooks::RegisteredHook>>,
    pub tool_search_index: Arc<dyn ToolSearchPort>,
    /// Skills 扫描端口（available-commands / agents 扫描经此访问）。
    pub agent_catalog: Arc<dyn AgentCatalogPort>,
    /// 插件管理端口（plugin/* 命令面经此访问）。
    pub plugin_manager: Arc<dyn PluginManagerPort>,
    /// Settings hooks 加载端口（hook 组装配经此访问）。
    pub settings_hooks: Arc<dyn SettingsHooksPort>,
    pub shared_tools:
        Arc<parking_lot::RwLock<BTreeMap<String, Arc<dyn peri_agent::tools::BaseTool>>>>,
    /// Workflow agent 装配端口（peri-middlewares 实现，TUI 部署装配点构造后
    /// 经 [`assemble::HostAssemblyInput`] 注入；p1-wa 收口——ACP 不直接
    /// 引用 middlewares，见 `host/workflow_agent.rs`）。
    ///
    /// `not(emscripten)`：工厂实现依赖本地 workflow 运行时（`peri-mcp-workflow`
    /// 随目标移出构建面），该目标不装配本地 workflow 运行时路径。
    #[cfg(not(target_os = "emscripten"))]
    pub workflow_middleware_factory: Arc<dyn peri_agent::agent::workflow::WorkflowAgentFactory>,
    /// 会话资源门面：协议面、Agent transcript/subagent 与 middleware 的唯一会话行为
    /// 入口（同一个库句柄、同一份 owner 登记）；协议面与 Controller 都不再持有裸存储。
    pub session_resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
    /// 部署关闭权（non-Clone，装配点注入）：宿主在任务排空之后用它关闭会话存储。
    ///
    /// 只有部署（TUI/print/stdio 装配点）注入；会话级配置与测试为 `None`，业务侧
    /// （SessionManager/Controller/middleware）拿到的只有 `session_resources` 业务句柄，
    /// 没有任何关闭全局存储的路径。
    pub(crate) session_store_shutdown:
        Option<Box<dyn peri_acp_types::session_resources::SessionStoreShutdownPort>>,
    /// Controller 层宿主：dispatch 存储操作（load/list/fork/execute-command/rewind）
    /// 经此访问持久化存储（ARC-BOUNDARY-001 方向，不再直操 `thread_store`）；
    /// 3.0 批 2：事件发射（`publish_event`）/ 执行发起（`run_session`）亦经此宿主。
    pub controller: Arc<peri_controller::Controller>,
    pub langfuse_session: Option<Arc<peri_controller::langfuse::LangfuseSession>>,
    /// Only fresh assembly grants shutdown authority; externally injected shared sessions do not.
    pub(crate) langfuse_shutdown_owner: Option<peri_controller::langfuse::LangfuseShutdownOwner>,
    /// 配置源（读写路径决策的唯一事实源；`persist_config` 经此写回生效层，
    /// 与加载共享同一路径决策，见 `provider::store::ConfigSource`）。
    pub config_source: Arc<crate::provider::ConfigSource>,
    /// 共享 SessionManager：用于支撑 cascade cancel 子 agent 与 goal_state。
    ///
    /// TUI 本地仍维护 SessionState（history/frozen/agent_pool 等），但 SubAgent
    /// 注册/注销与 goal_state 通过 SessionManager 中的 AcpSession 记录管理，
    /// 保证 `run_session_loop` 接收 `Some(session_manager)` 时 cascade cancel 生效。
    pub session_manager: crate::session::SessionManager,
    /// stdio 部署命令过滤开关。
    ///
    /// 仅影响 stdio 部署单元（IDE client，`assemble_stdio_config` 置 `true`）；
    /// TUI / print 恒为 `false`（保留全部命令）。为 `true` 时，`rewind` /
    /// `clear`（含别名 `cls`/`reset`）不作为命令拦截，fall-through 进 agent
    /// 管线（当作普通文本消息发给模型）——IDE 客户端自管理这两个命令，服务端
    /// 不应执行清会话/回退操作。其余命令不受影响。
    pub stdio_command_filter: bool,
}

// ── Main server loop ────────────────────────────────────────────────────────

type SharedSessions = Arc<tokio::sync::Mutex<HashMap<String, SessionState>>>;
/// Per-session prompt serialization lock map（与 prompt dispatch 共用，
/// continuation scheduler 通过同一把锁串行化内部续跑）。
pub(crate) type PromptLocks = Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;

/// Main ACP server loop. Accepts any `AcpTransport` (mpsc for TUI, stdio for IDE).
///
/// `session/prompt` is spawned into a background task so the loop stays
/// responsive to `session/cancel` and other incoming messages.
///
/// **内部 AsyncContinuation**：spawn 一个 per-session coalesce 的 continuation
/// scheduler（见 [`run_continuation_scheduler`]）。被取消的 prompt 若有独立 bg
/// agent 结果完成（executor `on_bg_complete` 闭包已先 route 到 SessionInbox），
/// scheduler 合并 MQ 通知并重验持久控制状态后通过与用户 prompt
/// 相同的执行路径（pool / prompt lock / run_prompt 后处理）发起一次内部续跑。
pub async fn run_acp_server(
    transport: Arc<dyn crate::transport::AcpTransport>,
    cfg: AcpServerConfig,
) {
    let mut handle = spawn_acp_server(transport, cfg);
    let _ = handle.shutdown().await;
}

/// stdio 宿主入口：注入**调用方持有的**共享 session 集合（批 3 §7 #10：
/// legacy `type:cancel` 全 session 兜底中断回调需与宿主遍历同一 session map——
/// stdio 装配点构造 transport 时已注入取消回调，因此 session map 必须由装配点
/// 创建并注入，不能由本函数私建）。
pub(crate) async fn run_acp_server_with_sessions(
    transport: Arc<dyn crate::transport::AcpTransport>,
    cfg: AcpServerConfig,
    sessions: SharedSessions,
) {
    let mut handle = lifecycle::spawn_with_sessions(transport, cfg, sessions);
    let _ = handle.shutdown().await;
}

async fn run_acp_server_inner(
    transport: Arc<dyn crate::transport::AcpTransport>,
    mut cfg: AcpServerConfig,
    sessions: SharedSessions,
) -> lifecycle::HostExitContext {
    // Keep the sole strong task owner on this stack. The config captured by
    // tasks contains only its weak spawner.
    let task_owner = cfg
        .host_task_owner
        .take()
        .expect("AcpServerConfig missing HostTaskOwner");
    let mcp_task_owner = cfg
        .mcp_task_owner
        .take()
        .expect("AcpServerConfig missing McpTaskOwner");
    // OAuth 授权事件消费者：host 级事件（无 session 归属）。专用安全通道与
    // legacy TUI 通道分别按 initialize 协商值门控，互不隐式开启。
    let oauth_event_rx = cfg.oauth_event_rx.take();
    let cfg = Arc::new(cfg);
    oauth_delivery::spawn_oauth_consumer(oauth_event_rx, &transport, &cfg);
    let sessions: SharedSessions = sessions;
    // Per-session prompt serialization lock: ensures that when a prompt completes
    // (state.history updated) the next prompt for the same session sees the updated history.
    let prompt_locks: PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    // 内部 continuation 通知通道：executor on_bg_complete 闭包 → scheduler。
    let (cont_tx, cont_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::session::executor::ContinuationRequest>();
    let cont_tx = Arc::new(cont_tx);
    let (cron_cont_tx, cron_cont_rx) = tokio::sync::mpsc::unbounded_channel();
    cfg.session_manager.bind_cron_continuation(cron_cont_tx);
    let continuation_spawner = cfg.host_task_spawner.clone();
    let continuation_shutdown = cfg.host_task_spawner.shutdown_token();
    let _ = cfg.host_task_spawner.spawn(
        task_scope::HostTaskOwnerKind::Host,
        task_scope::HostTaskKind::ContinuationScheduler,
        run_continuation_scheduler(
            cont_rx,
            sessions.clone(),
            prompt_locks.clone(),
            Arc::clone(&cfg),
            Arc::clone(&transport),
            Arc::downgrade(&cont_tx),
            continuation_spawner.clone(),
            continuation_shutdown.clone(),
        ),
    );
    let _ = cfg.host_task_spawner.spawn(
        task_scope::HostTaskOwnerKind::Host,
        task_scope::HostTaskKind::ContinuationScheduler,
        run_cron_continuation_scheduler(
            cron_cont_rx,
            CronContinuationContext {
                sessions: sessions.clone(),
                prompt_locks: prompt_locks.clone(),
                cont_tx: Arc::clone(&cont_tx),
                cfg: Arc::clone(&cfg),
                transport: Arc::clone(&transport),
                task_spawner: continuation_spawner,
                shutdown: continuation_shutdown,
            },
        ),
    );

    let connection = Arc::new(tokio::sync::Mutex::new(connection::ConnectionContext::new(
        cfg.stdio_command_filter
            && (cfg.mcp_apps_relay.is_some()
                || cfg
                    .workspace_assembly
                    .as_ref()
                    .is_some_and(|source| source.mcp_profile.apps_enabled())),
    )));
    let connection_cancellation = connection.lock().await.cancellation();
    server_loop::ServerLoop {
        transport: &transport,
        cfg: &cfg,
        sessions: &sessions,
        prompt_locks: &prompt_locks,
        cont_tx: &cont_tx,
        connection: &connection,
        connection_cancellation: &connection_cancellation,
    }
    .run()
    .await;

    lifecycle::HostExitContext {
        task_owner,
        mcp_task_owner,
        cfg,
        sessions,
        prompt_locks,
        cont_tx: Some(cont_tx),
        connection,
        connection_cancellation,
        closing_sessions: std::collections::BTreeMap::new(),
    }
}

#[cfg(test)]
#[path = "oauth_delivery_test.rs"]
mod oauth_cap_tests;
