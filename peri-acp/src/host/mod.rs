//! ACP Host — transport-agnostic request handler（自 peri-tui 迁出归位）。
//!
//! Accepts any [`AcpTransport`] implementation (mpsc for TUI, stdio for IDE),
//! builds and executes ReAct agents, and pushes [`SessionUpdate`] notifications
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
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use crate::dispatch::prompt::extract_prompt_params;
pub use crate::session::state_builders::{
    apply_profile_effort, apply_thinking_effort, build_config_options, build_mode_state,
    parse_permission_mode,
};
use crate::transport::types::{AcpError, IncomingMessage};
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
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::provider::{LlmProvider, PeriConfig};
use peri_acp_types::event_data::PredictionAction;

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
mod prediction_projection;
mod prompt;
pub mod prompt_handle;
mod requests;
pub mod stage_builder;
pub mod stdio;
mod task_scope;
#[cfg(test)]
#[path = "unify_wire_baseline_test.rs"]
mod unify_wire_baseline_tests;
pub mod workflow_agent;

pub(crate) use continuation::run_continuation_scheduler;
pub(crate) use notify::{extract_session_id, handle_notification, send_session_info_update};
pub(crate) use prompt::run_prompt;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OAuthDeliveryPolicy {
    safe: bool,
    legacy: bool,
}

fn oauth_delivery_policy(caps: &peri_acp_types::PeriCaps) -> OAuthDeliveryPolicy {
    OAuthDeliveryPolicy {
        safe: caps.oauth,
        legacy: caps.agent_event,
    }
}

async fn send_safe_oauth_event(
    transport: &Arc<dyn crate::transport::AcpTransport>,
    notification: Result<
        crate::event::oauth::OAuthWireNotification,
        crate::event::oauth::OAuthWireError,
    >,
) {
    match notification {
        Ok(notification) => {
            if let Err(error) = transport
                .send_notification("peri/oauth", notification.into_params())
                .await
            {
                tracing::debug!(error = %error, "safe OAuth notification send failed");
            }
        }
        Err(error) => tracing::warn!(
            error = %error,
            "OAuth notification rejected by safe wire boundary"
        ),
    }
}

async fn send_legacy_oauth_event(
    transport: &Arc<dyn crate::transport::AcpTransport>,
    event: crate::event::AcpEvent,
) {
    let event_json = match serde_json::to_string(&event) {
        Ok(json) => json,
        Err(error) => {
            tracing::error!(error = %error, "legacy OAuth event serialize failed");
            return;
        }
    };
    if let Err(error) = transport
        .send_notification(
            "peri/agent_event",
            serde_json::json!({
                "sessionId": "",
                "event_json": event_json,
            }),
        )
        .await
    {
        tracing::debug!(error = %error, "legacy OAuth notification send failed");
    }
}

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
    if let Some(mut rx) = oauth_event_rx {
        let oauth_transport = Arc::clone(&transport);
        let oauth_sessions = cfg.session_manager.clone();
        let dynamic_mcp = cfg.dynamic_mcp.clone();
        let shutdown = cfg.host_task_spawner.shutdown_token();
        let _ = cfg.host_task_spawner.spawn(
            task_scope::HostTaskOwnerKind::Host,
            task_scope::HostTaskKind::OAuthConsumer,
            async move {
            loop {
                let event = tokio::select! {
                    _ = shutdown.cancelled() => break,
                    event = rx.recv() => match event { Some(event) => event, None => break },
                };
                let caps = oauth_sessions.effective_host_caps();
                let policy = oauth_delivery_policy(&caps);
                match event {
                    crate::event::oauth::HostOAuthEvent::DynamicAuthorizationNeeded {
                        instance,
                        flow_id,
                        server_name: _,
                        authorization_url,
                    } => {
                        let Some(deployment) = dynamic_mcp.as_ref() else {
                            continue;
                        };
                        if policy.safe {
                            let _ = deployment.notify_authorization_needed(
                                &instance,
                                &flow_id,
                                &authorization_url,
                            );
                        }
                    }
                    crate::event::oauth::HostOAuthEvent::AuthorizationNeeded {
                        flow_id,
                        server_name,
                        authorization_url,
                    } => {
                        if policy.safe {
                            match crate::event::oauth::OAuthWireNotification::authorization_needed(
                                flow_id.clone(),
                                server_name.clone(),
                                authorization_url.clone(),
                            ) {
                                Ok(notification) => {
                                    let _ = oauth_transport
                                        .send_notification("peri/oauth", notification.into_params())
                                        .await;
                                }
                                Err(error) => tracing::warn!(
                                    error = %error,
                                    "OAuth notification rejected by safe wire boundary"
                                ),
                            }
                        }
                        if policy.legacy {
                            send_legacy_oauth_event(
                                &oauth_transport,
                                crate::event::AcpEvent::OauthNeeded {
                                    server_name,
                                    auth_url: authorization_url,
                                },
                            )
                            .await;
                        }
                    }
                    crate::event::oauth::HostOAuthEvent::Completed {
                        flow_id,
                        server_name,
                    } => {
                        if policy.safe {
                            send_safe_oauth_event(
                                &oauth_transport,
                                crate::event::oauth::OAuthWireNotification::terminal(
                                    flow_id,
                                    server_name.clone(),
                                    crate::event::oauth::OAuthWireStatus::Completed,
                                ),
                            )
                            .await;
                        }
                        if policy.legacy {
                            send_legacy_oauth_event(
                                &oauth_transport,
                                crate::event::AcpEvent::OauthCompleted { server_name },
                            )
                            .await;
                        }
                    }
                    crate::event::oauth::HostOAuthEvent::Failed {
                        flow_id,
                        server_name,
                        failure_class,
                        legacy_error,
                    } => {
                        if policy.safe {
                            send_safe_oauth_event(
                                &oauth_transport,
                                crate::event::oauth::OAuthWireNotification::failed(
                                    flow_id,
                                    server_name.clone(),
                                    failure_class,
                                ),
                            )
                            .await;
                        }
                        if policy.legacy {
                            send_legacy_oauth_event(
                                &oauth_transport,
                                crate::event::AcpEvent::OauthFailed {
                                    server_name,
                                    error: legacy_error,
                                },
                            )
                            .await;
                        }
                    }
                    crate::event::oauth::HostOAuthEvent::Cancelled {
                        flow_id,
                        server_name,
                    } => {
                        if policy.safe {
                            send_safe_oauth_event(
                                &oauth_transport,
                                crate::event::oauth::OAuthWireNotification::terminal(
                                    flow_id,
                                    server_name.clone(),
                                    crate::event::oauth::OAuthWireStatus::Cancelled,
                                ),
                            )
                            .await;
                        }
                        if policy.legacy {
                            send_legacy_oauth_event(
                                &oauth_transport,
                                crate::event::AcpEvent::OauthFailed {
                                    server_name,
                                    error: "OAuth authorization cancelled".to_string(),
                                },
                            )
                            .await;
                        }
                    }
                    crate::event::oauth::HostOAuthEvent::Restored {
                        flow_id,
                        server_name,
                    } => {
                        if policy.safe {
                            send_safe_oauth_event(
                                &oauth_transport,
                                crate::event::oauth::OAuthWireNotification::terminal(
                                    flow_id,
                                    server_name.clone(),
                                    crate::event::oauth::OAuthWireStatus::Restored,
                                ),
                            )
                            .await;
                        }
                        if policy.legacy {
                            send_legacy_oauth_event(
                                &oauth_transport,
                                crate::event::AcpEvent::OauthRestored { server_name },
                            )
                            .await;
                        }
                    }
                }
            }
        });
    }
    let sessions: SharedSessions = sessions;
    // Per-session prompt serialization lock: ensures that when a prompt completes
    // (state.history updated) the next prompt for the same session sees the updated history.
    let prompt_locks: PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    // 内部 continuation 通知通道：executor on_bg_complete 闭包 → scheduler。
    let (cont_tx, cont_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::session::executor::ContinuationRequest>();
    let cont_tx = Arc::new(cont_tx);
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
            continuation_spawner,
            continuation_shutdown,
        ),
    );

    let connection = Arc::new(tokio::sync::Mutex::new(connection::ConnectionContext::new(
        cfg.stdio_command_filter && cfg.mcp_apps_relay.is_some(),
    )));
    let connection_cancellation = connection.lock().await.cancellation();
    while let Some(msg) = transport.recv().await {
        match msg {
            IncomingMessage::Request { id, method, params } => {
                if method == "session/prompt" {
                    // Spawn long-running prompt execution so the server loop
                    // continues processing session/cancel notifications.
                    let prompt_session_id = extract_session_id(&params, "").to_string();
                    if !prompt_session_id.is_empty() {
                        if let Some(relay) = cfg.mcp_apps_relay.as_ref() {
                            relay.begin_session_turn(&prompt_session_id);
                        }
                    }
                    let sessions = sessions.clone();
                    let transport = Arc::clone(&transport);
                    let prompt_locks = prompt_locks.clone();
                    let cfg = Arc::clone(&cfg);
                    let cont_tx = cont_tx.clone();
                    let prompt_spawner = cfg.host_task_spawner.clone();
                    let rejected_transport = Arc::clone(&transport);
                    let rejected_id = id.clone();
                    let spawn_result = prompt_spawner.spawn(
                        task_scope::HostTaskOwnerKind::Session,
                        task_scope::HostTaskKind::Prompt,
                        async move {
                            let result = dispatch_prompt_turn(
                                params,
                                false,
                                None,
                                &sessions,
                                &prompt_locks,
                                &transport,
                                &cfg,
                                &cont_tx,
                            )
                            .await;
                            if let Err(error) = transport.send_response(id, result).await {
                                tracing::warn!(%error, "prompt terminal response send failed");
                                return;
                            }
                            if !prompt_session_id.is_empty() {
                                send_session_info_update(transport.as_ref(), &prompt_session_id)
                                    .await;
                            }
                        },
                    );
                    if spawn_result.is_err() {
                        if let Err(error) = rejected_transport
                            .send_response(
                                rejected_id,
                                Err(crate::transport::types::AcpError::new(
                                    -32800,
                                    "request cancelled",
                                )),
                            )
                            .await
                        {
                            tracing::warn!(%error, "rejected prompt response send failed");
                        }
                    }
                } else if matches!(
                    method.as_str(),
                    "peri/mcp/open" | "peri/mcp/app" | "peri/mcp/resource"
                ) {
                    let transport = Arc::clone(&transport);
                    let relay = cfg.mcp_apps_relay.clone();
                    let connection = Arc::clone(&connection);
                    let app_spawner = cfg.host_task_spawner.clone();
                    let connection_cancellation = connection_cancellation.clone();
                    let rejected_transport = Arc::clone(&transport);
                    let rejected_id = id.clone();
                    let spawn_result = app_spawner.spawn(
                        task_scope::HostTaskOwnerKind::Connection,
                        task_scope::HostTaskKind::McpAppsRelay,
                        async move {
                            let result = tokio::select! {
                                _ = connection_cancellation.cancelled() => {
                                    Err(crate::transport::types::AcpError::new(-32800, "request cancelled"))
                                }
                                result = async {
                                    match method.as_str() {
                                        "peri/mcp/open" => {
                                            let mut connection = connection.lock().await;
                                            mcp_apps::handle_request(
                                                &method,
                                                &params,
                                                &mut connection,
                                                relay.as_ref(),
                                            )
                                            .await
                                        }
                                        _ => {
                                            let mut request_connection = {
                                                let connection = connection.lock().await;
                                                connection.snapshot_for_request()
                                            };
                                            mcp_apps::handle_request(
                                                &method,
                                                &params,
                                                &mut request_connection,
                                                relay.as_ref(),
                                            )
                                            .await
                                        }
                                    }
                                } => result,
                            };
                            if let Err(error) = transport.send_response(id, result).await {
                                tracing::warn!(%error, "MCP Apps terminal response send failed");
                            }
                        },
                    );
                    if spawn_result.is_err() {
                        let _ = rejected_transport
                            .send_response(
                                rejected_id,
                                Err(crate::transport::types::AcpError::new(
                                    -32800,
                                    "request cancelled",
                                )),
                            )
                            .await;
                    }
                } else {
                    let closed_session_id =
                        matches!(method.as_str(), "session/close" | "session/delete")
                            .then(|| extract_session_id(&params, "").to_string())
                            .filter(|session_id| !session_id.is_empty());
                    let result = {
                        let mut sessions = sessions.lock().await;
                        handle_request(&method, &params, &cfg, &mut sessions, &transport).await
                    };
                    if method == "initialize" && result.is_ok() {
                        connection.lock().await.commit_initialize();
                    }
                    if result.is_ok() {
                        if let (Some(session_id), Some(relay)) =
                            (closed_session_id.as_deref(), cfg.mcp_apps_relay.as_ref())
                        {
                            relay.close_session(session_id);
                        }
                    }
                    let new_session_id = (method == "session/new")
                        .then(|| {
                            result
                                .as_ref()
                                .ok()?
                                .get("sessionId")?
                                .as_str()
                                .map(str::to_owned)
                        })
                        .flatten();
                    let response_sent = transport.send_response(id, result).await.is_ok();
                    if response_sent {
                        if let Some(session_id) = new_session_id {
                            requests::session_lifecycle::after_new_response(
                                &cfg,
                                &transport,
                                &session_id,
                            )
                            .await;
                        }
                    }
                }
            }
            IncomingMessage::Notification { method, params } => {
                if method == "session/cancel" {
                    let session_id = extract_session_id(&params, "");
                    if !session_id.is_empty() {
                        if let Some(relay) = cfg.mcp_apps_relay.as_ref() {
                            relay.close_session(session_id);
                        }
                    }
                }
                // session/cancel 可能需要在锁外补发 continuation 请求
                // （race 兜底：bg 结果已 route 为 Defer，但通知可能在 cancel
                // 置位前被 scheduler 跳过）。unbounded send 虽不阻塞，仍统一
                // 在释放 sessions 锁后发送，避免 notify 路径持锁触碰 scheduler。
                let cont_req = {
                    let mut sessions = sessions.lock().await;
                    handle_notification(&method, &params, &mut sessions, &cfg)
                };
                if let Some(req) = cont_req {
                    let _ = cont_tx.send(req);
                }
            }
            IncomingMessage::Response { .. } => {
                // Responses are routed internally by the transport's pending map.
            }
        }
    }

    // Transport EOF is the host's single ownership transaction.
    connection_cancellation.cancel();
    let connection_id = connection.lock().await.id().to_string();
    if let Some(relay) = cfg.mcp_apps_relay.as_ref() {
        relay.close_connection(&connection_id);
    }
    connection.lock().await.begin_close();
    task_owner.begin_shutdown();
    if let Some(dynamic_mcp) = cfg.dynamic_mcp.as_ref() {
        dynamic_mcp.begin_shutdown();
    }
    if let Some(pool) = cfg.mcp_pool.as_ref() {
        pool.begin_shutdown();
    }
    mcp_task_owner.begin_shutdown();
    drop(cont_tx);
    let (local_ids, mut lsp_pools) = {
        let sessions = sessions.lock().await;
        let mut ids = Vec::with_capacity(sessions.len());
        let mut pools = Vec::new();
        for (session_id, state) in sessions.iter() {
            ids.push(session_id.clone());
            if let Some(token) = state.cancel_token.as_ref() {
                token.cancel();
            }
            if let Some(pool) = state.lsp_pool.as_ref() {
                pools.push(Arc::clone(pool));
            }
        }
        (ids, pools)
    };
    let mut all_ids: BTreeSet<String> = local_ids.into_iter().collect();
    all_ids.extend(cfg.session_manager.session_ids());
    for session_id in &all_ids {
        cfg.session_manager.pre_close_session(session_id);
    }
    let host_report = task_owner.shutdown().await;
    if let task_scope::HostShutdownReport::Incomplete { unfinished } = host_report {
        tracing::warn!(unfinished, "ACP host task drain incomplete");
    }
    let dynamic_report = if let Some(dynamic_mcp) = cfg.dynamic_mcp.as_ref() {
        let report = dynamic_mcp.shutdown().await;
        if let peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Incomplete {
            unfinished_instances,
        } = report
        {
            tracing::warn!(unfinished_instances, "Dynamic MCP service drain incomplete");
        }
        report
    } else {
        peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete
    };
    let _ = mcp_task_owner.shutdown().await;
    let mut session_close_failures = 0usize;
    for session_id in &all_ids {
        if let Err(error) = cfg.session_manager.close_session(session_id).await {
            session_close_failures += 1;
            tracing::warn!(session_id = %session_id, error = %error, "session close during host shutdown failed");
        }
    }
    {
        let mut sessions = sessions.lock().await;
        for (_, state) in sessions.drain() {
            if let Some(pool) = state.lsp_pool {
                lsp_pools.push(pool);
            }
        }
    }
    prompt_locks.lock().await.clear();
    let mut unique_lsp = Vec::<Arc<dyn LspPoolPort>>::new();
    for pool in lsp_pools {
        if !unique_lsp.iter().any(|known| Arc::ptr_eq(known, &pool)) {
            unique_lsp.push(pool);
        }
    }
    for pool in unique_lsp {
        pool.shutdown().await;
    }
    let pool_report = if let Some(pool) = cfg.mcp_pool.as_ref() {
        let report = pool.shutdown().await;
        if let peri_acp_types::ports::McpPoolShutdownReport::Incomplete {
            settled_services,
            unfinished_services,
            failed_services,
        } = report
        {
            tracing::warn!(
                settled_services,
                unfinished_services,
                failed_services,
                "MCP pool service drain incomplete"
            );
        }
        report
    } else {
        peri_acp_types::ports::McpPoolShutdownReport::Complete {
            settled_services: 0,
            failed_services: 0,
        }
    };
    let terminal_report = task_scope::HostTerminalShutdownReport::aggregate(
        host_report,
        dynamic_report,
        pool_report,
        session_close_failures,
    );
    match terminal_report {
        task_scope::HostTerminalShutdownReport::Complete { .. } => {
            tracing::info!(?terminal_report, "ACP host terminal shutdown complete");
        }
        task_scope::HostTerminalShutdownReport::Incomplete { .. } => {
            tracing::warn!(?terminal_report, "ACP host terminal shutdown incomplete");
        }
    }
}

/// 用户 prompt 与内部 AsyncContinuation 的**共享执行路径**。
///
/// 复用同一套：AgentPool 取出/归还、per-session prompt lock、run_prompt 后处理
/// （history 持久化 / cancel 回滚 / recall 回写）、prediction fork。continuation
/// 不发送 ACP response（无 request id），且不触发 prediction。
///
/// 用户显式新 prompt 会清除未运行的 continuation：置位前先
/// `continuation_armed = false` 并递增 `continuation_epoch`（scheduler 在
/// 获取 prompt lock 后校验代际，见 continuation.rs）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_prompt_turn(
    params: Value,
    is_continuation: bool,
    continuation_epoch: Option<u64>,
    sessions: &SharedSessions,
    prompt_locks: &PromptLocks,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    cfg: &AcpServerConfig,
    cont_tx: &tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>,
) -> Result<Value, AcpError> {
    let prompt_session_id = extract_session_id(&params, "").to_string();

    // 多读者 + 单 writer lease：prompt 是写入操作，仅 writer 可提交。
    // 协议无客户端身份字段，writer 恒为 session 创建方（"default"）——
    // 未来引入 clientId 后此处按请求方判定即可（见 lease 模块文档）。
    {
        let sessions = sessions.lock().await;
        if let Some(state) = sessions.get(&prompt_session_id) {
            if !state.lease.is_writer("default") {
                return Err(AcpError::new(
                    -32603,
                    "read-only observer cannot submit prompt",
                ));
            }
        }
    }

    // 用户显式新 prompt 清掉未运行的 continuation（scheduler 的原子 take 与
    // epoch 校验保证不会重复/过期执行）。必须在等待 prompt lock 前递增代际，
    // 使已排队的 continuation 失效；continuation 自身仅在真正拿到锁后才标记
    // in_flight，避免其尚在排队时掩盖对原 prompt 的取消。
    if !is_continuation {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&prompt_session_id) {
            state.continuation_armed = false;
            state.continuation_epoch += 1;
        }
    }

    // 挂起注入：session 当前在 await_wake 挂起（turn 在途但 idle，通常因 bg
    // 任务活跃——executor 在 run_react_loop 挂起期间置 idle_suspended 标志）。
    // 若在此等待 per-session prompt lock，注入会阻塞至当前 turn 完成——bg 任务
    // 可能长达数分钟，用户输入表现为"nothing happen"（TUI 侧 submit_consumer
    // 串行 await prompt RPC，被挂起的 RPC 卡住，后续提交全部排队）。
    // 正确语义：直接把用户消息推入 session inbox（Prompt + wake），挂起的
    // run_react_loop 醒来后由 Receive drain_all 消费，在**同一 turn** 内继续。
    // 注入后立即返回——当前 turn 的 TurnDone 会携带原 request_id（挂起时
    // 该 turn 已在执行），TUI 侧仅用 request_id 做 stale TurnInterrupted 配对，
    // TurnDone 路径不比对（见 peri-tui acp_events/turn.rs）。
    // NOTE: 此分支仅处理用户 prompt（is_continuation=false）。prompt_with_bg_results
    // 的 bgResults 在 run_session_loop 内 push Defer——挂起注入路径不携带
    // bgResults（该 RPC 仅 stdio 会话使用，allow_await_wake=false 永不挂起）。
    if !is_continuation && cfg.session_manager.is_idle_suspended(&prompt_session_id) {
        let (_, content, _attachments) = extract_prompt_params(&params)?;
        if let Some(inbox) = cfg.session_manager.session_inbox_for(&prompt_session_id) {
            inbox.handle().push_prompt(
                peri_acp_types::session::MessageSource::UserInput,
                BaseMessage::human(content),
            );
            tracing::info!(
                session_id = %prompt_session_id,
                "prompt injected while turn suspended (await_wake); loop will wake and consume"
            );
            return Ok(serde_json::json!({}));
        }
    }

    let prompt_lock = {
        let mut locks = prompt_locks.lock().await;
        locks
            .entry(prompt_session_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };

    // Serialize prompts per session: wait for any in-flight prompt to finish
    // so that state.history is up-to-date when this prompt reads it.
    let _guard = prompt_lock.lock().await;

    // AsyncContinuation 与用户 prompt 竞争时，必须在持有同一 prompt lock 后
    // 校验代际与 pending callback：此时不会与 Receive 的 drain_all 并发，确认
    // 的 Defer 会由随后的 continuation 消费。无 callback 则不构建 agent 空跑。
    if let Some(epoch) = continuation_epoch {
        let dispatchable = {
            let sessions = sessions.lock().await;
            sessions.get(&prompt_session_id).is_some_and(|state| {
                let (has_subagent, has_mq) = cfg
                    .session_manager
                    .get_session(&prompt_session_id)
                    .map(|session| {
                        (
                            session.v2_message_queue.has_pending_defer(
                                &peri_acp_types::session::MessageSource::SubAgentComplete,
                            ),
                            session.v2_message_queue.needs_mq_continuation(),
                        )
                    })
                    .unwrap_or((false, false));
                continuation::continuation_dispatchable(state, epoch, has_subagent, has_mq)
            })
        };
        if !dispatchable {
            tracing::debug!(
                session_id = %prompt_session_id,
                "continuation: superseded (newer prompt or Defer consumed), aborting"
            );
            return Ok(serde_json::Value::Null);
        }
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&prompt_session_id) {
            state.continuation_in_flight = true;
            state.continuation_mq_steering_pending = false;
        }
    }

    // Extract AgentPool from session, wrap in Arc<Mutex> for
    // in-place modification inside executor.
    //
    // 取出必须在 prompt lock 之内：continuation 与用户 prompt 共用同一把
    // per-session 锁，若在锁外取出，并发的用户 prompt 会取走被 `mem::replace`
    // 换出的空池并先行归还，导致两轮共享同一缓存的两个池实例互相覆盖、
    // 缓存丢失（跨轮次热缓存是本池的核心价值）。归还仍在锁内（函数末尾）。
    let pool_arc = {
        let mut sessions = sessions.lock().await;
        let pool = sessions
            .get_mut(&prompt_session_id)
            .map(|s| std::mem::take(&mut s.agent_pool))
            .unwrap_or_default();
        Arc::new(parking_lot::Mutex::new(pool))
    };

    let result = run_prompt(
        params,
        sessions,
        &cfg.provider,
        &cfg.peri_config,
        &cfg.permission_mode,
        cfg.cron_scheduler.clone(),
        &cfg.plugin_skill_roots,
        &cfg.plugin_agent_dirs,
        &cfg.plugin_loaded,
        &cfg.hook_groups,
        cfg.mcp_pool.clone(),
        cfg.dynamic_mcp.clone(),
        cfg.channel_state.clone(),
        cfg.tool_search_index.clone(),
        cfg.skills.clone(),
        cfg.shared_tools.clone(),
        &cfg.plugin_lsp_servers,
        transport,
        &cfg.thread_store,
        &cfg.controller,
        cfg.langfuse_session.clone(),
        pool_arc.clone(),
        cfg.session_manager.clone(),
        &cfg.workflow_middleware_factory,
        Some(cont_tx.clone()),
        is_continuation,
        cfg.stdio_command_filter,
    )
    .await;

    // Prediction: agent 成功完成后发起预测输入请求（仅用户 prompt；
    // 内部 continuation 不触发，避免 bg 结果驱动的续跑再叠一次预测调用）
    if !is_continuation && result.is_ok() {
        let pred_transport = Arc::clone(transport);
        let pred_session_id = prompt_session_id.clone();
        let pred_provider = cfg.provider.clone();
        let pred_sessions = sessions.clone();
        let pred_thread_store = cfg.thread_store.clone();
        let pred_caps_registry = cfg.session_manager.caps_registry();

        let _ = cfg.host_task_spawner.spawn(
            task_scope::HostTaskOwnerKind::Session,
            task_scope::HostTaskKind::Prediction,
            async move {
                tracing::debug!("Prediction task started");
                // 从 session 获取最新历史与当前标题
                let (history, cwd, current_title) = {
                    let sessions = pred_sessions.lock().await;
                    match sessions.get(&pred_session_id) {
                        Some(s) => (s.history.clone(), s.cwd.clone(), s.title.clone()),
                        None => {
                            tracing::debug!("Prediction: session not found");
                            return;
                        }
                    }
                };

                // 最近 10 条非 System 消息是软窗口；工具调用 batch 会完整扩展，
                // 历史中本就不完整的 batch 则整组丢弃。
                let recent = prediction_projection::project_prediction_history(&history);

                if recent.is_empty() {
                    tracing::debug!("Prediction: no recent messages");
                    return;
                }
                tracing::debug!(count = recent.len(), "Prediction: got messages");

                // 直接复用已构建的 LlmProvider（绕过 from_config）
                let llm_provider = pred_provider.read().clone();
                tracing::debug!("Prediction: LLM provider ready");

                // Facade：agent 构建与执行统一由 peri-acp executor 承担，
                // TUI 层不再直接构建 Agent（遵守 CLAUDE.md [TRAP]）。
                // L5：LLM 构造（AgentModelBridge）在协议面完成，执行体只收 ReactLLM。
                let llm: Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync> =
                    Box::new(peri_agent::agent::model_bridge::AgentModelBridge::new(
                        Arc::from(llm_provider.into_model()),
                    ));
                let result = crate::session::executor::execute_prediction(
                    llm,
                    recent,
                    &cwd,
                    current_title.as_deref(),
                )
                .await;

                match result {
                    Ok(actions) => {
                        if actions.is_empty() {
                            tracing::debug!("Prediction: empty actions");
                            return;
                        }
                        // 元数据动作写入 session 状态；标题变更待持久化并推送
                        let mut applied_title: Option<String> = None;
                        {
                            let mut sessions = pred_sessions.lock().await;
                            if let Some(state) = sessions.get_mut(&pred_session_id) {
                                for action in &actions {
                                    match action {
                                        PredictionAction::SetTitle { title } => {
                                            let title = title.trim();
                                            if !title.is_empty() {
                                                state.title = Some(title.to_string());
                                                applied_title = Some(title.to_string());
                                            }
                                        }
                                        PredictionAction::AddTag { tag }
                                            if !state.tags.contains(tag) =>
                                        {
                                            state.tags.push(tag.clone());
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        // 标题变更：持久化到 thread store，并推送 session/update
                        // 供标题栏与外部客户端刷新（与 session/rename 行为一致）
                        if let Some(title) = applied_title {
                            if let Err(e) = pred_thread_store
                                .update_title(&pred_session_id, &title)
                                .await
                            {
                                tracing::warn!(
                                    session_id = %pred_session_id,
                                    error = %e,
                                    "Prediction: failed to persist title"
                                );
                            }
                            notify::send_session_info_update_with_title(
                                pred_transport.as_ref(),
                                &pred_session_id,
                                Some(&title),
                            )
                            .await;
                        }
                        let caps = pred_caps_registry
                            .get(&pred_session_id)
                            .map(|r| r.clone())
                            .unwrap_or_default();
                        if caps.prediction {
                            // text 字段取首个 Placeholder（兼容旧消费方）
                            let text = actions
                                .iter()
                                .find_map(|a| match a {
                                    PredictionAction::Placeholder { text } => Some(text.clone()),
                                    _ => None,
                                })
                                .unwrap_or_default();
                            let actions_json: Vec<serde_json::Value> = actions
                                .iter()
                                .filter_map(|a| serde_json::to_value(a).ok())
                                .collect();
                            tracing::debug!(
                                count = actions.len(),
                                "Prediction ready, sending notification"
                            );
                            let _ = pred_transport
                                .send_notification(
                                    "peri/prediction_ready",
                                    serde_json::json!({
                                        "sessionId": pred_session_id,
                                        "text": text,
                                        "actions": actions_json,
                                    }),
                                )
                                .await;
                        } else {
                            tracing::debug!(
                                "Prediction ready but cap not declared, suppressing notification"
                            );
                        }
                    }
                    Err(crate::session::executor::PredictionError::Failed(e)) => {
                        tracing::debug!(error = %e, "Prediction task failed");
                    }
                    Err(crate::session::executor::PredictionError::Timeout) => {
                        tracing::debug!("Prediction task timed out (30s)");
                    }
                }
            },
        );
    }

    // Restore AgentPool back into session (still inside the per-session prompt
    // lock — see the take-out comment above) and clear the continuation in-flight
    // marker. Both writes are unconditional after run_prompt returns, so every
    // non-panic path restores the pool and clears the marker.
    let mq_steering_reschedule = {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&prompt_session_id) {
            if let Ok(mutex) = Arc::try_unwrap(pool_arc) {
                state.agent_pool = mutex.into_inner();
            }
            state.continuation_in_flight = false;
            let pending = state.continuation_mq_steering_pending;
            let needs_mq = cfg
                .session_manager
                .get_session(&prompt_session_id)
                .map(|session| session.v2_message_queue.needs_mq_continuation())
                .unwrap_or(false);
            if pending && !needs_mq {
                state.continuation_mq_steering_pending = false;
            }
            pending && needs_mq
        } else {
            false
        }
    };
    if mq_steering_reschedule {
        let _ = cont_tx.send(crate::session::executor::ContinuationRequest {
            session_id: prompt_session_id.clone(),
            kind: peri_acp_types::tasks::BgTaskKind::Agent,
            mq_steering: true,
        });
        tracing::debug!(
            session_id = %prompt_session_id,
            "continuation: rescheduled MQ steering after in-flight turn ended"
        );
    }

    result
}

#[cfg(test)]
mod oauth_cap_tests {
    use super::{oauth_delivery_policy, OAuthDeliveryPolicy};

    #[test]
    fn test_oauth_delivery_policy_keeps_safe_and_legacy_caps_independent() {
        let safe_only = peri_acp_types::PeriCaps {
            oauth: true,
            ..Default::default()
        };
        assert_eq!(
            oauth_delivery_policy(&safe_only),
            OAuthDeliveryPolicy {
                safe: true,
                legacy: false
            }
        );
        let legacy_only = peri_acp_types::PeriCaps {
            agent_event: true,
            ..Default::default()
        };
        assert_eq!(
            oauth_delivery_policy(&legacy_only),
            OAuthDeliveryPolicy {
                safe: false,
                legacy: true
            }
        );
    }
}
