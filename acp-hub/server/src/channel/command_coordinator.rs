//! CommandCoordinator：每 chat 串行命令队列 + commandId 去重 + 两阶段 Ack
//! （架构 §4.3/§4.4/§7.4，设计稿 `f5-channel-control.md` §7）。
//!
//! **核心纪律**（§7.4 规则 6 + §4.4）：commandId 去重检查、入队上限检查与
//! `in_flight` 标记必须在**同一临界区**完成（Rust 无 JS 单线程原子性）——
//! 本模块以 `tokio::sync::Mutex<()>` 包住「outbox 去重判定 → try_reserve →
//! outbox.insert」三连；去重记录持久化到 outbox（F3，跨 server 重启有效）。
//!
//! 提交点顺序（§4.4/§6.2 prompt 路径，由执行器保证）：
//! `intent_durable → translate（rpcId 登记）→ forward_rpc → dispatched → 投影
//! user entry → L3（JSON-RPC response 匹配 rpcId）→ delivery_confirmed →
//! projection_committed → completed → committed Ack`；无增量窗口耗尽（issue
//! #3：窗口内无事件投递）→ delivery_unknown（路径 B：非幂等禁止自动重发，§4.4）。
//!
//! create 序列（§6.2）：`spawn（10s）→ spawn_ack → initialize（10s）→
//! session/new（30s binding）→ bind → committed(chatId)`；任一步超时 →
//! `AGENT_UNAVAILABLE`(retryable) + 清理半创建状态（补发 kill，§6.2）。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use chrono::Utc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, warn};
use uuid::Uuid;
use yrs::updates::decoder::Decode as _;
use yrs::{Map as _, ReadTxn as _, Transact as _};

use acp_hub_proto::ack::{AckStatus, ActionAck, ActionError, ErrorCode};
use acp_hub_proto::action::{ActionEnvelope, CreateChatPayload, PromptChatPayload};
use acp_hub_proto::frame::Frame;
use acp_hub_proto::instance::{InstanceKill, InstanceSpawn};
use acp_hub_proto::schema::{SessionSummaryProjection, TurnStatus};
use acp_hub_proto::session::SessionListFrame;
use acp_hub_proto::session::{PromptDeliveryStatus, PromptStatusFrame, PromptStatusItem};

use crate::auth::audit::audit;
use crate::auth::ConnectionCtx;
use crate::channel::broadcaster::OutboundMsg;
use crate::channel::relay_event_handler::RelayEventHandler;
use crate::control::WorkspaceRegistry;
use crate::control::{ChatError, ChatRegistry, ChatState};
use crate::control::{InstanceError, InstanceRegistry, SpawnOutcome};
use crate::control::{ProjectService, ProjectServiceError};
use crate::persist::metadata::{payload_hash, BeginCommand, MetadataError, NewSession};
use crate::persist::outbox::{
    CommandRecovery, CommandType, LastError, NewOutboxRecord, OutboxStatus, RetryableClass,
};
use crate::persist::Store;
use crate::protocol::{
    extract_agent_config, validate_cwd, OutboundCtx, OutboundMessage, Translator,
};
use crate::state::aggregator::ApplyReason;
use crate::state::doc_manager::BatchConfig;
use crate::state::doc_manager::{DocCommand, DocManager, SubmitError, SubmitResult};

/// 默认 instance（§4.3 P5：instance_id 缺省 = 本机）。
pub const DEFAULT_INSTANCE_ID: &str = "local";

/// 默认 ACP 启动命令（架构 §11「默认 `peri acp`，可配置」；M1 起经
/// `Config::acp_cmd` 可配——config.toml `acp_cmd` 数组或
/// `ACP_HUB_ACP_CMD` 空格拆分，见 `crate::config`）。
pub use crate::config::DEFAULT_ACP_CMD;

/// L3 确认超时（§4.4 路径 B：issue #3 增量窗口——窗口内该 chat 无事件投递
/// → delivery_unknown；默认 30s，测试注入短值）【决策：设计稿 §16 测试 13
/// 的 30s 常量，非 §16 配置表项】。
pub const L3_TIMEOUT: Duration = Duration::from_secs(30);

/// session/list 轮询间隔（§6.3：10s 全量同步；幂等，响应中不存在的旧条目
/// 删除——自愈）。
pub const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// session/list 单次请求超时（§6.3；超过即放弃本轮，下轮重试）。
pub const SESSION_POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// 提交结果（同步返回的部分）：accepted 立即；终态经连接发送队列。
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitAck {
    /// 已入队（accepted，§4.4：只表示进入有界处理队列）。
    Accepted { command_id: String },
    /// 已提交命令重发（§4.4）：duplicate + 原 turnId，**不重复调用 Agent**。
    Duplicate(ActionAck),
    /// 同步失败（RATE_LIMITED/CHAT_NOT_FOUND/INVALID_STATE…）→ action_error。
    Failed(ActionError),
    /// Coordinator wrote accepted and terminal frames through the same queue.
    Handled,
}

/// 执行器命令（submit 入队载荷；终态经 `tx` 回客户端连接）。
#[derive(Debug, Clone)]
pub struct ExecCmd {
    /// 客户端连接上下文（审计）。
    pub ctx: ConnectionCtx,
    /// hub 侧 chat_id（create：submit 时生成的新 id）。
    pub chat_id: String,
    /// 原始 Action。
    pub action: ActionEnvelope,
    /// 客户端连接发送队列（committed/error 回投）。
    pub tx: mpsc::Sender<OutboundMsg>,
}

/// 每 chat 串行命令队列（上限 64，§7.4 规则 1）+ commandId 去重（§4.4）。
#[derive(Clone)]
pub struct CommandCoordinator {
    inner: Arc<CoordInner>,
}

struct CoordInner {
    /// 去重 + 入队 + in_flight 同一临界区（§7.4 规则 6；tokio Mutex 可跨 await）。
    gate: Mutex<()>,
    store: Arc<Store>,
    doc: Arc<DocManager>,
    instance: Arc<InstanceRegistry>,
    chats: ChatRegistry,
    /// 工作区注册表（独立于 chat 的上层概念：定义本地目录 cwd，其下新建
    /// 对话继承——ACP 进程工作目录 + session/list 查询面）。
    workspaces: WorkspaceRegistry,
    relay: Arc<RelayEventHandler>,
    translator: Arc<Translator>,
    queue_cap: usize,
    /// 每 chat 执行器（串行消费；lazy spawn）。
    executors: RwLock<HashMap<String, mpsc::Sender<ExecCmd>>>,
    /// 全局 create 执行器【决策】create 的 chat_id 是新的、无既有队列；
    /// 独立串行队列承担（M1 低频，单队列足够）。
    create_tx: RwLock<Option<mpsc::Sender<ExecCmd>>>,
    /// create 全局去重索引（command_id → chat_id，§4.4）：create 的
    /// chat_id 由 server 生成，客户端重发无法指定，故跨 chat 按
    /// commandId 查（启动时从 outbox 重建）。
    create_index: RwLock<HashMap<Uuid, Uuid>>,
    /// M1 默认目录 = server 进程工作目录（§4.3 裁决）。
    default_cwd: String,
    /// ACP 启动命令（§11 默认 `peri acp`）。
    acp_cmd: Vec<String>,
    /// spawn/initialize/binding 超时（§6.2：10s/10s/30s）。
    spawn_timeout: Duration,
    initialize_timeout: Duration,
    binding_timeout: Duration,
    /// 同一 chat 同时只允许一个会话变更操作（session/load 与
    /// chat/session-new 共享）：load 的回放通知先于 RPC 响应，若并发执行
    /// 会让两个历史流写入同一个 Yjs Doc；session-new 同理（新会话通知
    /// 与既有流交错）。
    loads_in_flight: StdMutex<HashSet<String>>,
    /// project 级 session discovery 单飞；避免重复打开多个临时 ACP 进程。
    discoveries_in_flight: StdMutex<HashSet<String>>,
    /// L3 确认超时（§4.4 路径 B 默认 30s；测试注入短值）。
    l3_timeout: Duration,
    projects: RwLock<Option<ProjectService>>,
    history_sink: RwLock<Option<Arc<crate::control::StoreSink>>>,
    /// Process-local observers attached by same-command reconciliation. Durable
    /// restart truth remains the outbox; this map only fans a live terminal
    /// result to replacement connections.
    terminal_watchers: Mutex<HashMap<Uuid, Vec<mpsc::Sender<OutboundMsg>>>>,
    /// Process-local terminal fallback when the durable terminal append itself
    /// fails. Restart reconciliation repairs the outbox before gateway ready;
    /// this map prevents observers from hanging during the current process.
    terminal_failures: Mutex<HashMap<Uuid, ActionError>>,
    terminal_failure_overflow: AtomicBool,
}

struct LoadFlightGuard {
    inner: Arc<CoordInner>,
    chat_id: String,
}

struct DiscoveryFlightGuard {
    inner: Arc<CoordInner>,
    project_id: String,
}

impl Drop for DiscoveryFlightGuard {
    fn drop(&mut self) {
        self.inner
            .discoveries_in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.project_id);
    }
}

impl Drop for LoadFlightGuard {
    fn drop(&mut self) {
        self.inner
            .loads_in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.chat_id);
    }
}

impl CommandCoordinator {
    /// 装配（hub 调用；`default_cwd` = 进程工作目录，§4.3 裁决；
    /// `acp_cmd` = ACP 启动命令，默认 `["peri","acp"]`，§11）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<Store>,
        doc: Arc<DocManager>,
        instance: Arc<InstanceRegistry>,
        chats: ChatRegistry,
        relay: Arc<RelayEventHandler>,
        cfg: &BatchConfig,
        acp_cmd: Vec<String>,
        spawn_timeout: Duration,
        initialize_timeout: Duration,
        binding_timeout: Duration,
    ) -> Self {
        Self::with_l3_timeout(
            store,
            doc,
            instance,
            chats,
            relay,
            cfg,
            acp_cmd,
            spawn_timeout,
            initialize_timeout,
            binding_timeout,
            L3_TIMEOUT,
        )
    }

    /// 带 L3 超时参数的装配（测试注入短值）。
    #[allow(clippy::too_many_arguments)]
    pub fn with_l3_timeout(
        store: Arc<Store>,
        doc: Arc<DocManager>,
        instance: Arc<InstanceRegistry>,
        chats: ChatRegistry,
        relay: Arc<RelayEventHandler>,
        cfg: &BatchConfig,
        acp_cmd: Vec<String>,
        spawn_timeout: Duration,
        initialize_timeout: Duration,
        binding_timeout: Duration,
        l3_timeout: Duration,
    ) -> Self {
        let default_cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".to_string());
        CommandCoordinator {
            inner: Arc::new(CoordInner {
                gate: Mutex::new(()),
                store,
                doc,
                instance,
                chats: chats.clone(),
                // workspace 注册表与 chats 共用同一 Registry 状态源（§5.2
                // server 状态源单写）；hub 装配后经 rebuild_workspaces 从
                // Registry Doc 重建内存表（跨重启可见）。
                workspaces: WorkspaceRegistry::new(chats.registry()),
                relay,
                translator: Arc::new(Translator::new()),
                queue_cap: cfg.chat_queue,
                executors: RwLock::new(HashMap::new()),
                create_tx: RwLock::new(None),
                create_index: RwLock::new(HashMap::new()),
                default_cwd,
                acp_cmd,
                spawn_timeout,
                initialize_timeout,
                binding_timeout,
                loads_in_flight: StdMutex::new(HashSet::new()),
                discoveries_in_flight: StdMutex::new(HashSet::new()),
                l3_timeout,
                projects: RwLock::new(None),
                history_sink: RwLock::new(None),
                terminal_watchers: Mutex::new(HashMap::new()),
                terminal_failures: Mutex::new(HashMap::new()),
                terminal_failure_overflow: AtomicBool::new(false),
            }),
        }
    }

    pub async fn install_project_service(&self, projects: ProjectService) {
        *self.inner.projects.write().await = Some(projects);
    }

    pub async fn install_history_sink(&self, sink: Arc<crate::control::StoreSink>) {
        *self.inner.history_sink.write().await = Some(sink);
    }

    /// 工作区注册表（hub 装配：启动恢复重建）。
    pub async fn rebuild_workspaces(&self) {
        self.inner.workspaces.rebuild().await;
    }

    /// create 全局去重索引重建（§4.4：跨 server 重启有效——启动时从 outbox
    /// 重放重建）。hub 装配时（store.recover 完成后）调用一次。
    pub async fn rebuild_create_index(&self) {
        let mut idx: HashMap<Uuid, Uuid> = HashMap::new();
        for (cid, store) in self.inner.store.chats_snapshot() {
            let recs = store
                .outbox()
                .lock()
                .await
                .records()
                .cloned()
                .collect::<Vec<_>>();
            for rec in recs {
                if rec.command_type == CommandType::Create {
                    idx.insert(rec.command_id, cid);
                }
            }
        }
        let mut cur = self.inner.create_index.write().await;
        // 合并而非覆盖：运行期新建的记录（submit 登记）不得被重建冲掉。
        for (k, v) in idx {
            cur.entry(k).or_insert(v);
        }
    }

    /// Reconcile prompt outbox and exact v2 chat projection evidence before
    /// the gateway accepts clients. This is the only startup point where both
    /// durable stores are available; process-local watchers/executors do not
    /// participate in the decision.
    pub async fn reconcile_prompt_delivery_after_restart(&self) -> Result<(), String> {
        let Some(sink) = self.inner.history_sink.read().await.clone() else {
            return Err("history sink unavailable during prompt reconciliation".into());
        };
        for (chat_id, store) in self.inner.store.chats_snapshot() {
            let chat_id_text = chat_id.to_string();
            let evidence = prompt_projection_evidence(
                sink.snapshot(&acp_hub_proto::conn::DocId::chat(&chat_id_text))
                    .await,
                sink.snapshot(&acp_hub_proto::conn::DocId::session(&chat_id_text))
                    .await,
            )
            .map(|(evidence, _)| evidence)
            .unwrap_or_default();
            let records = store
                .outbox()
                .lock()
                .await
                .records()
                .filter(|record| record.command_type == CommandType::Prompt)
                .cloned()
                .collect::<Vec<_>>();
            let exact_terminal = records
                .iter()
                .filter_map(|record| {
                    let projected = evidence.get(&record.command_id)?;
                    exact_prompt_terminal_evidence(record, projected).then_some(record.command_id)
                })
                .collect::<HashSet<_>>();
            store
                .outbox()
                .lock()
                .await
                .reconcile_prompt_delivery_after_restart(&exact_terminal)
                .map_err(|error| error.to_string())?;

            let reconciled = store
                .outbox()
                .lock()
                .await
                .records()
                .filter(|record| record.command_type == CommandType::Prompt)
                .cloned()
                .collect::<Vec<_>>();
            let known = reconciled
                .iter()
                .map(|record| record.command_id)
                .collect::<HashSet<_>>();
            for record in reconciled {
                let Some(turn_id) = record.turn_id else {
                    continue;
                };
                let (state, code) = match record.status {
                    OutboxStatus::Completed => ("completed", None),
                    OutboxStatus::Failed => (
                        "failed_not_delivered",
                        record.last_error.as_ref().map(|error| error.code.as_str()),
                    ),
                    OutboxStatus::DeliveryUnknown => ("delivery_unknown", Some("DELIVERY_UNKNOWN")),
                    _ => continue,
                };
                sink.reconcile_prompt_entry_delivery(
                    &chat_id_text,
                    &format!("{turn_id}:user"),
                    state,
                    code,
                )
                .await
                .map_err(|error| error.to_string())?;
            }
            for (command_id, projected) in evidence {
                if known.contains(&command_id) || !is_v2_pending_orphan(&projected) {
                    continue;
                }
                let Some(entry_id) = projected.entry_id else {
                    continue;
                };
                sink.reconcile_prompt_entry_delivery(
                    &chat_id_text,
                    &entry_id,
                    "failed_not_delivered",
                    Some("AGENT_UNAVAILABLE"),
                )
                .await
                .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    async fn submit_metadata_action(
        &self,
        ctx: &ConnectionCtx,
        action: ActionEnvelope,
        tx: mpsc::Sender<OutboundMsg>,
    ) -> SubmitAck {
        let command_id = extract_command_id(&action).unwrap_or_default();
        if Uuid::parse_str(&command_id).is_err() {
            return SubmitAck::Failed(action_error(
                command_id,
                ErrorCode::InvalidState,
                "invalid commandId",
                false,
            ));
        }
        let Some(projects) = self.inner.projects.read().await.clone() else {
            return SubmitAck::Failed(action_error(
                command_id,
                ErrorCode::AgentUnavailable,
                "metadata catalog unavailable",
                true,
            ));
        };
        let hash = match payload_hash(&action) {
            Ok(v) => v,
            Err(_) => {
                return SubmitAck::Failed(action_error(
                    command_id,
                    ErrorCode::InvalidState,
                    "invalid metadata payload",
                    false,
                ))
            }
        };
        // Validate against the authoritative catalog before reserving the
        // command id. A rejected request must not leave an in-progress dedup row.
        let mut prepared_create = None;
        let mut prepared_open = None;
        match &action {
            ActionEnvelope::ProjectCreate { payload, .. } => {
                if let Err(e) = validate_cwd(&payload.cwd) {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        &format!("invalid cwd: {e}"),
                        false,
                    ));
                }
                if !std::path::Path::new(&payload.cwd).is_dir() {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "cwd not found",
                        false,
                    ));
                }
            }
            ActionEnvelope::ProjectArchive { payload, .. } => {
                let project_exists = matches!(projects.metadata().project(&payload.project_id).await, Ok(Some(ref p)) if p.archived_at.is_none());
                if !project_exists {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "project not found or archived",
                        false,
                    ));
                }
                if self
                    .inner
                    .chats
                    .has_live_workspace(&payload.project_id)
                    .await
                {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "project has a running session; close it before archiving",
                        false,
                    ));
                }
            }
            ActionEnvelope::ProjectRestore { payload, .. } => {
                if !matches!(projects.metadata().project(&payload.project_id).await, Ok(Some(ref p)) if p.archived_at.is_some())
                {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "archived project not found",
                        false,
                    ));
                }
            }
            ActionEnvelope::ProjectRename { payload, .. } => {
                if payload.name.trim().is_empty()
                    || !matches!(projects.metadata().project(&payload.project_id).await, Ok(Some(ref p)) if p.archived_at.is_none())
                {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "active project not found or name empty",
                        false,
                    ));
                }
            }
            ActionEnvelope::PersistedSessionRename { payload, .. } => {
                if payload.name.trim().is_empty()
                    || !matches!(
                        projects.metadata().session(&payload.session_id).await,
                        Ok(Some(ref session)) if session.archived_at.is_none()
                    )
                {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "session not found or name empty",
                        false,
                    ));
                }
            }
            ActionEnvelope::PersistedSessionArchive { payload, .. } => {
                let Some(session) = projects
                    .metadata()
                    .session(&payload.session_id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|session| session.archived_at.is_none())
                else {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "active session not found",
                        false,
                    ));
                };
                if let Some(acp_id) = session.acp_session_id.as_deref() {
                    if self.inner.chats.has_live_acp_session(acp_id).await {
                        return SubmitAck::Failed(action_error(
                            command_id,
                            ErrorCode::InvalidState,
                            "session has a running instance; close it before archiving",
                            false,
                        ));
                    }
                }
            }
            ActionEnvelope::PersistedSessionRestore { payload, .. } => {
                let restorable = match projects.metadata().session(&payload.session_id).await {
                    Ok(Some(session)) if session.archived_at.is_some() => matches!(
                        projects.metadata().project(&session.project_id).await,
                        Ok(Some(ref project)) if project.archived_at.is_none()
                    ),
                    _ => false,
                };
                if !restorable {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "archived session or active project not found",
                        false,
                    ));
                }
            }
            ActionEnvelope::PersistedSessionImport { payload, .. } => {
                let Some(project) = projects
                    .metadata()
                    .project(&payload.project_id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|p| p.archived_at.is_none())
                else {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "project not found or archived",
                        false,
                    ));
                };
                let candidate = self
                    .inner
                    .chats
                    .registry()
                    .list_legacy_sessions()
                    .await
                    .ok()
                    .and_then(|items| {
                        items.into_iter().find(|s| {
                            s.session_id == payload.acp_session_id && s.cwd == project.cwd
                        })
                    });
                if candidate.is_none() {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "ACP session is not available for this project",
                        false,
                    ));
                }
            }
            ActionEnvelope::PersistedSessionCreate { payload, .. } => {
                let Some(project) = projects
                    .metadata()
                    .project(&payload.project_id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|p| p.archived_at.is_none())
                else {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "project not found or archived",
                        false,
                    ));
                };
                prepared_create = Some((project, Uuid::new_v4().to_string()));
            }
            ActionEnvelope::PersistedSessionOpen { payload, .. } => {
                let Some(session) = projects
                    .metadata()
                    .session(&payload.session_id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|session| session.archived_at.is_none())
                else {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "session not found",
                        false,
                    ));
                };
                if session.lifecycle != "ready" || session.acp_session_id.is_none() {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "session is not ready",
                        false,
                    ));
                }
                let Some(project) = projects
                    .metadata()
                    .project(&session.project_id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|p| p.archived_at.is_none())
                else {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "project not found or archived",
                        false,
                    ));
                };
                let live_chat = if let (Some(chat), Some(acp)) = (
                    session.last_chat_id.as_deref(),
                    session.acp_session_id.as_deref(),
                ) {
                    self.inner
                        .chats
                        .entry(chat)
                        .await
                        .filter(|e| !e.state.is_terminal() && e.session_id.as_deref() == Some(acp))
                        .map(|_| chat.to_string())
                } else {
                    None
                };
                prepared_open = Some((project, session, live_chat));
            }
            _ => unreachable!(),
        }
        let (project_hint, session_hint) = match &action {
            ActionEnvelope::ProjectArchive { payload, .. }
            | ActionEnvelope::ProjectRestore { payload, .. } => {
                (Some(payload.project_id.as_str()), None)
            }
            ActionEnvelope::ProjectRename { payload, .. } => {
                (Some(payload.project_id.as_str()), None)
            }
            ActionEnvelope::PersistedSessionCreate { payload, .. } => {
                (Some(payload.project_id.as_str()), None)
            }
            ActionEnvelope::PersistedSessionOpen { payload, .. } => {
                (None, Some(payload.session_id.as_str()))
            }
            ActionEnvelope::PersistedSessionRename { payload, .. } => {
                (None, Some(payload.session_id.as_str()))
            }
            ActionEnvelope::PersistedSessionArchive { payload, .. }
            | ActionEnvelope::PersistedSessionRestore { payload, .. } => {
                (None, Some(payload.session_id.as_str()))
            }
            ActionEnvelope::PersistedSessionImport { payload, .. } => {
                (Some(payload.project_id.as_str()), None)
            }
            _ => (None, None),
        };
        let new_session = prepared_create.as_ref().map(|(project, id)| NewSession {
            id,
            project_id: &project.id,
            title: match &action {
                ActionEnvelope::PersistedSessionCreate { payload, .. } => payload.title.as_deref(),
                _ => None,
            },
        });
        let activate = prepared_create
            .as_ref()
            .map(|(_, id)| id.as_str())
            .or_else(|| {
                prepared_open
                    .as_ref()
                    .and_then(|(_, session, live)| live.is_none().then_some(session.id.as_str()))
            });
        match projects
            .metadata()
            .begin_command_with_activation(
                &command_id,
                action.type_str(),
                &hash,
                project_hint,
                session_hint.or_else(|| prepared_create.as_ref().map(|(_, id)| id.as_str())),
                new_session,
                activate,
            )
            .await
        {
            Ok(BeginCommand::Existing) => match projects.metadata().command(&command_id).await {
                Ok(Some(c)) if c.phase == "committed" => {
                    return SubmitAck::Duplicate(ActionAck {
                        command_id,
                        status: AckStatus::Duplicate,
                        turn_id: None,
                        chat_id: c.chat_id,
                        project_id: c.project_id,
                        session_id: c.session_id,
                        acp_session_id: c.acp_session_id,
                        committed_projection_version: None,
                    })
                }
                Ok(Some(c)) if c.phase == "projection_pending" => {
                    if projects.reproject().await.is_err()
                        || projects
                            .metadata()
                            .update_command(
                                &command_id,
                                "committed",
                                c.project_id.as_deref(),
                                c.session_id.as_deref(),
                                c.chat_id.as_deref(),
                                c.acp_session_id.as_deref(),
                                None,
                            )
                            .await
                            .is_err()
                    {
                        return SubmitAck::Failed(action_error(
                            command_id,
                            ErrorCode::AgentUnavailable,
                            "metadata projection retry failed",
                            true,
                        ));
                    }
                    return SubmitAck::Duplicate(ActionAck {
                        command_id,
                        status: AckStatus::Duplicate,
                        turn_id: None,
                        chat_id: c.chat_id,
                        project_id: c.project_id,
                        session_id: c.session_id,
                        acp_session_id: c.acp_session_id,
                        committed_projection_version: None,
                    });
                }
                Ok(Some(c)) if c.phase == "reconciliation_required" => {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "metadata command requires reconciliation",
                        false,
                    ))
                }
                Ok(Some(c)) if c.phase == "failed" => {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::AgentUnavailable,
                        c.error_code.as_deref().unwrap_or("metadata command failed"),
                        false,
                    ))
                }
                Ok(_) => {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "metadata command already in progress",
                        false,
                    ))
                }
                Err(_) => {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::AgentUnavailable,
                        "metadata command lookup failed",
                        true,
                    ))
                }
            },
            Err(MetadataError::Conflict(_)) => {
                return SubmitAck::Failed(action_error(
                    command_id,
                    ErrorCode::InvalidState,
                    "commandId reused with different payload",
                    false,
                ))
            }
            Err(_) => {
                return SubmitAck::Failed(action_error(
                    command_id,
                    ErrorCode::DeliveryUnknown,
                    "metadata command persist failed",
                    true,
                ))
            }
            Ok(BeginCommand::New) => {}
        }
        if tx
            .send(OutboundMsg::Frame(Frame::ActionAck(ActionAck {
                command_id: command_id.clone(),
                status: AckStatus::Accepted,
                turn_id: None,
                chat_id: None,
                project_id: None,
                session_id: None,
                acp_session_id: None,
                committed_projection_version: None,
            })))
            .await
            .is_err()
        {
            return SubmitAck::Handled;
        }
        let cmd = ExecCmd {
            ctx: ctx.clone(),
            chat_id: String::new(),
            action: action.clone(),
            tx: tx.clone(),
        };
        match action {
            ActionEnvelope::ProjectCreate { payload, .. } => {
                let id = Uuid::new_v4().to_string();
                let name = if payload.name.trim().is_empty() {
                    std::path::Path::new(&payload.cwd)
                        .file_name()
                        .map(|v| v.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Project".into())
                } else {
                    payload.name.trim().to_string()
                };
                let instance = payload
                    .instance_id
                    .as_deref()
                    .unwrap_or(DEFAULT_INSTANCE_ID);
                match projects
                    .create_project_metadata(&id, &name, &payload.cwd, instance)
                    .await
                {
                    Ok(p) => {
                        if projects
                            .metadata()
                            .update_command(
                                &command_id,
                                "projection_pending",
                                Some(&id),
                                None,
                                None,
                                None,
                                None,
                            )
                            .await
                            .is_err()
                            || projects.reproject().await.is_err()
                            || projects.mirror_legacy_workspace(&p).await.is_err()
                            || projects
                                .metadata()
                                .update_command(
                                    &command_id,
                                    "committed",
                                    Some(&id),
                                    None,
                                    None,
                                    None,
                                    None,
                                )
                                .await
                                .is_err()
                        {
                            let _ = projects
                                .metadata()
                                .update_command(
                                    &command_id,
                                    "reconciliation_required",
                                    Some(&id),
                                    None,
                                    None,
                                    None,
                                    Some("project_projection_or_finalize_failed"),
                                )
                                .await;
                            return SubmitAck::Failed(action_error(
                                command_id,
                                ErrorCode::AgentUnavailable,
                                "project commit barrier failed",
                                true,
                            ));
                        }
                        self.send_metadata_ack(
                            &cmd,
                            AckStatus::Committed,
                            Some(&id),
                            None,
                            None,
                            None,
                        )
                        .await;
                    }
                    Err(_) => {
                        let _ = projects
                            .metadata()
                            .update_command(
                                &command_id,
                                "reconciliation_required",
                                Some(&id),
                                None,
                                None,
                                None,
                                Some("project_persist_or_projection_failed"),
                            )
                            .await;
                        self.send_error(
                            &cmd,
                            ErrorCode::DeliveryUnknown,
                            "project persist/projection failed",
                            false,
                        )
                        .await;
                    }
                }
            }
            ActionEnvelope::ProjectArchive { payload, .. } => {
                if projects
                    .archive_project_metadata(&payload.project_id)
                    .await
                    .is_err()
                    || projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "projection_pending",
                            Some(&payload.project_id),
                            None,
                            None,
                            None,
                            None,
                        )
                        .await
                        .is_err()
                {
                    let _ = projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "reconciliation_required",
                            Some(&payload.project_id),
                            None,
                            None,
                            None,
                            Some("archive_projection_failed"),
                        )
                        .await;
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "project archive requires reconciliation",
                        false,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if projects.reproject().await.is_err() {
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "project archive projection pending",
                        true,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if projects
                    .metadata()
                    .update_command(
                        &command_id,
                        "committed",
                        Some(&payload.project_id),
                        None,
                        None,
                        None,
                        None,
                    )
                    .await
                    .is_err()
                {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::AgentUnavailable,
                        "project command finalize failed",
                        true,
                    ));
                }
                self.send_metadata_ack(
                    &cmd,
                    AckStatus::Committed,
                    Some(&payload.project_id),
                    None,
                    None,
                    None,
                )
                .await;
            }
            ActionEnvelope::ProjectRestore { payload, .. } => {
                if projects
                    .restore_project_metadata(&payload.project_id)
                    .await
                    .is_err()
                    || projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "projection_pending",
                            Some(&payload.project_id),
                            None,
                            None,
                            None,
                            None,
                        )
                        .await
                        .is_err()
                {
                    let _ = projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "reconciliation_required",
                            Some(&payload.project_id),
                            None,
                            None,
                            None,
                            Some("restore_projection_failed"),
                        )
                        .await;
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "project restore requires reconciliation",
                        false,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if projects.reproject().await.is_err() {
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "project restore projection pending",
                        true,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if projects
                    .metadata()
                    .update_command(
                        &command_id,
                        "committed",
                        Some(&payload.project_id),
                        None,
                        None,
                        None,
                        None,
                    )
                    .await
                    .is_err()
                {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::AgentUnavailable,
                        "project command finalize failed",
                        true,
                    ));
                }
                self.send_metadata_ack(
                    &cmd,
                    AckStatus::Committed,
                    Some(&payload.project_id),
                    None,
                    None,
                    None,
                )
                .await;
            }
            ActionEnvelope::ProjectRename { payload, .. } => {
                if projects
                    .rename_project_metadata(&payload.project_id, payload.name.trim())
                    .await
                    .is_err()
                    || projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "projection_pending",
                            Some(&payload.project_id),
                            None,
                            None,
                            None,
                            None,
                        )
                        .await
                        .is_err()
                {
                    let _ = projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "reconciliation_required",
                            Some(&payload.project_id),
                            None,
                            None,
                            None,
                            Some("project_rename_projection_failed"),
                        )
                        .await;
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "project rename requires reconciliation",
                        false,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if projects.reproject().await.is_err() {
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "project rename projection pending",
                        true,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if projects
                    .metadata()
                    .update_command(
                        &command_id,
                        "committed",
                        Some(&payload.project_id),
                        None,
                        None,
                        None,
                        None,
                    )
                    .await
                    .is_err()
                {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::AgentUnavailable,
                        "project command finalize failed",
                        true,
                    ));
                }
                self.send_metadata_ack(
                    &cmd,
                    AckStatus::Committed,
                    Some(&payload.project_id),
                    None,
                    None,
                    None,
                )
                .await;
            }
            ActionEnvelope::PersistedSessionRename { payload, .. } => {
                if payload.name.trim().is_empty()
                    || projects
                        .rename_session_metadata(&payload.session_id, payload.name.trim())
                        .await
                        .is_err()
                {
                    let _ = projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "reconciliation_required",
                            None,
                            Some(&payload.session_id),
                            None,
                            None,
                            Some("rename_projection_failed"),
                        )
                        .await;
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "session rename requires reconciliation",
                        false,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                let rec = projects
                    .metadata()
                    .session(&payload.session_id)
                    .await
                    .ok()
                    .flatten();
                let project_id = rec.as_ref().map(|r| r.project_id.clone());
                let acp_session_id = rec.as_ref().and_then(|r| r.acp_session_id.clone());
                if projects
                    .metadata()
                    .update_command(
                        &command_id,
                        "projection_pending",
                        project_id.as_deref(),
                        Some(&payload.session_id),
                        None,
                        acp_session_id.as_deref(),
                        None,
                    )
                    .await
                    .is_err()
                {
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "session rename projection state failed",
                        true,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if projects.reproject().await.is_err() {
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "session rename projection pending",
                        true,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if projects
                    .metadata()
                    .update_command(
                        &command_id,
                        "committed",
                        project_id.as_deref(),
                        Some(&payload.session_id),
                        None,
                        acp_session_id.as_deref(),
                        None,
                    )
                    .await
                    .is_err()
                {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::AgentUnavailable,
                        "session rename finalize failed",
                        true,
                    ));
                }
                self.send_metadata_ack(
                    &cmd,
                    AckStatus::Committed,
                    project_id.as_deref(),
                    Some(&payload.session_id),
                    None,
                    acp_session_id,
                )
                .await;
            }
            ActionEnvelope::PersistedSessionArchive { payload, .. }
            | ActionEnvelope::PersistedSessionRestore { payload, .. } => {
                let archive = matches!(&cmd.action, ActionEnvelope::PersistedSessionArchive { .. });
                let mutation = if archive {
                    projects.archive_session_metadata(&payload.session_id).await
                } else {
                    projects.restore_session_metadata(&payload.session_id).await
                };
                let rec = projects
                    .metadata()
                    .session(&payload.session_id)
                    .await
                    .ok()
                    .flatten();
                let project_id = rec.as_ref().map(|record| record.project_id.as_str());
                let acp_id = rec
                    .as_ref()
                    .and_then(|record| record.acp_session_id.as_deref());
                if matches!(
                    &mutation,
                    Err(ProjectServiceError::Metadata(
                        MetadataError::InvalidState(_)
                            | MetadataError::NotFound(_)
                            | MetadataError::Conflict(_)
                    ))
                ) {
                    let _ = projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "failed",
                            project_id,
                            Some(&payload.session_id),
                            None,
                            acp_id,
                            Some("invalid_state"),
                        )
                        .await;
                    self.send_error(
                        &cmd,
                        ErrorCode::InvalidState,
                        "session lifecycle changed before mutation",
                        false,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if mutation.is_err()
                    || projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "projection_pending",
                            project_id,
                            Some(&payload.session_id),
                            None,
                            acp_id,
                            None,
                        )
                        .await
                        .is_err()
                {
                    let error = if archive {
                        "session_archive_projection_failed"
                    } else {
                        "session_restore_projection_failed"
                    };
                    let _ = projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "reconciliation_required",
                            project_id,
                            Some(&payload.session_id),
                            None,
                            acp_id,
                            Some(error),
                        )
                        .await;
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "session lifecycle change requires reconciliation",
                        false,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if projects.reproject().await.is_err() {
                    self.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "session lifecycle projection pending",
                        true,
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                if projects
                    .metadata()
                    .update_command(
                        &command_id,
                        "committed",
                        project_id,
                        Some(&payload.session_id),
                        None,
                        acp_id,
                        None,
                    )
                    .await
                    .is_err()
                {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::AgentUnavailable,
                        "session lifecycle command finalize failed",
                        true,
                    ));
                }
                self.send_metadata_ack(
                    &cmd,
                    AckStatus::Committed,
                    project_id,
                    Some(&payload.session_id),
                    None,
                    acp_id.map(str::to_string),
                )
                .await;
            }
            ActionEnvelope::PersistedSessionImport { payload, .. } => {
                let Some(project) = projects
                    .metadata()
                    .project(&payload.project_id)
                    .await
                    .ok()
                    .flatten()
                else {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "project not found",
                        false,
                    ));
                };
                let candidate = self
                    .inner
                    .chats
                    .registry()
                    .list_legacy_sessions()
                    .await
                    .ok()
                    .and_then(|items| {
                        items.into_iter().find(|s| {
                            s.session_id == payload.acp_session_id && s.cwd == project.cwd
                        })
                    });
                let Some(candidate) = candidate else {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::InvalidState,
                        "ACP session disappeared before import",
                        false,
                    ));
                };
                let logical_id = Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("acp-hub:imported-session:{}", candidate.session_id).as_bytes(),
                )
                .to_string();
                let imported = projects
                    .metadata()
                    .import_explicit_session(
                        &logical_id,
                        &project.id,
                        &candidate.session_id,
                        &candidate.title,
                        &candidate.updated_at,
                    )
                    .await;
                let Ok(imported) = imported else {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::AgentUnavailable,
                        "session import persist failed",
                        true,
                    ));
                };
                if projects
                    .metadata()
                    .update_command(
                        &command_id,
                        "projection_pending",
                        Some(&project.id),
                        Some(&imported.id),
                        None,
                        Some(&candidate.session_id),
                        None,
                    )
                    .await
                    .is_err()
                    || projects.reproject().await.is_err()
                    || projects
                        .metadata()
                        .update_command(
                            &command_id,
                            "committed",
                            Some(&project.id),
                            Some(&imported.id),
                            None,
                            Some(&candidate.session_id),
                            None,
                        )
                        .await
                        .is_err()
                {
                    return SubmitAck::Failed(action_error(
                        command_id,
                        ErrorCode::AgentUnavailable,
                        "session import commit failed",
                        true,
                    ));
                }
                self.send_metadata_ack(
                    &cmd,
                    AckStatus::Committed,
                    Some(&project.id),
                    Some(&imported.id),
                    None,
                    Some(candidate.session_id),
                )
                .await;
            }
            ActionEnvelope::PersistedSessionCreate { payload, .. } => {
                let (project, session_id) = prepared_create.expect("validated create preparation");
                self.spawn_persisted_activation(
                    cmd,
                    projects,
                    project,
                    session_id,
                    payload.title,
                    None,
                );
            }
            ActionEnvelope::PersistedSessionOpen { payload: _, .. } => {
                let (project, session, live_chat) =
                    prepared_open.expect("validated open preparation");
                let acp_id = session.acp_session_id.clone().expect("validated ACP id");
                if let Some(chat) = live_chat.as_deref() {
                    if projects
                        .metadata()
                        .record_session_runtime(&session.id, chat)
                        .await
                        .is_err()
                        || projects
                            .metadata()
                            .update_command(
                                &command_id,
                                "committed",
                                Some(&session.project_id),
                                Some(&session.id),
                                Some(chat),
                                Some(&acp_id),
                                None,
                            )
                            .await
                            .is_err()
                    {
                        return SubmitAck::Failed(action_error(
                            command_id,
                            ErrorCode::AgentUnavailable,
                            "session open finalize failed",
                            true,
                        ));
                    }
                    self.send_metadata_ack(
                        &cmd,
                        AckStatus::Committed,
                        Some(&session.project_id),
                        Some(&session.id),
                        Some(chat),
                        Some(acp_id),
                    )
                    .await;
                    return SubmitAck::Handled;
                }
                self.spawn_persisted_activation(
                    cmd,
                    projects,
                    project,
                    session.id,
                    session.acp_title,
                    Some(acp_id),
                );
            }
            _ => unreachable!(),
        }
        SubmitAck::Handled
    }

    fn spawn_persisted_activation(
        &self,
        cmd: ExecCmd,
        projects: ProjectService,
        project: crate::persist::metadata::ProjectRecord,
        session_id: String,
        title: Option<String>,
        acp_id: Option<String>,
    ) {
        let me = self.clone();
        tokio::spawn(async move {
            let chat_command_id = Uuid::new_v4().to_string();
            let (inner_tx, mut inner_rx) = mpsc::channel(16);
            let create = ActionEnvelope::Create {
                command_id: chat_command_id.clone(),
                payload: acp_hub_proto::action::CreateChatPayload {
                    instance_id: Some(project.instance_id.clone()),
                    cwd: Some(project.cwd.clone()),
                    title: title.clone(),
                    acp_session_id: acp_id.clone(),
                    workspace_id: None,
                },
            };
            if projects
                .metadata()
                .activation_phase(&session_id, "dispatch_pending", None, acp_id.as_deref())
                .await
                .is_err()
                || projects
                    .metadata()
                    .update_command(
                        &extract_command_id(&cmd.action).unwrap_or_default(),
                        "dispatched",
                        Some(&project.id),
                        Some(&session_id),
                        None,
                        acp_id.as_deref(),
                        None,
                    )
                    .await
                    .is_err()
            {
                me.reconcile_activation(&cmd, &projects, &session_id, "dispatch_barrier_failed")
                    .await;
                return;
            }
            // Persist the unsafe-to-retry boundary before inner submit can
            // enqueue/spawn any ACP lifecycle work.
            if projects
                .metadata()
                .activation_phase(&session_id, "dispatched", None, acp_id.as_deref())
                .await
                .is_err()
                || projects
                    .metadata()
                    .update_command(
                        &extract_command_id(&cmd.action).unwrap_or_default(),
                        "dispatched",
                        Some(&project.id),
                        Some(&session_id),
                        None,
                        acp_id.as_deref(),
                        None,
                    )
                    .await
                    .is_err()
            {
                me.reconcile_activation(&cmd, &projects, &session_id, "dispatched_barrier_failed")
                    .await;
                return;
            }
            match Box::pin(me.submit(&cmd.ctx, create, inner_tx)).await {
                SubmitAck::Accepted { .. } => {}
                _ => {
                    if projects
                        .metadata()
                        .fail_session(&session_id, "activation_submit_failed")
                        .await
                        .is_err()
                        || projects.reproject().await.is_err()
                    {
                        let _ = projects
                            .metadata()
                            .mark_reconciliation_required(
                                &session_id,
                                "activation_submit_failure_barrier_failed",
                            )
                            .await;
                    }
                    let _ = projects
                        .metadata()
                        .update_command(
                            &extract_command_id(&cmd.action).unwrap_or_default(),
                            "failed",
                            Some(&project.id),
                            Some(&session_id),
                            None,
                            acp_id.as_deref(),
                            Some("activation_submit_failed"),
                        )
                        .await;
                    me.send_error(
                        &cmd,
                        ErrorCode::AgentUnavailable,
                        "activation submit failed",
                        true,
                    )
                    .await;
                    return;
                }
            }
            while let Some(msg) = inner_rx.recv().await {
                match msg {
                    OutboundMsg::Frame(Frame::ActionAck(ack))
                        if ack.status == AckStatus::Committed =>
                    {
                        let Some(chat_id) = ack.chat_id else { break };
                        let bound = me
                            .inner
                            .chats
                            .entry(&chat_id)
                            .await
                            .and_then(|e| e.session_id);
                        let Some(acp_session_id) =
                            ack.acp_session_id.or_else(|| acp_id.clone()).or(bound)
                        else {
                            break;
                        };
                        if projects
                            .metadata()
                            .activation_phase(
                                &session_id,
                                "acp_id_durable",
                                Some(&chat_id),
                                Some(&acp_session_id),
                            )
                            .await
                            .is_err()
                        {
                            me.reconcile_activation(
                                &cmd,
                                &projects,
                                &session_id,
                                "acp_id_barrier_failed",
                            )
                            .await;
                            return;
                        }
                        let original = extract_command_id(&cmd.action).unwrap_or_default();
                        if projects
                            .metadata()
                            .finalize_session_and_command(
                                &original,
                                &session_id,
                                &project.id,
                                &acp_session_id,
                                title.as_deref(),
                                &chat_id,
                            )
                            .await
                            .is_err()
                        {
                            me.reconcile_activation(
                                &cmd,
                                &projects,
                                &session_id,
                                "finalize_barrier_failed",
                            )
                            .await;
                            return;
                        }
                        if projects.reproject().await.is_err() {
                            let _ = projects
                                .metadata()
                                .mark_reconciliation_required(&session_id, "projection_failed")
                                .await;
                            let _ = projects
                                .metadata()
                                .update_command(
                                    &original,
                                    "reconciliation_required",
                                    Some(&project.id),
                                    Some(&session_id),
                                    Some(&chat_id),
                                    Some(&acp_session_id),
                                    Some("projection_failed"),
                                )
                                .await;
                            me.send_error(
                                &cmd,
                                ErrorCode::AgentUnavailable,
                                "session projection failed",
                                true,
                            )
                            .await;
                            return;
                        }
                        if projects
                            .metadata()
                            .update_command(
                                &original,
                                "committed",
                                Some(&project.id),
                                Some(&session_id),
                                Some(&chat_id),
                                Some(&acp_session_id),
                                None,
                            )
                            .await
                            .is_err()
                        {
                            me.reconcile_activation(
                                &cmd,
                                &projects,
                                &session_id,
                                "command_commit_barrier_failed",
                            )
                            .await;
                            return;
                        }
                        me.send_metadata_ack(
                            &cmd,
                            AckStatus::Committed,
                            Some(&project.id),
                            Some(&session_id),
                            Some(&chat_id),
                            Some(acp_session_id),
                        )
                        .await;
                        return;
                    }
                    OutboundMsg::Frame(Frame::ActionError(_)) => {
                        let original = extract_command_id(&cmd.action).unwrap_or_default();
                        let _ = projects
                            .metadata()
                            .reconcile_activation_and_command(
                                &session_id,
                                &original,
                                "activation_failed_or_unknown",
                            )
                            .await;
                        let _ = projects.reproject().await;
                        me.send_error(
                            &cmd,
                            ErrorCode::AgentUnavailable,
                            "session activation failed; reconciliation required",
                            false,
                        )
                        .await;
                        return;
                    }
                    _ => {}
                }
            }
            let original = extract_command_id(&cmd.action).unwrap_or_default();
            let _ = projects
                .metadata()
                .reconcile_activation_and_command(
                    &session_id,
                    &original,
                    "activation_channel_closed",
                )
                .await;
            let _ = projects.reproject().await;
            me.send_error(
                &cmd,
                ErrorCode::AgentUnavailable,
                "session activation outcome unknown",
                false,
            )
            .await;
        });
    }

    async fn send_metadata_ack(
        &self,
        cmd: &ExecCmd,
        status: AckStatus,
        project_id: Option<&str>,
        session_id: Option<&str>,
        chat_id: Option<&str>,
        acp_id: Option<String>,
    ) {
        let _ = cmd
            .tx
            .send(OutboundMsg::Frame(Frame::ActionAck(ActionAck {
                command_id: extract_command_id(&cmd.action).unwrap_or_default(),
                status,
                turn_id: None,
                chat_id: chat_id.map(str::to_string),
                project_id: project_id.map(str::to_string),
                session_id: session_id.map(str::to_string),
                acp_session_id: acp_id,
                committed_projection_version: None,
            })))
            .await;
    }

    async fn reconcile_activation(
        &self,
        cmd: &ExecCmd,
        projects: &ProjectService,
        session_id: &str,
        code: &str,
    ) {
        let original = extract_command_id(&cmd.action).unwrap_or_default();
        let _ = projects
            .metadata()
            .reconcile_activation_and_command(session_id, &original, code)
            .await;
        let _ = projects.reproject().await;
        self.send_error(
            cmd,
            ErrorCode::AgentUnavailable,
            "session activation requires reconciliation",
            false,
        )
        .await;
    }

    /// 提交入口（§7.4 规则 6）：临界区内 去重判定 → try_reserve → 入队。
    ///
    /// `tx` 为客户端连接发送队列（执行器终态回投）。
    pub async fn submit(
        &self,
        ctx: &ConnectionCtx,
        action: ActionEnvelope,
        tx: mpsc::Sender<OutboundMsg>,
    ) -> SubmitAck {
        if matches!(
            action,
            ActionEnvelope::ProjectCreate { .. }
                | ActionEnvelope::ProjectArchive { .. }
                | ActionEnvelope::ProjectRestore { .. }
                | ActionEnvelope::ProjectRename { .. }
                | ActionEnvelope::PersistedSessionCreate { .. }
                | ActionEnvelope::PersistedSessionOpen { .. }
                | ActionEnvelope::PersistedSessionRename { .. }
                | ActionEnvelope::PersistedSessionArchive { .. }
                | ActionEnvelope::PersistedSessionRestore { .. }
                | ActionEnvelope::PersistedSessionImport { .. }
        ) {
            return self.submit_metadata_action(ctx, action, tx).await;
        }
        // commandId（幂等键，uuid 形态，§4.3）。
        let command_id_str = match extract_command_id(&action) {
            Some(c) => c,
            None => {
                return SubmitAck::Failed(action_error(
                    String::new(),
                    ErrorCode::InvalidState,
                    "missing commandId",
                    false,
                ))
            }
        };
        let command_id = match uuid::Uuid::parse_str(&command_id_str) {
            Ok(id) => id,
            Err(_) => {
                return SubmitAck::Failed(action_error(
                    command_id_str,
                    ErrorCode::InvalidState,
                    "invalid commandId (uuid expected)",
                    false,
                ))
            }
        };

        // workspace 管理命令（独立于 chat 的上层概念）：不占 chat 队列/outbox/
        // reserve——管理面低频操作，直接执行后回 committed（无两阶段队列语义）。
        if matches!(
            action,
            ActionEnvelope::WorkspaceCreate { .. } | ActionEnvelope::WorkspaceRemove { .. }
        ) {
            return self
                .exec_workspace_command(ctx, &action, tx, &command_id_str)
                .await;
        }

        // session/list 按需查询（§6.3）：无副作用只读查询，同样不走 chat
        // 队列/outbox——直接向 agent 侧发 session/list RPC，结果经
        // session_list 下行帧回投（agent 侧是真实数据源，非轮询投影过滤）。
        if let ActionEnvelope::SessionList { .. } = &action {
            return self
                .exec_session_list(ctx, &action, tx, &command_id_str)
                .await;
        }

        if let ActionEnvelope::PersistedSessionPromptStatus { .. } = &action {
            return self
                .exec_prompt_status(ctx, &action, tx, &command_id_str)
                .await;
        }

        if let ActionEnvelope::PersistedSessionDiscover { .. } = &action {
            return self.exec_project_session_discover(ctx, action, tx).await;
        }

        // chat/load 会话切换（§8.5）：在当前对话（其 ACP 进程）内把目标
        // 历史会话加载为进程的当前会话——会话是进程内实体，**不新建
        // chat/进程**（点击 SessionList 历史会话 = 当前对话内 load）。
        // 低频直通（同 workspace/session-list 管理面），不走 chat 队列。
        if let ActionEnvelope::Load { .. } = &action {
            return self.exec_load_chat(ctx, &action, tx, &command_id_str).await;
        }

        // chat/session-new（§8.5）：当前对话内新建 ACP 会话——等价 create
        // 序列的 `session/new` 一步（进程已存在，无 spawn/initialize）。
        // 低频直通（同 load），不走 chat 队列。
        if let ActionEnvelope::SessionNew { .. } = &action {
            return self
                .exec_session_new(ctx, &action, tx, &command_id_str)
                .await;
        }

        let _guard = self.inner.gate.lock().await;

        // ---- 1. chat_id 解析 / create 前置（§6.2）----
        let chat_id: uuid::Uuid = match &action {
            ActionEnvelope::Create { payload, .. } => {
                // create：server 生成新 chat_id（§6.2 server 生成 id 的
                // 唯一告知路径）。客户端重发同 commandId 时无法指定
                // chat_id——先按 commandId 全局去重（§4.4：committed →
                // duplicate，不重复调用 Agent；索引启动时从 outbox 重建，
                // 跨 server 重启有效）。
                if let Some(sid) = self
                    .inner
                    .create_index
                    .read()
                    .await
                    .get(&command_id)
                    .cloned()
                {
                    if let Some(s_store) = self.inner.store.chat(sid) {
                        if let Some(rec) = s_store.outbox_get(command_id).await {
                            match dedup_verdict(&rec) {
                                DedupVerdict::Duplicate => {
                                    return SubmitAck::Duplicate(ActionAck {
                                        command_id: command_id_str,
                                        status: AckStatus::Duplicate,
                                        turn_id: rec.turn_id.map(|t| t.to_string()),
                                        chat_id: Some(sid.to_string()),
                                        project_id: None,
                                        session_id: None,
                                        acp_session_id: None,
                                        committed_projection_version: None,
                                    })
                                }
                                DedupVerdict::RedeliverFailed => {
                                    let err = rec.last_error.clone().unwrap_or_else(|| {
                                        LastError::from_error_code(ErrorCode::InvalidState)
                                    });
                                    return SubmitAck::Failed(ActionError {
                                        command_id: command_id_str,
                                        code: error_code_from_str(&err.code),
                                        message: "command previously failed; retry not permitted"
                                            .to_string(),
                                        retryable: err.retryable,
                                        retry_after_ms: None,
                                    });
                                }
                                DedupVerdict::RedeliverUnknown => {
                                    return SubmitAck::Failed(action_error(
                                        command_id_str,
                                        ErrorCode::DeliveryUnknown,
                                        "delivery unknown; automatic retry not permitted (path B)",
                                        false,
                                    ))
                                }
                                DedupVerdict::InProgress => {
                                    if self.inner.terminal_failure_overflow.load(Ordering::Acquire)
                                    {
                                        return SubmitAck::Failed(action_error(
                                            command_id_str,
                                            ErrorCode::DeliveryUnknown,
                                            "delivery terminal storage is degraded; observer attachment is blocked",
                                            false,
                                        ));
                                    }
                                    if let Some(error) = self
                                        .inner
                                        .terminal_failures
                                        .lock()
                                        .await
                                        .get(&command_id)
                                        .cloned()
                                    {
                                        return SubmitAck::Failed(error);
                                    }
                                    let mut watchers = self.inner.terminal_watchers.lock().await;
                                    if let Some(latest) = s_store.outbox_get(command_id).await {
                                        if let Some(replay) = terminal_replay(
                                            command_id_str.clone(),
                                            &latest,
                                            Some(sid.to_string()),
                                        ) {
                                            return replay;
                                        }
                                    }
                                    if !attach_terminal_watcher_locked(
                                        &mut watchers,
                                        command_id,
                                        tx.clone(),
                                    ) {
                                        return SubmitAck::Failed(action_error(
                                            command_id_str,
                                            ErrorCode::RateLimited,
                                            "too many command observers",
                                            false,
                                        ));
                                    }
                                    return SubmitAck::Accepted {
                                        command_id: command_id_str,
                                    };
                                }
                                DedupVerdict::Proceed => {}
                            }
                        }
                    }
                }
                match self.prepare_create(ctx, payload, &command_id_str).await {
                    Ok(sid) => {
                        // 登记全局 create 索引（重发去重，§4.4）。
                        self.inner
                            .create_index
                            .write()
                            .await
                            .insert(command_id, sid);
                        sid
                    }
                    Err(ack) => return ack,
                }
            }
            other => match extract_chat_id(other) {
                Some(sid) => match uuid::Uuid::parse_str(&sid) {
                    Ok(id) => id,
                    Err(_) => {
                        return SubmitAck::Failed(action_error(
                            command_id_str,
                            ErrorCode::InvalidState,
                            "invalid chatId (uuid expected)",
                            false,
                        ))
                    }
                },
                None => {
                    return SubmitAck::Failed(action_error(
                        command_id_str,
                        ErrorCode::InvalidState,
                        "missing chatId",
                        false,
                    ))
                }
            },
        };

        let chat_id_str = chat_id.to_string();
        let store = match self.inner.store.chat(chat_id) {
            Some(s) => s,
            None => {
                return SubmitAck::Failed(action_error(
                    command_id_str,
                    ErrorCode::ChatNotFound,
                    "chat not found",
                    false,
                ))
            }
        };

        // ---- 2. 去重判定（outbox 记录，§4.4）----
        let resume_permission = if let Some(rec) = store.outbox_get(command_id).await {
            if rec.recovery.is_some() {
                if !permission_recovery_payload_matches(&rec, &action) {
                    return SubmitAck::Failed(action_error(
                        command_id_str,
                        ErrorCode::InvalidState,
                        "command payload conflicts with durable permission recovery evidence",
                        false,
                    ));
                }
                if rec.status != OutboxStatus::IntentDurable {
                    return SubmitAck::Failed(action_error(
                        command_id_str,
                        ErrorCode::DeliveryUnknown,
                        "permission delivery result is unknown; automatic retry is not permitted",
                        false,
                    ));
                }
                if self
                    .inner
                    .chats
                    .entry(&chat_id_str)
                    .await
                    .is_none_or(|entry| entry.state.is_terminal())
                {
                    return SubmitAck::Failed(action_error(
                        command_id_str,
                        ErrorCode::DeliveryUnknown,
                        "the original runtime no longer exists; permission delivery requires operator reconciliation",
                        false,
                    ));
                }
                true
            } else {
                match dedup_verdict(&rec) {
                    DedupVerdict::Duplicate => {
                        return SubmitAck::Duplicate(ActionAck {
                            command_id: command_id_str,
                            status: AckStatus::Duplicate,
                            turn_id: rec.turn_id.map(|t| t.to_string()),
                            chat_id: None,
                            project_id: None,
                            session_id: None,
                            acp_session_id: None,
                            committed_projection_version: None,
                        })
                    }
                    DedupVerdict::RedeliverFailed => {
                        let err = rec
                            .last_error
                            .clone()
                            .unwrap_or_else(|| LastError::from_error_code(ErrorCode::InvalidState));
                        return SubmitAck::Failed(ActionError {
                            command_id: command_id_str,
                            code: error_code_from_str(&err.code),
                            message: "command previously failed; retry not permitted".to_string(),
                            retryable: err.retryable,
                            retry_after_ms: None,
                        });
                    }
                    DedupVerdict::RedeliverUnknown => {
                        return SubmitAck::Failed(action_error(
                            command_id_str,
                            ErrorCode::DeliveryUnknown,
                            "delivery unknown; automatic retry not permitted (path B)",
                            false,
                        ))
                    }
                    DedupVerdict::InProgress => {
                        if self.inner.terminal_failure_overflow.load(Ordering::Acquire) {
                            return SubmitAck::Failed(action_error(
                                command_id_str,
                                ErrorCode::DeliveryUnknown,
                                "delivery terminal storage is degraded; observer attachment is blocked",
                                false,
                            ));
                        }
                        if let Some(error) = self
                            .inner
                            .terminal_failures
                            .lock()
                            .await
                            .get(&command_id)
                            .cloned()
                        {
                            return SubmitAck::Failed(error);
                        }
                        if let ActionEnvelope::Prompt { payload, .. } = &action {
                            let incoming = match prompt_payload_fingerprint(payload) {
                                Ok(value) => value,
                                Err(error) => {
                                    return SubmitAck::Failed(action_error(
                                        command_id_str,
                                        ErrorCode::InvalidState,
                                        &format!("prompt fingerprint failed: {error}"),
                                        false,
                                    ));
                                }
                            };
                            if rec.payload_fingerprint.as_deref() != Some(incoming.as_str()) {
                                return SubmitAck::Failed(action_error(
                                    command_id_str,
                                    ErrorCode::InvalidState,
                                    "commandId payload conflicts with durable prompt intent",
                                    false,
                                ));
                            }
                        }
                        let mut watchers = self.inner.terminal_watchers.lock().await;
                        if let Some(latest) = store.outbox_get(command_id).await {
                            if let Some(replay) =
                                terminal_replay(command_id_str.clone(), &latest, None)
                            {
                                return replay;
                            }
                        }
                        if !attach_terminal_watcher_locked(&mut watchers, command_id, tx.clone()) {
                            return SubmitAck::Failed(action_error(
                                command_id_str,
                                ErrorCode::RateLimited,
                                "too many command observers",
                                false,
                            ));
                        }
                        return SubmitAck::Accepted {
                            command_id: command_id_str,
                        };
                    }
                    DedupVerdict::Proceed => {}
                }
                false
            }
        } else {
            false
        };

        // ---- 3. 入队上限（§7.4 规则 6：同一临界区）----
        if !self.inner.doc.try_reserve(&chat_id_str).await {
            return SubmitAck::Failed(action_error(
                command_id_str,
                ErrorCode::RateLimited,
                "command queue full",
                false,
            ));
        }

        // ---- 4. outbox 落盘（去重记录持久化，§4.4）----
        let turn_id = match &action {
            ActionEnvelope::Prompt { .. } => Some(uuid::Uuid::new_v4()),
            _ => None,
        };
        let command_type = command_type_of(&action);
        let retryable_class = command_type.default_retryable_class();
        if !resume_permission {
            if let Err(e) = store.outbox().lock().await.insert(NewOutboxRecord {
                command_id,
                chat_id,
                command_type,
                turn_id,
                retryable_class,
            }) {
                warn!(chat_id = %chat_id, command_id = %command_id, error = ?e, "outbox insert failed");
                self.inner.doc.release_reserve(&chat_id_str).await;
                return SubmitAck::Failed(action_error(
                    command_id_str,
                    ErrorCode::AgentUnavailable,
                    "outbox persist failed",
                    true,
                ));
            }
            // insert(received) → mark_accepted（同一临界区）：accepted Ack 语义
            // 与 outbox 状态机对齐（Received → Accepted → IntentDurable，§4.4；
            // 执行器 mark_intent_durable 要求前置 Accepted）。
            if let Err(e) = store.outbox().lock().await.mark_accepted(command_id) {
                warn!(chat_id = %chat_id, command_id = %command_id, error = ?e, "outbox mark_accepted failed");
                let _ = store.outbox().lock().await.mark_failed(
                    command_id,
                    LastError::from_error_code(ErrorCode::AgentUnavailable),
                );
                self.inner.doc.release_reserve(&chat_id_str).await;
                return SubmitAck::Failed(action_error(
                    command_id_str,
                    ErrorCode::AgentUnavailable,
                    "outbox mark_accepted failed",
                    false,
                ));
            }
            if let ActionEnvelope::Prompt { payload, .. } = &action {
                let fingerprint = match prompt_payload_fingerprint(payload) {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = store.outbox().lock().await.mark_failed(
                            command_id,
                            LastError::from_error_code(ErrorCode::InvalidState),
                        );
                        self.inner.doc.release_reserve(&chat_id_str).await;
                        return SubmitAck::Failed(action_error(
                            command_id_str,
                            ErrorCode::InvalidState,
                            &format!("prompt fingerprint failed: {error}"),
                            false,
                        ));
                    }
                };
                if let Err(error) = store
                    .outbox()
                    .lock()
                    .await
                    .set_prompt_payload_fingerprint(command_id, fingerprint)
                {
                    warn!(chat_id = %chat_id, command_id = %command_id, error = ?error, "prompt fingerprint persist failed");
                    let terminal_persisted = store
                        .outbox()
                        .lock()
                        .await
                        .mark_failed(
                            command_id,
                            LastError::from_error_code(ErrorCode::AgentUnavailable),
                        )
                        .is_ok();
                    if !terminal_persisted {
                        self.inner
                            .terminal_failure_overflow
                            .store(true, Ordering::Release);
                    }
                    self.inner.doc.release_reserve(&chat_id_str).await;
                    return SubmitAck::Failed(action_error(
                        command_id_str,
                        if terminal_persisted {
                            ErrorCode::AgentUnavailable
                        } else {
                            ErrorCode::DeliveryUnknown
                        },
                        "prompt identity could not be durably established",
                        false,
                    ));
                }
            }
        }

        // ---- 5. 入队执行器（accepted 立即返回，§4.4）----
        let cmd = ExecCmd {
            ctx: ctx.clone(),
            chat_id: chat_id_str.clone(),
            action,
            tx,
        };
        let queued = if command_type == CommandType::Create {
            self.enqueue_create(cmd).await
        } else {
            self.enqueue(chat_id_str.clone(), cmd).await
        };
        if !queued {
            // 入队失败（执行器已退出）：补偿释放名额（§7.4 reserve/release 配对）。
            let _ = store.outbox().lock().await.mark_failed(
                command_id,
                LastError::from_error_code(ErrorCode::RateLimited),
            );
            self.inner.doc.release_reserve(&chat_id_str).await;
            return SubmitAck::Failed(action_error(
                command_id_str,
                ErrorCode::RateLimited,
                "executor queue full",
                false,
            ));
        }
        audit(
            "command.submit",
            Some(&command_id_str),
            Some(&ctx.token_id),
            "ok",
            std::time::Duration::ZERO,
            None,
        );
        SubmitAck::Accepted {
            command_id: command_id_str,
        }
    }

    /// create 前置（临界区内）：生成 chat_id + 解析 cwd（workspace_id →
    /// workspace.cwd；否则 payload.cwd；否则 server 默认目录）+ 建持久化
    /// 目录 + 打开 doc + 登记 chat + outbox 目录就绪。
    async fn prepare_create(
        &self,
        ctx: &ConnectionCtx,
        payload: &CreateChatPayload,
        command_id: &str,
    ) -> Result<uuid::Uuid, SubmitAck> {
        let chat_id = uuid::Uuid::new_v4();
        let instance_id = payload
            .instance_id
            .clone()
            .unwrap_or_else(|| DEFAULT_INSTANCE_ID.to_string());
        // cwd 解析（workspace 继承优先，§6.3 workspace 扩展）：workspace_id
        // 存在但查不到 → 明确失败（不静默回退到默认目录，否则用户以为在
        // workspace 下建了对话、实际跑在 server 目录）。payload.cwd 直传
        // 时须过形态校验（后续 initialize_rpc 内部 validate_cwd expect——
        // 客户端输入必须在此拦截，防 panic）。
        let cwd = match &payload.workspace_id {
            Some(ws_id) => match self.inner.workspaces.get(ws_id).await {
                Some(ws) => ws.cwd,
                None => {
                    return Err(SubmitAck::Failed(action_error(
                        command_id.to_string(),
                        ErrorCode::InvalidState,
                        &format!("workspace not found: {ws_id}"),
                        false,
                    )))
                }
            },
            None => match &payload.cwd {
                Some(c) if !c.trim().is_empty() => {
                    if let Err(e) = validate_cwd(c) {
                        return Err(SubmitAck::Failed(action_error(
                            command_id.to_string(),
                            ErrorCode::InvalidState,
                            &format!("invalid cwd: {e}"),
                            false,
                        )));
                    }
                    c.clone()
                }
                _ => self.inner.default_cwd.clone(),
            },
        };
        // 标题缺省（§6.5 服务端单写会话元数据）：前端 create 可不传 title，
        // 缺省「会话 {短 id}」——列表不显示裸 id。
        let title = payload
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("会话 {}", &chat_id.to_string()[..8]));
        if let Err(e) = self.inner.store.create_chat(chat_id) {
            warn!(error = ?e, "create chat store failed");
            return Err(SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::AgentUnavailable,
                "chat store create failed",
                true,
            )));
        }
        if let Err(e) = self
            .inner
            .doc
            .open_chat(
                &chat_id.to_string(),
                &instance_id,
                Some(&title),
                Some(&cwd),
                payload.workspace_id.as_deref(),
            )
            .await
        {
            warn!(chat_id = %chat_id, error = ?e, "open chat failed");
            return Err(SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::AgentUnavailable,
                "chat doc open failed",
                true,
            )));
        }
        if let Err(e) = self
            .inner
            .chats
            .register(
                &chat_id.to_string(),
                &instance_id,
                Some(&title),
                &cwd,
                payload.workspace_id.as_deref(),
            )
            .await
        {
            warn!(chat_id = %chat_id, error = ?e, "chat register failed");
            return Err(SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::AgentUnavailable,
                "chat register failed",
                true,
            )));
        }
        let _ = ctx;
        Ok(chat_id)
    }

    /// 入队到 per-chat 执行器（lazy spawn，§7.4 规则 1 串行）。
    async fn enqueue(&self, chat_id: String, cmd: ExecCmd) -> bool {
        let tx = {
            let executors = self.inner.executors.read().await;
            executors.get(&chat_id).cloned()
        };
        let tx = match tx {
            Some(tx) => tx,
            None => {
                let (tx, rx) = mpsc::channel(self.inner.queue_cap);
                let me = self.clone();
                let sid = chat_id.clone();
                tokio::spawn(async move {
                    me.executor_loop(sid, rx).await;
                });
                self.inner
                    .executors
                    .write()
                    .await
                    .insert(chat_id, tx.clone());
                tx
            }
        };
        tx.send(cmd).await.is_ok()
    }

    /// 入队到全局 create 执行器（串行，§6.2 create 时序）。
    async fn enqueue_create(&self, cmd: ExecCmd) -> bool {
        let tx = {
            let lock = self.inner.create_tx.read().await;
            lock.clone()
        };
        let tx = match tx {
            Some(tx) => tx,
            None => {
                let (tx, rx) = mpsc::channel(self.inner.queue_cap);
                let me = self.clone();
                tokio::spawn(async move {
                    me.executor_loop(String::from("__create__"), rx).await;
                });
                *self.inner.create_tx.write().await = Some(tx.clone());
                tx
            }
        };
        tx.send(cmd).await.is_ok()
    }

    /// 执行器循环（每 chat 串行消费，§7.4 规则 1）。
    async fn executor_loop(&self, chat_id: String, mut rx: mpsc::Receiver<ExecCmd>) {
        while let Some(cmd) = rx.recv().await {
            let started = std::time::Instant::now();
            self.exec_command(&chat_id, &cmd).await;
            // 命令消费完成：释放 try_reserve 名额（§7.4 reserve/release 配对）。
            self.inner.doc.release_reserve(&cmd.chat_id).await;
            debug!(
                chat_id,
                command_id = extract_command_id(&cmd.action).unwrap_or_default(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "command executed"
            );
        }
        // 通道关闭：清理表项（防御；正常关闭路径由 hub 统一清理）。
        if chat_id != "__create__" {
            let mut executors = self.inner.executors.write().await;
            if let Some(tx) = executors.get(&chat_id) {
                if tx.is_closed() {
                    executors.remove(&chat_id);
                }
            }
        }
    }

    /// 命令分发（§7 方法面）。
    async fn exec_command(&self, chat_id: &str, cmd: &ExecCmd) {
        match &cmd.action {
            ActionEnvelope::Prompt { .. } => self.exec_prompt(chat_id, cmd).await,
            ActionEnvelope::Create { .. } => self.exec_create(chat_id, cmd).await,
            ActionEnvelope::Cancel { .. } => self.exec_forward(chat_id, cmd).await,
            ActionEnvelope::Close { .. } => self.exec_close(chat_id, cmd).await,
            ActionEnvelope::ResolvePermission { .. } => self.exec_resolve(chat_id, cmd).await,
            _ => {
                // M1 action type 白名单外的 action（Load/SubscribeEvents/
                // UnsubscribeEvents）已在 chat_channel::dispatch_action
                // 拦截（§4.8）；此处为防御路径。
                self.send_error(
                    cmd,
                    ErrorCode::UnsupportedFrame,
                    "unsupported action",
                    false,
                )
                .await;
            }
        }
    }

    /// workspace 管理命令直接执行（管理面，低频；不入 chat 队列/outbox/
    /// reserve——无幂等去重，重复提交产生重复定义，UI 层可控）。成功后经
    /// 连接发送队列回 committed；失败回 action_error。submit 层面返回
    /// Accepted（chat_channel 发 accepted ack，与 create 两阶段一致）。
    async fn exec_workspace_command(
        &self,
        ctx: &ConnectionCtx,
        action: &ActionEnvelope,
        tx: mpsc::Sender<OutboundMsg>,
        command_id: &str,
    ) -> SubmitAck {
        let cmd = ExecCmd {
            ctx: ctx.clone(),
            chat_id: String::new(),
            action: action.clone(),
            tx,
        };
        match action {
            ActionEnvelope::WorkspaceCreate { payload, .. } => {
                let Some(projects) = self.inner.projects.read().await.clone() else {
                    return SubmitAck::Failed(action_error(
                        command_id.to_string(),
                        ErrorCode::AgentUnavailable,
                        "metadata catalog unavailable",
                        true,
                    ));
                };
                if let Err(e) = validate_cwd(&payload.cwd) {
                    self.send_error(
                        &cmd,
                        ErrorCode::InvalidState,
                        &format!("invalid cwd: {e}"),
                        false,
                    )
                    .await;
                } else if !std::path::Path::new(&payload.cwd).is_dir() {
                    self.send_error(
                        &cmd,
                        ErrorCode::InvalidState,
                        &format!("cwd not found: {}", payload.cwd),
                        false,
                    )
                    .await;
                } else {
                    let id = Uuid::new_v4().to_string();
                    let name = if payload.name.trim().is_empty() {
                        std::path::Path::new(&payload.cwd)
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_else(|| id[..8].to_string())
                    } else {
                        payload.name.trim().to_string()
                    };
                    match projects
                        .create_project(&id, &name, &payload.cwd, DEFAULT_INSTANCE_ID)
                        .await
                    {
                        Ok(rec) if projects.mirror_legacy_workspace(&rec).await.is_ok() => {
                            self.inner.workspaces.rebuild().await;
                            audit(
                                "workspace.create",
                                Some(command_id),
                                Some(&cmd.ctx.token_id),
                                "ok",
                                std::time::Duration::ZERO,
                                None,
                            );
                            debug!(workspace_id = %rec.id, cwd = %rec.cwd, "workspace created");
                            self.send_committed(&cmd, None, None).await;
                        }
                        Err(e) => {
                            warn!(error = ?e, "workspace create metadata write failed");
                            self.send_error(
                                &cmd,
                                ErrorCode::AgentUnavailable,
                                "workspace metadata write failed",
                                true,
                            )
                            .await;
                        }
                        Ok(_) => {
                            self.send_error(
                                &cmd,
                                ErrorCode::AgentUnavailable,
                                "workspace projection failed",
                                true,
                            )
                            .await
                        }
                    }
                }
            }
            ActionEnvelope::WorkspaceRemove { payload, .. } => {
                let Some(projects) = self.inner.projects.read().await.clone() else {
                    return SubmitAck::Failed(action_error(
                        command_id.to_string(),
                        ErrorCode::AgentUnavailable,
                        "metadata catalog unavailable",
                        true,
                    ));
                };
                match projects.archive_project(&payload.workspace_id).await {
                    Ok(()) => {
                        if projects
                            .metadata()
                            .project(&payload.workspace_id)
                            .await
                            .is_ok()
                        {
                            let _ = self
                                .inner
                                .workspaces
                                .registry()
                                .remove_workspace(&payload.workspace_id)
                                .await;
                            self.inner.workspaces.rebuild().await;
                        }
                        audit(
                            "workspace.remove",
                            Some(command_id),
                            Some(&cmd.ctx.token_id),
                            "ok",
                            std::time::Duration::ZERO,
                            None,
                        );
                        debug!(workspace_id = %payload.workspace_id, "workspace removed");
                        self.send_committed(&cmd, None, None).await;
                    }
                    Err(crate::control::ProjectServiceError::Metadata(
                        MetadataError::NotFound(id),
                    )) => {
                        self.send_error(
                            &cmd,
                            ErrorCode::InvalidState,
                            &format!("workspace not found: {id}"),
                            false,
                        )
                        .await;
                    }
                    Err(e) => {
                        warn!(error = ?e, "workspace remove registry write failed");
                        self.send_error(
                            &cmd,
                            ErrorCode::DeliveryUnknown,
                            "workspace registry write failed",
                            true,
                        )
                        .await;
                    }
                }
            }
            _ => unreachable!("dispatch guarantees workspace command"),
        }
        SubmitAck::Accepted {
            command_id: command_id.to_string(),
        }
    }

    /// session/list 按需查询执行（§6.3 workspace 扩展）：agent 侧是真实
    /// 数据源——不依赖轮询投影的前端过滤。
    ///
    /// 流程：chat record 解析 (instance_id, cwd) → 复用 L3 匹配
    /// （register_rpc + forward_rpc + 响应匹配）→ 结果经 `session_list`
    /// 下行帧回投客户端连接。只读查询：无 outbox/队列/副作用，失败回
    /// `action_error`（不静默）。submit 层面返回 Accepted（chat_channel 发
    /// accepted ack），结果帧随后异步到达。
    async fn exec_session_list(
        &self,
        ctx: &ConnectionCtx,
        action: &ActionEnvelope,
        tx: mpsc::Sender<OutboundMsg>,
        command_id: &str,
    ) -> SubmitAck {
        let payload = match action {
            ActionEnvelope::SessionList { payload, .. } => payload,
            _ => unreachable!("dispatch guarantees session/list"),
        };
        // chat record：不存在 → CHAT_NOT_FOUND；终态/未 binding（无 ACP
        // 进程）→ INVALID_STATE（查询面不存在，§6.3）。
        let Some(rec) = self.inner.chats.entry(&payload.chat_id).await else {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::ChatNotFound,
                "chat not found",
                false,
            ));
        };
        if rec.state.is_terminal() || rec.session_id.is_none() {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::InvalidState,
                "chat has no active ACP process",
                false,
            ));
        }
        let instance_id = rec.instance_id.clone();
        let cwd = rec.cwd.clone();
        // 转发目标 = hub chat id（instance 进程表键，与轮询/命令转发一致）；
        // **不可**用 bound 的 acp session id——instance 以 hub id 寻址 ACP
        // 进程（§6.2 spawn 时注册），session_id 只是 binding 的校验/关联键。
        let chat_id = payload.chat_id.clone();
        // 当前活跃会话（§8.5：列表标注「当前」用；load 切换后随之更新）。
        let current_active = rec.session_id.clone();

        // 后台执行：L3 匹配（与轮询同款，§4.4 路径 B 超时语义）——结果
        // 帧经 tx 回投；失败回 action_error。
        let me = self.clone();
        let cmd_id = command_id.to_string();
        let token_id = ctx.token_id.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let rpc_id = me.inner.translator.alloc_rpc_id();
            let msg = serde_json::json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "method": "session/list",
                "params": { "cwd": cwd },
            });
            let rx = me
                .inner
                .relay
                .register_rpc(&rpc_id, "session_list".to_string())
                .await;
            if let Err(e) = me
                .inner
                .instance
                .forward_rpc(&instance_id, &chat_id, &msg)
                .await
            {
                me.inner.relay.cancel_rpc(&rpc_id).await;
                audit(
                    "session.list",
                    Some(&cmd_id),
                    Some(&token_id),
                    "forward_failed",
                    started.elapsed(),
                    None,
                );
                let _ = tx
                    .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                        cmd_id,
                        ErrorCode::InstanceOffline,
                        &format!("session list forward failed: {e}"),
                        true,
                    ))))
                    .await;
                return;
            }
            match tokio::time::timeout(SESSION_POLL_TIMEOUT, rx).await {
                Ok(Ok(r)) if r.get("error").is_none() => {
                    let mut entries = parse_session_list_response(&r);
                    // 条目标注所属 cwd（查询面，§6.3）+ 当前会话标记
                    // （§8.5）：会话是**进程内实体**——列表属于本对话（进程），
                    // 与当前活跃会话同 id 的条目带 bound_chat_id（= 本 chat_id，
                    // 前端标「当前」）；其余为历史会话（None，点击可 load 切换）。
                    for e in &mut entries {
                        e.cwd = cwd.clone();
                        e.bound_chat_id =
                            if Some(e.session_id.as_str()) == current_active.as_deref() {
                                Some(chat_id.clone())
                            } else {
                                None
                            };
                    }
                    me.refresh_catalog_titles(&entries).await;
                    audit(
                        "session.list",
                        Some(&cmd_id),
                        Some(&token_id),
                        "ok",
                        started.elapsed(),
                        None,
                    );
                    let _ = tx
                        .send(OutboundMsg::Frame(Frame::SessionList(SessionListFrame {
                            command_id: cmd_id,
                            chat_id: chat_id.clone(),
                            sessions: entries,
                        })))
                        .await;
                }
                _ => {
                    // 超时/错误响应/通道关闭：撤销 pending 表项回 error
                    // （session/list 无副作用，可安全重试）。
                    me.inner.relay.cancel_rpc(&rpc_id).await;
                    audit(
                        "session.list",
                        Some(&cmd_id),
                        Some(&token_id),
                        "timeout",
                        started.elapsed(),
                        None,
                    );
                    let _ = tx
                        .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                            cmd_id,
                            ErrorCode::AgentUnavailable,
                            "session list query timeout",
                            true,
                        ))))
                        .await;
                }
            }
        });
        SubmitAck::Accepted {
            command_id: command_id.to_string(),
        }
    }

    /// Read-only, body-free recovery view for a logical session. Authorization
    /// begins at the catalog identity; historical chat ids are resolved only
    /// from server-owned provenance.
    async fn exec_prompt_status(
        &self,
        _ctx: &ConnectionCtx,
        action: &ActionEnvelope,
        tx: mpsc::Sender<OutboundMsg>,
        command_id: &str,
    ) -> SubmitAck {
        let ActionEnvelope::PersistedSessionPromptStatus { payload, .. } = action else {
            unreachable!("dispatch guarantees session/prompt-status")
        };
        let Some(projects) = self.inner.projects.read().await.clone() else {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::AgentUnavailable,
                "project catalog unavailable",
                true,
            ));
        };
        let Some(session) = projects
            .metadata()
            .session(&payload.session_id)
            .await
            .ok()
            .flatten()
            .filter(|session| session.origin != "legacy_hidden")
        else {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::InvalidState,
                "session not found",
                false,
            ));
        };
        let runtimes = match projects.metadata().session_runtimes(&session.id).await {
            Ok(runtimes) => runtimes,
            Err(_) => {
                return SubmitAck::Failed(action_error(
                    command_id.to_string(),
                    ErrorCode::AgentUnavailable,
                    "session runtime history unavailable",
                    true,
                ))
            }
        };
        let sink = self.inner.history_sink.read().await.clone();
        let store = self.inner.store.clone();
        let session_id = session.id;
        let response_command_id = command_id.to_string();
        tokio::spawn(async move {
            let mut prompts_by_command = HashMap::<String, PromptStatusItem>::new();
            let mut evidence_incomplete = sink.is_none();
            for runtime in runtimes {
                let Ok(chat_id) = Uuid::parse_str(&runtime.chat_id) else {
                    evidence_incomplete = true;
                    continue;
                };
                let Some(chat_store) = store.chat(chat_id) else {
                    evidence_incomplete = true;
                    continue;
                };
                let evidence = if let Some(sink) = &sink {
                    let evidence = prompt_projection_evidence(
                        sink.snapshot(&acp_hub_proto::conn::DocId::chat(&runtime.chat_id))
                            .await,
                        sink.snapshot(&acp_hub_proto::conn::DocId::session(&runtime.chat_id))
                            .await,
                    );
                    match evidence {
                        Some((evidence, complete)) => {
                            evidence_incomplete |= !complete;
                            evidence
                        }
                        None => {
                            evidence_incomplete = true;
                            HashMap::new()
                        }
                    }
                } else {
                    HashMap::new()
                };
                for record in chat_store.outbox_records().await {
                    if record.command_type != CommandType::Prompt {
                        continue;
                    }
                    let projection = evidence.get(&record.command_id).cloned();
                    let item = normalize_prompt_status(record, projection.as_ref());
                    match prompts_by_command.entry(item.command_id.clone()) {
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(item);
                        }
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            evidence_incomplete = true;
                            Self::merge_conflicting_prompt_status(entry.get_mut(), item);
                        }
                    }
                }
            }
            let mut prompts = prompts_by_command.into_values().collect::<Vec<_>>();
            prompts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
            const LIMIT: usize = 200;
            let truncated = prompts.len() > LIMIT;
            if truncated {
                let mut unresolved = prompts
                    .iter()
                    .filter(|item| {
                        matches!(
                            item.status,
                            PromptDeliveryStatus::DeliveryUnknown | PromptDeliveryStatus::Projected
                        )
                    })
                    .take(LIMIT)
                    .cloned()
                    .collect::<Vec<_>>();
                let unresolved_ids = unresolved
                    .iter()
                    .map(|item| item.command_id.clone())
                    .collect::<HashSet<_>>();
                let remaining = LIMIT.saturating_sub(unresolved.len());
                unresolved.extend(
                    prompts
                        .into_iter()
                        .filter(|item| !unresolved_ids.contains(&item.command_id))
                        .take(remaining),
                );
                prompts = unresolved;
            }
            let _ = tx
                .send(OutboundMsg::Frame(Frame::PromptStatus(PromptStatusFrame {
                    command_id: response_command_id,
                    session_id,
                    runtime_restored: false,
                    truncated,
                    evidence_incomplete,
                    prompts,
                })))
                .await;
        });
        SubmitAck::Accepted {
            command_id: command_id.to_string(),
        }
    }

    /// Global command ids are expected to be unique. If historical stores violate
    /// that invariant, emit one conservative fact rather than duplicate or choose
    /// whichever store happened to be iterated first.
    fn merge_conflicting_prompt_status(
        existing: &mut PromptStatusItem,
        conflicting: PromptStatusItem,
    ) {
        existing.status = PromptDeliveryStatus::DeliveryUnknown;
        existing.error_code = None;
        if existing.turn_id != conflicting.turn_id {
            existing.turn_id = None;
        }
        if conflicting.created_at < existing.created_at {
            existing.created_at = conflicting.created_at;
        }
        if conflicting.updated_at > existing.updated_at {
            existing.updated_at = conflicting.updated_at;
        }
    }

    /// Project-scoped ACP session discovery. Unlike legacy `session/list`, the
    /// caller does not need an already-active logical session. A live runtime
    /// for the same project is reused when available; otherwise a private
    /// initialize/list/kill process is created without registering a normal
    /// runtime chat or logical session in Registry/SQLite. It receives only a
    /// private heartbeat-ownership lease in ChatRegistry.
    async fn exec_project_session_discover(
        &self,
        ctx: &ConnectionCtx,
        action: ActionEnvelope,
        tx: mpsc::Sender<OutboundMsg>,
    ) -> SubmitAck {
        let (command_id, project_id) = match action {
            ActionEnvelope::PersistedSessionDiscover {
                command_id,
                payload,
            } => (command_id, payload.project_id),
            _ => unreachable!("dispatch guarantees session/discover"),
        };
        if Uuid::parse_str(&command_id).is_err() {
            return SubmitAck::Failed(action_error(
                command_id,
                ErrorCode::InvalidState,
                "invalid commandId",
                false,
            ));
        }
        let Some(projects) = self.inner.projects.read().await.clone() else {
            return SubmitAck::Failed(action_error(
                command_id,
                ErrorCode::AgentUnavailable,
                "metadata catalog unavailable",
                true,
            ));
        };
        let Some(project) = projects
            .metadata()
            .project(&project_id)
            .await
            .ok()
            .flatten()
            .filter(|project| project.archived_at.is_none())
        else {
            return SubmitAck::Failed(action_error(
                command_id,
                ErrorCode::InvalidState,
                "active project not found",
                false,
            ));
        };
        {
            let mut flights = self
                .inner
                .discoveries_in_flight
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !flights.insert(project_id.clone()) {
                return SubmitAck::Failed(action_error(
                    command_id,
                    ErrorCode::InvalidState,
                    "session discovery already in progress for this project",
                    true,
                ));
            }
        }
        let guard = DiscoveryFlightGuard {
            inner: self.inner.clone(),
            project_id: project_id.clone(),
        };
        let cmd = ExecCmd {
            ctx: ctx.clone(),
            chat_id: String::new(),
            action: ActionEnvelope::PersistedSessionDiscover {
                command_id: command_id.clone(),
                payload: acp_hub_proto::action::ProjectArchivePayload {
                    project_id: project_id.clone(),
                },
            },
            tx,
        };
        self.send_metadata_ack(
            &cmd,
            AckStatus::Accepted,
            Some(&project_id),
            None,
            None,
            None,
        )
        .await;
        let me = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            let result = me.discover_project_sessions(&project).await;
            match result {
                Ok(_) => {
                    audit(
                        "session.discover",
                        Some(&command_id),
                        Some(&cmd.ctx.token_id),
                        "ok",
                        Duration::ZERO,
                        None,
                    );
                    me.send_metadata_ack(
                        &cmd,
                        AckStatus::Committed,
                        Some(&project_id),
                        None,
                        None,
                        None,
                    )
                    .await;
                }
                Err(message) => {
                    me.send_error(&cmd, ErrorCode::AgentUnavailable, &message, true)
                        .await;
                }
            }
        });
        SubmitAck::Handled
    }

    async fn discover_project_sessions(
        &self,
        project: &crate::persist::metadata::ProjectRecord,
    ) -> Result<usize, String> {
        let reusable = self
            .inner
            .chats
            .all_chats()
            .await
            .into_iter()
            .find(|(_, chat)| {
                !chat.state.is_terminal()
                    && chat.session_id.is_some()
                    && chat.instance_id == project.instance_id
                    && chat.cwd == project.cwd
            })
            .map(|(chat_id, _)| chat_id);
        if let Some(chat_id) = reusable {
            return self
                .discover_sessions_through_runtime(&project.instance_id, &chat_id, &project.cwd)
                .await;
        }

        let chat_id = Uuid::new_v4().to_string();
        self.inner.chats.register_ephemeral(&chat_id).await;
        let result = self
            .discover_sessions_through_ephemeral(&project.instance_id, &chat_id, &project.cwd)
            .await;
        let kill = InstanceKill {
            command_id: Uuid::new_v4().to_string(),
            chat_id: chat_id.clone(),
            grace: Some(1_000),
        };
        if let Err(error) = self
            .inner
            .instance
            .send_kill(&project.instance_id, kill)
            .await
        {
            warn!(chat_id, error = ?error, "discovery runtime cleanup failed");
        }
        self.inner.chats.unregister_ephemeral(&chat_id).await;
        result
    }

    async fn discover_sessions_through_ephemeral(
        &self,
        instance_id: &str,
        chat_id: &str,
        cwd: &str,
    ) -> Result<usize, String> {
        let spawn = InstanceSpawn {
            command_id: Uuid::new_v4().to_string(),
            chat_id: chat_id.to_string(),
            cmd: self.inner.acp_cmd.clone(),
            cwd: cwd.to_string(),
            env: None,
        };
        match tokio::time::timeout(
            self.inner.spawn_timeout,
            self.inner.instance.send_spawn(instance_id, spawn),
        )
        .await
        {
            Ok(Ok(SpawnOutcome::Acked(ack))) if ack.ok => {}
            Ok(Ok(_)) => return Err("ACP discovery process failed to start".to_string()),
            Ok(Err(error)) => return Err(format!("ACP discovery spawn failed: {error}")),
            Err(_) => return Err("ACP discovery spawn timed out".to_string()),
        }
        let (rpc_id, message) = self.inner.translator.initialize_rpc(cwd);
        let rx = self
            .inner
            .relay
            .register_rpc(&rpc_id, "session_discover_initialize".to_string())
            .await;
        if let Err(error) = self
            .inner
            .instance
            .forward_rpc(instance_id, chat_id, &message)
            .await
        {
            self.inner.relay.cancel_rpc(&rpc_id).await;
            return Err(format!("ACP discovery initialize failed: {error}"));
        }
        match tokio::time::timeout(self.inner.initialize_timeout, rx).await {
            Ok(Ok(response)) if response.get("error").is_none() => {}
            _ => return Err("ACP discovery initialize timed out or was rejected".to_string()),
        }
        self.discover_sessions_through_runtime(instance_id, chat_id, cwd)
            .await
    }

    async fn discover_sessions_through_runtime(
        &self,
        instance_id: &str,
        chat_id: &str,
        cwd: &str,
    ) -> Result<usize, String> {
        let rpc_id = self.inner.translator.alloc_rpc_id();
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "session/list",
            "params": { "cwd": cwd },
        });
        let rx = self
            .inner
            .relay
            .register_rpc(&rpc_id, "session_discover".to_string())
            .await;
        if let Err(error) = self
            .inner
            .instance
            .forward_rpc(instance_id, chat_id, &message)
            .await
        {
            self.inner.relay.cancel_rpc(&rpc_id).await;
            return Err(format!("ACP session discovery failed: {error}"));
        }
        let response = match tokio::time::timeout(SESSION_POLL_TIMEOUT, rx).await {
            Ok(Ok(response)) if response.get("error").is_none() => response,
            _ => return Err("ACP session discovery timed out or was rejected".to_string()),
        };
        let mut entries = parse_session_list_response(&response);
        for entry in &mut entries {
            entry.cwd = cwd.to_string();
            entry.bound_chat_id = None;
        }
        self.refresh_catalog_titles(&entries).await;
        let count = entries.len();
        self.inner
            .chats
            .registry()
            .apply_sessions(entries)
            .await
            .map_err(|error| format!("session discovery projection failed: {error}"))?;
        Ok(count)
    }

    /// chat/load 会话切换执行（§8.5）：在当前对话（其 ACP 进程）内把
    /// 目标历史会话加载为进程的当前会话——**不新建 chat/进程**（会话是
    /// 进程内实体，随进程消亡；进程可先后持有多个会话，load 即切换）。
    ///
    /// 流程：chat record 解析 (instance_id, cwd) → 开回放窗口
    /// （BeginLoadReplay，清空旧内容重放目标会话）→ 复用 L3 匹配
    /// （register_rpc + forward_rpc `session/load`）→ 成功更新 chat 的
    /// 当前会话（switch_session，relay 逐帧 binding 校验需要命中新
    /// sessionId）→ committed；失败回 `action_error`。低频直通（同
    /// session/list），不走 chat 队列/outbox；submit 层面返回 Accepted，
    /// 终态帧异步回投。
    async fn exec_load_chat(
        &self,
        ctx: &ConnectionCtx,
        action: &ActionEnvelope,
        tx: mpsc::Sender<OutboundMsg>,
        command_id: &str,
    ) -> SubmitAck {
        let payload = match action {
            ActionEnvelope::Load { payload, .. } => payload,
            _ => unreachable!("dispatch guarantees chat/load"),
        };
        let Some(rec) = self.inner.chats.entry(&payload.chat_id).await else {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::ChatNotFound,
                "chat not found",
                false,
            ));
        };
        if rec.state.is_terminal() {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::InvalidState,
                "chat terminal; cannot load session",
                false,
            ));
        }
        if rec.session_id.is_none() {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::InvalidState,
                "chat has no active ACP process",
                false,
            ));
        }
        if self
            .inner
            .chats
            .active_turn(&payload.chat_id)
            .await
            .is_some()
        {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::InvalidState,
                "chat has an active turn; cannot switch session",
                false,
            ));
        }
        {
            let mut loads = self
                .inner
                .loads_in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !loads.insert(payload.chat_id.clone()) {
                return SubmitAck::Failed(action_error(
                    command_id.to_string(),
                    ErrorCode::RateLimited,
                    "session load already in progress",
                    true,
                ));
            }
        }
        let load_guard = LoadFlightGuard {
            inner: self.inner.clone(),
            chat_id: payload.chat_id.clone(),
        };
        let instance_id = rec.instance_id.clone();
        let cwd = rec.cwd.clone();
        // 转发目标 = hub chat id（instance 进程表键，§6.2）。
        let chat_id = payload.chat_id.clone();
        let acp_session_id = payload.acp_session_id.clone();
        // 旧会话：预绑定后 load 失败时恢复（agent 侧失败仍在旧会话；
        // §8.5 会话列表「当前」标注依据）。已检查 session_id 非空。
        let prev_session_id = rec.session_id.clone();

        let me = self.clone();
        let cmd_id = command_id.to_string();
        let token_id = ctx.token_id.clone();
        tokio::spawn(async move {
            let _load_guard = load_guard;
            let started = std::time::Instant::now();
            // 1. 预绑定（§8.5）：ACP spec 强制 replay before response——
            //    回放通知先于 load 响应到达，binding 须先建立否则回放帧
            //    被 relay 以 binding_missing 丢弃（create 路径同款预绑定，
            //    exec_create）。目标会话已绑定另一 chat → 终态错误。
            if let Err(e) = me
                .inner
                .chats
                .switch_session(&chat_id, &acp_session_id)
                .await
            {
                audit(
                    "chat.load",
                    Some(&cmd_id),
                    Some(&token_id),
                    "pre_bind_failed",
                    started.elapsed(),
                    None,
                );
                let msg = match &e {
                    ChatError::BindingConflict(existing) => {
                        format!("该 ACP 会话已在对话 {existing} 中打开（请从对话列表切换）")
                    }
                    other => format!("pre-bind failed: {other}"),
                };
                let _ = tx
                    .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                        cmd_id,
                        ErrorCode::InvalidState,
                        &msg,
                        false,
                    ))))
                    .await;
                return;
            }
            // 2. 开回放窗口（清空旧内容，重放目标会话；拒绝 → 恢复旧会话）。
            if let SubmitResult::Rejected(_) = me
                .inner
                .doc
                .submit_command(
                    &chat_id,
                    DocCommand::BeginLoadReplay {
                        acp_session_id: acp_session_id.clone(),
                    },
                )
                .await
            {
                me.restore_session_after_load(&chat_id, prev_session_id.as_deref())
                    .await;
                audit(
                    "chat.load",
                    Some(&cmd_id),
                    Some(&token_id),
                    "begin_replay_rejected",
                    started.elapsed(),
                    None,
                );
                let _ = tx
                    .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                        cmd_id,
                        ErrorCode::AgentUnavailable,
                        "begin replay failed",
                        true,
                    ))))
                    .await;
                return;
            }
            // 3. session/load RPC（L3 匹配；cwd 与进程绑定目录一致）。
            let (rpc_id, msg) = me.inner.translator.session_load_rpc(&cwd, &acp_session_id);
            let rx = me.inner.relay.register_rpc(&rpc_id, cmd_id.clone()).await;
            if let Err(e) = me
                .inner
                .instance
                .forward_rpc(&instance_id, &chat_id, &msg)
                .await
            {
                me.inner.relay.cancel_rpc(&rpc_id).await;
                me.restore_session_after_load(&chat_id, prev_session_id.as_deref())
                    .await;
                audit(
                    "chat.load",
                    Some(&cmd_id),
                    Some(&token_id),
                    "forward_failed",
                    started.elapsed(),
                    None,
                );
                let _ = tx
                    .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                        cmd_id,
                        ErrorCode::InstanceOffline,
                        &format!("session load forward failed: {e}"),
                        true,
                    ))))
                    .await;
                return;
            }
            match tokio::time::timeout(SESSION_POLL_TIMEOUT, rx).await {
                Ok(Ok(r)) if r.get("error").is_none() => {
                    // 4. 成功：所有历史通知按 ACP 契约先于响应到达；writer
                    // 队列串行处理 End，终态化回放 turn 并恢复实时投影规则。
                    let _ = me
                        .inner
                        .doc
                        .submit_command(&chat_id, DocCommand::EndLoadReplay)
                        .await;
                    audit(
                        "chat.load",
                        Some(&cmd_id),
                        Some(&token_id),
                        "ok",
                        started.elapsed(),
                        None,
                    );
                    let _ = tx
                        .send(OutboundMsg::Frame(Frame::ActionAck(ActionAck {
                            command_id: cmd_id,
                            status: AckStatus::Committed,
                            turn_id: None,
                            chat_id: Some(chat_id),
                            project_id: None,
                            session_id: None,
                            acp_session_id: None,
                            committed_projection_version: None,
                        })))
                        .await;
                }
                Ok(Ok(_)) => {
                    // L3 错误响应（如会话不存在）：可重试。恢复旧会话
                    // （agent 侧 load 失败仍在旧会话，预绑定不落地）。
                    me.inner.relay.cancel_rpc(&rpc_id).await;
                    me.restore_session_after_load(&chat_id, prev_session_id.as_deref())
                        .await;
                    audit(
                        "chat.load",
                        Some(&cmd_id),
                        Some(&token_id),
                        "rejected",
                        started.elapsed(),
                        None,
                    );
                    let _ = tx
                        .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                            cmd_id,
                            ErrorCode::AgentUnavailable,
                            "session/load rejected",
                            true,
                        ))))
                        .await;
                }
                _ => {
                    me.inner.relay.cancel_rpc(&rpc_id).await;
                    me.restore_session_after_load(&chat_id, prev_session_id.as_deref())
                        .await;
                    audit(
                        "chat.load",
                        Some(&cmd_id),
                        Some(&token_id),
                        "timeout",
                        started.elapsed(),
                        None,
                    );
                    let _ = tx
                        .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                            cmd_id,
                            ErrorCode::AgentUnavailable,
                            "session/load timeout",
                            true,
                        ))))
                        .await;
                }
            }
        });
        SubmitAck::Accepted {
            command_id: command_id.to_string(),
        }
    }

    /// load 失败恢复（§8.5）：预绑定已把 chat 当前会话指向目标会话，但
    /// agent 侧 load 失败时仍在旧会话——把当前会话指回旧会话（会话列表
    /// 「当前」标注依据）。binding 保留（同「旧会话 binding 保留」策略，
    /// 同 chat 映射无害）。仅 load 失败路径调用，成功路径不触发。
    async fn restore_session_after_load(&self, chat_id: &str, prev_session_id: Option<&str>) {
        // 无论失败发生在 forward、RPC error 还是 timeout，都必须退出回放模式；
        // 否则后续实时事件会继续走 replay 归位规则。
        let _ = self
            .inner
            .doc
            .submit_command(chat_id, DocCommand::EndLoadReplay)
            .await;
        let Some(prev) = prev_session_id else { return };
        if let Err(e) = self.inner.chats.switch_session(chat_id, prev).await {
            warn!(chat_id, error = ?e, "load failure: restore session failed");
        }
        // agent 投影恢复旧值（BeginLoadReplay 已把 acp_session_id 换成新值）。
        let _ = self
            .inner
            .doc
            .submit_command(
                chat_id,
                DocCommand::SetAgentSessionId {
                    acp_session_id: prev.to_string(),
                },
            )
            .await;
    }

    /// chat/session-new 执行（§8.5 当前对话内新建 ACP 会话）：不新建
    /// chat/进程——进程已存在，等价 create 序列的 `session/new` 一步
    /// （无 spawn/initialize）。
    ///
    /// 流程：chat record 解析 (instance_id, cwd) → translator 的 SessionNew
    /// 分支构造 `session/new` JSON-RPC（cwd 与进程绑定目录一致）→ L3 匹配
    /// （register_rpc + forward_rpc）→ 响应含新 sessionId → binding 更新
    /// （registry `bind` + chat doc `SetAgentSessionId`，与 create 的
    /// session/new 成功路径一致）→ committed ack（携带 acpSessionId，
    /// 跨任务契约 §3）；RPC 失败/超时 → action_error（可重试，参照 load）。
    /// 低频直通（同 load），不走 chat 队列/outbox；submit 层面返回
    /// Accepted，终态帧异步回投。
    async fn exec_session_new(
        &self,
        ctx: &ConnectionCtx,
        action: &ActionEnvelope,
        tx: mpsc::Sender<OutboundMsg>,
        command_id: &str,
    ) -> SubmitAck {
        let payload = match action {
            ActionEnvelope::SessionNew { payload, .. } => payload,
            _ => unreachable!("dispatch guarantees chat/session-new"),
        };
        let Some(rec) = self.inner.chats.entry(&payload.chat_id).await else {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::ChatNotFound,
                "chat not found",
                false,
            ));
        };
        if rec.state.is_terminal() {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::InvalidState,
                "chat terminal; cannot create session",
                false,
            ));
        }
        if rec.session_id.is_none() {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::InvalidState,
                "chat has no active ACP process",
                false,
            ));
        }
        if self
            .inner
            .chats
            .active_turn(&payload.chat_id)
            .await
            .is_some()
        {
            return SubmitAck::Failed(action_error(
                command_id.to_string(),
                ErrorCode::InvalidState,
                "chat has an active turn; cannot create session",
                false,
            ));
        }
        // 并发保护：与 session/load 共享 in_flight 集合——同一 chat 同时
        // 只允许一个会话变更操作（load/session-new 交错会让绑定与新会话
        // 通知串流）。
        {
            let mut ops = self
                .inner
                .loads_in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !ops.insert(payload.chat_id.clone()) {
                return SubmitAck::Failed(action_error(
                    command_id.to_string(),
                    ErrorCode::RateLimited,
                    "session operation already in progress",
                    true,
                ));
            }
        }
        let op_guard = LoadFlightGuard {
            inner: self.inner.clone(),
            chat_id: payload.chat_id.clone(),
        };
        let instance_id = rec.instance_id.clone();
        let cwd = rec.cwd.clone();
        // 转发目标 = hub chat id（instance 进程表键，§6.2）。
        let chat_id = payload.chat_id.clone();

        let me = self.clone();
        let cmd_id = command_id.to_string();
        let token_id = ctx.token_id.clone();
        let action = action.clone();
        tokio::spawn(async move {
            let _op_guard = op_guard;
            let started = std::time::Instant::now();
            // 1. 翻译（rpcId 由 translate 分配，帧 id 即 register_rpc 键，
            //    §6.1）+ L3 匹配。acp_session_id 仅作 OutboundCtx 占位
            //    （SessionNew 方法面不使用，§4.3 表）。
            let msg = match me.inner.translator.translate(
                &action,
                &OutboundCtx {
                    cwd: cwd.clone(),
                    acp_session_id: String::new(),
                },
            ) {
                Ok(OutboundMessage::JsonRpc(v)) => v,
                _ => {
                    audit(
                        "chat.session_new",
                        Some(&cmd_id),
                        Some(&token_id),
                        "translate_failed",
                        started.elapsed(),
                        None,
                    );
                    let _ = tx
                        .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                            cmd_id,
                            ErrorCode::InvalidState,
                            "translate failed",
                            false,
                        ))))
                        .await;
                    return;
                }
            };
            let rpc_id = msg["id"].as_str().unwrap_or_default().to_string();
            let rx = me.inner.relay.register_rpc(&rpc_id, cmd_id.clone()).await;
            if let Err(e) = me
                .inner
                .instance
                .forward_rpc(&instance_id, &chat_id, &msg)
                .await
            {
                me.inner.relay.cancel_rpc(&rpc_id).await;
                audit(
                    "chat.session_new",
                    Some(&cmd_id),
                    Some(&token_id),
                    "forward_failed",
                    started.elapsed(),
                    None,
                );
                let _ = tx
                    .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                        cmd_id,
                        ErrorCode::InstanceOffline,
                        &format!("session new forward failed: {e}"),
                        true,
                    ))))
                    .await;
                return;
            }
            match tokio::time::timeout(me.inner.binding_timeout, rx).await {
                Ok(Ok(r)) if r.get("error").is_none() => {
                    // 2. 成功：解析新 sessionId；缺 sessionId → 视为失败。
                    let Some(new_sid) = extract_session_id(&r) else {
                        me.inner.relay.cancel_rpc(&rpc_id).await;
                        audit(
                            "chat.session_new",
                            Some(&cmd_id),
                            Some(&token_id),
                            "missing_session_id",
                            started.elapsed(),
                            None,
                        );
                        let _ = tx
                            .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                                cmd_id,
                                ErrorCode::AgentUnavailable,
                                "session/new response missing sessionId",
                                true,
                            ))))
                            .await;
                        return;
                    };
                    // 3. binding 更新（§5.4）：registry（bindings + chat
                    //    record 当前会话）+ chat doc agent.acp_session_id。
                    if let Err(e) = me.inner.chats.bind(&chat_id, &new_sid).await {
                        let msg = match &e {
                            ChatError::BindingConflict(existing) => {
                                format!("该 ACP 会话已在对话 {existing} 中打开（请从对话列表切换）")
                            }
                            other => format!("bind failed: {other}"),
                        };
                        audit(
                            "chat.session_new",
                            Some(&cmd_id),
                            Some(&token_id),
                            "bind_failed",
                            started.elapsed(),
                            None,
                        );
                        let _ = tx
                            .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                                cmd_id,
                                ErrorCode::InvalidState,
                                &msg,
                                false,
                            ))))
                            .await;
                        return;
                    }
                    let _ = me
                        .inner
                        .doc
                        .submit_command(
                            &chat_id,
                            DocCommand::SetAgentSessionId {
                                acp_session_id: new_sid.clone(),
                            },
                        )
                        .await;
                    // 模型/effort 投影（§5.4）：handle_new 不发
                    // config_option_update 通知，响应体 configOptions 即
                    // model/effort 唯一路径（部分更新，任一缺失跳过）。
                    if let Some(cfg) = r
                        .get("result")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|o| o.get("configOptions"))
                        .and_then(serde_json::Value::as_array)
                    {
                        let (model, effort) = extract_agent_config(cfg);
                        if model.is_some() || effort.is_some() {
                            let _ = me
                                .inner
                                .doc
                                .submit_command(
                                    &chat_id,
                                    DocCommand::SetAgentConfig { model, effort },
                                )
                                .await;
                        }
                    }
                    audit(
                        "chat.session_new",
                        Some(&cmd_id),
                        Some(&token_id),
                        "ok",
                        started.elapsed(),
                        None,
                    );
                    // 4. committed ack（携带新 acpSessionId，前端据此刷新
                    //    会话列表「当前」标记，跨任务契约 §3）。
                    let _ = tx
                        .send(OutboundMsg::Frame(Frame::ActionAck(ActionAck {
                            command_id: cmd_id,
                            status: AckStatus::Committed,
                            turn_id: None,
                            chat_id: Some(chat_id),
                            project_id: None,
                            session_id: None,
                            acp_session_id: Some(new_sid),
                            committed_projection_version: None,
                        })))
                        .await;
                }
                Ok(Ok(_)) => {
                    // L3 错误响应（如会话数超限）：可重试。binding 未变更
                    // （新会话尚未建立，无残留）。
                    me.inner.relay.cancel_rpc(&rpc_id).await;
                    audit(
                        "chat.session_new",
                        Some(&cmd_id),
                        Some(&token_id),
                        "rejected",
                        started.elapsed(),
                        None,
                    );
                    let _ = tx
                        .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                            cmd_id,
                            ErrorCode::AgentUnavailable,
                            "session/new rejected",
                            true,
                        ))))
                        .await;
                }
                _ => {
                    me.inner.relay.cancel_rpc(&rpc_id).await;
                    audit(
                        "chat.session_new",
                        Some(&cmd_id),
                        Some(&token_id),
                        "timeout",
                        started.elapsed(),
                        None,
                    );
                    let _ = tx
                        .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                            cmd_id,
                            ErrorCode::AgentUnavailable,
                            "session/new timeout",
                            true,
                        ))))
                        .await;
                }
            }
        });
        SubmitAck::Accepted {
            command_id: command_id.to_string(),
        }
    }

    /// prompt 执行（§4.4 提交点纪律 + §6.5 服务端单写）。
    async fn exec_prompt(&self, chat_id: &str, cmd: &ExecCmd) {
        let command_id_str = extract_command_id(&cmd.action).unwrap_or_default();
        let payload = match &cmd.action {
            ActionEnvelope::Prompt { payload, .. } => payload,
            _ => unreachable!("dispatch guarantees prompt"),
        };
        let Some(store) = self
            .inner
            .store
            .chat(chat_uuid(chat_id).unwrap_or_default())
        else {
            return;
        };
        let command_id = match uuid::Uuid::parse_str(&command_id_str) {
            Ok(id) => id,
            Err(_) => {
                self.send_error(cmd, ErrorCode::InvalidState, "invalid commandId", false)
                    .await;
                return;
            }
        };
        // turn_id：submit 临界区内生成并持久化（§4.4：同 commandId 重试复用
        // 同一 turnId）。
        let turn_id = store
            .outbox_get(command_id)
            .await
            .and_then(|r| r.turn_id)
            .unwrap_or_else(uuid::Uuid::new_v4);
        let entry_id = format!("{turn_id}:user");
        let fingerprint = match prompt_payload_fingerprint(payload) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                warn!(chat_id, error = ?error, "prompt fingerprint failed");
                self.fail_terminal(
                    chat_id,
                    command_id,
                    cmd,
                    ErrorCode::InvalidState,
                    "prompt fingerprint failed",
                )
                .await;
                return;
            }
        };
        // 1. Persist the authoritative Pending body before any external
        // side-effect barrier. The chat projection and outbox share command,
        // turn and fingerprint evidence but intentionally remain separate
        // crash-reconcilable stores.
        let created_at = Utc::now().to_rfc3339();
        match self
            .inner
            .doc
            .submit_command(
                chat_id,
                DocCommand::RegisterPendingPromptEntry {
                    turn_id: turn_id.to_string(),
                    entry_id: entry_id.clone(),
                    text: payload.message.clone(),
                    author_user_id: None,
                    source_command_id: command_id_str.clone(),
                    payload_fingerprint: fingerprint.clone(),
                    created_at,
                },
            )
            .await
        {
            SubmitResult::Applied(result)
                if result.reason == Some(ApplyReason::SourceCommandConflict) =>
            {
                self.fail_terminal(
                    chat_id,
                    command_id,
                    cmd,
                    ErrorCode::InvalidState,
                    "prompt identity conflicts with durable projection",
                )
                .await;
                return;
            }
            SubmitResult::PersistFailed => {
                self.fail_retryable(
                    chat_id,
                    command_id,
                    cmd,
                    ErrorCode::AgentUnavailable,
                    "pending prompt projection persist failed",
                )
                .await;
                return;
            }
            SubmitResult::Rejected(SubmitError::ChatNotFound) => {
                self.fail_terminal(
                    chat_id,
                    command_id,
                    cmd,
                    ErrorCode::ChatNotFound,
                    "chat not found",
                )
                .await;
                return;
            }
            _ => {}
        }
        if let Err(e) = store
            .outbox()
            .lock()
            .await
            .mark_prompt_intent_durable(command_id, fingerprint)
        {
            warn!(chat_id, error = ?e, "mark_prompt_intent_durable failed");
            self.fail_terminal(
                chat_id,
                command_id,
                cmd,
                ErrorCode::AgentUnavailable,
                "prompt intent could not be persisted",
            )
            .await;
            return;
        }
        // 2. binding 校验 + 翻译（rpcId 登记，§4.4）。
        let Some(entry) = self.inner.chats.entry(chat_id).await else {
            self.fail_terminal(
                chat_id,
                command_id,
                cmd,
                ErrorCode::ChatNotFound,
                "chat not found",
            )
            .await;
            return;
        };
        let Some(acp_session_id) = entry.session_id.clone() else {
            self.fail_terminal(
                chat_id,
                command_id,
                cmd,
                ErrorCode::InvalidState,
                "chat binding not established",
            )
            .await;
            return;
        };
        let instance_id = entry.instance_id.clone();
        let msg = match self.inner.translator.translate(
            &cmd.action,
            &OutboundCtx {
                cwd: entry.cwd.clone(),
                acp_session_id: acp_session_id.clone(),
            },
        ) {
            Ok(OutboundMessage::JsonRpc(v)) => v,
            Ok(_) => {
                self.fail_terminal(
                    chat_id,
                    command_id,
                    cmd,
                    ErrorCode::InvalidState,
                    "unexpected outbound shape",
                )
                .await;
                return;
            }
            Err(e) => {
                self.fail_terminal(
                    chat_id,
                    command_id,
                    cmd,
                    ErrorCode::InvalidState,
                    &format!("translate failed: {e}"),
                )
                .await;
                return;
            }
        };
        let rpc_id = msg["id"].as_str().unwrap_or_default().to_string();
        let mut rx = self
            .inner
            .relay
            .register_rpc(&rpc_id, command_id_str.clone())
            .await;
        // 3. Persist the no-redelivery barrier before the frame can enter the
        // instance writer. Any ambiguity after this point is DeliveryUnknown.
        if let Err(error) = store
            .outbox()
            .lock()
            .await
            .mark_dispatch_barrier(command_id, Utc::now())
        {
            warn!(chat_id, error = ?error, "mark_dispatch_barrier failed");
            self.inner.relay.cancel_rpc(&rpc_id).await;
            self.fail_terminal(
                chat_id,
                command_id,
                cmd,
                ErrorCode::AgentUnavailable,
                "prompt dispatch barrier could not be persisted",
            )
            .await;
            return;
        }
        if let Err(e) = self
            .inner
            .instance
            .forward_rpc(&instance_id, chat_id, &msg)
            .await
        {
            warn!(chat_id, error = ?e, "prompt forward outcome unknown after barrier");
            self.inner.relay.cancel_rpc(&rpc_id).await;
            let _ = store
                .outbox()
                .lock()
                .await
                .mark_delivery_unknown(command_id);
            let _ = self
                .inner
                .doc
                .submit_command(
                    chat_id,
                    DocCommand::SetPromptEntryDelivery {
                        entry_id: entry_id.clone(),
                        delivery_state: "delivery_unknown".to_string(),
                        delivery_error_code: Some("DELIVERY_UNKNOWN".to_string()),
                        completed_at: None,
                    },
                )
                .await;
            self.send_error(
                cmd,
                ErrorCode::DeliveryUnknown,
                "prompt may have executed; retry is blocked",
                false,
            )
            .await;
            return;
        }
        if let Err(e) = store
            .outbox()
            .lock()
            .await
            .mark_dispatched(command_id, Utc::now())
        {
            warn!(chat_id, error = ?e, "mark_dispatched failed");
            self.inner.relay.cancel_rpc(&rpc_id).await;
            let _ = store
                .outbox()
                .lock()
                .await
                .mark_delivery_unknown(command_id);
            let _ = self
                .inner
                .doc
                .submit_command(
                    chat_id,
                    DocCommand::SetPromptEntryDelivery {
                        entry_id: entry_id.clone(),
                        delivery_state: "delivery_unknown".to_string(),
                        delivery_error_code: Some("DELIVERY_UNKNOWN".to_string()),
                        completed_at: None,
                    },
                )
                .await;
            self.send_error(
                cmd,
                ErrorCode::DeliveryUnknown,
                "prompt was accepted by the instance but its delivery state could not be persisted",
                false,
            )
            .await;
            return;
        }
        self.inner
            .chats
            .set_active_turn(chat_id, &turn_id.to_string())
            .await;
        // The prompt has crossed the ACP dispatch boundary and the server has
        // attempted its authoritative user-entry projection. Seed only the
        // Hub-owned navigation fallback; failure must not change prompt
        // delivery semantics or masquerade as an ACP rename.
        if let Some(projects) = self.inner.projects.read().await.clone() {
            if let Err(error) = projects
                .seed_prompt_title(&acp_session_id, &payload.message)
                .await
            {
                warn!(chat_id, error = ?error, "prompt title projection failed");
            }
        }
        // 5. L3 等待（§4.4 路径 B 变体；issue #3）：prompt 响应只在 turn
        //    结束回——超时语义 = 「无增量窗口」：窗口（l3_timeout）内该
        //    chat 无任何事件投递才判 delivery_unknown；有事件投递（relay
        //    submit/补推成功 → touch_active_turn 续命）则继续等（长流式
        //    turn 不得被 30s 硬超时误杀）。边界：LLM 静默思考（无任何
        //    session/update 事件）> 窗口仍超时——与 issue 措辞一致。
        let result = loop {
            match tokio::time::timeout(self.inner.l3_timeout, &mut rx).await {
                Ok(Ok(response)) => break Ok(response), // 终态路径（下方主体）
                Ok(Err(_)) => break Err(()),            // 通道断（rx 全 drop）
                Err(_elapsed) => {
                    // 窗口到期：登记表已清理（断链/进程退出/他处终结）或该
                    // chat 无增量投递 → delivery_unknown。**并入 Err(()) 主体，
                    // 不得 return**（评审 P1-1：无终态 return 使命令卡
                    // Dispatched，outbox 重放不回退 Dispatched → 客户端无
                    // ack 不可重试；agent 崩溃是常见路径）。
                    let active = self.inner.chats.active_turn(chat_id).await;
                    if active != Some(turn_id.to_string()) {
                        break Err(());
                    }
                    let idle = self.inner.chats.active_turn_idle(chat_id).await;
                    if idle.is_none_or(|d| d > self.inner.l3_timeout) {
                        break Err(()); // 无增量窗口耗尽 → delivery_unknown
                    }
                    // 有续命（窗口内有事件投递）：继续等（rx 未动，无状态泄漏）。
                }
            }
        };
        match result {
            Ok(response) => {
                let is_error = response.get("error").is_some();
                if is_error {
                    let _ = store
                        .outbox()
                        .lock()
                        .await
                        .mark_delivery_unknown(command_id);
                    self.set_prompt_delivery_for_command(
                        chat_id,
                        command_id,
                        "delivery_unknown",
                        Some("DELIVERY_UNKNOWN"),
                    )
                    .await;
                    self.send_error(
                        cmd,
                        ErrorCode::DeliveryUnknown,
                        "ACP returned an error after accepting the prompt; replay is blocked",
                        false,
                    )
                    .await;
                    // 活动 turn 清理：L3 error 也是 turn 的终结（§7.2 终态
                    // 可逆性——不得让表项滞留阻塞后续 load「有活动 turn」校验）。
                    self.inner.chats.clear_active_turn(chat_id).await;
                    return;
                }
                if let Err(e) = store
                    .outbox()
                    .lock()
                    .await
                    .mark_delivery_confirmed(command_id)
                {
                    warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
                    let _ = store
                        .outbox()
                        .lock()
                        .await
                        .mark_delivery_unknown(command_id);
                    self.set_prompt_delivery_for_command(
                        chat_id,
                        command_id,
                        "delivery_unknown",
                        Some("DELIVERY_UNKNOWN"),
                    )
                    .await;
                    self.inner.chats.clear_active_turn(chat_id).await;
                    self.send_error(
                        cmd,
                        ErrorCode::DeliveryUnknown,
                        "prompt completed but its delivery confirmation could not be persisted",
                        false,
                    )
                    .await;
                    return;
                }
                // 终态注入（§7.2 宿主驱动 turn 模型；照抄 @fenix/chat-channel
                // acp-channel.ts：prompt 响应 `result.stopReason` → turn 终态，
                // `cancelled` → Cancelled，其余 → Completed）。真实 peri 不发
                // turn_complete 通知，唯一终态信号就是 prompt 的 L3 响应。
                let stop_reason = response
                    .get("result")
                    .and_then(|r| r.get("stopReason"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                // stopReason → turn 终态（对齐参考实现 turn_failed 语义：
                // failed/error 不得视为正常 Completed）。
                let terminal_status = match stop_reason {
                    "cancelled" => TurnStatus::Cancelled,
                    "failed" | "error" => TurnStatus::Failed,
                    _ => TurnStatus::Completed,
                };
                if !self
                    .persist_turn_terminal(
                        chat_id,
                        &turn_id.to_string(),
                        terminal_status,
                        command_id,
                        cmd,
                    )
                    .await
                {
                    self.inner.chats.clear_active_turn(chat_id).await;
                    return;
                }
                let completed_at = Utc::now().to_rfc3339();
                if matches!(
                    self.inner
                        .doc
                        .submit_command(
                            chat_id,
                            DocCommand::SetPromptEntryDelivery {
                                entry_id: entry_id.clone(),
                                delivery_state: "completed".to_string(),
                                delivery_error_code: None,
                                completed_at: Some(completed_at),
                            },
                        )
                        .await,
                    SubmitResult::PersistFailed | SubmitResult::Rejected(_)
                ) {
                    let _ = store
                        .outbox()
                        .lock()
                        .await
                        .mark_delivery_unknown(command_id);
                    self.inner.chats.clear_active_turn(chat_id).await;
                    self.send_error(
                        cmd,
                        ErrorCode::DeliveryUnknown,
                        "prompt completed but durable projection is uncertain",
                        false,
                    )
                    .await;
                    return;
                }
                if let Err(e) = store
                    .outbox()
                    .lock()
                    .await
                    .mark_projection_committed(command_id)
                {
                    warn!(chat_id, error = ?e, "mark_projection_committed failed");
                    let _ = store
                        .outbox()
                        .lock()
                        .await
                        .mark_delivery_unknown(command_id);
                    self.inner.chats.clear_active_turn(chat_id).await;
                    self.send_error(
                        cmd,
                        ErrorCode::DeliveryUnknown,
                        "prompt projection exists but its commit barrier is uncertain",
                        false,
                    )
                    .await;
                    return;
                }
                if let Err(e) = store.outbox().lock().await.mark_completed(command_id) {
                    warn!(chat_id, error = ?e, "mark_completed failed");
                    let _ = store
                        .outbox()
                        .lock()
                        .await
                        .mark_delivery_unknown(command_id);
                    self.inner.chats.clear_active_turn(chat_id).await;
                    self.send_error(
                        cmd,
                        ErrorCode::DeliveryUnknown,
                        "prompt projection is durable but command completion is uncertain",
                        false,
                    )
                    .await;
                    return;
                }
                self.inner.chats.clear_active_turn(chat_id).await;
                self.send_committed(cmd, Some(&turn_id.to_string()), None)
                    .await;
                audit(
                    "command.committed",
                    Some(&command_id_str),
                    Some(&cmd.ctx.token_id),
                    "ok",
                    std::time::Duration::ZERO,
                    None,
                );
            }
            Err(()) => {
                // 无增量窗口耗尽（30s 无 L3 且无事件投递）→ delivery_unknown
                // （路径 B：非幂等禁止自动重发，§4.4）。
                self.inner.relay.cancel_rpc(&rpc_id).await;
                if let Err(e) = store
                    .outbox()
                    .lock()
                    .await
                    .mark_delivery_unknown(command_id)
                {
                    warn!(chat_id, error = ?e, "mark_delivery_unknown failed");
                }
                // 活动 turn 清理：delivery_unknown 时 turn 无法终结（§7.2），
                // 但不得阻塞后续 load——表项清除，投影保留（前端可见 pending）。
                self.inner.chats.clear_active_turn(chat_id).await;
                let _ = self
                    .inner
                    .doc
                    .submit_command(
                        chat_id,
                        DocCommand::SetPromptEntryDelivery {
                            entry_id,
                            delivery_state: "delivery_unknown".to_string(),
                            delivery_error_code: Some("DELIVERY_UNKNOWN".to_string()),
                            completed_at: None,
                        },
                    )
                    .await;
                self.send_error(
                    cmd,
                    ErrorCode::DeliveryUnknown,
                    "delivery unknown; automatic retry not permitted (path B)",
                    false,
                )
                .await;
            }
        }
    }

    /// create 执行（§6.2 时序：spawn → initialize → session/new → binding）。
    async fn exec_create(&self, _chat_id: &str, cmd: &ExecCmd) {
        let command_id_str = extract_command_id(&cmd.action).unwrap_or_default();
        let command_id = match uuid::Uuid::parse_str(&command_id_str) {
            Ok(id) => id,
            Err(_) => {
                self.send_error(cmd, ErrorCode::InvalidState, "invalid commandId", false)
                    .await;
                return;
            }
        };
        // create 的真实 chat_id 由 submit 生成并写入 `cmd.chat_id`
        // （§6.2 server 生成 id 的唯一告知路径）；executor 的 `chat_id`
        // 参数是全局 create 执行器的 `__create__` 标记，遮蔽为真实 UUID。
        let chat_id = cmd.chat_id.clone();
        let payload = match &cmd.action {
            ActionEnvelope::Create { payload, .. } => payload,
            _ => unreachable!("dispatch guarantees create"),
        };
        let instance_id = payload
            .instance_id
            .clone()
            .unwrap_or_else(|| DEFAULT_INSTANCE_ID.to_string());
        // cwd：prepare_create 已按 workspace 继承解析并写入 ChatRecord——
        // 从这里取（spawn/initialize/session_new/load 全链路一致）。
        let Some(entry) = self.inner.chats.entry(&chat_id).await else {
            warn!(chat_id, "create: chat entry missing (register 前置失败?)");
            self.send_error(cmd, ErrorCode::ChatNotFound, "chat not found", false)
                .await;
            self.cleanup_create(&chat_id, &instance_id).await;
            return;
        };
        let cwd = entry.cwd.clone();
        let Some(store) = self
            .inner
            .store
            .chat(chat_uuid(&chat_id).unwrap_or_default())
        else {
            return;
        };
        if let Err(e) = store.outbox().lock().await.mark_intent_durable(command_id) {
            warn!(chat_id, error = ?e, "mark_intent_durable failed");
            return;
        }
        // 1. spawn（10s，§6.2）。
        let spawn_cmd = InstanceSpawn {
            command_id: command_id_str.clone(),
            chat_id: chat_id.to_string(),
            cmd: self.inner.acp_cmd.clone(),
            cwd: cwd.clone(),
            env: None,
        };
        let spawn = match tokio::time::timeout(
            self.inner.spawn_timeout,
            self.inner.instance.send_spawn(&instance_id, spawn_cmd),
        )
        .await
        {
            Ok(Ok(SpawnOutcome::Acked(a))) => Some(a),
            Ok(Err(e)) => {
                self.fail_retryable(
                    &chat_id,
                    command_id,
                    cmd,
                    instance_error_code(&e),
                    "spawn failed",
                )
                .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
            Err(_) => {
                self.fail_retryable(
                    &chat_id,
                    command_id,
                    cmd,
                    ErrorCode::AgentUnavailable,
                    "spawn timeout (10s)",
                )
                .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
        };
        let spawn = spawn.expect("spawn outcome");
        if !spawn.ok {
            self.fail_retryable(
                &chat_id,
                command_id,
                cmd,
                ErrorCode::AgentUnavailable,
                "agent spawn failed",
            )
            .await;
            self.cleanup_create(&chat_id, &instance_id).await;
            return;
        }
        // L1+L2（§4.4：create 的 delivery_confirmed 只要求 spawn_ack）。
        if let Err(e) = store
            .outbox()
            .lock()
            .await
            .mark_dispatched(command_id, Utc::now())
        {
            warn!(chat_id, error = ?e, "mark_dispatched failed");
            return;
        }
        if let Err(e) = store
            .outbox()
            .lock()
            .await
            .mark_delivery_confirmed(command_id)
        {
            warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
            return;
        }
        // 2. initialize（10s）。
        let (init_rpc_id, init_msg) = self.inner.translator.initialize_rpc(&cwd);
        let init_rx = self
            .inner
            .relay
            .register_rpc(&init_rpc_id, command_id_str.clone())
            .await;
        if let Err(e) = self
            .inner
            .instance
            .forward_rpc(&instance_id, &chat_id, &init_msg)
            .await
        {
            self.fail_retryable(
                &chat_id,
                command_id,
                cmd,
                instance_error_code(&e),
                "initialize forward failed",
            )
            .await;
            self.cleanup_create(&chat_id, &instance_id).await;
            return;
        }
        match tokio::time::timeout(self.inner.initialize_timeout, init_rx).await {
            Ok(Ok(r)) if r.get("error").is_none() => {}
            Ok(Ok(_)) => {
                self.fail_retryable(
                    &chat_id,
                    command_id,
                    cmd,
                    ErrorCode::AgentUnavailable,
                    "initialize rejected",
                )
                .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
            Ok(Err(_)) | Err(_) => {
                self.fail_retryable(
                    &chat_id,
                    command_id,
                    cmd,
                    ErrorCode::AgentUnavailable,
                    "initialize timeout (10s)",
                )
                .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
        }
        // 3. binding（§6.2 session/new；§8.5 session/load 历史恢复）。
        //    load 路径（create 携带 acp_session_id）：binding 以请求参数为准
        //    （load 响应体不含 sessionId）——**预绑定**：回放通知先于 load
        //    响应到达（ACP spec 强制 replay before response），绑定须先建立
        //    否则回放帧被 relay 以 binding_missing 丢弃；BeginLoadReplay
        //    同理须先入 writer 队列（回放帧要等 ACP 处理 stdin 才流出，
        //    命令先入队安全）。
        let load_session = payload.acp_session_id.clone();
        let (binding_rpc_id, binding_msg) = if let Some(sid) = &load_session {
            if let Err(e) = self.inner.chats.bind(&chat_id, sid).await {
                warn!(chat_id, error = ?e, "pre-bind failed");
                // BindingConflict：该 ACP 会话已绑定到另一 chat（§6.2 binding
                // 全局 one-to-one；常见于点击当前活跃会话/已打开的会话）——
                // 重试无意义，终态错误 + 可读提示（携带既有 chat_id 供前端
                // 从对话列表切换）。
                let msg = match &e {
                    ChatError::BindingConflict(existing) => {
                        format!("该 ACP 会话已在对话 {existing} 中打开（请从对话列表切换）")
                    }
                    other => format!("pre-bind failed: {other}"),
                };
                self.fail_terminal(&chat_id, command_id, cmd, ErrorCode::InvalidState, &msg)
                    .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
            if let SubmitResult::Rejected(_) = self
                .inner
                .doc
                .submit_command(
                    &chat_id,
                    DocCommand::BeginLoadReplay {
                        acp_session_id: sid.clone(),
                    },
                )
                .await
            {
                self.fail_retryable(
                    &chat_id,
                    command_id,
                    cmd,
                    ErrorCode::AgentUnavailable,
                    "begin replay failed",
                )
                .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
            self.inner.translator.session_load_rpc(&cwd, sid)
        } else {
            self.inner
                .translator
                .session_new_rpc(&cwd, payload.title.as_deref())
        };
        let binding_rx = self
            .inner
            .relay
            .register_rpc(&binding_rpc_id, command_id_str.clone())
            .await;
        if let Err(e) = self
            .inner
            .instance
            .forward_rpc(&instance_id, &chat_id, &binding_msg)
            .await
        {
            let what = if load_session.is_some() {
                "session/load"
            } else {
                "session/new"
            };
            self.fail_retryable(
                &chat_id,
                command_id,
                cmd,
                instance_error_code(&e),
                &format!("{what} forward failed"),
            )
            .await;
            self.cleanup_create(&chat_id, &instance_id).await;
            return;
        }
        let acp_session_id = match &load_session {
            // load：等待响应确认（成功/失败），binding 用请求参数。
            Some(sid) => match tokio::time::timeout(self.inner.binding_timeout, binding_rx).await {
                Ok(Ok(r)) if r.get("error").is_none() => Some(sid.clone()),
                Ok(Ok(_)) => {
                    self.fail_retryable(
                        &chat_id,
                        command_id,
                        cmd,
                        ErrorCode::AgentUnavailable,
                        "session/load rejected",
                    )
                    .await;
                    self.cleanup_create(&chat_id, &instance_id).await;
                    return;
                }
                Ok(Err(_)) | Err(_) => None,
            },
            None => match tokio::time::timeout(self.inner.binding_timeout, binding_rx).await {
                Ok(Ok(r)) => {
                    let sid = extract_session_id(&r);
                    // handle_new 不发 config_option_update 通知，响应体
                    // configOptions 即 model/effort 唯一路径（§5.4）。
                    if let Some(cfg) = r
                        .get("result")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|o| o.get("configOptions"))
                        .and_then(serde_json::Value::as_array)
                    {
                        let (model, effort) = extract_agent_config(cfg);
                        if model.is_some() || effort.is_some() {
                            let _ = self
                                .inner
                                .doc
                                .submit_command(
                                    &chat_id,
                                    DocCommand::SetAgentConfig { model, effort },
                                )
                                .await;
                        }
                    }
                    sid
                }
                Ok(Err(_)) | Err(_) => None,
            },
        };
        let Some(acp_session_id) = acp_session_id else {
            self.fail_retryable(
                &chat_id,
                command_id,
                cmd,
                ErrorCode::AgentUnavailable,
                "binding timeout (30s)",
            )
            .await;
            self.cleanup_create(&chat_id, &instance_id).await;
            return;
        };
        // 4. binding（§6.2）。load 路径已预绑定（步骤 3）。
        if load_session.is_none() {
            if let Err(e) = self.inner.chats.bind(&chat_id, &acp_session_id).await {
                warn!(chat_id, error = ?e, "bind failed");
                self.fail_retryable(
                    &chat_id,
                    command_id,
                    cmd,
                    ErrorCode::AgentUnavailable,
                    "bind failed",
                )
                .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
            // agent 投影写回（§5.4）：session/new 的绑定建立后
            // agent.acp_session_id 落 doc（load 路径由 BeginLoadReplay 写）。
            let _ = self
                .inner
                .doc
                .submit_command(
                    &chat_id,
                    DocCommand::SetAgentSessionId {
                        acp_session_id: acp_session_id.clone(),
                    },
                )
                .await;
        }
        // 5. 终态（§4.4：projection_committed → completed → committed）。
        if let Err(e) = store
            .outbox()
            .lock()
            .await
            .mark_projection_committed(command_id)
        {
            warn!(chat_id, error = ?e, "mark_projection_committed failed");
            return;
        }
        // §8.5：load 路径退出回放模式（回放通知已全部先于响应进入 writer）。
        if load_session.is_some() {
            let _ = self
                .inner
                .doc
                .submit_command(&chat_id, DocCommand::EndLoadReplay)
                .await;
        }
        if let Err(e) = store.outbox().lock().await.mark_completed(command_id) {
            warn!(chat_id, error = ?e, "mark_completed failed");
            return;
        }
        self.send_committed(cmd, None, Some(&chat_id)).await;
        audit(
            "command.committed",
            Some(&command_id_str),
            Some(&cmd.ctx.token_id),
            "ok",
            std::time::Duration::ZERO,
            None,
        );
    }

    /// cancel 执行（§7.2）：L1+L2（send 成功）后等待 L3 确认。
    async fn exec_forward(&self, chat_id: &str, cmd: &ExecCmd) {
        let command_id_str = extract_command_id(&cmd.action).unwrap_or_default();
        let command_id = match uuid::Uuid::parse_str(&command_id_str) {
            Ok(id) => id,
            Err(_) => {
                self.send_error(cmd, ErrorCode::InvalidState, "invalid commandId", false)
                    .await;
                return;
            }
        };
        let Some(store) = self
            .inner
            .store
            .chat(chat_uuid(chat_id).unwrap_or_default())
        else {
            return;
        };
        if let Err(e) = store.outbox().lock().await.mark_intent_durable(command_id) {
            warn!(chat_id, error = ?e, "mark_intent_durable failed");
            return;
        }
        let Some(entry) = self.inner.chats.entry(chat_id).await else {
            self.send_error(cmd, ErrorCode::ChatNotFound, "chat not found", false)
                .await;
            return;
        };
        let Some(acp_session_id) = entry.session_id.clone() else {
            self.send_error(
                cmd,
                ErrorCode::InvalidState,
                "chat binding not established",
                false,
            )
            .await;
            return;
        };
        let instance_id = entry.instance_id.clone();
        let msg = match self.inner.translator.translate(
            &cmd.action,
            &OutboundCtx {
                cwd: entry.cwd.clone(),
                acp_session_id,
            },
        ) {
            Ok(OutboundMessage::JsonRpc(v)) => v,
            _ => {
                self.send_error(cmd, ErrorCode::InvalidState, "translate failed", false)
                    .await;
                return;
            }
        };
        let rpc_id = msg["id"].as_str().unwrap_or_default().to_string();
        // 无 id 帧 = notification（真实 peri session/cancel，§4.3 表 cancel 无
        // turnId）：无 ack 路由，走 notification 透传（发送成功即 L1 完成）；
        // 有 id 帧（resolve）走标准 forward_rpc + L3 等待。
        let is_notification = rpc_id.is_empty();
        // §7.2 cancel 前置：取消请求转发前将活动 turn 置 cancelling（参考
        // 实现 cancel 语义——取消发出即进入取消中；终态由 agent 的
        // interrupted 事件或通知/超时路径注入）。登记表有活动 turn 才推进
        // （无活动 turn 的 cancel 仅确认命令）。
        if matches!(cmd.action, ActionEnvelope::Cancel { .. }) {
            if let Some(turn_id) = self.inner.chats.active_turn(chat_id).await {
                let _ = self
                    .inner
                    .doc
                    .submit_command(chat_id, DocCommand::MarkTurnCancelling { turn_id })
                    .await;
            }
        }
        let forward = if is_notification {
            self.inner
                .instance
                .forward_notification(&instance_id, chat_id, &msg)
                .await
        } else {
            self.inner
                .instance
                .forward_rpc(&instance_id, chat_id, &msg)
                .await
        };
        if let Err(e) = forward {
            self.fail_retryable(
                chat_id,
                command_id,
                cmd,
                instance_error_code(&e),
                "forward failed",
            )
            .await;
            return;
        }
        if let Err(e) = store
            .outbox()
            .lock()
            .await
            .mark_dispatched(command_id, Utc::now())
        {
            warn!(chat_id, error = ?e, "mark_dispatched failed");
            return;
        }
        if is_notification {
            // notification：无响应帧可等——发送成功即 L3 等价确认（§7.2
            // 注入 Cancelled 终态；active turn 不存在则无终态可注入，仅确认
            // 命令）。
            if let Err(e) = store
                .outbox()
                .lock()
                .await
                .mark_delivery_confirmed(command_id)
            {
                warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
                return;
            }
            if let Some(turn_id) = self.inner.chats.active_turn(chat_id).await {
                if !self
                    .persist_turn_terminal(
                        chat_id,
                        &turn_id,
                        TurnStatus::Cancelled,
                        command_id,
                        cmd,
                    )
                    .await
                {
                    self.inner.chats.clear_active_turn(chat_id).await;
                    return;
                }
                // 活动 turn 清理：Cancelled 已注入（终态），表项须随终态
                // 清除（§7.2）——否则滞留阻塞后续 load。
                self.inner.chats.clear_active_turn(chat_id).await;
            }
            if let Err(e) = store
                .outbox()
                .lock()
                .await
                .mark_projection_committed(command_id)
            {
                warn!(chat_id, error = ?e, "mark_projection_committed failed");
                return;
            }
            if let Err(e) = store.outbox().lock().await.mark_completed(command_id) {
                warn!(chat_id, error = ?e, "mark_completed failed");
                return;
            }
            self.send_committed(cmd, None, None).await;
            return;
        }
        // L3（resolve 的 ACP 确认，§4.4）。
        let rx = self
            .inner
            .relay
            .register_rpc(&rpc_id, command_id_str.clone())
            .await;
        match tokio::time::timeout(self.inner.l3_timeout, rx).await {
            Ok(Ok(r)) if r.get("error").is_none() => {
                if let Err(e) = store
                    .outbox()
                    .lock()
                    .await
                    .mark_delivery_confirmed(command_id)
                {
                    warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
                    return;
                }
                if let Err(e) = store
                    .outbox()
                    .lock()
                    .await
                    .mark_projection_committed(command_id)
                {
                    warn!(chat_id, error = ?e, "mark_projection_committed failed");
                    return;
                }
                if let Err(e) = store.outbox().lock().await.mark_completed(command_id) {
                    warn!(chat_id, error = ?e, "mark_completed failed");
                    return;
                }
                self.send_committed(cmd, None, None).await;
            }
            Ok(Ok(_)) => {
                self.fail_terminal(
                    chat_id,
                    command_id,
                    cmd,
                    ErrorCode::AgentUnavailable,
                    "agent rejected command",
                )
                .await;
            }
            Ok(Err(_)) | Err(_) => {
                self.inner.relay.cancel_rpc(&rpc_id).await;
                let _ = store
                    .outbox()
                    .lock()
                    .await
                    .mark_delivery_unknown(command_id);
                // §7.2 cancel 超时强制终态（参考实现 DEFAULT_CANCEL_TIMEOUT_MS
                // 语义）：cancel 的确认超时后不得停留 cancelling——注入
                // Cancelled 终态并清理登记表（agent 侧可能仍在取消中，但
                // 视图/登记不得永久阻塞）。
                if matches!(cmd.action, ActionEnvelope::Cancel { .. }) {
                    if let Some(turn_id) = self.inner.chats.active_turn(chat_id).await {
                        // The command verdict is already delivery_unknown. The
                        // terminal view below is cleanup evidence only; it must
                        // not attempt a second outbox transition or emit a
                        // second action_error.
                        let _ = self
                            .submit_turn_terminal(chat_id, &turn_id, TurnStatus::Cancelled)
                            .await;
                        self.inner.chats.clear_active_turn(chat_id).await;
                    }
                }
                self.send_error(
                    cmd,
                    ErrorCode::DeliveryUnknown,
                    "delivery unknown; automatic retry not permitted (path B)",
                    false,
                )
                .await;
            }
        }
    }

    /// 注入 turn 终态（§7.2）：控制面 DocCommand（不经聚合器 seq 水位——
    /// 宿主注入无 instance 流 seq，事件路径 seq=0 会被判 SeqOutOfOrder 拒绝）。
    /// 语义同聚合器 TurnTerminal 事件分支：active_turn 匹配且非终态 → 终态
    /// 迁移 + assistant entry 迁移。真实 peri 无独立终态通知——prompt L3
    /// 响应 stopReason / cancel 发送成功即触发。
    async fn persist_turn_terminal(
        &self,
        chat_id: &str,
        turn_id: &str,
        status: TurnStatus,
        command_id: uuid::Uuid,
        cmd: &ExecCmd,
    ) -> bool {
        match self.submit_turn_terminal(chat_id, turn_id, status).await {
            SubmitResult::Applied(result)
                if result.applied || result.reason == Some(ApplyReason::DuplicateIdempotent) =>
            {
                true
            }
            SubmitResult::Applied(_) | SubmitResult::Rejected(_) | SubmitResult::PersistFailed => {
                let Some(store) = self
                    .inner
                    .store
                    .chat(chat_uuid(chat_id).unwrap_or_default())
                else {
                    self.send_error(
                        cmd,
                        ErrorCode::DeliveryUnknown,
                        "prompt executed but its terminal projection could not be recorded",
                        false,
                    )
                    .await;
                    return false;
                };
                if let Err(err) = store
                    .outbox()
                    .lock()
                    .await
                    .mark_delivery_unknown(command_id)
                {
                    warn!(chat_id, error = ?err, "terminal projection failure could not be recorded");
                }
                self.set_prompt_delivery_for_command(
                    chat_id,
                    command_id,
                    "delivery_unknown",
                    Some("DELIVERY_UNKNOWN"),
                )
                .await;
                self.send_error(
                    cmd,
                    ErrorCode::DeliveryUnknown,
                    "prompt executed but its terminal projection was not durably committed",
                    false,
                )
                .await;
                false
            }
        }
    }

    async fn submit_turn_terminal(
        &self,
        chat_id: &str,
        turn_id: &str,
        status: TurnStatus,
    ) -> SubmitResult {
        self.inner
            .doc
            .submit_command(
                chat_id,
                DocCommand::SetTurnTerminal {
                    turn_id: turn_id.to_string(),
                    status,
                    completed_at: Utc::now().to_rfc3339(),
                },
            )
            .await
    }

    /// close 执行（§4.3「关闭并 kill 对应 ACP 进程」；offline 语义 §7.6）。
    async fn exec_close(&self, chat_id: &str, cmd: &ExecCmd) {
        let command_id_str = extract_command_id(&cmd.action).unwrap_or_default();
        let command_id = match uuid::Uuid::parse_str(&command_id_str) {
            Ok(id) => id,
            Err(_) => {
                self.send_error(cmd, ErrorCode::InvalidState, "invalid commandId", false)
                    .await;
                return;
            }
        };
        let Some(store) = self
            .inner
            .store
            .chat(chat_uuid(chat_id).unwrap_or_default())
        else {
            return;
        };
        if let Err(e) = store.outbox().lock().await.mark_intent_durable(command_id) {
            warn!(chat_id, error = ?e, "mark_intent_durable failed");
            return;
        }
        let Some(entry) = self.inner.chats.entry(chat_id).await else {
            self.send_error(cmd, ErrorCode::ChatNotFound, "chat not found", false)
                .await;
            return;
        };
        let instance_id = entry.instance_id.clone();
        let kill = InstanceKill {
            command_id: command_id_str.clone(),
            chat_id: chat_id.to_string(),
            grace: None,
        };
        // kill_ack = L1+L2（§4.4：close 的 delivery_confirmed 只要求 L1+L2）。
        let kill_ok = match self.inner.instance.send_kill(&instance_id, kill).await {
            Ok(outcome) => match outcome {
                crate::control::KillOutcome::Acked(a) => a.ok,
            },
            Err(e) => {
                let code = instance_error_code(&e);
                if code == ErrorCode::InstanceOffline {
                    // §7.6：offline 时 close → INSTANCE_OFFLINE(retryable) +
                    // pending_close 标记（重连自动补发 kill）。
                    let _ = self.inner.chats.request_close_offline(chat_id).await;
                }
                self.fail_retryable(chat_id, command_id, cmd, code, "kill failed")
                    .await;
                return;
            }
        };
        if !kill_ok {
            self.fail_retryable(
                chat_id,
                command_id,
                cmd,
                ErrorCode::AgentUnavailable,
                "kill rejected by instance",
            )
            .await;
            return;
        }
        if let Err(e) = store
            .outbox()
            .lock()
            .await
            .mark_dispatched(command_id, Utc::now())
        {
            warn!(chat_id, error = ?e, "mark_dispatched failed");
            return;
        }
        if let Err(e) = store
            .outbox()
            .lock()
            .await
            .mark_delivery_confirmed(command_id)
        {
            warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
            return;
        }
        // 投影 Closed 终态 + 提交（§7.3）。
        if let SubmitResult::PersistFailed = self
            .inner
            .doc
            .submit_command(
                chat_id,
                DocCommand::SetChatTerminal {
                    status: acp_hub_proto::schema::ChatStatus::Closed,
                },
            )
            .await
        {
            warn!(chat_id, "close projection persist failed");
        }
        if let Err(e) = store
            .outbox()
            .lock()
            .await
            .mark_projection_committed(command_id)
        {
            warn!(chat_id, error = ?e, "mark_projection_committed failed");
            return;
        }
        if let Err(e) = store.outbox().lock().await.mark_completed(command_id) {
            warn!(chat_id, error = ?e, "mark_completed failed");
            return;
        }
        let _ = self
            .inner
            .chats
            .transition(chat_id, ChatState::Closed)
            .await;
        store.mark_closed(Utc::now());
        let _ = self.inner.doc.close_chat(chat_id).await;
        self.send_committed(cmd, None, None).await;
    }

    /// resolve 执行（§7.4 规则 4：CAS 迁移成功后才下发 ACP）。
    async fn exec_resolve(&self, chat_id: &str, cmd: &ExecCmd) {
        let command_id_str = extract_command_id(&cmd.action).unwrap_or_default();
        let command_id = match uuid::Uuid::parse_str(&command_id_str) {
            Ok(id) => id,
            Err(_) => {
                self.send_error(cmd, ErrorCode::InvalidState, "invalid commandId", false)
                    .await;
                return;
            }
        };
        let payload = match &cmd.action {
            ActionEnvelope::ResolvePermission { payload, .. } => payload,
            _ => unreachable!("dispatch guarantees resolve"),
        };
        let Some(store) = self
            .inner
            .store
            .chat(chat_uuid(chat_id).unwrap_or_default())
        else {
            return;
        };
        let persisted_recovery = store
            .outbox_get(command_id)
            .await
            .and_then(|record| record.recovery.map(|recovery| *recovery));
        if persisted_recovery.is_none() {
            if let Err(e) = store.outbox().lock().await.mark_intent_durable(command_id) {
                warn!(chat_id, error = ?e, "mark_intent_durable failed");
                return;
            }
        }
        // 官方 request 的 ACP response 材料必须先于 Control Doc CAS
        // 落入同一 command outbox。否则 server 在 CAS 后崩溃会只留下
        // resolved 投影，却无法重建 response。
        let live_permission = self
            .inner
            .relay
            .claim_pending_permission(&payload.permission_id, &command_id_str, payload.decision)
            .await;
        let recovery = live_permission
            .as_ref()
            .map(|permission| CommandRecovery::PermissionResponse {
                permission_id: payload.permission_id.clone(),
                request_id: permission.request_id.clone(),
                options: permission.options.clone(),
                decision: payload.decision,
            })
            .or(persisted_recovery);
        if let Some(evidence) = recovery.clone() {
            if let Err(e) = store
                .outbox()
                .lock()
                .await
                .set_recovery(command_id, evidence)
            {
                warn!(chat_id, error = ?e, "persist permission recovery failed");
                return;
            }
        }
        // 1. CAS（§7.4 规则 4：pending → resolved 原子一次）。
        let replay_same_decision = match self
            .inner
            .doc
            .submit_command(
                chat_id,
                DocCommand::ResolvePermission {
                    permission_id: payload.permission_id.clone(),
                    decision: payload.decision,
                },
            )
            .await
        {
            SubmitResult::Applied(r)
                if !r.applied && r.reason == Some(ApplyReason::PermissionDecisionReplay) =>
            {
                true
            }
            SubmitResult::Applied(r) if !r.applied => {
                // 已裁决/已过期/未知 → 幂等 duplicate（§7.4 规则 4；§4.4
                // duplicate ack），命令账本清除【决策：CAS 非 Migrated 的命令
                // 未产生副作用，tombstone 释放 commandId】。
                let _ = store.outbox().lock().await.clear_for_retry(command_id);
                let _ = cmd
                    .tx
                    .send(OutboundMsg::Frame(Frame::ActionAck(ActionAck {
                        command_id: command_id_str.clone(),
                        status: AckStatus::Duplicate,
                        turn_id: None,
                        chat_id: None,
                        project_id: None,
                        session_id: None,
                        acp_session_id: None,
                        committed_projection_version: None,
                    })))
                    .await;
                return;
            }
            SubmitResult::Rejected(SubmitError::ChatNotFound) => {
                self.send_error(cmd, ErrorCode::ChatNotFound, "chat not found", false)
                    .await;
                return;
            }
            SubmitResult::PersistFailed => {
                warn!(chat_id, "resolve CAS persist failed (degraded)");
                return;
            }
            _ => false,
        };
        // 2. 双轨（OQ7 裁决 / 评审 P0-1）：官方 `session/request_permission`
        //    （take 命中）→ 官方响应帧（无 L3，JSON-RPC response 无回执，
        //    §4.4 以 forward_ack 为确认点）；原始形态 `permission_request`
        //    （map_raw:475 路径，未命中）→ 旧轨 translate + register_rpc +
        //    L3（原样保留）。两轨共享 CAS 幂等，转发目标同为 entry 归属
        //    instance。
        if replay_same_decision && recovery.is_none() {
            let _ = store.outbox().lock().await.clear_for_retry(command_id);
            let _ = cmd
                .tx
                .send(OutboundMsg::Frame(Frame::ActionAck(ActionAck {
                    command_id: command_id_str.clone(),
                    status: AckStatus::Duplicate,
                    turn_id: None,
                    chat_id: None,
                    project_id: None,
                    session_id: None,
                    acp_session_id: None,
                    committed_projection_version: None,
                })))
                .await;
            return;
        }
        let Some(entry) = self.inner.chats.entry(chat_id).await else {
            return;
        };
        let instance_id = entry.instance_id.clone();
        match recovery {
            Some(CommandRecovery::PermissionResponse {
                permission_id,
                request_id,
                options,
                decision,
            }) => {
                if permission_id != payload.permission_id || decision != payload.decision {
                    self.fail_terminal(
                        chat_id,
                        command_id,
                        cmd,
                        ErrorCode::InvalidState,
                        "permission recovery evidence mismatch",
                    )
                    .await;
                    return;
                }
                // 官方轨：响应帧构造不经 translate（评审 P0-1）；id = agent
                // request id 原样回显（string/number 均合法，instance_registry
                // forward_rpc 已放宽提取）。
                let msg =
                    self.inner
                        .translator
                        .permission_response_rpc(&request_id, decision, &options);
                // mark_dispatched 先行（评审 P2-a：outbox Dispatched→
                // DeliveryConfirmed 强校验，漏写则 mark_delivery_confirmed
                // 被拒、命令卡 IntentDurable）。
                if let Err(e) = store
                    .outbox()
                    .lock()
                    .await
                    .mark_dispatched(command_id, Utc::now())
                {
                    warn!(chat_id, error = ?e, "mark_dispatched failed");
                    return;
                }
                // L1+L2：forward_ack 即确认点（无 L3 等待）。
                if let Err(e) = self
                    .inner
                    .instance
                    .forward_rpc(&instance_id, chat_id, &msg)
                    .await
                {
                    self.fail_recoverable(
                        chat_id,
                        command_id,
                        cmd,
                        instance_error_code(&e),
                        "forward failed",
                    )
                    .await;
                    return;
                }
                if let Err(e) = store
                    .outbox()
                    .lock()
                    .await
                    .mark_delivery_confirmed(command_id)
                {
                    warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
                    return;
                }
                if let Err(e) = store.outbox().lock().await.clear_recovery(command_id) {
                    warn!(chat_id, error = ?e, "clear permission recovery failed");
                    return;
                }
                if let Err(e) = store
                    .outbox()
                    .lock()
                    .await
                    .mark_projection_committed(command_id)
                {
                    warn!(chat_id, error = ?e, "mark_projection_committed failed");
                    return;
                }
                if let Err(e) = store.outbox().lock().await.mark_completed(command_id) {
                    warn!(chat_id, error = ?e, "mark_completed failed");
                    return;
                }
                self.inner
                    .relay
                    .remove_pending_permission(&payload.permission_id)
                    .await;
                self.send_committed(cmd, None, None).await;
            }
            None => {
                // 原始轨（peri 私有 permission_request，map_raw:475 路径）：
                // 现有 translate + register_rpc + L3 逻辑原样保留（评审
                // P0-1：translate 的 ResolvePermission 分支原样保留，此轨
                // 不变）。
                let Some(acp_session_id) = entry.session_id.clone() else {
                    return;
                };
                let msg = match self.inner.translator.translate(
                    &cmd.action,
                    &OutboundCtx {
                        cwd: entry.cwd.clone(),
                        acp_session_id,
                    },
                ) {
                    Ok(OutboundMessage::JsonRpc(v)) => v,
                    _ => {
                        self.send_error(cmd, ErrorCode::InvalidState, "translate failed", false)
                            .await;
                        return;
                    }
                };
                let rpc_id = msg["id"].as_str().unwrap_or_default().to_string();
                let rx = self
                    .inner
                    .relay
                    .register_rpc(&rpc_id, command_id_str.clone())
                    .await;
                if let Err(e) = self
                    .inner
                    .instance
                    .forward_rpc(&instance_id, chat_id, &msg)
                    .await
                {
                    // 遗留私有 permission_request 没有可持久的官方
                    // request 回投材料，CAS 已经裁后无法在重启间
                    // 证明一次安全重放。因此不得对客户谎报 retryable。
                    self.fail_terminal(
                        chat_id,
                        command_id,
                        cmd,
                        instance_error_code(&e),
                        "legacy permission response was not delivered; retry is not safe",
                    )
                    .await;
                    return;
                }
                if let Err(e) = store
                    .outbox()
                    .lock()
                    .await
                    .mark_dispatched(command_id, Utc::now())
                {
                    warn!(chat_id, error = ?e, "mark_dispatched failed");
                    return;
                }
                // 3. L3。
                match tokio::time::timeout(self.inner.l3_timeout, rx).await {
                    Ok(Ok(r)) if r.get("error").is_none() => {
                        if let Err(e) = store
                            .outbox()
                            .lock()
                            .await
                            .mark_delivery_confirmed(command_id)
                        {
                            warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
                            return;
                        }
                        if let Err(e) = store
                            .outbox()
                            .lock()
                            .await
                            .mark_projection_committed(command_id)
                        {
                            warn!(chat_id, error = ?e, "mark_projection_committed failed");
                            return;
                        }
                        if let Err(e) = store.outbox().lock().await.mark_completed(command_id) {
                            warn!(chat_id, error = ?e, "mark_completed failed");
                            return;
                        }
                        self.send_committed(cmd, None, None).await;
                    }
                    _ => {
                        self.inner.relay.cancel_rpc(&rpc_id).await;
                        let _ = store
                            .outbox()
                            .lock()
                            .await
                            .mark_delivery_unknown(command_id);
                        self.send_error(
                            cmd,
                            ErrorCode::DeliveryUnknown,
                            "delivery unknown; automatic retry not permitted (path B)",
                            false,
                        )
                        .await;
                    }
                }
            }
        }
    }

    /// 半创建状态清理（§6.2）：补发 kill + 关闭 doc/会话/持久化目录。
    async fn cleanup_create(&self, chat_id: &str, instance_id: &str) {
        let kill = InstanceKill {
            command_id: uuid::Uuid::new_v4().to_string(),
            chat_id: chat_id.to_string(),
            grace: None,
        };
        if let Err(e) = self.inner.instance.send_kill(instance_id, kill).await {
            debug!(chat_id, error = ?e, "cleanup kill failed (already gone)");
        }
        let _ = self.inner.doc.close_chat(chat_id).await;
        let _ = self
            .inner
            .chats
            .transition(chat_id, ChatState::Closed)
            .await;
        if let Ok(sid) = uuid::Uuid::parse_str(chat_id) {
            let _ = self.inner.store.remove_chat(sid);
        }
    }

    /// retryable 失败（§4.4）：mark_failed + clear_for_retry（允许重发）+
    /// action_error(retryable=true)。
    async fn fail_retryable(
        &self,
        chat_id: &str,
        command_id: uuid::Uuid,
        cmd: &ExecCmd,
        code: ErrorCode,
        message: &str,
    ) {
        let last = LastError::from_error_code(code);
        let Some(store) = self
            .inner
            .store
            .chat(chat_uuid(chat_id).unwrap_or_default())
        else {
            return;
        };
        if let Err(e) = store.outbox().lock().await.mark_failed(command_id, last) {
            warn!(chat_id, error = ?e, "mark_failed failed");
        }
        let status = store
            .outbox_get(command_id)
            .await
            .map(|record| record.status);
        if status == Some(OutboxStatus::DeliveryUnknown) {
            self.set_prompt_delivery_for_command(
                chat_id,
                command_id,
                "delivery_unknown",
                Some("DELIVERY_UNKNOWN"),
            )
            .await;
            self.send_error(
                cmd,
                ErrorCode::DeliveryUnknown,
                "delivery may have occurred; automatic retry is blocked",
                false,
            )
            .await;
            return;
        }
        let _ = store.outbox().lock().await.clear_for_retry(command_id);
        self.set_prompt_delivery_for_command(
            chat_id,
            command_id,
            "failed_not_delivered",
            Some(error_code_name(code)),
        )
        .await;
        let retryable = code.default_retryable();
        self.send_error(cmd, code, message, retryable).await;
    }

    /// 已有持久化恢复证据的 retryable 失败。`mark_failed` 会将已投递
    /// 尝试回退到 `intent_durable`；与普通失败不同，这里不能 tombstone，
    /// 否则会丢失安全重放所需的 commandId 和 ACP response 材料。
    async fn fail_recoverable(
        &self,
        chat_id: &str,
        command_id: uuid::Uuid,
        cmd: &ExecCmd,
        code: ErrorCode,
        message: &str,
    ) {
        let last = LastError::from_error_code(code);
        let Some(store) = self
            .inner
            .store
            .chat(chat_uuid(chat_id).unwrap_or_default())
        else {
            return;
        };
        if let Err(e) = store.outbox().lock().await.mark_failed(command_id, last) {
            warn!(chat_id, error = ?e, "mark recoverable failure failed");
        }
        if store
            .outbox_get(command_id)
            .await
            .is_some_and(|record| record.status == OutboxStatus::DeliveryUnknown)
        {
            self.set_prompt_delivery_for_command(
                chat_id,
                command_id,
                "delivery_unknown",
                Some("DELIVERY_UNKNOWN"),
            )
            .await;
            self.send_error(
                cmd,
                ErrorCode::DeliveryUnknown,
                "delivery may have occurred; automatic retry is blocked",
                false,
            )
            .await;
            return;
        }
        self.send_error(cmd, code, message, code.default_retryable())
            .await;
    }

    /// 终态失败（§4.4 非 retryable）：mark_failed（保留记录）+
    /// action_error(retryable=false)。
    async fn fail_terminal(
        &self,
        chat_id: &str,
        command_id: uuid::Uuid,
        cmd: &ExecCmd,
        code: ErrorCode,
        message: &str,
    ) {
        let last = LastError::from_error_code(code);
        let Some(store) = self
            .inner
            .store
            .chat(chat_uuid(chat_id).unwrap_or_default())
        else {
            return;
        };
        if let Err(e) = store.outbox().lock().await.mark_failed(command_id, last) {
            warn!(chat_id, error = ?e, "mark_failed failed");
        }
        let delivery_unknown = store
            .outbox_get(command_id)
            .await
            .is_some_and(|record| record.status == OutboxStatus::DeliveryUnknown);
        if delivery_unknown {
            self.set_prompt_delivery_for_command(
                chat_id,
                command_id,
                "delivery_unknown",
                Some("DELIVERY_UNKNOWN"),
            )
            .await;
            self.send_error(
                cmd,
                ErrorCode::DeliveryUnknown,
                "delivery may have occurred; automatic retry is blocked",
                false,
            )
            .await;
        } else {
            self.set_prompt_delivery_for_command(
                chat_id,
                command_id,
                "failed_not_delivered",
                Some(error_code_name(code)),
            )
            .await;
            self.send_error(cmd, code, message, false).await;
        }
    }

    async fn set_prompt_delivery_for_command(
        &self,
        chat_id: &str,
        command_id: Uuid,
        delivery_state: &str,
        delivery_error_code: Option<&str>,
    ) {
        let Some(store) = self
            .inner
            .store
            .chat(chat_uuid(chat_id).unwrap_or_default())
        else {
            return;
        };
        let Some(record) = store.outbox_get(command_id).await else {
            return;
        };
        if record.command_type != CommandType::Prompt {
            return;
        }
        let Some(turn_id) = record.turn_id else {
            return;
        };
        let result = self
            .inner
            .doc
            .submit_command(
                chat_id,
                DocCommand::SetPromptEntryDelivery {
                    entry_id: format!("{turn_id}:user"),
                    delivery_state: delivery_state.to_string(),
                    delivery_error_code: delivery_error_code.map(str::to_string),
                    completed_at: None,
                },
            )
            .await;
        if matches!(result, SubmitResult::PersistFailed) {
            warn!(chat_id, %command_id, "prompt delivery projection persist failed");
        }
    }

    async fn publish_terminal_watchers(
        &self,
        command_id: Uuid,
        msg: OutboundMsg,
        original: &mpsc::Sender<OutboundMsg>,
    ) {
        let watchers = self
            .inner
            .terminal_watchers
            .lock()
            .await
            .remove(&command_id)
            .unwrap_or_default();
        for watcher in watchers {
            if !watcher.same_channel(original) && !watcher.is_closed() {
                let _ = watcher.send(msg.clone()).await;
            }
        }
    }

    async fn send_error(&self, cmd: &ExecCmd, code: ErrorCode, message: &str, retryable: bool) {
        let command_id = extract_command_id(&cmd.action).unwrap_or_default();
        audit(
            "command.error",
            Some(&command_id),
            Some(&cmd.ctx.token_id),
            "error",
            std::time::Duration::ZERO,
            None,
        );
        let error = action_error(command_id.clone(), code, message, retryable);
        if code == ErrorCode::DeliveryUnknown {
            if let Ok(id) = Uuid::parse_str(&command_id) {
                let durable = if let Some(store) = self
                    .inner
                    .store
                    .chat(chat_uuid(&cmd.chat_id).unwrap_or_default())
                {
                    store
                        .outbox_get(id)
                        .await
                        .is_some_and(|record| record.status == OutboxStatus::DeliveryUnknown)
                } else {
                    false
                };
                if !durable {
                    let mut failures = self.inner.terminal_failures.lock().await;
                    if failures.len() >= 1_024 && !failures.contains_key(&id) {
                        self.inner
                            .terminal_failure_overflow
                            .store(true, Ordering::Release);
                    } else {
                        failures.insert(id, error.clone());
                    }
                }
            }
        }
        let outbound = OutboundMsg::Frame(Frame::ActionError(error));
        let _ = cmd.tx.send(outbound.clone()).await;
        if let Ok(id) = Uuid::parse_str(&command_id) {
            self.publish_terminal_watchers(id, outbound, &cmd.tx).await;
        }
    }

    async fn send_committed(&self, cmd: &ExecCmd, turn_id: Option<&str>, chat_id: Option<&str>) {
        let command_id = extract_command_id(&cmd.action).unwrap_or_default();
        let outbound = OutboundMsg::Frame(Frame::ActionAck(ActionAck {
            command_id: command_id.clone(),
            status: AckStatus::Committed,
            turn_id: turn_id.map(str::to_string),
            chat_id: chat_id.map(str::to_string),
            project_id: None,
            session_id: None,
            acp_session_id: None,
            committed_projection_version: None,
        }));
        let _ = cmd.tx.send(outbound.clone()).await;
        if let Ok(id) = Uuid::parse_str(&command_id) {
            self.publish_terminal_watchers(id, outbound, &cmd.tx).await;
        }
    }

    // ── session/list 轮询（§6.3：10s 全量同步投影，服务端侧）─────────────

    /// 启动 session/list 轮询任务（hub 装配时调用一次；§6.3「响应中不存在
    /// 的旧条目删除——自愈」由 [`DocCommand::RegistryApplySessions`] 幂等
    /// diff 保证）。sessions 是 instance 级数据，按 instance 去重轮询、
    /// 投影到全局 Registry Doc（不随 chat 销毁/重建）。
    ///
    /// 【决策】轮询在 **server 侧**而非 instance：instance 是哑管道
    /// （ACP 子进程 stdio 透传），JSON-RPC 响应经 pending_rpc（§4.4 L3）
    /// 按 rpc_id 匹配，无需 instance 参与；且响应的投影写入走控制面
    /// （DocCommand，不经聚合器 seq 水位——宿主注入无 instance 流 seq，
    /// `SetTurnTerminal` 同源）。
    pub fn spawn_session_poller(&self) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(SESSION_POLL_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // 消费首 tick：窗口从此刻开始计时（interval 首 tick 立即就绪）。
            ticker.tick().await;
            loop {
                ticker.tick().await;
                me.poll_sessions_once().await;
            }
        });
    }

    /// 单轮轮询：按 (instance, cwd) 去重（ACP 会话是 instance 级数据且按
    /// cwd 分面——不同 workspace 目录的会话互不相交，§6.3 workspace 扩展），
    /// 每个组合取一个非终态已绑定 chat 作为转发通道，投影到全局 Registry
    /// Doc（不随 chat 销毁/重建）。
    async fn poll_sessions_once(&self) {
        let chats = self.inner.chats.all_chats().await;
        let mut per_cwd: HashMap<(String, String), String> = HashMap::new();
        for (chat_id, record) in chats {
            // 终态（ended/closed/crashed）chat 的 ACP 进程已退出，跳过；
            // 未 binding（create 序列未完成）也跳过——进程可能仍在
            // spawn/initialize，最小查询面。
            if record.state.is_terminal() || record.session_id.is_none() {
                continue;
            }
            per_cwd
                .entry((record.instance_id.clone(), record.cwd.clone()))
                .or_insert(chat_id);
        }
        for ((instance_id, cwd), chat_id) in per_cwd {
            let me = self.clone();
            tokio::spawn(async move {
                me.poll_instance_sessions(&instance_id, &chat_id, &cwd)
                    .await;
            });
        }
    }

    /// 单个 (instance, cwd) 的 session/list 请求：L3 匹配（register_rpc +
    /// oneshot）→ 解析（条目带 cwd）→ `RegistryState::apply_sessions`（投影
    /// 到 Registry Doc，§6.3）。
    async fn poll_instance_sessions(&self, instance_id: &str, chat_id: &str, cwd: &str) {
        let rpc_id = self.inner.translator.alloc_rpc_id();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "session/list",
            "params": { "cwd": cwd },
        });
        let rx = self
            .inner
            .relay
            .register_rpc(&rpc_id, "session_list".to_string())
            .await;
        if let Err(e) = self
            .inner
            .instance
            .forward_rpc(instance_id, chat_id, &msg)
            .await
        {
            // forward 失败（instance 离线等）：撤销 pending 表项，下轮重试。
            self.inner.relay.cancel_rpc(&rpc_id).await;
            debug!(chat_id, instance_id, error = ?e, "session poll forward failed");
            return;
        }
        match tokio::time::timeout(SESSION_POLL_TIMEOUT, rx).await {
            Ok(Ok(r)) if r.get("error").is_none() => {
                let mut entries = parse_session_list_response(&r);
                // 条目标注所属 cwd（per-cwd 全量同步的投影面，§6.3）。
                for e in &mut entries {
                    e.cwd = cwd.to_string();
                }
                self.refresh_catalog_titles(&entries).await;
                if let Err(e) = self.inner.chats.registry().apply_sessions(entries).await {
                    warn!(chat_id, instance_id, error = ?e, "session poll apply failed");
                }
            }
            _ => {
                // 超时/错误响应/通道关闭：撤销 pending 表项，下轮重试
                // （§6.3 自愈；session/list 无副作用，可安全重发）。
                self.inner.relay.cancel_rpc(&rpc_id).await;
                debug!(chat_id, "session poll timeout");
            }
        }
    }

    async fn refresh_catalog_titles(&self, entries: &[SessionSummaryProjection]) {
        let projects = self.inner.projects.read().await.clone();
        let Some(projects) = projects else { return };
        if let Err(error) = projects.refresh_acp_titles(entries).await {
            warn!(error = ?error, "ACP session title metadata refresh failed");
        }
    }
}

#[derive(Debug, Clone, Default)]
struct PromptProjectionEvidence {
    projected: bool,
    terminal: bool,
    entry_id: Option<String>,
    turn_id: Option<Uuid>,
    delivery_schema_version: Option<i64>,
    delivery_state: Option<String>,
    payload_fingerprint: Option<String>,
    conflicted: bool,
}

fn exact_prompt_terminal_evidence(
    record: &crate::persist::outbox::OutboxRecord,
    projected: &PromptProjectionEvidence,
) -> bool {
    projected.projected
        && !projected.conflicted
        && projected.terminal
        && projected.delivery_schema_version == Some(2)
        && projected.turn_id == record.turn_id
        && projected.payload_fingerprint.is_some()
        && projected.payload_fingerprint.as_deref() == record.payload_fingerprint.as_deref()
}

fn is_v2_pending_orphan(projected: &PromptProjectionEvidence) -> bool {
    projected.delivery_schema_version == Some(2)
        && !projected.conflicted
        && projected.payload_fingerprint.is_some()
        && projected.delivery_state.as_deref() == Some("pending")
}

fn prompt_projection_evidence(
    chat_snapshot: Option<(Vec<u8>, u32)>,
    session_snapshot: Option<(Vec<u8>, u32)>,
) -> Option<(HashMap<Uuid, PromptProjectionEvidence>, bool)> {
    let mut evidence = HashMap::<Uuid, PromptProjectionEvidence>::new();
    let (chat_update, _) = chat_snapshot?;
    let chat = yrs::Doc::new();
    let update = yrs::Update::decode_v1(&chat_update).ok()?;
    chat.transact_mut().apply_update(update).ok()?;
    let txn = chat.transact();
    let root = txn.get_map(crate::state::factory::ROOT)?;
    let entries = root
        .get(&txn, "entries")
        .and_then(|value| value.cast::<yrs::MapRef>().ok())?;
    let mut turns = HashMap::<String, Uuid>::new();
    let mut terminal_turns = HashSet::<String>::new();
    for (entry_id, value) in entries.iter(&txn) {
        let Ok(entry) = value.cast::<yrs::MapRef>() else {
            continue;
        };
        let role = entry
            .get(&txn, "role")
            .and_then(|value| value.cast::<String>().ok());
        let turn_id = entry
            .get(&txn, "turn_id")
            .and_then(|value| value.cast::<String>().ok());
        if role.as_deref() == Some("assistant") {
            let status = entry
                .get(&txn, "status")
                .and_then(|value| value.cast::<String>().ok());
            if status
                .as_deref()
                .is_some_and(|status| matches!(status, "completed" | "error" | "cancelled"))
            {
                if let Some(turn_id) = turn_id {
                    terminal_turns.insert(turn_id);
                }
            }
            continue;
        }
        if role.as_deref() != Some("user") {
            continue;
        }
        let Some(command_id) = entry
            .get(&txn, "source_command_id")
            .and_then(|value| value.cast::<String>().ok())
            .and_then(|value| Uuid::parse_str(&value).ok())
        else {
            continue;
        };
        let candidate_entry_id = entry_id.to_string();
        let candidate_turn_id = turn_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok());
        let candidate_schema = entry
            .get(&txn, "delivery_schema_version")
            .and_then(|value| value.cast::<i64>().ok());
        let candidate_state = entry
            .get(&txn, "delivery_state")
            .and_then(|value| value.cast::<String>().ok());
        let candidate_fingerprint = entry
            .get(&txn, "payload_fingerprint")
            .and_then(|value| value.cast::<String>().ok());
        let item = evidence.entry(command_id).or_default();
        if item.projected
            && (item.entry_id.as_deref() != Some(candidate_entry_id.as_str())
                || item.turn_id != candidate_turn_id
                || item.payload_fingerprint != candidate_fingerprint)
        {
            item.conflicted = true;
        }
        item.projected = true;
        item.entry_id = Some(candidate_entry_id);
        item.turn_id = candidate_turn_id;
        item.delivery_schema_version = candidate_schema;
        item.delivery_state = candidate_state;
        item.payload_fingerprint = candidate_fingerprint;
        if let Some(turn_id) = turn_id {
            turns.insert(turn_id, command_id);
        }
    }
    for turn in terminal_turns {
        if let Some(command_id) = turns.get(&turn).copied() {
            evidence.entry(command_id).or_default().terminal = true;
        }
    }
    drop(txn);

    let mut complete = session_snapshot.is_some();
    if let Some((session_update, _)) = session_snapshot {
        let session = yrs::Doc::new();
        if let Ok(update) = yrs::Update::decode_v1(&session_update) {
            if session.transact_mut().apply_update(update).is_ok() {
                let txn = session.transact();
                if let Some(map) = txn
                    .get_map(crate::state::factory::ROOT)
                    .and_then(|root| root.get(&txn, "session"))
                    .and_then(|value| value.cast::<yrs::MapRef>().ok())
                {
                    let turn_id = map
                        .get(&txn, "active_turn_id")
                        .and_then(|value| value.cast::<String>().ok());
                    let status = map
                        .get(&txn, "active_turn_status")
                        .and_then(|value| value.cast::<String>().ok());
                    if status.as_deref().is_some_and(|status| {
                        matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
                    }) {
                        if let Some(command_id) = turn_id.and_then(|turn| turns.get(&turn).copied())
                        {
                            evidence.entry(command_id).or_default().terminal = true;
                        }
                    }
                } else {
                    complete = false;
                }
            } else {
                complete = false;
            }
        } else {
            complete = false;
        }
    }
    Some((evidence, complete))
}

fn normalize_prompt_status(
    record: crate::persist::outbox::OutboxRecord,
    projection: Option<&PromptProjectionEvidence>,
) -> PromptStatusItem {
    let projection = projection.cloned().unwrap_or_default();
    let status = match record.status {
        OutboxStatus::DeliveryUnknown => PromptDeliveryStatus::DeliveryUnknown,
        OutboxStatus::Completed if projection.projected && projection.terminal => {
            PromptDeliveryStatus::Completed
        }
        OutboxStatus::Failed => PromptDeliveryStatus::Failed,
        _ if projection.projected => PromptDeliveryStatus::Projected,
        _ => PromptDeliveryStatus::DeliveryUnknown,
    };
    PromptStatusItem {
        command_id: record.command_id.to_string(),
        turn_id: record.turn_id.map(|turn| turn.to_string()),
        status,
        created_at: record.created_at.to_rfc3339(),
        updated_at: record.updated_at.to_rfc3339(),
        error_code: matches!(
            status,
            PromptDeliveryStatus::Failed | PromptDeliveryStatus::DeliveryUnknown
        )
        .then(|| record.last_error.map(|error| error.code))
        .flatten(),
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn action_error(
    command_id: String,
    code: ErrorCode,
    message: &str,
    retryable: bool,
) -> ActionError {
    ActionError {
        command_id,
        code,
        message: message.to_string(),
        retryable,
        retry_after_ms: None,
    }
}

fn command_type_of(action: &ActionEnvelope) -> CommandType {
    match action {
        ActionEnvelope::Create { .. } => CommandType::Create,
        ActionEnvelope::Prompt { .. } => CommandType::Prompt,
        ActionEnvelope::Cancel { .. } => CommandType::Cancel,
        ActionEnvelope::Close { .. } => CommandType::Close,
        ActionEnvelope::ResolvePermission { .. } => CommandType::Resolve,
        _ => CommandType::Prompt, // 白名单外 action 在 gateway 已拦（防御）
    }
}

fn extract_command_id(action: &ActionEnvelope) -> Option<String> {
    match action {
        ActionEnvelope::ProjectCreate { command_id, .. }
        | ActionEnvelope::ProjectArchive { command_id, .. }
        | ActionEnvelope::ProjectRestore { command_id, .. }
        | ActionEnvelope::ProjectRename { command_id, .. }
        | ActionEnvelope::PersistedSessionCreate { command_id, .. }
        | ActionEnvelope::PersistedSessionOpen { command_id, .. }
        | ActionEnvelope::PersistedSessionRename { command_id, .. }
        | ActionEnvelope::PersistedSessionArchive { command_id, .. }
        | ActionEnvelope::PersistedSessionRestore { command_id, .. }
        | ActionEnvelope::PersistedSessionImport { command_id, .. }
        | ActionEnvelope::PersistedSessionDiscover { command_id, .. }
        | ActionEnvelope::PersistedSessionPromptStatus { command_id, .. }
        | ActionEnvelope::Create { command_id, .. }
        | ActionEnvelope::Load { command_id, .. }
        | ActionEnvelope::Close { command_id, .. }
        | ActionEnvelope::Prompt { command_id, .. }
        | ActionEnvelope::SessionNew { command_id, .. }
        | ActionEnvelope::Cancel { command_id, .. }
        | ActionEnvelope::ResolvePermission { command_id, .. }
        | ActionEnvelope::SubscribeEvents { command_id, .. }
        | ActionEnvelope::UnsubscribeEvents { command_id, .. }
        | ActionEnvelope::WorkspaceCreate { command_id, .. }
        | ActionEnvelope::WorkspaceRemove { command_id, .. }
        | ActionEnvelope::SessionList { command_id, .. } => Some(command_id.clone()),
    }
}

fn prompt_payload_fingerprint(payload: &PromptChatPayload) -> Result<String, MetadataError> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CanonicalPromptPayload<'a> {
        action_type: &'static str,
        chat_id: &'a str,
        message: &'a str,
        effort: Option<&'a str>,
    }
    payload_hash(&CanonicalPromptPayload {
        action_type: "chat/prompt",
        chat_id: &payload.chat_id,
        message: &payload.message,
        effort: payload.effort.as_deref(),
    })
}

/// Caller holds the watcher mutex across the durable terminal re-read and
/// this insertion. Terminal publication acquires the same mutex, making
/// attach-vs-publish a single ordering decision rather than a double-send
/// race.
fn attach_terminal_watcher_locked(
    watchers: &mut HashMap<Uuid, Vec<mpsc::Sender<OutboundMsg>>>,
    command_id: Uuid,
    tx: mpsc::Sender<OutboundMsg>,
) -> bool {
    const MAX_PER_COMMAND: usize = 8;
    const MAX_GLOBAL: usize = 256;
    watchers
        .values_mut()
        .for_each(|senders| senders.retain(|sender| !sender.is_closed()));
    let total = watchers.values().map(Vec::len).sum::<usize>();
    let entry = watchers.entry(command_id).or_default();
    if entry.iter().any(|sender| sender.same_channel(&tx)) {
        return true;
    }
    if entry.len() >= MAX_PER_COMMAND || total >= MAX_GLOBAL {
        return false;
    }
    entry.push(tx);
    true
}

fn extract_chat_id(action: &ActionEnvelope) -> Option<String> {
    match action {
        ActionEnvelope::Prompt { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::Cancel { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::Close { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::ResolvePermission { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::Load { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::SessionNew { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::Create { .. } => None,
        ActionEnvelope::ProjectCreate { .. }
        | ActionEnvelope::ProjectArchive { .. }
        | ActionEnvelope::ProjectRestore { .. }
        | ActionEnvelope::ProjectRename { .. }
        | ActionEnvelope::PersistedSessionCreate { .. }
        | ActionEnvelope::PersistedSessionOpen { .. }
        | ActionEnvelope::PersistedSessionRename { .. } => None,
        ActionEnvelope::PersistedSessionArchive { .. }
        | ActionEnvelope::PersistedSessionRestore { .. } => None,
        ActionEnvelope::PersistedSessionImport { .. } => None,
        ActionEnvelope::PersistedSessionDiscover { .. } => None,
        ActionEnvelope::PersistedSessionPromptStatus { .. } => None,
        ActionEnvelope::SubscribeEvents { .. } | ActionEnvelope::UnsubscribeEvents { .. } => None,
        // workspace 管理命令 / session/list 按需查询：submit 层直接执行
        // （不解析 chat_id）。
        ActionEnvelope::WorkspaceCreate { .. }
        | ActionEnvelope::WorkspaceRemove { .. }
        | ActionEnvelope::SessionList { .. } => None,
    }
}

/// 去重判定（§4.4 表：committed → duplicate；delivery_unknown 非幂等 →
/// 禁止自动重发；failed → 重发原错误；retryable 失败已 clear_for_retry 的
/// 记录（已 tombstone）→ 放行）。
fn dedup_verdict(rec: &crate::persist::outbox::OutboxRecord) -> DedupVerdict {
    match rec.status {
        OutboxStatus::Completed => DedupVerdict::Duplicate,
        OutboxStatus::Failed => DedupVerdict::RedeliverFailed,
        OutboxStatus::DeliveryUnknown => {
            if rec.retryable_class == RetryableClass::SafeToRedeliver {
                DedupVerdict::Proceed
            } else {
                DedupVerdict::RedeliverUnknown
            }
        }
        OutboxStatus::Received
        | OutboxStatus::Accepted
        | OutboxStatus::IntentDurable
        | OutboxStatus::Dispatched
        | OutboxStatus::DeliveryConfirmed
        | OutboxStatus::ProjectionCommitted => DedupVerdict::InProgress,
    }
}

fn terminal_replay(
    command_id: String,
    rec: &crate::persist::outbox::OutboxRecord,
    chat_id: Option<String>,
) -> Option<SubmitAck> {
    match rec.status {
        OutboxStatus::Completed => Some(SubmitAck::Duplicate(ActionAck {
            command_id,
            status: AckStatus::Duplicate,
            turn_id: rec.turn_id.map(|turn| turn.to_string()),
            chat_id,
            project_id: None,
            session_id: None,
            acp_session_id: None,
            committed_projection_version: None,
        })),
        OutboxStatus::Failed => {
            let error = rec
                .last_error
                .clone()
                .unwrap_or_else(|| LastError::from_error_code(ErrorCode::InvalidState));
            Some(SubmitAck::Failed(ActionError {
                command_id,
                code: error_code_from_str(&error.code),
                message: "command previously failed; retry not permitted".to_string(),
                retryable: error.retryable,
                retry_after_ms: None,
            }))
        }
        OutboxStatus::DeliveryUnknown => Some(SubmitAck::Failed(action_error(
            command_id,
            ErrorCode::DeliveryUnknown,
            "delivery outcome is unknown; retry is blocked",
            false,
        ))),
        _ => None,
    }
}

/// 验证重试 action 是持久恢复证据所绑定的原 permission payload。
/// 是否处于可恢复阶段由调用方独立判定，避免把 `dispatched`
/// 的投递未知误报成 payload 篡改。
fn permission_recovery_payload_matches(
    rec: &crate::persist::outbox::OutboxRecord,
    action: &ActionEnvelope,
) -> bool {
    let ActionEnvelope::ResolvePermission { payload, .. } = action else {
        return false;
    };
    matches!(
        rec.recovery.as_deref(),
        Some(CommandRecovery::PermissionResponse {
            permission_id,
            decision,
            ..
        }) if permission_id == &payload.permission_id && decision == &payload.decision
    )
}

enum DedupVerdict {
    Duplicate,
    RedeliverFailed,
    RedeliverUnknown,
    InProgress,
    Proceed,
}

/// instance 错误 → 稳定错误码（§4.4 retryable 分类事实源）。
fn instance_error_code(e: &InstanceError) -> ErrorCode {
    match e {
        InstanceError::Offline => ErrorCode::InstanceOffline,
        InstanceError::Timeout
        | InstanceError::ForwardRejected(_)
        | InstanceError::UnknownInstance(_)
        | InstanceError::ConnectionGone => ErrorCode::AgentUnavailable,
    }
}

/// chat_id（hub 侧，uuid 形态）→ Uuid。
fn chat_uuid(chat_id: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(chat_id).ok()
}

/// LastError.code（String，§4.4 稳定码）→ ErrorCode（脱敏映射；未知串 →
/// INVALID_STATE，防御）。
fn error_code_from_str(s: &str) -> ErrorCode {
    match s {
        "UNAUTHENTICATED" => ErrorCode::Unauthenticated,
        "FORBIDDEN" => ErrorCode::Forbidden,
        "CHAT_NOT_FOUND" => ErrorCode::ChatNotFound,
        "INSTANCE_OFFLINE" => ErrorCode::InstanceOffline,
        "VERSION_CONFLICT" => ErrorCode::VersionConflict,
        "INVALID_STATE" => ErrorCode::InvalidState,
        "RATE_LIMITED" => ErrorCode::RateLimited,
        "AGENT_UNAVAILABLE" => ErrorCode::AgentUnavailable,
        "DELIVERY_UNKNOWN" => ErrorCode::DeliveryUnknown,
        "PAYLOAD_TOO_LARGE" => ErrorCode::PayloadTooLarge,
        _ => ErrorCode::InvalidState,
    }
}

fn error_code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unauthenticated => "UNAUTHENTICATED",
        ErrorCode::Forbidden => "FORBIDDEN",
        ErrorCode::ChatNotFound => "CHAT_NOT_FOUND",
        ErrorCode::InstanceOffline => "INSTANCE_OFFLINE",
        ErrorCode::VersionConflict => "VERSION_CONFLICT",
        ErrorCode::InvalidState => "INVALID_STATE",
        ErrorCode::RateLimited => "RATE_LIMITED",
        ErrorCode::AgentUnavailable => "AGENT_UNAVAILABLE",
        ErrorCode::DeliveryUnknown => "DELIVERY_UNKNOWN",
        ErrorCode::PayloadTooLarge => "PAYLOAD_TOO_LARGE",
        ErrorCode::UnsupportedFrame => "UNSUPPORTED_FRAME",
        _ => "INVALID_STATE",
    }
}

/// session/new response 解析：`result.sessionId` / `result.session_id` / result
/// 直接为字符串。
fn extract_session_id(response: &serde_json::Value) -> Option<String> {
    let result = response.get("result")?;
    if let Some(s) = result.get("sessionId").and_then(serde_json::Value::as_str) {
        return Some(s.to_string());
    }
    if let Some(s) = result.get("session_id").and_then(serde_json::Value::as_str) {
        return Some(s.to_string());
    }
    result.as_str().map(str::to_string)
}

/// session/list response 解析（纯函数，可单测）：`result.sessions[]` →
/// [`SessionSummaryProjection`]。
///
/// peri ACP 的 `SessionInfo` 序列化为 camelCase（sessionId/title/updatedAt，
/// 无 status 字段 → 缺省空串）；兼容 snake_case 与缺失字段兜底。条目缺
/// session_id 时丢弃（防御）。
pub fn parse_session_list_response(response: &serde_json::Value) -> Vec<SessionSummaryProjection> {
    let mut out = Vec::new();
    let Some(entries) = response
        .get("result")
        .and_then(|r| r.get("sessions"))
        .and_then(serde_json::Value::as_array)
    else {
        return out;
    };
    for e in entries {
        let str_or = |k_camel: &str, k_snake: &str| -> String {
            e.get(k_camel)
                .or_else(|| e.get(k_snake))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let session_id = str_or("sessionId", "session_id");
        if session_id.is_empty() {
            continue;
        }
        out.push(SessionSummaryProjection {
            session_id,
            title: str_or("title", "title"),
            status: str_or("status", "status"),
            updated_at: str_or("updatedAt", "updated_at"),
            // cwd 由轮询侧按 (instance, cwd) 查询面标注（§6.3 workspace 扩展）。
            cwd: String::new(),
            // 绑定标注由调用方填写（轮询投影不写；按需查询在 exec_session_list
            // 按 binding 表补齐，§8.5 激活语义）。
            bound_chat_id: None,
        });
    }
    out
}

#[cfg(test)]
#[path = "command_coordinator_test.rs"]
mod command_coordinator_test;
