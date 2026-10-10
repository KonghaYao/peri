//! MessageTranscript v2 — 会话消息权威存储
//!
//! Transcript 是会话全部消息的唯一真相源。核心特性：
//! - **MessageId 寻址**：内部维护 `HashMap<MessageId, usize>` 索引表，O(1) 查找
//! - **只追加优先**：正常 ReAct 循环中消息仅尾部追加，禁止 prepend/中间插入
//! - **Staging 两阶段写入**：AI 消息 + ToolResult 原子提交
//! - **标记代替删除**：`truncated` / `excluded` 标记用于 Compact，消息本体不变
//! - **异步持久化**：append 后通过 unbounded_channel 异步触发 ThreadStore 写入

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use anyhow::anyhow;

use crate::agent::compact_v2::projection::MessageProjectionDirective;
use crate::messages::{BaseMessage, MessageContent, MessageId};
use crate::thread::ThreadId;

use peri_acp_types::session_resources::{RewindBoundary, SessionResources};
use peri_acp_types::store::history;
use peri_acp_types::store::{MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::{
    encode_system_reminder, reminder_egress_allowed, ReminderAudience, TrustedSystemReminder,
};

use persistence::{PersistenceBudget, Reservation};

/// 待持久化积压默认上限（条数）。
///
/// 覆盖 channel 队列 + writer 待批量 + in-flight 批次三处；阈值是保守上界，
/// 由 F 阶段按真实后端测量后校准（不得为了跑通而放宽到「等于没有界」）。
pub const DEFAULT_PENDING_MAX_ITEMS: usize = 1024;

/// 待持久化积压默认上限（字节，按 canonical payload 编码长度估算）。
pub const DEFAULT_PENDING_MAX_BYTES: usize = 16 * 1024 * 1024;

// The command interceptor retains a clone while its cancellable pipeline owns the
// transcript, so dropping that future cannot erase the persistence outcome.
#[derive(Debug, Clone, Default)]
pub struct CompactionCommitState(Arc<AtomicU8>);

impl CompactionCommitState {
    pub fn is_uncertain(&self) -> bool {
        self.0.load(Ordering::SeqCst) == 1
    }

    pub fn has_committed(&self) -> bool {
        self.0.load(Ordering::SeqCst) == 2
    }

    fn mark_pending(&self) {
        self.0.store(1, Ordering::SeqCst);
    }

    fn mark_committed(&self) {
        self.0.store(2, Ordering::SeqCst);
    }
}

// ─── TranscriptEntry ──────────────────────────────────────────────────────────

/// Transcript 中的单条逻辑条目。Reminder 不是伪造的空 Human message。
#[derive(Debug, Clone)]
pub enum TranscriptEntry {
    Message(BaseMessage),
    Reminder {
        id: MessageId,
        reminder: TrustedSystemReminder,
    },
}

impl TranscriptEntry {
    pub fn id(&self) -> MessageId {
        match self {
            Self::Message(message) => message.id(),
            Self::Reminder { id, .. } => *id,
        }
    }

    pub fn as_message(&self) -> Option<&BaseMessage> {
        match self {
            Self::Message(message) => Some(message),
            Self::Reminder { .. } => None,
        }
    }

    pub fn message(&self) -> &BaseMessage {
        self.as_message()
            .expect("canonical reminder has no stored BaseMessage")
    }

    /// 该条目是否应向目标受众投递。
    ///
    /// 普通消息恒可见；canonical reminder 一律经 [`reminder_egress_allowed`] 的单一
    /// 投递规则判定（`Required` 只在声明受众内不可被屏蔽，`DiagnosticOnly`
    /// 默认只进 Diagnostics）。持久化保留 canonical reminder，过滤只发生在出口。
    pub fn visible_to(&self, audience: ReminderAudience) -> bool {
        match self {
            Self::Message(_) => true,
            Self::Reminder { reminder, .. } => {
                reminder_egress_allowed(reminder.as_reminder(), audience)
            }
        }
    }

    /// Canonical model projection shared by normal Reason and compact rendering.
    /// 转为 canonical 持久化载荷（与 `persisted_payloads` 同一映射，不另立格式）。
    pub fn into_payload(self) -> PersistedPayload {
        match self {
            Self::Message(message) => PersistedPayload::Message(message),
            Self::Reminder { id, reminder } => PersistedPayload::SystemReminder { id, reminder },
        }
    }

    pub fn project_message(&self) -> anyhow::Result<BaseMessage> {
        match self {
            Self::Message(message) => {
                let reminders = peri_acp_types::compact_reminder::legacy_compact_reminders(message);
                if reminders.is_empty() {
                    return Ok(message.clone());
                }
                let encoded = reminders
                    .iter()
                    .map(|reminder| {
                        peri_acp_types::system_reminder::encode_legacy_system_reminder(
                            &reminder.body,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("\n");
                Ok(BaseMessage::Human {
                    id: message.id(),
                    content: MessageContent::text(encoded),
                })
            }
            Self::Reminder { id, reminder } => Ok(BaseMessage::Human {
                id: *id,
                content: MessageContent::text(encode_system_reminder(reminder)?),
            }),
        }
    }
}

// ─── StagedData ───────────────────────────────────────────────────────────────

/// 两阶段写入的暂存数据
///
/// AI 消息（含 tool_calls）先暂存，Act 阶段收集 ToolResult 后原子提交。
/// 提交前这些消息对 LLM 请求不可见。
#[derive(Debug, Clone)]
pub struct StagedData {
    pub ai_message: BaseMessage,
    pub tool_results: Vec<BaseMessage>,
}

// ─── PersistOp ────────────────────────────────────────────────────────────────

/// 持久化操作 — 通过异步通道传递富操作给 writer task
///
/// 每个会产生写入的操作携带自己的[预算预留](Reservation)：追加方在持锁时同步预留，
/// writer 在效果确定后归还。Barrier / Shutdown 不产生写入，也就不占额度。
#[derive(Debug)]
pub enum PersistOp {
    /// 追加新消息（canonical payload）
    Append {
        payload: PersistedPayload,
        reserved: Reservation,
    },
    /// Transcript rewind 至指定 id（保留目标本身，删除其后记录）
    RewindTo {
        id: MessageId,
        reserved: Reservation,
    },
    /// 更新消息标记（投影变更集）
    UpdateFlags {
        id: MessageId,
        flags: MessageFlags,
        reserved: Reservation,
    },
    /// 批量应用 compaction 标记变更（一次完整投影行为，缓存由资源侧维护）
    ApplyCompactionBatch {
        updates: Vec<(MessageId, MessageFlags)>,
        reserved: Reservation,
    },
    /// 确认此前所有持久化操作均已实际调用 store
    Barrier(tokio::sync::oneshot::Sender<anyhow::Result<()>>),
    /// 优雅关闭：flush 剩余积压后退出 writer task（Drop / shutdown_persistence 发送）
    Shutdown,
}

mod persistence;

// ─── MessageTranscript ────────────────────────────────────────────────────────

/// 会话消息权威存储（v2）
///
/// 所有外部操作一律按 MessageId 寻址。内部通过 `id_index` 索引表支持 O(1) 查找。
/// `ancestor_len` 标记祖先消息边界，Fork/Background Agent 继承的祖先消息只读。
pub struct MessageTranscript {
    /// 消息列表（顺序即对话时间线）
    entries: Vec<TranscriptEntry>,
    /// id → Vec 下标索引表（O(1) 查找）
    id_index: HashMap<MessageId, usize>,
    /// messages[..ancestor_len] = 只读祖先消息
    ancestor_len: usize,
    /// 两阶段写入暂存区
    staged: Option<StagedData>,
    /// 消息标记（truncated / excluded）
    flags: HashMap<MessageId, MessageFlags>,
    /// 异步持久化发送端
    persist_tx: Option<Arc<tokio::sync::mpsc::UnboundedSender<PersistOp>>>,
    /// 持久化 writer task 的 AbortHandle
    persist_handle: Option<tokio::task::AbortHandle>,
    /// 持久化目标 thread id
    thread_id: Option<ThreadId>,
    /// 当前执行期间是否已提交 Full Compact。
    ///
    /// 此标记不持久化；executor 用它区分 Full Compact 的合法可见快照和
    /// 取消后可能不完整的临时 transcript。
    full_compaction_committed: bool,
    compaction_commit_state: CompactionCommitState,
    /// 会话资源门面引用（保留 Arc 让资源在 transcript 存活期间不被释放，
    /// spawned writer task 持有独立 clone）——compact 生命周期等**需要确认结果**的
    /// 写入直接经它执行，不走普通 PersistOp 队列。
    session_resources: Option<Arc<dyn SessionResources>>,
    /// 待持久化预算（与 writer 共享）：条数/字节有界，预留失败即 sticky 失败。
    budget: Option<Arc<PersistenceBudget>>,
}

impl std::fmt::Debug for MessageTranscript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageTranscript")
            .field("entries_len", &self.entries.len())
            .field("id_index_len", &self.id_index.len())
            .field("ancestor_len", &self.ancestor_len)
            .field("has_staged", &self.staged.is_some())
            .field("flags_len", &self.flags.len())
            .field("has_persistence", &self.persist_tx.is_some())
            .finish()
    }
}

impl Default for MessageTranscript {
    fn default() -> Self {
        Self::new()
    }
}

/// 持久化提醒的交接端口：资源门面、会话 id、可选的持久化写队列。
pub type IdempotentReminderPort = (
    Arc<dyn SessionResources>,
    ThreadId,
    Option<Arc<tokio::sync::mpsc::UnboundedSender<PersistOp>>>,
);

impl MessageTranscript {
    /// 创建空 Transcript
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            id_index: HashMap::new(),
            ancestor_len: 0,
            staged: None,
            flags: HashMap::new(),
            persist_tx: None,
            persist_handle: None,
            thread_id: None,
            full_compaction_committed: false,
            compaction_commit_state: CompactionCommitState::default(),
            session_resources: None,
            budget: None,
        }
    }

    fn push_loaded_payload(&mut self, payload: PersistedPayload) {
        let entry = match payload {
            PersistedPayload::Message(message) => TranscriptEntry::Message(message),
            PersistedPayload::SystemReminder { id, reminder } => {
                TranscriptEntry::Reminder { id, reminder }
            }
        };
        self.id_index.insert(entry.id(), self.entries.len());
        self.entries.push(entry);
    }

    /// 装载主 Agent 已持久化的历史，保持为可压缩的自有消息。
    ///
    /// 与 [`Self::with_ancestor_payloads`] 不同，此路径不移动 `ancestor_len`；普通
    /// session 的跨 turn 历史仍属于当前 Agent，Full Compact 必须能够排除它。
    pub fn with_own_payloads(mut self, payloads: Vec<PersistedPayload>) -> Self {
        for payload in payloads {
            self.push_loaded_payload(payload);
        }
        self
    }

    /// 装载来源明确的继承 payload，并将其划入只读 ancestor region。
    ///
    /// 调用方若拿到“父快照 + 当前 thread 历史”的平铺列表，必须先恢复来源边界；
    /// 不得把混合 ownership 的列表整体传入本方法。
    pub fn with_ancestor_payloads(mut self, payloads: Vec<PersistedPayload>) -> Self {
        for payload in payloads {
            self.push_loaded_payload(payload);
        }
        self.ancestor_len = self.entries.len();
        self
    }

    /// 设置祖先消息（Fork/Background Agent 从父 Agent 继承）
    ///
    /// 祖先消息只读——Compact 仅操作边界之后的自有消息。
    pub fn with_ancestor(mut self, messages: Vec<BaseMessage>) -> Self {
        let len = messages.len();
        for msg in &messages {
            let id = msg.id();
            self.id_index.insert(id, self.entries.len());
            self.entries.push(TranscriptEntry::Message(msg.clone()));
        }
        self.ancestor_len = len;
        self
    }

    /// 绑定会话资源门面
    ///
    /// 绑定后 append / rewind / 投影变更自动异步写入门面（FIFO 单一 writer）。
    /// 积压有界性由 [`PersistenceBudget`] 保证：追加在持锁时同步预留，writer 在
    /// 效果确定后归还；预留失败即 sticky 失败（[`Self::persistence_failure`]），
    /// 调用方据此停止后续工作并重载会话。
    pub fn with_persistence(self, store: Arc<dyn SessionResources>, thread_id: ThreadId) -> Self {
        let budget = PersistenceBudget::new(DEFAULT_PENDING_MAX_ITEMS, DEFAULT_PENDING_MAX_BYTES);
        self.bind_persistence(store, thread_id, budget)
    }

    /// 绑定持久化后端并显式指定待持久化预算（测试与阈值测量入口）。
    pub fn with_persistence_budget(
        self,
        store: Arc<dyn SessionResources>,
        thread_id: ThreadId,
        budget: Arc<PersistenceBudget>,
    ) -> Self {
        self.bind_persistence(store, thread_id, budget)
    }

    fn bind_persistence(
        mut self,
        store: Arc<dyn SessionResources>,
        thread_id: ThreadId,
        budget: Arc<PersistenceBudget>,
    ) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<PersistOp>();
        self.persist_tx = Some(Arc::new(tx));
        self.thread_id = Some(thread_id.clone());
        self.session_resources = Some(store.clone());
        self.budget = Some(Arc::clone(&budget));

        let handle = tokio::spawn(persistence::run_writer(store, thread_id, budget, rx));
        self.persist_handle = Some(handle.abort_handle());

        self
    }

    /// 待持久化失败原因（sticky）：预算耗尽、writer 终态失败或写通道关闭。
    ///
    /// 非 `None` 表示热态已不可信：数据可能已进内存但未落盘，调用方必须停止后续
    /// 模型/工具工作并让会话走冷重载，不得把当前快照当作已保存。
    pub fn persistence_failure(&self) -> Option<String> {
        self.budget.as_ref().and_then(|budget| budget.failure())
    }

    /// 是否已进入 sticky 持久化失败。
    pub fn has_persistence_failure(&self) -> bool {
        self.persistence_failure().is_some()
    }

    /// 预算句柄（测试断言积压上界用）。
    pub fn persistence_budget(&self) -> Option<Arc<PersistenceBudget>> {
        self.budget.clone()
    }

    // ── 查询 ──────────────────────────────────────────────────────────────────

    /// 获取全部条目（不可变引用）
    pub fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }

    /// Canonical persistence snapshot preserving message/reminder discriminants and stable IDs.
    pub fn persisted_payloads(&self) -> Vec<PersistedPayload> {
        self.entries
            .iter()
            .cloned()
            .map(|entry| match entry {
                TranscriptEntry::Message(message) => PersistedPayload::Message(message),
                TranscriptEntry::Reminder { id, reminder } => {
                    PersistedPayload::SystemReminder { id, reminder }
                }
            })
            .collect()
    }

    /// 获取所有**可见**普通消息（跳过 excluded 与 canonical reminder）
    pub fn visible_messages(&self) -> Vec<&BaseMessage> {
        self.entries
            .iter()
            .filter(|entry| !self.flags(entry.id()).excluded)
            .filter_map(TranscriptEntry::as_message)
            .collect()
    }

    /// Projects visible transcript entries for the model. Canonical reminders are encoded only here.
    ///
    /// 只有声明 [`ReminderAudience::Model`] 的 canonical reminder 进入模型请求；
    /// Tui-only / Diagnostics-only / DiagnosticOnly 提醒既不编码也不投递。
    pub fn visible_model_messages(&self) -> anyhow::Result<Vec<BaseMessage>> {
        self.entries
            .iter()
            .filter(|entry| !self.flags(entry.id()).excluded)
            .filter(|entry| entry.visible_to(ReminderAudience::Model))
            .map(TranscriptEntry::project_message)
            .collect()
    }

    pub fn append_system_reminder(&mut self, reminder: TrustedSystemReminder) -> MessageId {
        let id = MessageId::new();
        let idx = self.entries.len();
        self.id_index.insert(id, idx);
        self.entries
            .push(TranscriptEntry::Reminder { id, reminder });
        self.persist_appended_entry(self.entries[idx].clone());
        id
    }

    /// Mirror an already committed, stable-ID reminder into the in-memory view.
    /// The caller must commit through SessionResources first; this path never
    /// enters the asynchronous transcript writer a second time.
    pub fn mirror_committed_reminder(
        &mut self,
        id: MessageId,
        reminder: TrustedSystemReminder,
    ) -> bool {
        if self.id_index.contains_key(&id) {
            return false;
        }
        let idx = self.entries.len();
        self.id_index.insert(id, idx);
        self.entries
            .push(TranscriptEntry::Reminder { id, reminder });
        true
    }

    pub fn mirror_committed_payload(&mut self, payload: PersistedPayload) -> bool {
        if self.id_index.contains_key(&payload.id()) {
            return false;
        }
        self.push_loaded_payload(payload);
        true
    }

    pub fn idempotent_reminder_port(&self) -> Option<IdempotentReminderPort> {
        Some((
            Arc::clone(self.session_resources.as_ref()?),
            self.thread_id.as_ref()?.clone(),
            self.persist_tx_handle(),
        ))
    }

    /// 获取所有**可见**消息的 owned Arc 快照（跳过 excluded 标记的消息）
    ///
    /// 用于在事件边界（如 `RenderEvent::TurnCompleted`）向 TUI/ACP 消费方传递
    /// 权威 transcript 快照。这是客户端出口：只有声明
    /// [`ReminderAudience::Tui`] 的 canonical reminder 进入快照，Model-only 内容
    /// （如 recall）不得出现在客户端 wire 上。
    ///
    /// **注意**：构建快照时仍会逐条深拷贝消息本体（需要过滤 excluded 并取得
    /// 独立所有权，无法与内部 `entries` 直接共享）；Arc 只保证快照在后续
    /// 事件管道多级传递时不再被重复深拷贝。
    pub fn visible_snapshot(&self) -> Arc<Vec<BaseMessage>> {
        let filtered: Vec<BaseMessage> = self
            .entries
            .iter()
            .filter(|entry| {
                let f = self.flags.get(&entry.id());
                match f {
                    None => true,
                    Some(flags) => !flags.excluded,
                }
            })
            .filter(|entry| entry.visible_to(ReminderAudience::Tui))
            .map(|entry| entry.project_message().expect("validated transcript entry"))
            .collect();
        Arc::new(filtered)
    }

    pub fn with_compaction_commit_state(mut self, state: CompactionCommitState) -> Self {
        self.compaction_commit_state = state;
        self
    }

    pub fn compaction_commit_state(&self) -> CompactionCommitState {
        self.compaction_commit_state.clone()
    }

    /// 当前执行期间是否已提交 Full Compact。
    pub fn full_compaction_committed(&self) -> bool {
        self.full_compaction_committed
    }

    /// 标记 Full Compact 已成功写入持久化存储和内存 transcript。
    pub fn mark_full_compaction_committed(&mut self) {
        self.full_compaction_committed = true;
    }

    /// 按 id 获取条目（O(1)）
    pub fn get(&self, id: MessageId) -> Option<&TranscriptEntry> {
        self.id_index.get(&id).map(|&idx| &self.entries[idx])
    }

    /// 获取消息标记（无标记时返回默认值）
    pub fn flags(&self, id: MessageId) -> MessageFlags {
        self.flags.get(&id).cloned().unwrap_or_default()
    }

    /// 按 id 获取消息标记，消息不存在时返回 None
    ///
    /// 与 `flags()` 不同：此方法先确认 id 存在于索引表中，
    /// 不存在则返回 `None`（而非返回默认标记）。
    pub fn get_flags(&self, id: MessageId) -> Option<MessageFlags> {
        self.id_index.get(&id)?;
        Some(self.flags(id))
    }

    /// 祖先消息数量
    pub fn ancestor_len(&self) -> usize {
        self.ancestor_len
    }

    /// 消息总数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // ── 写入 ──────────────────────────────────────────────────────────────────

    /// 追加单条消息，返回其 MessageId
    ///
    /// 仅用于非 AI 消息（Human / System / 独立 ToolResult）。
    /// AI 消息（含 tool_calls）应使用 staging 流程。
    pub fn append(&mut self, message: BaseMessage) -> MessageId {
        let id = message.id();
        let idx = self.entries.len();
        self.id_index.insert(id, idx);
        self.entries.push(TranscriptEntry::Message(message));
        // 异步持久化（先同步预留额度；无法预留时只留 sticky 失败，不假装已保存）
        self.persist_appended_entry(self.entries[idx].clone());
        id
    }

    /// 批量追加消息，返回所有 MessageId
    pub fn append_batch(&mut self, messages: Vec<BaseMessage>) -> Vec<MessageId> {
        let mut ids = Vec::with_capacity(messages.len());
        for msg in messages {
            let id = msg.id();
            let idx = self.entries.len();
            self.id_index.insert(id, idx);
            self.entries.push(TranscriptEntry::Message(msg));
            ids.push(id);
            self.persist_appended_entry(self.entries[idx].clone());
        }
        ids
    }

    /// 按 MessageId 替换消息内容（in-place，不改变 id_index）
    ///
    /// 仅更新 `entries` 中的消息本体。ID 不存在时 no-op。
    /// 不触发异步持久化（假设调用方会在后续正常写入路径中持久化）。
    pub fn replace_by_id(&mut self, message: BaseMessage) {
        if let Some(&idx) = self.id_index.get(&message.id()) {
            self.entries[idx] = TranscriptEntry::Message(message);
        }
    }

    // ── Staging ────────────────────────────────────────────────────────────────

    /// 暂存 AI 消息（含 tool_calls），不写入主列表
    ///
    /// 若已有暂存数据，先丢弃旧的（同一轮不应出现两个 AI 消息）。
    pub fn stage_ai_message(&mut self, ai_message: BaseMessage) {
        self.staged = Some(StagedData {
            ai_message,
            tool_results: Vec::new(),
        });
    }

    /// 向暂存区追加 ToolResult
    ///
    /// 必须在 `stage_ai_message` 之后调用，否则 no-op。
    pub fn stage_tool_result(&mut self, tool_result: BaseMessage) {
        if let Some(ref mut staged) = self.staged {
            staged.tool_results.push(tool_result);
        }
    }

    /// 原子提交暂存数据到主列表
    ///
    /// 提交顺序：AI 消息 → ToolResult 列表。
    /// 提交后清空暂存区，触发持久化。
    pub fn commit_staged(&mut self) {
        let staged = match self.staged.take() {
            Some(s) => s,
            None => return,
        };

        // 写入 AI 消息
        let ai_id = staged.ai_message.id();
        let ai_idx = self.entries.len();
        self.id_index.insert(ai_id, ai_idx);
        self.entries
            .push(TranscriptEntry::Message(staged.ai_message));
        self.persist_appended_entry(self.entries[ai_idx].clone());

        // 写入 ToolResult 列表
        for tool_result in staged.tool_results {
            let id = tool_result.id();
            let idx = self.entries.len();
            self.id_index.insert(id, idx);
            self.entries.push(TranscriptEntry::Message(tool_result));
            self.persist_appended_entry(self.entries[idx].clone());
        }
    }

    /// 丢弃暂存数据（Cancel/Error 时调用）
    pub fn discard_staged(&mut self) {
        self.staged = None;
    }

    /// 是否有暂存数据
    pub fn has_staged(&self) -> bool {
        self.staged.is_some()
    }

    // ── 标记 ──────────────────────────────────────────────────────────────────

    fn can_update_own_flags(&self, id: MessageId) -> bool {
        let allowed = self
            .id_index
            .get(&id)
            .is_some_and(|index| *index >= self.ancestor_len);
        if !allowed {
            tracing::warn!(?id, "ignoring flag mutation outside own transcript region");
        }
        allowed
    }

    /// 设置 truncated 标记（Micro compact）
    pub fn set_truncated(&mut self, id: MessageId, value: bool) {
        if !self.can_update_own_flags(id) {
            return;
        }
        self.flags.entry(id).or_default().truncated = value;
        let flags = self.flags[&id].clone();
        self.persist_flags(id, flags);
    }

    /// 设置 excluded 标记（Full / Smart compact）
    pub fn set_excluded(&mut self, id: MessageId, value: bool) {
        if !self.can_update_own_flags(id) {
            return;
        }
        self.flags.entry(id).or_default().excluded = value;
        let flags = self.flags[&id].clone();
        self.persist_flags(id, flags);
    }

    /// 设置 projection directive（Micro compact）
    ///
    /// 与 `set_truncated` 配合使用：Micro compact 完成后，将 planner 生成的
    /// per-message directive 持久化到 flags，避免后续每 turn 重新规划。
    /// 设置 projection 的同时也会设置 truncated=true。
    pub fn set_flags_projection(&mut self, id: MessageId, directive: MessageProjectionDirective) {
        if !self.can_update_own_flags(id) {
            return;
        }
        let existing = self.flags.get(&id).cloned().unwrap_or_default();
        let flags = history::flags_with_projection(&existing, directive);
        self.flags.insert(id, flags.clone());
        self.persist_flags(id, flags);
    }

    /// 清除指定消息的所有标记
    pub fn clear_flags(&mut self, id: MessageId) {
        if !self.can_update_own_flags(id) {
            return;
        }
        self.flags.remove(&id);
        self.persist_flags(id, MessageFlags::default());
    }

    /// 批量恢复消息标记（用于 session 恢复时从持久化存储加载 flags）
    ///
    /// 仅插入非默认标记，不触发持久化（持久化已有完整 flags 数据）。
    pub fn set_flags_batch(&mut self, batch: std::collections::HashMap<MessageId, MessageFlags>) {
        for (id, flags) in batch {
            if flags != MessageFlags::default() {
                self.flags.insert(id, flags);
            }
        }
    }

    /// 原子提交 compaction 生命周期到持久化存储及内存 transcript。
    ///
    /// 仅在 store 事务成功后更新内存；事务已经持久化全部变更，不能再排队普通 PersistOp。
    pub async fn commit_compaction_lifecycle(
        &mut self,
        lifecycle: crate::thread::CompactionChange,
    ) -> anyhow::Result<()> {
        if self.compaction_commit_state.is_uncertain() {
            return Err(anyhow!(
                "compact persistence outcome is unknown; reload the session"
            ));
        }

        let (store, thread_id) = match (&self.session_resources, &self.thread_id) {
            (Some(store), Some(thread_id)) => (store.clone(), thread_id.clone()),
            _ => return Err(anyhow!("compact lifecycle requires persistence")),
        };

        for (id, _) in &lifecycle.flag_updates {
            if !self.id_index.contains_key(id) {
                return Err(anyhow!(
                    "compact lifecycle flag target id {id:?} not found in transcript"
                ));
            }
            if self.id_index[id] < self.ancestor_len {
                return Err(anyhow!(
                    "compact lifecycle cannot mutate ancestor message {id:?}"
                ));
            }
        }

        // 追加消息的 id 必须与既有历史不冲突：同一 id 已存在或批次内重复都必须在
        // 产生任何副作用之前失败（规则与 adapter 共用，见 store::history）。
        history::ensure_distinct_ids(
            &history::appended_payloads(&lifecycle.appended_messages),
            |id| self.id_index.contains_key(&id),
        )
        .map_err(|error| anyhow!("compact lifecycle rejected: {error}"))?;

        // Both awaits can be cancelled, or report an error after durable effects. Only
        // applying the acknowledged lifecycle to memory makes this snapshot safe again.
        //
        // 顺序固定：先 flush 既有积压，再提交一次完整 compaction 行为（摘要、flags、
        // 计数与缓存视图同一事务），成功后才改内存。失败/未证明时保留磁盘事实并让
        // 热态保持失效（`is_uncertain`），不把内存视图推进到磁盘前面。
        self.compaction_commit_state.mark_pending();
        self.flush_persistence().await?;
        if let Err(error) = store.apply_compaction(&thread_id, &lifecycle).await {
            return Err(anyhow!(
                "compact persistence did not commit; reload the session to recover: {error}"
            ));
        }
        self.apply_compaction_lifecycle_memory(&lifecycle);
        self.compaction_commit_state.mark_committed();

        Ok(())
    }

    /// 应用已成功持久化的 compaction lifecycle，不发送普通 PersistOp。
    fn apply_compaction_lifecycle_memory(&mut self, lifecycle: &crate::thread::CompactionChange) {
        history::apply_flag_updates(&mut self.flags, &lifecycle.flag_updates);

        for message in &lifecycle.appended_messages {
            let id = message.id();
            let idx = self.entries.len();
            self.id_index.insert(id, idx);
            self.entries.push(TranscriptEntry::Message(message.clone()));
        }
    }

    // ── 重建 ──────────────────────────────────────────────────────────────────

    /// 用新消息列表替换内部状态（Compact 专用）
    ///
    /// 消费 self，返回新 Transcript。保留 `ancestor_len`、持久化绑定等配置。
    /// `entries` 参数为 `(BaseMessage, MessageFlags)` 对，保留标记。
    pub fn rebuild(mut self, entries: Vec<(BaseMessage, MessageFlags)>) -> Self {
        let mut new_entries = Vec::with_capacity(entries.len());
        let mut new_index = HashMap::with_capacity(entries.len());
        let mut new_flags = HashMap::with_capacity(entries.len());

        for (idx, (msg, flags)) in entries.into_iter().enumerate() {
            let id = msg.id();
            new_index.insert(id, idx);
            new_entries.push(TranscriptEntry::Message(msg));
            // 仅存非默认标记
            if flags != MessageFlags::default() {
                new_flags.insert(id, flags);
            }
        }

        Self {
            entries: new_entries,
            id_index: new_index,
            flags: new_flags,
            ancestor_len: self.ancestor_len,
            staged: None,
            persist_tx: self.persist_tx.take(),
            persist_handle: self.persist_handle.take(),
            thread_id: self.thread_id.take(),
            full_compaction_committed: self.full_compaction_committed,
            compaction_commit_state: self.compaction_commit_state.clone(),
            session_resources: self.session_resources.take(),
            budget: self.budget.take(),
        }
    }

    // ── Rewind ─────────────────────────────────────────────────────────────────

    /// 截断 Transcript 至指定消息（含）
    ///
    /// 同步收缩索引表、清空 staging。
    /// 若 id 不存在返回错误。
    pub fn rewind_to(&mut self, id: MessageId) -> Result<(), anyhow::Error> {
        let ids: Vec<MessageId> = self.entries.iter().map(TranscriptEntry::id).collect();
        // transcript rewind 保留目标本身（KeepThrough）；用户 rewind 的 RemoveFrom
        // 是另一个边界，两者共用 store::history 的边界规则。
        let keep_len = history::rewind_keep_len(&ids, RewindBoundary::KeepThrough(id))
            .map_err(|_| anyhow!("rewind target id {id:?} not found in transcript"))?;
        let target_idx = keep_len - 1;

        // ancestor 边界保护：不能 rewind 到祖先消息内部
        if target_idx < self.ancestor_len {
            return Err(anyhow!(
                "cannot rewind into ancestor region (ancestor_len={}, target_idx={})",
                self.ancestor_len,
                target_idx
            ));
        }

        // 清空暂存区
        self.staged = None;

        // 收集要移除的 id（用于清理索引和标记）
        let remove_ids: Vec<MessageId> = ids[keep_len..].to_vec();

        // 截断 entries
        self.entries.truncate(keep_len);

        // 收缩索引表
        for rid in &remove_ids {
            self.id_index.remove(rid);
            self.flags.remove(rid);
        }

        // 异步持久化 rewind（保留目标本身的 KeepThrough 边界）
        if let Some(reserved) = self.reserve_persistence(Reservation::marker(1)) {
            self.send_persist(PersistOp::RewindTo { id, reserved });
        }

        Ok(())
    }

    // ── 内部辅助 ────────────────────────────────────────────────────────────────

    /// 等待此前已排队的持久化操作完成。
    ///
    /// 同一 writer 按 FIFO 处理 barrier，因此收到确认时，所有此前操作都已调用 store。
    /// 首个持久化错误会保持为终态失败；后续 barrier 继续返回该错误，不消费或重试。
    pub async fn flush_persistence(&self) -> anyhow::Result<()> {
        let Some(tx) = self.persist_tx_handle() else {
            return Ok(());
        };
        Self::flush_via_tx(&tx).await
    }

    /// 同步取出持久化 writer 通道句柄（owned `Arc<Sender>`，Send）。
    ///
    /// 用途：调用方持有 `Arc<RwLock<MessageTranscript>>` 时，先在 guard 作用域内
    /// 同步提取句柄、释放 guard，再调用 [`flush_via_tx`] 异步等待——避免
    /// parking_lot guard 跨 await 存活（`!Send`，会令整个调用链 future 不满足
    /// `Send`，`tokio::spawn` 编译失败）。
    pub fn persist_tx_handle(&self) -> Option<Arc<tokio::sync::mpsc::UnboundedSender<PersistOp>>> {
        self.persist_tx.clone()
    }

    /// barrier 等待逻辑（基于 owned sender，Send 安全）
    pub async fn flush_via_tx(
        tx: &tokio::sync::mpsc::UnboundedSender<PersistOp>,
    ) -> anyhow::Result<()> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        tx.send(PersistOp::Barrier(ack_tx))
            .map_err(|_| anyhow!("transcript persistence writer channel closed"))?;
        ack_rx
            .await
            .map_err(|_| anyhow!("transcript persistence writer dropped barrier acknowledgement"))?
    }

    /// 预留一笔待持久化额度：持锁期间同步完成，不 await。
    ///
    /// 预算耗尽时**不发送**该操作并留下 sticky 失败——数据已进 canonical 内存但
    /// 没有落盘名额，调用方只能据 [`Self::persistence_failure`] 停止后续工作。
    fn reserve_persistence(&self, reservation: Reservation) -> Option<Reservation> {
        let budget = self.budget.as_ref()?;
        match budget.try_reserve(reservation) {
            Ok(()) => Some(reservation),
            Err(reason) => {
                tracing::error!(
                    items = reservation.items,
                    bytes = reservation.bytes,
                    "transcript persistence backlog limit reached: {reason}"
                );
                None
            }
        }
    }

    /// 追加条目落盘：按 canonical payload 编码长度预留额度后投递。
    fn persist_appended_entry(&self, entry: TranscriptEntry) {
        let payload = entry.into_payload();
        let Some(reserved) = self.reserve_persistence(Reservation::for_payload(&payload)) else {
            return;
        };
        self.send_persist(PersistOp::Append { payload, reserved });
    }

    /// 投影/flags 变更落盘：队列里只有 id 与标记，按条数预留。
    fn persist_flags(&self, id: MessageId, flags: MessageFlags) {
        let Some(reserved) = self.reserve_persistence(Reservation::marker(1)) else {
            return;
        };
        self.send_persist(PersistOp::UpdateFlags {
            id,
            flags,
            reserved,
        });
    }

    /// 发送持久化操作到 writer task（额度已在调用点预留）
    fn send_persist(&self, op: PersistOp) {
        if let Some(ref tx) = self.persist_tx {
            if let Err(e) = tx.send(op) {
                // 通道关闭 = 写入不会发生：与预算耗尽同样是 sticky 失败。
                if let Some(budget) = self.budget.as_ref() {
                    budget.mark_failed(&format!(
                        "transcript persistence writer channel closed: {e}"
                    ));
                }
                tracing::warn!("transcript persist send failed (channel closed): {e}");
            }
        }
    }

    /// 优雅关闭持久化 writer task：发送 `Shutdown` 信号，writer flush 剩余积压后自行退出。
    ///
    /// 不调用 `abort()`：abort 会立即取消任务，导致 `pending_appends` 和通道中未处理的
    /// 消息被直接丢弃（参照 `langfuse-client/src/batcher.rs` 的 Shutdown 模式）。
    /// Drop 是同步的无法 await，因此不等待 writer 完成：writer 持有 store 的独立 Arc
    /// （`with_persistence` 中 clone），detached 收尾安全。
    pub fn shutdown_persistence(&self) {
        if let Some(ref tx) = self.persist_tx {
            if let Err(e) = tx.send(PersistOp::Shutdown) {
                // writer 已退出（channel closed）时无需处理：退出前已 flush 剩余积压
                tracing::debug!("transcript persist shutdown send failed (channel closed): {e}");
            }
        }
    }
}

impl Drop for MessageTranscript {
    fn drop(&mut self) {
        self.shutdown_persistence();
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "transcript_test.rs"]
mod tests;

#[cfg(test)]
#[path = "transcript_projection_test.rs"]
mod projection_tests;
