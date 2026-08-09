//! DocManager：唯一提交边界（§5.6）+ 每 chat 单写者（§7.4）+ 16ms 微批次
//! （§6.4）+ 广播 + 唯一提交边界。
//!
//! 所有 Y.Doc 写入（聚合投影、控制面状态迁移、权限 CAS、定时器、Registry
//! 更新）都必须经 DocManager 的进程内单写通道；任何路径不得绕过 DocManager
//! 直写 yrs（§6.5）。yrs `transact_mut()` 并发 panic 由每 chat 单写者排除。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, trace, warn};

use acp_hub_proto::conn::DocId;
use acp_hub_proto::schema::{
    ActiveTurnProjection, EntryStatus, InstanceStatus, ChatStatus, ChatSummary, TurnStatus,
};
use yrs::{Map, ReadTxn, StateVector, Transact, WriteTxn};

use crate::state::aggregator::{Aggregator, ApplyReason, ApplyResult};
use crate::state::chat_writer;
use crate::state::doc_pair::DocPair;
use crate::state::factory::Factory;
use crate::state::normalized::{EventBody, NormalizedEvent};
use crate::state::permission::{self, CasOutcome};
use crate::state::registry::{
    DegradeCause, RegistryApplier, RegistryMsg, RegistryState,
};

/// 微批次/队列参数（§6.4/§8.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchConfig {
    /// 微批次窗口（§6.4 默认 16ms）。
    pub batch_window: Duration,
    /// 增量字节阈值（【决策】默认 4KB；与 §14 开放问题 2 的 4KB 截断对齐）。
    pub batch_bytes: usize,
    /// 每 chat 命令队列上限（§8.6 默认 64）。
    pub chat_queue: usize,
}

impl Default for BatchConfig {
    fn default() -> Self {
        BatchConfig {
            batch_window: Duration::from_millis(16),
            batch_bytes: 4096,
            chat_queue: 64,
        }
    }
}

/// 提交产物：update 的消费者（persist 实现落盘 + 归档；§8.4 提交点纪律）。
///
/// 【决策】trait 而非具体类型——F6 提供实现；DocManager 只要求「落盘完成后再
/// 应答」。
#[async_trait]
pub trait UpdateSink: Send + Sync {
    /// 落盘单个 Doc 的 update（per-commit fsync 默认由 F6 负责）；返回落盘结果。
    async fn persist_update(&self, doc: DocId, update: Vec<u8>) -> Result<(), PersistError>;
}

/// persist 错误（F6 返回；内容脱敏，不携带正文/路径细节）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("persist error: {0}")]
pub struct PersistError(pub String);

/// 广播载荷（§4.2 `ysync.update` 的素材；背压/合并/跳过属 broadcaster，F7）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocUpdate {
    pub doc: DocId,
    pub update: Vec<u8>,
}

/// 提交结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitResult {
    /// 已应用（含 applied=false 的幂等/守卫拒绝——调用方按 reason 处理）。
    Applied(ApplyResult),
    /// 队列满 / chat 已关闭等。
    Rejected(SubmitError),
    /// 落盘失败（F6 persist 错误；§17.2 degraded 输入）。
    PersistFailed,
}

/// 提交拒绝原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    /// 队列满（RATE_LIMITED 语义，§8.6）。
    QueueFull,
    /// chat 不存在 / 已关闭（CHAT_NOT_FOUND 语义）。
    ChatNotFound,
    /// 写者通道已关闭。
    ChannelClosed,
}

/// `yrs::Subscription` 的 Send 包装。
///
/// yrs 在非 `sync` feature 下未声明 `Subscription: Send`，但其内部字段
/// （`Origin(SmallVec<[u8; 8]>)` + `Weak<RefCell<Vec<Origin>>>`）均为 `Send`；
/// 按实际布局包装使 writer task 可跨 await 持有观察句柄。Drop 即退订。
#[allow(dead_code)] // 观察句柄：存活即注册，drop 即退订
struct SendSubscription(Option<yrs::Subscription>);

// SAFETY: 字段均为 Send（见上）。
unsafe impl Send for SendSubscription {}

/// DocManager 生命周期错误（open/close）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocManagerError {
    /// chat 不存在。
    #[error("chat not found: {0}")]
    ChatNotFound(String),
    /// 通道关闭。
    #[error("chat writer closed")]
    ChannelClosed,
}

/// 控制路径写入命令（§5.6「控制面状态迁移如 cancelling/interrupted/decision/
/// 标题、定时器 CAS」全部经此）。
#[derive(Debug, Clone, PartialEq)]
pub enum DocCommand {
    /// 服务端单写用户消息注册（§6.5；幂等：同 turn_id 跳过）。
    RegisterUserEntry {
        turn_id: String,
        entry_id: String,
        text: String,
        author_user_id: Option<String>,
        created_at: String,
    },
    /// 权限 CAS：resolve（pending → resolved 原子一次；§7.4 规则 4）。
    ResolvePermission {
        permission_id: String,
        decision: acp_hub_proto::action::PermissionDecision,
    },
    /// 权限 CAS：expire（pending → expired；定时器路径，§4.7）。
    ExpirePermission { permission_id: String },
    /// 断链 → 活动 turn 置 interrupted（§7.3 分区恢复；turn 级终态）。
    MarkTurnInterrupted { turn_id: String },
    /// 控制面 turn 终态（§7.2）：active_turn 匹配且非终态 → 终态迁移 +
    /// assistant entry 迁移。等价聚合器 TurnTerminal 事件分支，但走控制面
    /// （不经聚合器 seq 水位——宿主注入无 instance 流 seq）。
    SetTurnTerminal {
        turn_id: String,
        status: TurnStatus,
        completed_at: String,
    },
    /// 标题更新（§7.4 规则 5：可独立排队，仍经服务端命令写入）。
    UpdateTitle { title: String },
    /// 旧 turn 未完成时新 prompt 的裁决（§6.4：旧 assistant entry 置 cancelled，
    /// 不发 ACP cancel）。
    CancelStaleAssistantEntry { turn_id: String, entry_id: String },
    /// chat 级终态（ended/closed/crashed，§7.3）写视图。
    SetChatTerminal { status: ChatStatus },
    /// Registry：活跃 chat 摘要 upsert/移除/gap 同步（§12.4）。
    RegistryUpsertChat(ChatSummary),
    RegistryRemoveChat { chat_id: String },
    /// Registry：instance 视图与全局状态（§12.4/§12.5）。
    RegistryUpsertInstance(acp_hub_proto::schema::InstanceView),
    RegistrySetInstanceState {
        instance_id: String,
        status: InstanceStatus,
    },
    RegistrySetGlobal { status: acp_hub_proto::schema::GlobalStatus },
}

impl DocCommand {
    /// 是否为 Registry 系命令（路由到全局 registry 写者，§8.5【决策】）。
    fn is_registry(&self) -> bool {
        matches!(
            self,
            DocCommand::RegistryUpsertChat(_)
                | DocCommand::RegistryRemoveChat { .. }
                | DocCommand::RegistryUpsertInstance(_)
                | DocCommand::RegistrySetInstanceState { .. }
                | DocCommand::RegistrySetGlobal { .. }
        )
    }
}

/// 每 chat 写者句柄（§7.4 单写者）。
struct ChatHandle {
    tx: mpsc::Sender<ChatMsg>,
    /// 入队未消费计数（§7.4 规则 6：入队检查与 in_flight 标记同一临界区）。
    inflight: Arc<AtomicUsize>,
    writer: JoinHandle<()>,
}

/// 写者通道消息（§8.2）。
enum ChatMsg {
    /// 事件（聚合路径）；挂 oneshot 的调用方 await 落盘确认（§8.2 提交点纪律）。
    Event(NormalizedEvent, Option<oneshot::Sender<SubmitResult>>),
    /// 命令（控制路径）。
    Command(DocCommand, Option<oneshot::Sender<SubmitResult>>),
    /// 关闭：writer 完成在途批次后退出。
    Shutdown(oneshot::Sender<()>),
}

/// 唯一提交边界（§5.6）。
pub struct DocManager {
    chats: RwLock<HashMap<String, ChatHandle>>,
    registry_tx: mpsc::Sender<RegistryMsg>,
    cfg: BatchConfig,
    sink: Arc<dyn UpdateSink>,
    factory: Factory,
    /// 广播订阅者（unbounded；背压在下游 broadcaster，§6.4）。
    broadcast: Arc<RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>>,
}

impl DocManager {
    /// 创建 DocManager：spawn 全局 Registry 写者（Registry Doc 也是 Doc，受
    /// 唯一提交边界约束，§5.6）。Registry Doc 以全新结构起步。
    pub fn new(cfg: BatchConfig, sink: Arc<dyn UpdateSink>) -> Self {
        Self::with_recovered_registry(cfg, sink, None)
    }

    /// 同 [`Self::new`]，但可注入恢复后的 Registry Doc（§8.4.1：server 重启
    /// 时从 registry.log 重放，`registry.log` 非空时必须注入——否则 writer
    /// 以全新 client 重写结构，与镜像中历史结构的 CRDT LWW 冲突，后续增量
    /// 引用被覆盖的结构而不可见）。
    ///
    /// 注入的 doc 应已应用 registry.log 全部记录（历史 client 的结构/数据）；
    /// writer 后续增量以新 client 写入业务键，与历史键无冲突。
    pub fn with_recovered_registry(
        cfg: BatchConfig,
        sink: Arc<dyn UpdateSink>,
        recovered_registry: Option<yrs::Doc>,
    ) -> Self {
        let factory = Factory::new();
        let registry_doc = recovered_registry.unwrap_or_else(|| factory.create_registry_doc());
        let (registry_tx, registry_rx) = mpsc::channel::<RegistryMsg>(cfg.chat_queue);
        // Registry 写者：即到即写（§8.5），无微批次。
        let broadcast: Arc<RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>> =
            Arc::new(RwLock::new(vec![]));
        {
            let broadcast = broadcast.clone();
            let sink = sink.clone();
            tokio::spawn(registry_writer_loop(registry_doc, registry_rx, sink, broadcast));
        }
        DocManager {
            chats: RwLock::new(HashMap::new()),
            registry_tx,
            cfg,
            sink,
            factory,
            broadcast,
        }
    }

    /// Registry 状态源单写句柄（channel 层 instance 生命周期 / 恢复流程使用）。
    pub fn registry(&self) -> RegistryState {
        RegistryState::new(self.registry_tx.clone())
    }

    /// 打开 chat：Factory 创建双 Doc + ensure_schema（补结构）→ spawn writer
    /// task → RegistryState 写活跃摘要（§12.4）。重复打开按幂等处理（返回现有
    /// 句柄）。
    pub async fn open_chat(
        &self,
        chat_id: &str,
        instance_id: &str,
        title: Option<&str>,
    ) -> Result<(), DocManagerError> {
        {
            let chats = self.chats.read().await;
            if chats.contains_key(chat_id) {
                return Ok(());
            }
        }
        let pair = self.factory.create_chat_doc();
        let (tx, rx) = mpsc::channel::<ChatMsg>(self.cfg.chat_queue);
        let inflight = Arc::new(AtomicUsize::new(0));
        let broadcast = self.broadcast.clone();
        let sink = self.sink.clone();
        let registry = self.registry();
        let cfg = self.cfg;
        let chat_id = chat_id.to_string();
        let instance_id = instance_id.to_string();
        let title = title.map(|s| s.to_string());

        let writer = tokio::spawn(chat_writer_loop(
            chat_id.clone(),
            rx,
            pair,
            cfg,
            sink,
            broadcast,
            registry.clone(),
        ));

        self.chats.write().await.insert(
            chat_id.clone(),
            ChatHandle {
                tx,
                inflight,
                writer,
            },
        );

        // Registry 活跃摘要（§12.4 create 更新）。
        let summary = ChatSummary {
            id: chat_id.clone(),
            instance_id,
            title: title.unwrap_or_default(),
            status: "accepting".to_string(),
            gap: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(e) = registry.upsert_chat(summary).await {
            tracing::warn!(chat_id, error = ?e, "registry upsert chat failed");
        }
        debug!(chat_id, "chat opened");
        Ok(())
    }

    /// 同步入队检查（§7.4 同一临界区）：调用方在 outbox 去重索引更新前调用，
    /// 返回 false 表示队列满（RATE_LIMITED）或 chat 不存在（CHAT_NOT_FOUND）。
    pub async fn try_reserve(&self, chat_id: &str) -> bool {
        let chats = self.chats.read().await;
        let Some(handle) = chats.get(chat_id) else {
            return false;
        };
        handle
            .inflight
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                if n < self.cfg.chat_queue {
                    Some(n + 1)
                } else {
                    None
                }
            })
            .is_ok()
    }

    /// 释放 `try_reserve` 占用的名额（§7.4 配对：谁 reserve 谁 release）。
    ///
    /// 由调用方（command-coordinator 执行器）在命令消费完成后调用；
    /// 也用于 submit 内部 try_reserve 之后的失败路径补偿。
    pub async fn release_reserve(&self, chat_id: &str) {
        let chats = self.chats.read().await;
        if let Some(handle) = chats.get(chat_id) {
            handle.inflight.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// 聚合路径（F5 ACPChannel 产物 / 补推流）：经该 chat 写者应用。
    ///
    /// 应答语义（§8.2「需要应答的提交挂 oneshot」）：
    /// - delta 类事件（MessageDelta/ReasoningDelta）进入 16ms 微批次，**不挂
    ///   落盘应答**——入队即返回 `Applied`（落盘随批次 flush，§8.3）；调用方
    ///   不得把该返回值当作「已落盘」；
    /// - 控制类事件挂 oneshot，writer 应用 + flush + 落盘确认后回填
    ///   `SubmitResult`（提交点纪律 §4.4「投影 user entry → committed Ack」）。
    pub async fn submit_event(&self, ev: NormalizedEvent) -> SubmitResult {
        let chat_id = ev.chat_id.clone();
        let chats = self.chats.read().await;
        let Some(handle) = chats.get(&chat_id) else {
            return SubmitResult::Rejected(SubmitError::ChatNotFound);
        };
        // delta 类：入队即返回（§8.2 微批次不逐事件应答；挂 reply 会使调用方
        // 在窗口内阻塞至 flush，破坏流式语义）。
        if is_batchable(&ev.body) {
            if handle
                .tx
                .send(ChatMsg::Event(ev, None))
                .await
                .is_err()
            {
                handle.inflight.fetch_sub(1, Ordering::SeqCst);
                return SubmitResult::Rejected(SubmitError::ChannelClosed);
            }
            return SubmitResult::Applied(ApplyResult {
                applied: true,
                reason: None,
            });
        }
        let (reply, rx) = oneshot::channel();
        if handle
            .tx
            .send(ChatMsg::Event(ev, Some(reply)))
            .await
            .is_err()
        {
            handle.inflight.fetch_sub(1, Ordering::SeqCst);
            return SubmitResult::Rejected(SubmitError::ChannelClosed);
        }
        rx.await.unwrap_or(SubmitResult::Rejected(SubmitError::ChannelClosed))
    }

    /// 控制路径（F7 command-coordinator / 定时器）：注册 user entry、权限 CAS、
    /// 标题更新、断链 interrupted、gap 同步、Registry 更新等（§8.5 DocCommand
    /// 表）。
    ///
    /// Registry 系命令路由到全局 registry 写者（§8.5【决策】）；其余命令经
    /// `chat_id` 路由到对应 chat 写者。
    pub async fn submit_command(&self, chat_id: &str, cmd: DocCommand) -> SubmitResult {
        if cmd.is_registry() {
            return self.submit_registry_command(cmd).await;
        }
        let chats = self.chats.read().await;
        let Some(handle) = chats.get(chat_id) else {
            return SubmitResult::Rejected(SubmitError::ChatNotFound);
        };
        let (reply, rx) = oneshot::channel();
        if handle
            .tx
            .send(ChatMsg::Command(cmd, Some(reply)))
            .await
            .is_err()
        {
            handle.inflight.fetch_sub(1, Ordering::SeqCst);
            return SubmitResult::Rejected(SubmitError::ChannelClosed);
        }
        rx.await.unwrap_or(SubmitResult::Rejected(SubmitError::ChannelClosed))
    }

    async fn submit_registry_command(&self, cmd: DocCommand) -> SubmitResult {
        let (reply, rx) = oneshot::channel();
        if self
            .registry_tx
            .send(RegistryMsg::Command(cmd, reply))
            .await
            .is_err()
        {
            return SubmitResult::Rejected(SubmitError::ChannelClosed);
        }
        match rx.await {
            Ok(Ok(_)) => SubmitResult::Applied(ApplyResult {
                applied: true,
                reason: None,
            }),
            Ok(Err(e)) => {
                tracing::warn!(error = ?e, "registry command failed");
                SubmitResult::Rejected(SubmitError::ChannelClosed)
            }
            Err(_) => SubmitResult::Rejected(SubmitError::ChannelClosed),
        }
    }

    /// 关闭 chat：写者 drain 后退出；Doc 保留（终态视图供历史查看，§8.2）。
    pub async fn close_chat(&self, chat_id: &str) -> Result<(), DocManagerError> {
        let handle = {
            let mut chats = self.chats.write().await;
            chats.remove(chat_id)
        };
        let Some(handle) = handle else {
            return Err(DocManagerError::ChatNotFound(chat_id.to_string()));
        };
        let (reply, rx) = oneshot::channel();
        let _ = handle.tx.send(ChatMsg::Shutdown(reply)).await;
        let _ = rx.await;
        let _ = handle.writer.await;
        // Registry 活跃摘要移除（§12.4 close 清理）。
        let registry = self.registry();
        if let Err(e) = registry.remove_chat(chat_id).await {
            tracing::warn!(chat_id, error = ?e, "registry remove chat failed");
        }
        debug!(chat_id, "chat closed");
        Ok(())
    }

    /// 广播订阅（unbounded）：broadcaster（F7）消费做背压与 fan-out。
    /// 每次调用返回新的 receiver；发送方（writer）广播给全部订阅者。
    pub async fn subscribe_updates(&self) -> mpsc::UnboundedReceiver<DocUpdate> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.broadcast.write().await.push(tx);
        rx
    }
}

/// 每 chat 写者循环（§7.4 单写者：`&mut DocPair` 独占；§8.3 微批次）。
async fn chat_writer_loop(
    chat_id: String,
    mut rx: mpsc::Receiver<ChatMsg>,
    mut pair: DocPair,
    cfg: BatchConfig,
    sink: Arc<dyn UpdateSink>,
    broadcast: Arc<RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>>,
    registry: RegistryState,
) {
    // 观察回调：update 经 unbounded channel 送出（§6.4 同步回调不能 await）。
    let (chat_update_tx, mut chat_updates) = mpsc::unbounded_channel::<Vec<u8>>();
    let (control_update_tx, mut control_updates) = mpsc::unbounded_channel::<Vec<u8>>();
    let _sub_chat = SendSubscription(Some(
        pair.chat
            .observe_update_v1(move |_, e| {
                let _ = chat_update_tx.send(e.update.clone());
            })
            .unwrap_or_else(|e| panic!("chat observe_update failed: {e}")),
    ));
    let _sub_control = SendSubscription(Some(
        pair.control
            .observe_update_v1(move |_, e| {
                let _ = control_update_tx.send(e.update.clone());
            })
            .unwrap_or_else(|e| panic!("control observe_update failed: {e}")),
    ));

    // 初始化全量基线落盘（chat → control 顺序，§6.4）：Factory 结构初始化
    // 发生在 observe 订阅之前（open_chat），其 update 不会经回调产生——
    // 不下发则镜像（StoreSink）与 update 日志缺少 doc 基线，后续增量（pv
    // 覆盖写等带 origin 的更新）无法应用。全量基线 + 增量幂等（重复应用
    // 无害）。
    {
        let init_chat = pair
            .chat
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        if let Err(e) = sink.persist_update(DocId::chat(&chat_id), init_chat).await {
            warn!(chat_id, error = ?e, "chat init baseline persist failed");
        }
        let init_control = pair
            .control
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        if let Err(e) = sink.persist_update(DocId::control(&chat_id), init_control).await {
            warn!(chat_id, error = ?e, "control init baseline persist failed");
        }
    }

    let mut agg = Aggregator;
    // 微批次缓冲：仅 delta 类事件（§8.3）。
    let mut batch: Vec<(NormalizedEvent, Option<oneshot::Sender<SubmitResult>>)> = Vec::new();
    let mut batch_bytes = 0usize;
    let mut interval = tokio::time::interval(cfg.batch_window);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // interval 第一次 tick 立即就绪；消费掉以让窗口从此刻开始计时（§8.3）。
    interval.tick().await;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                flush_batch(&chat_id, &mut pair, &mut agg, &mut batch, &mut batch_bytes,
                    &mut chat_updates, &mut control_updates, &sink, &broadcast, &registry).await;
            }
            msg = rx.recv() => match msg {
                Some(ChatMsg::Event(ev, reply)) => {
                    if is_batchable(&ev.body) {
                        batch_bytes += estimate_bytes(&ev);
                        batch.push((ev, reply));
                        if batch_bytes >= cfg.batch_bytes {
                            flush_batch(&chat_id, &mut pair, &mut agg, &mut batch, &mut batch_bytes,
                                &mut chat_updates, &mut control_updates, &sink, &broadcast, &registry).await;
                        }
                    } else {
                        // 控制类先 flush（§6.4：状态不倒退）。
                        flush_batch(&chat_id, &mut pair, &mut agg, &mut batch, &mut batch_bytes,
                            &mut chat_updates, &mut control_updates, &sink, &broadcast, &registry).await;
                        let result = apply_event(&chat_id, &mut pair, &mut agg, &ev,
                            &mut chat_updates, &mut control_updates, &sink, &broadcast, &registry).await;
                        if let Some(r) = reply {
                            let _ = r.send(result);
                        }
                    }
                }
                Some(ChatMsg::Command(cmd, reply)) => {
                    // 控制类先 flush（§6.4）。
                    flush_batch(&chat_id, &mut pair, &mut agg, &mut batch, &mut batch_bytes,
                        &mut chat_updates, &mut control_updates, &sink, &broadcast, &registry).await;
                    let result = apply_command(&chat_id, &mut pair, cmd,
                        &mut chat_updates, &mut control_updates, &sink, &broadcast, &registry).await;
                    if let Some(r) = reply {
                        let _ = r.send(result);
                    }
                }
                Some(ChatMsg::Shutdown(reply)) => {
                    flush_batch(&chat_id, &mut pair, &mut agg, &mut batch, &mut batch_bytes,
                        &mut chat_updates, &mut control_updates, &sink, &broadcast, &registry).await;
                    let _ = reply.send(());
                    break;
                }
                None => break,
            }
        }
    }
    debug!(chat_id, "chat writer exited");
}

fn is_batchable(body: &EventBody) -> bool {
    matches!(body, EventBody::MessageDelta { .. } | EventBody::ReasoningDelta { .. })
}

fn estimate_bytes(ev: &NormalizedEvent) -> usize {
    match &ev.body {
        EventBody::MessageDelta { text, .. } | EventBody::ReasoningDelta { text, .. } => {
            64 + text.len()
        }
        _ => 128,
    }
}

/// 批次 flush（§8.3）：delta 合并为一次 chat 事务；gap 上报；回填应答。
#[allow(clippy::too_many_arguments)]
async fn flush_batch(
    chat_id: &str,
    pair: &mut DocPair,
    agg: &mut Aggregator,
    batch: &mut Vec<(NormalizedEvent, Option<oneshot::Sender<SubmitResult>>)>,
    batch_bytes: &mut usize,
    chat_updates: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    control_updates: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    sink: &Arc<dyn UpdateSink>,
    broadcast: &Arc<RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>>,
    registry: &RegistryState,
) {
    if batch.is_empty() {
        return;
    }
    let evs: Vec<NormalizedEvent> = batch.iter().map(|(ev, _)| ev.clone()).collect();
    let results = agg.apply_batch(pair, &evs);
    let persisted = persist_and_broadcast(
        chat_id,
        chat_updates,
        control_updates,
        sink,
        broadcast,
        registry,
    )
    .await;
    // gap 上报（§9.4/§12.4）。
    report_gap(chat_id, pair, registry).await;
    let replies: Vec<Option<oneshot::Sender<SubmitResult>>> =
        batch.drain(..).map(|(_, r)| r).collect();
    for (reply, result) in replies.into_iter().zip(results) {
        if let Some(r) = reply {
            let _ = r.send(if persisted {
                SubmitResult::Applied(result)
            } else {
                SubmitResult::PersistFailed
            });
        }
    }
    *batch_bytes = 0;
    debug!(
        chat_id,
        events = evs.len(),
        applied = evs.len(),
        "batch flushed"
    );
}

/// 持久化 + 广播当前事务产生的增量 update（顺序 chat → control，§6.4）。
/// 返回是否全部落盘成功。
#[allow(clippy::too_many_arguments)]
async fn persist_and_broadcast(
    chat_id: &str,
    chat_updates: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    control_updates: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    sink: &Arc<dyn UpdateSink>,
    broadcast: &Arc<RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>>,
    registry: &RegistryState,
) -> bool {
    let mut all_ok = true;
    // chat 事务先提交 → update 先到（§6.4 固定顺序）。
    while let Ok(update) = chat_updates.try_recv() {
        let doc = DocId::chat(chat_id);
        if sink.persist_update(doc.clone(), update.clone()).await.is_err() {
            all_ok = false;
            let _ = registry.report_condition(DegradeCause::PersistFailure).await;
            warn!(chat_id, "chat update persist failed");
        }
        broadcast_send(broadcast, DocUpdate { doc, update }).await;
    }
    while let Ok(update) = control_updates.try_recv() {
        let doc = DocId::control(chat_id);
        if sink.persist_update(doc.clone(), update.clone()).await.is_err() {
            all_ok = false;
            let _ = registry.report_condition(DegradeCause::PersistFailure).await;
            warn!(chat_id, "control update persist failed");
        }
        broadcast_send(broadcast, DocUpdate { doc, update }).await;
    }
    all_ok
}

async fn broadcast_send(
    broadcast: &Arc<RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>>,
    update: DocUpdate,
) {
    let senders = broadcast.read().await;
    for tx in senders.iter() {
        // unbounded：send 不阻塞（背压在下游 broadcaster，§6.4）。
        let _ = tx.send(update.clone());
    }
}

/// gap 上报：stream.gap_dirty → Registry chats[].gap 写回（§9.4/§12.4）。
async fn report_gap(chat_id: &str, pair: &mut DocPair, registry: &RegistryState) {
    if !pair.stream.gap_dirty {
        return;
    }
    pair.stream.gap_dirty = false;
    let gap = if pair.stream.gap_count > 0 || pair.stream.uncalibratable {
        Some(pair.stream.gap_count)
    } else {
        None
    };
    trace!(chat_id, gap_count = pair.stream.gap_count, uncalibratable = pair.stream.uncalibratable, "gap report");
    if let Err(e) = registry.set_chat_gap(chat_id, gap).await {
        // 上报失败不阻塞主流程；Registry 状态源会在下次机会补报。
        warn!(chat_id, error = ?e, "gap report to registry failed");
    }
    // gap 存在 → ChatGap degraded 条件（§17.2）；追平 → 清除。
    if gap.is_some() {
        let _ = registry.report_condition(DegradeCause::ChatGap).await;
    } else {
        let _ = registry.clear_condition(DegradeCause::ChatGap).await;
    }
}

/// 单事件应用（控制类路径）：apply → 持久化 → 广播。
#[allow(clippy::too_many_arguments)]
async fn apply_event(
    chat_id: &str,
    pair: &mut DocPair,
    agg: &mut Aggregator,
    ev: &NormalizedEvent,
    chat_updates: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    control_updates: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    sink: &Arc<dyn UpdateSink>,
    broadcast: &Arc<RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>>,
    registry: &RegistryState,
) -> SubmitResult {
    let result = agg.apply(pair, ev);
    let ok = persist_and_broadcast(
        chat_id,
        chat_updates,
        control_updates,
        sink,
        broadcast,
        registry,
    )
    .await;
    report_gap(chat_id, pair, registry).await;
    trace!(chat_id, seq = ev.seq, kind = ev.kind(), applied = result.applied, reason = ?result.reason, "event applied");
    if !ok {
        SubmitResult::PersistFailed
    } else {
        SubmitResult::Applied(result)
    }
}

/// 命令应用（§8.5 DocCommand 表）。
#[allow(clippy::too_many_arguments)]
async fn apply_command(
    chat_id: &str,
    pair: &mut DocPair,
    cmd: DocCommand,
    chat_updates: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    control_updates: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    sink: &Arc<dyn UpdateSink>,
    broadcast: &Arc<RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>>,
    registry: &RegistryState,
) -> SubmitResult {
    let result = match &cmd {
        DocCommand::RegisterUserEntry {
            turn_id,
            entry_id,
            text,
            author_user_id,
            created_at,
        } => {
            let mut txn = pair.chat_txn();
            let root = txn.get_or_insert_map(crate::state::factory::ROOT);
            let created = chat_writer::create_user_entry(
                &mut txn,
                &root,
                turn_id,
                entry_id,
                text,
                author_user_id.as_deref(),
                created_at,
            );
            let mut applied = ApplyResult {
                applied: true,
                reason: None,
            };
            if !created {
                applied = ApplyResult {
                    applied: false,
                    reason: Some(ApplyReason::DuplicateIdempotent),
                };
            }
            chat_writer::bump_projection_version(&mut txn, &root);
            drop(txn);
            // control 侧：active_turn 注册（§7.2 accepting）。
            let mut txn = pair.control_txn();
            let root = txn.get_or_insert_map(crate::state::factory::ROOT);
            let active = ActiveTurnProjection {
                turn_id: turn_id.clone(),
                turn_status: TurnStatus::Accepting,
                updated_at: created_at.clone(),
            };
            chat_writer::set_active_turn(&mut txn, &root, Some(&active));
            chat_writer::bump_projection_version(&mut txn, &root);
            applied
        }
        DocCommand::ResolvePermission {
            permission_id,
            decision,
        } => match permission::resolve(pair, permission_id, *decision) {
            CasOutcome::Migrated => {
                bump_control_projection(pair);
                ApplyResult {
                    applied: true,
                    reason: None,
                }
            }
            CasOutcome::Duplicate => ApplyResult {
                applied: false,
                reason: Some(ApplyReason::DuplicateIdempotent),
            },
            CasOutcome::Expired => ApplyResult {
                applied: false,
                reason: None,
            },
            CasOutcome::Unknown => ApplyResult {
                applied: false,
                reason: Some(ApplyReason::UnknownPermission),
            },
        },
        DocCommand::ExpirePermission { permission_id } => {
            match permission::expire(pair, permission_id) {
                CasOutcome::Migrated => {
                    bump_control_projection(pair);
                    ApplyResult {
                        applied: true,
                        reason: None,
                    }
                }
                CasOutcome::Duplicate => ApplyResult {
                    applied: false,
                    reason: Some(ApplyReason::DuplicateIdempotent),
                },
                CasOutcome::Expired => ApplyResult {
                    applied: false,
                    reason: None,
                },
                CasOutcome::Unknown => ApplyResult {
                    applied: false,
                    reason: Some(ApplyReason::UnknownPermission),
                },
            }
        }
        DocCommand::MarkTurnInterrupted { turn_id } => {
            // 读 active_turn：匹配且非终态 → 置 interrupted（§7.3）。
            let should_interrupt = {
                let txn = pair.control.transact();
                chat_writer::root_map_read(&txn)
                    .and_then(|root| root.get(&txn, "active_turn"))
                    .and_then(|v| v.cast::<yrs::MapRef>().ok())
                    .map(|m| {
                        let tid = m
                            .get(&txn, "turn_id")
                            .and_then(|t| t.cast::<String>().ok());
                        let status = m
                            .get(&txn, "turn_status")
                            .and_then(|t| t.cast::<String>().ok())
                            .unwrap_or_default();
                        (tid.as_deref() == Some(turn_id.as_str())) && !is_terminal_turn(&status)
                    })
                    .unwrap_or(false)
            };
            if !should_interrupt {
                ApplyResult {
                    applied: false,
                    reason: Some(ApplyReason::TurnTerminalGuard),
                }
            } else {
                let mut txn = pair.control_txn();
                let root = txn.get_or_insert_map(crate::state::factory::ROOT);
                let active = ActiveTurnProjection {
                    turn_id: turn_id.clone(),
                    turn_status: TurnStatus::Interrupted,
                    updated_at: chrono::Utc::now().to_rfc3339(),
                };
                chat_writer::set_active_turn(&mut txn, &root, Some(&active));
                chat_writer::bump_projection_version(&mut txn, &root);
                drop(txn);
                // chat 侧：assistant entry 置 cancelled（§7.2 中断收敛）。
                let mut txn = pair.chat_txn();
                let root = txn.get_or_insert_map(crate::state::factory::ROOT);
                chat_writer::migrate_entry_terminal(
                    &mut txn,
                    &root,
                    &format!("{turn_id}:assistant"),
                    EntryStatus::Cancelled,
                    &chrono::Utc::now().to_rfc3339(),
                    None,
                );
                chat_writer::bump_projection_version(&mut txn, &root);
                ApplyResult {
                    applied: true,
                    reason: None,
                }
            }
        }
        DocCommand::SetTurnTerminal {
            turn_id,
            status,
            completed_at,
        } => {
            // 终态守卫（§7.2）：active_turn 存在、turn_id 匹配且非终态才迁移。
            let should_terminate = {
                let txn = pair.control.transact();
                chat_writer::root_map_read(&txn)
                    .and_then(|root| root.get(&txn, "active_turn"))
                    .and_then(|v| v.cast::<yrs::MapRef>().ok())
                    .map(|m| {
                        let tid = m
                            .get(&txn, "turn_id")
                            .and_then(|t| t.cast::<String>().ok());
                        let st = m
                            .get(&txn, "turn_status")
                            .and_then(|t| t.cast::<String>().ok())
                            .unwrap_or_default();
                        (tid.as_deref() == Some(turn_id.as_str())) && !is_terminal_turn(&st)
                    })
                    .unwrap_or(false)
            };
            if !should_terminate {
                ApplyResult {
                    applied: false,
                    reason: Some(ApplyReason::TurnTerminalGuard),
                }
            } else {
                // control 侧：active_turn 终态迁移（§7.2）。
                let mut txn = pair.control_txn();
                let root = txn.get_or_insert_map(crate::state::factory::ROOT);
                let active = ActiveTurnProjection {
                    turn_id: turn_id.clone(),
                    turn_status: *status,
                    updated_at: completed_at.clone(),
                };
                chat_writer::set_active_turn(&mut txn, &root, Some(&active));
                chat_writer::bump_projection_version(&mut txn, &root);
                drop(txn);
                // chat 侧：assistant entry 终态迁移（§7.2 状态映射）。
                let entry_status = match status {
                    TurnStatus::Completed => EntryStatus::Completed,
                    TurnStatus::Failed => EntryStatus::Error,
                    TurnStatus::Cancelled | TurnStatus::Interrupted => EntryStatus::Cancelled,
                    _ => EntryStatus::Completed,
                };
                let mut txn = pair.chat_txn();
                let root = txn.get_or_insert_map(crate::state::factory::ROOT);
                chat_writer::migrate_entry_terminal(
                    &mut txn,
                    &root,
                    &format!("{turn_id}:assistant"),
                    entry_status,
                    completed_at,
                    None,
                );
                chat_writer::bump_projection_version(&mut txn, &root);
                ApplyResult {
                    applied: true,
                    reason: None,
                }
            }
        }
        DocCommand::UpdateTitle { title } => {
            let mut txn = pair.control_txn();
            let root = txn.get_or_insert_map(crate::state::factory::ROOT);
            let control = root.get_or_init::<_, yrs::MapRef>(&mut txn, "chat");
            control.insert(&mut txn, "title", title.clone());
            chat_writer::bump_projection_version(&mut txn, &root);
            ApplyResult {
                applied: true,
                reason: None,
            }
        }
        DocCommand::CancelStaleAssistantEntry {
            turn_id,
            entry_id,
        } => {
            let mut txn = pair.chat_txn();
            let root = txn.get_or_insert_map(crate::state::factory::ROOT);
            chat_writer::migrate_entry_terminal(
                &mut txn,
                &root,
                entry_id,
                EntryStatus::Cancelled,
                &chrono::Utc::now().to_rfc3339(),
                None,
            );
            chat_writer::bump_projection_version(&mut txn, &root);
            let _ = turn_id;
            ApplyResult {
                applied: true,
                reason: None,
            }
        }
        DocCommand::SetChatTerminal { status } => {
            let mut txn = pair.control_txn();
            let root = txn.get_or_insert_map(crate::state::factory::ROOT);
            let control = root.get_or_init::<_, yrs::MapRef>(&mut txn, "chat");
            control.insert(
                &mut txn,
                "status",
                crate::state::aggregator::chat_status_str(*status),
            );
            chat_writer::bump_projection_version(&mut txn, &root);
            ApplyResult {
                applied: true,
                reason: None,
            }
        }
        _ => unreachable!("registry 命令已路由到 registry 写者"),
    };

    let ok = persist_and_broadcast(
        chat_id,
        chat_updates,
        control_updates,
        sink,
        broadcast,
        registry,
    )
    .await;
    trace!(chat_id, cmd = command_kind(&cmd), applied = result.applied, "command applied");
    if !ok {
        SubmitResult::PersistFailed
    } else {
        SubmitResult::Applied(result)
    }
}

fn command_kind(cmd: &DocCommand) -> &'static str {
    match cmd {
        DocCommand::RegisterUserEntry { .. } => "register_user_entry",
        DocCommand::ResolvePermission { .. } => "resolve_permission",
        DocCommand::ExpirePermission { .. } => "expire_permission",
        DocCommand::MarkTurnInterrupted { .. } => "mark_turn_interrupted",
        DocCommand::SetTurnTerminal { .. } => "set_turn_terminal",
        DocCommand::UpdateTitle { .. } => "update_title",
        DocCommand::CancelStaleAssistantEntry { .. } => "cancel_stale_assistant_entry",
        DocCommand::SetChatTerminal { .. } => "set_chat_terminal",
        _ => "registry",
    }
}

fn is_terminal_turn(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "interrupted"
    )
}

fn bump_control_projection(pair: &mut DocPair) {
    let mut txn = pair.control_txn();
    let root = txn.get_or_insert_map(crate::state::factory::ROOT);
    chat_writer::bump_projection_version(&mut txn, &root);
}

/// Registry 写者循环（§8.5：即到即写，无微批次；Registry Doc 唯一写者）。
async fn registry_writer_loop(
    doc: yrs::Doc,
    mut rx: mpsc::Receiver<RegistryMsg>,
    sink: Arc<dyn UpdateSink>,
    broadcast: Arc<RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>>,
) {
    let mut applier = RegistryApplier::new(doc.clone());
    // Registry update 观察：经 channel 送出（§6.4 回调不能 await）。
    let (update_tx, mut update_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let _sub = SendSubscription(Some(
        doc.observe_update_v1(move |_, e| {
            let _ = update_tx.send(e.update.clone());
        })
        .unwrap_or_else(|e| panic!("registry observe_update failed: {e}")),
    ));

    // 初始化全量基线落盘（§8.4.1 Doc 补齐/§5.6）：Factory 结构初始化发生在
    // observe 订阅之前，其 update 不会经回调产生——若不下发，镜像（StoreSink）
    // 与 update 日志将缺少 doc 基线，后续增量（pv 覆盖写等带 origin 的更新）
    // 无法应用（yrs 缺依赖进 pending，永不满足）。下发的全量作为基线，后续
    // 增量即可完整应用（yrs 幂等，重复应用无害）。
    {
        let init = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        if let Err(e) = sink.persist_update(DocId::REGISTRY, init).await {
            warn!(error = ?e, "registry init baseline persist failed");
        }
    }

    while let Some(msg) = rx.recv().await {
        match msg {
            RegistryMsg::Command(cmd, reply) => {
                let r = applier.apply(&cmd);
                persist_registry_updates(&mut update_rx, &sink, &broadcast).await;
                let _ = reply.send(r);
            }
            RegistryMsg::SetChatGap {
                chat_id,
                gap,
                reply,
            } => {
                let r = applier.set_chat_gap(&chat_id, gap);
                persist_registry_updates(&mut update_rx, &sink, &broadcast).await;
                let _ = reply.send(r);
            }
            RegistryMsg::SetChatStatus {
                chat_id,
                status,
                reply,
            } => {
                let r = applier.set_chat_status(&chat_id, &status);
                persist_registry_updates(&mut update_rx, &sink, &broadcast).await;
                let _ = reply.send(r);
            }
        }
    }
}

async fn persist_registry_updates(
    update_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    sink: &Arc<dyn UpdateSink>,
    broadcast: &Arc<RwLock<Vec<mpsc::UnboundedSender<DocUpdate>>>>,
) {
    while let Ok(update) = update_rx.try_recv() {
        let doc = DocId::REGISTRY;
        if sink.persist_update(doc.clone(), update.clone()).await.is_err() {
            warn!("registry update persist failed");
        }
        broadcast_send(broadcast, DocUpdate { doc, update }).await;
    }
}
