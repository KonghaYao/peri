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
//! projection_committed → completed → committed Ack`；30s 无 L3 → delivery_unknown
//! （路径 B：非幂等禁止自动重发，§4.4）。
//!
//! create 序列（§6.2）：`spawn（10s）→ spawn_ack → initialize（10s）→
//! session/new（30s binding）→ bind → committed(chatId)`；任一步超时 →
//! `AGENT_UNAVAILABLE`(retryable) + 清理半创建状态（补发 kill，§6.2）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, warn};
use uuid::Uuid;

use acp_hub_proto::ack::{AckStatus, ActionAck, ActionError, ErrorCode};
use acp_hub_proto::action::{ActionEnvelope, CreateChatPayload};
use acp_hub_proto::frame::Frame;
use acp_hub_proto::instance::{InstanceKill, InstanceSpawn};
use acp_hub_proto::schema::TurnStatus;

use crate::auth::audit::audit;
use crate::auth::ConnectionCtx;
use crate::channel::broadcaster::OutboundMsg;
use crate::channel::relay_event_handler::RelayEventHandler;
use crate::control::{InstanceError, InstanceRegistry, SpawnOutcome};
use crate::control::{ChatRegistry, ChatState};
use crate::persist::outbox::{CommandType, LastError, NewOutboxRecord, OutboxStatus, RetryableClass};
use crate::persist::Store;
use crate::protocol::{OutboundCtx, OutboundMessage, Translator};
use crate::state::doc_manager::{DocCommand, DocManager, SubmitError, SubmitResult};
use crate::state::doc_manager::BatchConfig;

/// 默认 instance（§4.3 P5：instance_id 缺省 = 本机）。
pub const DEFAULT_INSTANCE_ID: &str = "local";

/// 默认 ACP 启动命令（架构 §11「默认 `peri acp`，可配置」；M1 起经
/// `Config::acp_cmd` 可配——config.toml `acp_cmd` 数组或
/// `ACP_HUB_ACP_CMD` 空格拆分，见 `crate::config`）。
pub use crate::config::DEFAULT_ACP_CMD;

/// L3 确认超时（§4.4 路径 B：30s 无响应 → delivery_unknown）【决策：设计稿
/// §16 测试 13 的 30s 常量，非 §16 配置表项】。
pub const L3_TIMEOUT: Duration = Duration::from_secs(30);

/// 提交结果（同步返回的部分）：accepted 立即；终态经连接发送队列。
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitAck {
    /// 已入队（accepted，§4.4：只表示进入有界处理队列）。
    Accepted { command_id: String },
    /// 已提交命令重发（§4.4）：duplicate + 原 turnId，**不重复调用 Agent**。
    Duplicate(ActionAck),
    /// 同步失败（RATE_LIMITED/CHAT_NOT_FOUND/INVALID_STATE…）→ action_error。
    Failed(ActionError),
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
    relay: Arc<RelayEventHandler>,
    translator: Translator,
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
    /// L3 确认超时（§4.4 路径 B 默认 30s；测试注入短值）。
    l3_timeout: Duration,
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
            store, doc, instance, chats, relay, cfg, acp_cmd,
            spawn_timeout, initialize_timeout, binding_timeout, L3_TIMEOUT,
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
                chats,
                relay,
                translator: Translator::new(),
                queue_cap: cfg.chat_queue,
                executors: RwLock::new(HashMap::new()),
                create_tx: RwLock::new(None),
                create_index: RwLock::new(HashMap::new()),
                default_cwd,
                acp_cmd,
                spawn_timeout,
                initialize_timeout,
                binding_timeout,
                l3_timeout,
            }),
        }
    }

    /// create 全局去重索引重建（§4.4：跨 server 重启有效——启动时从 outbox
    /// 重放重建）。hub 装配时（store.recover 完成后）调用一次。
    pub async fn rebuild_create_index(&self) {
        let mut idx: HashMap<Uuid, Uuid> = HashMap::new();
        for (cid, store) in self.inner.store.chats_snapshot() {
            let recs = store.outbox().lock().await.records().cloned().collect::<Vec<_>>();
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

    /// 提交入口（§7.4 规则 6）：临界区内 去重判定 → try_reserve → 入队。
    ///
    /// `tx` 为客户端连接发送队列（执行器终态回投）。
    pub async fn submit(
        &self,
        ctx: &ConnectionCtx,
        action: ActionEnvelope,
        tx: mpsc::Sender<OutboundMsg>,
    ) -> SubmitAck {
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
                                        ErrorCode::AgentUnavailable,
                                        "delivery unknown; automatic retry not permitted (path B)",
                                        false,
                                    ))
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
        if let Some(rec) = store.outbox_get(command_id).await {
            match dedup_verdict(&rec) {
                DedupVerdict::Duplicate => {
                    return SubmitAck::Duplicate(ActionAck {
                        command_id: command_id_str,
                        status: AckStatus::Duplicate,
                        turn_id: rec.turn_id.map(|t| t.to_string()),
                        chat_id: None,
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
                        message: "command previously failed; retry not permitted".to_string(),
                        retryable: err.retryable,
                        retry_after_ms: None,
                    });
                }
                DedupVerdict::RedeliverUnknown => {
                    return SubmitAck::Failed(action_error(
                        command_id_str,
                        ErrorCode::AgentUnavailable,
                        "delivery unknown; automatic retry not permitted (path B)",
                        false,
                    ))
                }
                DedupVerdict::Proceed => {}
            }
        }

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
        if let Err(e) = store
            .outbox()
            .lock()
            .await
            .insert(NewOutboxRecord {
                command_id,
                chat_id,
                command_type,
                turn_id,
                retryable_class,
            })
        {
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
            self.inner.doc.release_reserve(&chat_id_str).await;
            return SubmitAck::Failed(action_error(
                command_id_str,
                ErrorCode::AgentUnavailable,
                "outbox mark_accepted failed",
                true,
            ));
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

    /// create 前置（临界区内）：生成 chat_id + 建持久化目录 + 打开 doc +
    /// 登记 chat + outbox 目录就绪。
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
            .open_chat(&chat_id.to_string(), &instance_id, Some(&title))
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
            .register(&chat_id.to_string(), &instance_id, Some(&title))
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
                self.inner.executors.write().await.insert(chat_id, tx.clone());
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
                chat_id, command_id = extract_command_id(&cmd.action).unwrap_or_default(),
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
        // 1. intent durable（§4.4 提交点纪律第一步）。
        if let Err(e) = store.outbox().lock().await.mark_intent_durable(command_id) {
            warn!(chat_id, error = ?e, "mark_intent_durable failed");
            return;
        }
        // 2. binding 校验 + 翻译（rpcId 登记，§4.4）。
        let Some(entry) = self.inner.chats.entry(chat_id).await else {
            self.send_error(cmd, ErrorCode::ChatNotFound, "chat not found", false)
                .await;
            return;
        };
        let Some(acp_session_id) = entry.session_id.clone() else {
            self.send_error(cmd, ErrorCode::InvalidState, "chat binding not established", false)
                .await;
            return;
        };
        let instance_id = entry.instance_id.clone();
        let msg = match self.inner.translator.translate(
            &cmd.action,
            &OutboundCtx {
                cwd: self.inner.default_cwd.clone(),
                acp_session_id,
                turn_id: turn_id.to_string(),
            },
        ) {
            Ok(OutboundMessage::JsonRpc(v)) => v,
            Ok(_) => {
                self.send_error(cmd, ErrorCode::InvalidState, "unexpected outbound shape", false)
                    .await;
                return;
            }
            Err(e) => {
                self.send_error(cmd, ErrorCode::InvalidState, &format!("translate failed: {e}"), false)
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
        // 3. 下发（L1+L2：forward_ack = M1 转发确认，§4.4）。
        if let Err(e) = self.inner.instance.forward_rpc(&instance_id, chat_id, &msg).await {
            self.fail_retryable(chat_id, command_id, cmd, instance_error_code(&e), "forward failed")
                .await;
            return;
        }
        if let Err(e) = store.outbox().lock().await.mark_dispatched(command_id, Utc::now()) {
            warn!(chat_id, error = ?e, "mark_dispatched failed");
            return;
        }
        // 4. 投影 user entry（L1+L2 后，§6.4 提交点纪律；服务端单写，§6.5）。
        let entry_id = format!("{turn_id}:user");
        let created_at = Utc::now().to_rfc3339();
        match self
            .inner
            .doc
            .submit_command(
                chat_id,
                DocCommand::RegisterUserEntry {
                    turn_id: turn_id.to_string(),
                    entry_id: entry_id.clone(),
                    text: payload.message.clone(),
                    author_user_id: None,
                    created_at: created_at.clone(),
                },
            )
            .await
        {
            SubmitResult::Rejected(SubmitError::ChatNotFound) => {
                self.send_error(cmd, ErrorCode::ChatNotFound, "chat not found", false)
                    .await;
                return;
            }
            SubmitResult::PersistFailed => {
                warn!(chat_id, "user entry projection persist failed (degraded)");
            }
            _ => {}
        }
        self.inner
            .chats
            .set_active_turn(chat_id, &turn_id.to_string())
            .await;
        // 5. L3 等待（§4.4 路径 B；超时 → delivery_unknown）。
        match tokio::time::timeout(self.inner.l3_timeout, rx).await {
            Ok(Ok(response)) => {
                let is_error = response.get("error").is_some();
                if is_error {
                    self.fail_terminal(
                        chat_id, command_id, cmd,
                        ErrorCode::AgentUnavailable,
                        "agent rejected prompt (L3 error)",
                    )
                    .await;
                    return;
                }
                if let Err(e) = store.outbox().lock().await.mark_delivery_confirmed(command_id) {
                    warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
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
                let terminal_status = if stop_reason == "cancelled" {
                    TurnStatus::Cancelled
                } else {
                    TurnStatus::Completed
                };
                self.inject_turn_terminal(chat_id, &turn_id.to_string(), terminal_status)
                    .await;
                if let Err(e) = store.outbox().lock().await.mark_projection_committed(command_id) {
                    warn!(chat_id, error = ?e, "mark_projection_committed failed");
                    return;
                }
                if let Err(e) = store.outbox().lock().await.mark_completed(command_id) {
                    warn!(chat_id, error = ?e, "mark_completed failed");
                    return;
                }
                self.inner.chats.clear_active_turn(chat_id).await;
                self.send_committed(cmd, Some(&turn_id.to_string()), None).await;
                audit(
                    "command.committed",
                    Some(&command_id_str),
                    Some(&cmd.ctx.token_id),
                    "ok",
                    std::time::Duration::ZERO,
                    None,
                );
            }
            Ok(Err(_)) | Err(_) => {
                // 30s 无 L3 → delivery_unknown（路径 B：非幂等禁止自动重发，
                // §4.4）。
                self.inner.relay.cancel_rpc(&rpc_id).await;
                if let Err(e) = store.outbox().lock().await.mark_delivery_unknown(command_id) {
                    warn!(chat_id, error = ?e, "mark_delivery_unknown failed");
                    return;
                }
                self.send_error(
                    cmd,
                    ErrorCode::AgentUnavailable,
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
            cwd: self.inner.default_cwd.clone(),
            env: None,
        };
        let spawn = match tokio::time::timeout(self.inner.spawn_timeout, self.inner.instance.send_spawn(&instance_id, spawn_cmd)).await {
            Ok(Ok(SpawnOutcome::Acked(a))) => Some(a),
            Ok(Err(e)) => {
                self.fail_retryable(&chat_id, command_id, cmd, instance_error_code(&e), "spawn failed")
                    .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
            Err(_) => {
                self.fail_retryable(&chat_id, command_id, cmd, ErrorCode::AgentUnavailable, "spawn timeout (10s)")
                    .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
        };
        let spawn = spawn.expect("spawn outcome");
        if !spawn.ok {
            self.fail_retryable(&chat_id, command_id, cmd, ErrorCode::AgentUnavailable, "agent spawn failed")
                .await;
            self.cleanup_create(&chat_id, &instance_id).await;
            return;
        }
        // L1+L2（§4.4：create 的 delivery_confirmed 只要求 spawn_ack）。
        if let Err(e) = store.outbox().lock().await.mark_dispatched(command_id, Utc::now()) {
            warn!(chat_id, error = ?e, "mark_dispatched failed");
            return;
        }
        if let Err(e) = store.outbox().lock().await.mark_delivery_confirmed(command_id) {
            warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
            return;
        }
        // 2. initialize（10s）。
        let (init_rpc_id, init_msg) = self.inner.translator.initialize_rpc(&self.inner.default_cwd);
        let init_rx = self
            .inner
            .relay
            .register_rpc(&init_rpc_id, command_id_str.clone())
            .await;
        if let Err(e) = self.inner.instance.forward_rpc(&instance_id, &chat_id, &init_msg).await {
            self.fail_retryable(&chat_id, command_id, cmd, instance_error_code(&e), "initialize forward failed")
                .await;
            self.cleanup_create(&chat_id, &instance_id).await;
            return;
        }
        match tokio::time::timeout(self.inner.initialize_timeout, init_rx).await {
            Ok(Ok(r)) if r.get("error").is_none() => {}
            Ok(Ok(_)) => {
                self.fail_retryable(&chat_id, command_id, cmd, ErrorCode::AgentUnavailable, "initialize rejected")
                    .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
            Ok(Err(_)) | Err(_) => {
                self.fail_retryable(&chat_id, command_id, cmd, ErrorCode::AgentUnavailable, "initialize timeout (10s)")
                    .await;
                self.cleanup_create(&chat_id, &instance_id).await;
                return;
            }
        }
        // 3. session/new（30s binding，§6.2）。
        let (new_rpc_id, new_msg) = self
            .inner
            .translator
            .session_new_rpc(&self.inner.default_cwd, payload.title.as_deref());
        let new_rx = self
            .inner
            .relay
            .register_rpc(&new_rpc_id, command_id_str.clone())
            .await;
        if let Err(e) = self.inner.instance.forward_rpc(&instance_id, &chat_id, &new_msg).await {
            self.fail_retryable(&chat_id, command_id, cmd, instance_error_code(&e), "session/new forward failed")
                .await;
            self.cleanup_create(&chat_id, &instance_id).await;
            return;
        }
        let acp_session_id = match tokio::time::timeout(self.inner.binding_timeout, new_rx).await {
            Ok(Ok(r)) => extract_session_id(&r),
            Ok(Err(_)) | Err(_) => None,
        };
        let Some(acp_session_id) = acp_session_id else {
            self.fail_retryable(&chat_id, command_id, cmd, ErrorCode::AgentUnavailable, "binding timeout (30s)")
                .await;
            self.cleanup_create(&chat_id, &instance_id).await;
            return;
        };
        // 4. binding（§6.2）。
        if let Err(e) = self
            .inner
            .chats
            .bind(&chat_id, &acp_session_id)
            .await
        {
            warn!(chat_id, error = ?e, "bind failed");
            self.fail_retryable(&chat_id, command_id, cmd, ErrorCode::AgentUnavailable, "bind failed")
                .await;
            self.cleanup_create(&chat_id, &instance_id).await;
            return;
        }
        // 5. 终态（§4.4：projection_committed → completed → committed）。
        if let Err(e) = store.outbox().lock().await.mark_projection_committed(command_id) {
            warn!(chat_id, error = ?e, "mark_projection_committed failed");
            return;
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
            self.send_error(cmd, ErrorCode::InvalidState, "chat binding not established", false)
                .await;
            return;
        };
        let instance_id = entry.instance_id.clone();
        let msg = match self.inner.translator.translate(
            &cmd.action,
            &OutboundCtx {
                cwd: self.inner.default_cwd.clone(),
                acp_session_id,
                // cancel/resolve 方法面无 turnId（§4.3 表），占位不注入。
                turn_id: String::new(),
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
        let forward = if is_notification {
            self.inner
                .instance
                .forward_notification(&instance_id, chat_id, &msg)
                .await
        } else {
            self.inner.instance.forward_rpc(&instance_id, chat_id, &msg).await
        };
        if let Err(e) = forward {
            self.fail_retryable(chat_id, command_id, cmd, instance_error_code(&e), "forward failed")
                .await;
            return;
        }
        if let Err(e) = store.outbox().lock().await.mark_dispatched(command_id, Utc::now()) {
            warn!(chat_id, error = ?e, "mark_dispatched failed");
            return;
        }
        if is_notification {
            // notification：无响应帧可等——发送成功即 L3 等价确认（§7.2
            // 注入 Cancelled 终态；active turn 不存在则无终态可注入，仅确认
            // 命令）。
            if let Err(e) = store.outbox().lock().await.mark_delivery_confirmed(command_id) {
                warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
                return;
            }
            if let Some(turn_id) = self.inner.chats.active_turn(chat_id).await {
                self.inject_turn_terminal(chat_id, &turn_id, TurnStatus::Cancelled)
                    .await;
            }
            if let Err(e) = store.outbox().lock().await.mark_projection_committed(command_id) {
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
                if let Err(e) = store.outbox().lock().await.mark_delivery_confirmed(command_id) {
                    warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
                    return;
                }
                if let Err(e) = store.outbox().lock().await.mark_projection_committed(command_id) {
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
                self.fail_terminal(chat_id, command_id, cmd, ErrorCode::AgentUnavailable, "agent rejected command")
                    .await;
            }
            Ok(Err(_)) | Err(_) => {
                self.inner.relay.cancel_rpc(&rpc_id).await;
                let _ = store.outbox().lock().await.mark_delivery_unknown(command_id);
                self.send_error(
                    cmd,
                    ErrorCode::AgentUnavailable,
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
    async fn inject_turn_terminal(
        &self,
        chat_id: &str,
        turn_id: &str,
        status: TurnStatus,
    ) {
        let _ = self
            .inner
            .doc
            .submit_command(
                chat_id,
                DocCommand::SetTurnTerminal {
                    turn_id: turn_id.to_string(),
                    status,
                    completed_at: Utc::now().to_rfc3339(),
                },
            )
            .await;
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
                    let _ = self
                        .inner
                        .chats
                        .request_close_offline(chat_id)
                        .await;
                }
                self.fail_retryable(chat_id, command_id, cmd, code, "kill failed")
                    .await;
                return;
            }
        };
        if !kill_ok {
            self.fail_retryable(chat_id, command_id, cmd, ErrorCode::AgentUnavailable, "kill rejected by instance")
                .await;
            return;
        }
        if let Err(e) = store.outbox().lock().await.mark_dispatched(command_id, Utc::now()) {
            warn!(chat_id, error = ?e, "mark_dispatched failed");
            return;
        }
        if let Err(e) = store.outbox().lock().await.mark_delivery_confirmed(command_id) {
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
        if let Err(e) = store.outbox().lock().await.mark_projection_committed(command_id) {
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
        if let Err(e) = store.outbox().lock().await.mark_intent_durable(command_id) {
            warn!(chat_id, error = ?e, "mark_intent_durable failed");
            return;
        }
        // 1. CAS（§7.4 规则 4：pending → resolved 原子一次）。
        match self
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
            _ => {}
        }
        // 2. 翻译 + 下发（L1+L2）。
        let Some(entry) = self.inner.chats.entry(chat_id).await else {
            return;
        };
        let Some(acp_session_id) = entry.session_id.clone() else {
            return;
        };
        let instance_id = entry.instance_id.clone();
        let msg = match self.inner.translator.translate(
            &cmd.action,
            &OutboundCtx {
                cwd: self.inner.default_cwd.clone(),
                acp_session_id,
                // cancel/resolve 方法面无 turnId（§4.3 表），占位不注入。
                turn_id: String::new(),
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
        if let Err(e) = self.inner.instance.forward_rpc(&instance_id, chat_id, &msg).await {
            self.fail_retryable(chat_id, command_id, cmd, instance_error_code(&e), "forward failed")
                .await;
            return;
        }
        if let Err(e) = store.outbox().lock().await.mark_dispatched(command_id, Utc::now()) {
            warn!(chat_id, error = ?e, "mark_dispatched failed");
            return;
        }
        // 3. L3。
        match tokio::time::timeout(self.inner.l3_timeout, rx).await {
            Ok(Ok(r)) if r.get("error").is_none() => {
                if let Err(e) = store.outbox().lock().await.mark_delivery_confirmed(command_id) {
                    warn!(chat_id, error = ?e, "mark_delivery_confirmed failed");
                    return;
                }
                if let Err(e) = store.outbox().lock().await.mark_projection_committed(command_id) {
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
                let _ = store.outbox().lock().await.mark_delivery_unknown(command_id);
                self.send_error(
                    cmd,
                    ErrorCode::AgentUnavailable,
                    "delivery unknown; automatic retry not permitted (path B)",
                    false,
                )
                .await;
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
        let _ = self.inner.chats.transition(chat_id, ChatState::Closed).await;
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
        let _ = store.outbox().lock().await.clear_for_retry(command_id);
        let retryable = code.default_retryable();
        self.send_error(cmd, code, message, retryable).await;
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
        self.send_error(cmd, code, message, false).await;
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
        let _ = cmd
            .tx
            .send(OutboundMsg::Frame(Frame::ActionError(action_error(
                command_id, code, message, retryable,
            ))))
            .await;
    }

    async fn send_committed(&self, cmd: &ExecCmd, turn_id: Option<&str>, chat_id: Option<&str>) {
        let command_id = extract_command_id(&cmd.action).unwrap_or_default();
        let _ = cmd
            .tx
            .send(OutboundMsg::Frame(Frame::ActionAck(ActionAck {
                command_id,
                status: AckStatus::Committed,
                turn_id: turn_id.map(str::to_string),
                chat_id: chat_id.map(str::to_string),
                committed_projection_version: None,
            })))
            .await;
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn action_error(command_id: String, code: ErrorCode, message: &str, retryable: bool) -> ActionError {
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
        ActionEnvelope::Create { command_id, .. }
        | ActionEnvelope::Load { command_id, .. }
        | ActionEnvelope::Close { command_id, .. }
        | ActionEnvelope::Prompt { command_id, .. }
        | ActionEnvelope::Cancel { command_id, .. }
        | ActionEnvelope::ResolvePermission { command_id, .. }
        | ActionEnvelope::SubscribeEvents { command_id, .. }
        | ActionEnvelope::UnsubscribeEvents { command_id, .. } => Some(command_id.clone()),
    }
}

fn extract_chat_id(action: &ActionEnvelope) -> Option<String> {
    match action {
        ActionEnvelope::Prompt { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::Cancel { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::Close { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::ResolvePermission { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::Load { payload, .. } => Some(payload.chat_id.clone()),
        ActionEnvelope::Create { .. } => None,
        ActionEnvelope::SubscribeEvents { .. } | ActionEnvelope::UnsubscribeEvents { .. } => None,
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
        _ => DedupVerdict::Duplicate,
    }
}

enum DedupVerdict {
    Duplicate,
    RedeliverFailed,
    RedeliverUnknown,
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
        "PAYLOAD_TOO_LARGE" => ErrorCode::PayloadTooLarge,
        _ => ErrorCode::InvalidState,
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

#[cfg(test)]
#[path = "command_coordinator_test.rs"]
mod command_coordinator_test;
