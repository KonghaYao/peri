//! ACP Host — transport-agnostic request handler（自 peri-tui 迁出归位）。
//!
//! Accepts any [`crate::transport::AcpTransport`] implementation (mpsc for TUI, stdio for IDE),
//! builds and executes ReAct agents, and pushes [`agent_client_protocol::schema::v1::SessionUpdate`] notifications
//! back through the transport. ACP Host = 部署单元（`docs/top-level.md` §7/§19）：
//! 由 cli/TUI 作为部署装配点启动，TUI 进程不再持有控制面。
//!
//! **Cancel architecture**: `session/prompt` execution is spawned into a
//! background tokio task so the main server loop remains responsive to
//! `session/cancel` notifications. Sessions are shared via
//! `Arc<tokio::sync::Mutex<HashMap>>`.
//!
//! **多读者 + 单 writer lease**（[`lease`]）：每个 session 的 writer 唯一
//! （可提交输入/取消），观察者只读。策略先行，协议级扩展另立 issue。

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
use peri_acp_types::interaction::ChannelState;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::plugin::PluginManagerPort;
use peri_acp_types::ports::{
    LspPoolPort, McpPoolPort, McpTaskOwnerPort, SkillsPort, ToolSearchPort, WorkflowMiddlewarePort,
};
use tokio_util::sync::CancellationToken;

use crate::provider::{LlmProvider, PeriConfig};

pub mod assemble;
pub(crate) mod compact_config;
mod connection;
mod continuation;
pub mod controller_ports;
#[cfg(test)]
#[path = "executor_flow_test.rs"]
mod executor_flow_tests;
pub mod lease;
mod mcp_apps;
mod notify;
mod oauth_delivery;
mod prediction;
mod prediction_projection;
mod prompt;
mod prompt_dispatch;
pub mod prompt_handle;
mod requests;
mod server_loop;
mod shutdown;
pub mod stage_builder;
pub mod stdio;
mod task_scope;
#[cfg(test)]
#[path = "unify_wire_baseline_test.rs"]
mod unify_wire_baseline_tests;
pub mod workflow_agent;

pub(crate) use continuation::{
    run_continuation_scheduler, run_cron_continuation_scheduler, CronContinuationContext,
};
pub(crate) use notify::{extract_session_id, handle_notification, send_session_info_update};
pub(crate) use prompt::run_prompt;
pub(crate) use prompt_dispatch::dispatch_prompt_turn;
pub(crate) use requests::handle_request;

// ── Session state ────────────────────────────────────────────────────────────

pub(crate) struct SessionState {
    // 以下字段 stdio 路径的 typed handler（host::stdio）需要读写，批 1 起
    // 提升为 pub(crate)；其余沿用 host 内部模块可见性。
    #[allow(dead_code)] // session 标识字段，保留供调试
    pub(crate) session_id: String,
    pub(crate) thread_id: String,
    pub(crate) cwd: String,
    pub(crate) history: Vec<BaseMessage>,
    /// Canonical persisted history; `history` is a compatibility projection for legacy commands.
    pub(crate) history_payloads: Vec<peri_acp_types::store::PersistedPayload>,
    pub(crate) cancel_token: Option<CancellationToken>,
    // ── Frozen session data (populated at creation, immutable thereafter) ──
    pub(crate) frozen: Option<crate::session::executor::FrozenSessionData>,
    /// Recall items from previous turn (injected as <system-reminder> in next user message).
    pub(crate) recall_items: Vec<String>,
    /// Session-scoped agent component pool for reusing heavy objects across prompts.
    pub(crate) agent_pool: crate::session::agent_pool::AgentPool,
    /// Session 级 WorkflowMiddleware（session/new 时创建，跨 turn 复用）。
    pub(crate) workflow_middleware: Option<Arc<dyn WorkflowMiddlewarePort>>,
    /// Session 级 LSP 服务器池（session/new 时创建，跨 turn 复用；H1）。
    pub(crate) lsp_pool: Option<Arc<dyn LspPoolPort>>,
    // ── Prediction 写入的会话元数据（MVP：仅存储，不展示）──
    /// 预测生成的会话标题（未来 /rename 与标题栏显示使用）。
    pub(crate) title: Option<String>,
    /// 预测生成的会话标签（未来按标签检索使用）。
    pub(crate) tags: Vec<String>,
    // ── 内部 AsyncContinuation 调度状态（private，仅 scheduler/notify 访问）──
    /// 被取消 prompt 的续跑标记：`session/cancel` 置位（只影响当前 prompt，
    /// 即 cancel 时正在运行的那一轮）；bg agent 完成通知到达 scheduler 后
    /// 原子 take，只运行一次。用户显式新 prompt 清除未运行的标记。
    continuation_armed: bool,
    /// prompt 代际计数：每次用户显式 prompt 递增。continuation 在 take 之后、
    /// 获取 prompt lock 之后校验代际未变——用户新 prompt 可清掉已排队但
    /// 尚未运行的 continuation。
    continuation_epoch: u64,
    /// 当前是否有 continuation 在执行（dispatch_prompt_turn 置位、结束时清除，
    /// 与 pool 取出/归还同一临界区）。`session/cancel` 取消的是续跑本身时
    /// 排除置位 armed——否则会形成"取消续跑 → 再续跑"的自动链式续跑。
    continuation_in_flight: bool,
    /// 下一次 continuation dispatch 按 MQ steering 校验（非 SubAgentComplete）。
    continuation_mq_steering_pending: bool,
    /// 多读者 + 单 writer lease：session 创建方（writer）唯一可提交输入/取消。
    ///
    /// 协议无客户端身份字段（`clientId` 属协议级扩展，另立 issue），writer 恒为
    /// `"default"`；prompt/cancel 入口经 [`lease::WriterLease::is_writer`] 校验。
    pub(crate) lease: lease::WriterLease,
}

// ── Server config ────────────────────────────────────────────────────────────

/// All cross-session configuration needed by the ACP server.
pub struct AcpServerConfig {
    pub(crate) host_task_owner: Option<task_scope::HostTaskOwner>,
    pub(crate) host_task_spawner: task_scope::HostTaskSpawner,
    pub(crate) mcp_task_owner: Option<Box<dyn McpTaskOwnerPort>>,
    pub provider: Arc<parking_lot::RwLock<LlmProvider>>,
    pub peri_config: Arc<parking_lot::RwLock<PeriConfig>>,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    pub mcp_pool: Option<Arc<dyn McpPoolPort>>,
    /// Optional stdio-only MCP Apps backend. Absence keeps the capability fail closed.
    pub mcp_apps_relay: Option<Arc<dyn peri_acp_types::mcp_apps::McpAppsRelayPort>>,
    pub dynamic_mcp: Option<Arc<dyn peri_acp_types::ports::DynamicMcpDeploymentPort>>,
    /// OAuth 授权事件通道（host 级，跨 session）：装配点创建 (tx, rx) 并注入
    /// tx（MCP 授权回调经此转发 AcpEvent），run_acp_server take rx 后 spawn
    /// 消费者 task，以 `peri/agent_event` notification（sessionId 为空串，
    /// host 级事件不做 session 过滤）送达 TUI。
    pub oauth_event_tx:
        Option<tokio::sync::mpsc::UnboundedSender<crate::event::oauth::HostOAuthEvent>>,
    pub(crate) oauth_event_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<crate::event::oauth::HostOAuthEvent>>,
    pub channel_state: Option<Arc<ChannelState>>,
    pub plugin_skill_roots: Vec<peri_acp_types::skills::SkillRoot>,
    /// 插件命令静态条目（Phase 6 B2：`plugin_data.all_commands` 经
    /// `plugin_route_entries` 预转；会话创建时 register_all，注册顺序 =
    /// 内置 → 本地 skills（C1）→ 插件（本字段）→ 动态注入（发现管线异步））。
    pub plugin_command_entries: Vec<RouteEntry>,
    pub plugin_agent_dirs: Vec<std::path::PathBuf>,
    pub plugin_hooks: Vec<peri_acp_types::hooks::RegisteredHook>,
    /// 仅插件 hooks（不含 settings hooks；`plugin/list` 命令面数据源——
    /// TUI hooks 面板经 ACP 拿数据，M-TUI 收口）。
    pub plugin_hooks_only: Vec<peri_acp_types::hooks::RegisteredHook>,
    pub plugin_loaded: Vec<peri_acp_types::plugin::LoadedPlugin>,
    pub hook_groups: Vec<Vec<peri_acp_types::hooks::RegisteredHook>>,
    pub plugin_lsp_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
    pub tool_search_index: Arc<dyn ToolSearchPort>,
    /// Skills 扫描端口（available-commands / agents 扫描经此访问）。
    pub skills: Arc<dyn SkillsPort>,
    /// 插件管理端口（plugin/* 命令面经此访问）。
    pub plugin_manager: Arc<dyn PluginManagerPort>,
    /// Settings hooks 加载端口（hook 组装配经此访问）。
    pub settings_hooks: Arc<dyn SettingsHooksPort>,
    pub shared_tools:
        Arc<parking_lot::RwLock<BTreeMap<String, Arc<dyn peri_agent::tools::BaseTool>>>>,
    /// Workflow agent 装配端口（peri-middlewares 实现，TUI 部署装配点构造后
    /// 经 [`assemble::HostAssemblyInput`] 注入；p1-wa 收口——ACP 不直接
    /// 引用 middlewares，见 `host/workflow_agent.rs`）。
    pub workflow_middleware_factory:
        Arc<dyn peri_agent::agent::workflow::WorkflowMiddlewareFactory>,
    pub thread_store: Arc<dyn peri_acp_types::store::ThreadStore>,
    /// Controller 层宿主：dispatch 存储操作（load/list/fork/execute-command/rewind）
    /// 经此访问持久化存储（ARC-BOUNDARY-001 方向，不再直操 `thread_store`）；
    /// 3.0 批 2：事件发射（`publish_event`）/ 执行发起（`run_session`）亦经此宿主。
    pub controller: Arc<peri_controller::Controller>,
    pub langfuse_session: Option<Arc<peri_controller::langfuse::LangfuseSession>>,
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
/// scheduler 原子 take `SessionState::continuation_armed` 后通过与用户 prompt
/// 相同的执行路径（pool / prompt lock / run_prompt 后处理）发起一次内部续跑。
pub async fn run_acp_server(
    transport: Arc<dyn crate::transport::AcpTransport>,
    cfg: AcpServerConfig,
) {
    let sessions: SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    run_acp_server_inner(transport, cfg, sessions).await;
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
    run_acp_server_inner(transport, cfg, sessions).await;
}

async fn run_acp_server_inner(
    transport: Arc<dyn crate::transport::AcpTransport>,
    mut cfg: AcpServerConfig,
    sessions: SharedSessions,
) {
    // Keep the sole strong task owner on this stack. The config captured by
    // tasks contains only its weak spawner.
    let mut task_owner = cfg
        .host_task_owner
        .take()
        .expect("AcpServerConfig missing HostTaskOwner");
    let mut mcp_task_owner = cfg
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
                cfg: Arc::clone(&cfg),
                transport: Arc::clone(&transport),
                cont_tx: Arc::clone(&cont_tx),
                task_spawner: continuation_spawner,
                shutdown: continuation_shutdown,
            },
        ),
    );

    let connection = Arc::new(tokio::sync::Mutex::new(connection::ConnectionContext::new(
        cfg.stdio_command_filter && cfg.mcp_apps_relay.is_some(),
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

    shutdown::shutdown_host(
        &mut task_owner,
        mcp_task_owner.as_mut(),
        &cfg,
        &sessions,
        &prompt_locks,
        cont_tx,
        &connection,
        &connection_cancellation,
    )
    .await;
}

#[cfg(test)]
#[path = "oauth_delivery_test.rs"]
mod oauth_cap_tests;
