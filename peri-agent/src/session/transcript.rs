//! MessageTranscript v2 — 会话消息权威存储
//!
//! Transcript 是会话全部消息的唯一真相源。核心特性：
//! - **MessageId 寻址**：内部维护 `HashMap<MessageId, usize>` 索引表，O(1) 查找
//! - **只追加优先**：正常 ReAct 循环中消息仅尾部追加，禁止 prepend/中间插入
//! - **Staging 两阶段写入**：AI 消息 + ToolResult 原子提交
//! - **标记代替删除**：`truncated` / `excluded` 标记用于 Compact，消息本体不变
//! - **异步持久化**：append 后通过 unbounded_channel 异步触发 ThreadStore 写入

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

use crate::messages::{BaseMessage, MessageId};
use crate::thread::{ThreadId, ThreadStore};

// ─── TranscriptEntry ──────────────────────────────────────────────────────────

/// Transcript 中的单条消息条目
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub message: BaseMessage,
}

// ─── MessageFlags ─────────────────────────────────────────────────────────────

/// 消息标记 — Compact 用，标记代替删除
///
/// - `truncated`：Micro compact 标记，LLM 请求时截断该消息输出
/// - `excluded`：Full / Smart compact 标记，LLM 请求时跳过该消息
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFlags {
    pub truncated: bool,
    pub excluded: bool,
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
#[derive(Debug, Clone)]
pub enum PersistOp {
    /// 追加新消息
    Append(BaseMessage),
    /// Rewind 至指定 id（删除该 id 之后的所有记录）
    RewindTo(MessageId),
    /// 更新消息标记
    UpdateFlags(MessageId, MessageFlags),
}

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
    /// 持久化后端引用（保留 Arc 让 store 在 transcript 存活期间不被释放，
    /// spawned writer task 持有独立 clone）
    store: Option<Arc<dyn ThreadStore>>,
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
            store: None,
        }
    }

    /// 设置祖先消息（Fork/Background Agent 从父 Agent 继承）
    ///
    /// 祖先消息只读——Compact 仅操作边界之后的自有消息。
    pub fn with_ancestor(mut self, messages: Vec<BaseMessage>) -> Self {
        let len = messages.len();
        for msg in &messages {
            let id = msg.id();
            self.id_index.insert(id, self.entries.len());
            self.entries.push(TranscriptEntry {
                message: msg.clone(),
            });
        }
        self.ancestor_len = len;
        self
    }

    /// 绑定持久化后端
    ///
    /// 绑定后 append / rewind / 标记变更自动异步写入 ThreadStore。
    /// 使用有序通道保证操作按调用顺序执行。
    pub fn with_persistence(mut self, store: Arc<dyn ThreadStore>, thread_id: ThreadId) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PersistOp>();
        self.persist_tx = Some(Arc::new(tx));
        self.thread_id = Some(thread_id.clone());
        self.store = Some(store.clone());

        let tid = thread_id;
        let handle = tokio::spawn(async move {
            let mut processed: u64 = 0;
            let mut last_warn_at: u64 = 0;
            while let Some(op) = rx.recv().await {
                let result = match op {
                    PersistOp::Append(msg) => store.append_message(&tid, msg).await,
                    PersistOp::RewindTo(id) => store.delete_messages_since(&tid, &id).await,
                    PersistOp::UpdateFlags(id, flags) => {
                        store
                            .update_message_flags(&id, flags.truncated, flags.excluded)
                            .await
                    }
                };
                if let Err(e) = result {
                    tracing::warn!("transcript persist failed: {e}");
                }
                processed = processed.saturating_add(1);
                let bucket = processed / 1000;
                if bucket > last_warn_at {
                    last_warn_at = bucket;
                    tracing::trace!(
                        thread_id = %tid,
                        processed,
                        "transcript persist writer: 已处理 {processed} 条操作"
                    );
                }
            }
        });
        self.persist_handle = Some(handle.abort_handle());

        self
    }

    // ── 查询 ──────────────────────────────────────────────────────────────────

    /// 获取全部条目（不可变引用）
    pub fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }

    /// 获取所有**可见**消息（跳过 excluded 标记的消息）
    ///
    /// LLM 请求构造时使用此方法获取有效消息列表。
    pub fn visible_messages(&self) -> Vec<&BaseMessage> {
        self.entries
            .iter()
            .filter(|entry| {
                let f = self.flags.get(&entry.message.id());
                match f {
                    None => true,
                    Some(flags) => !flags.excluded,
                }
            })
            .map(|entry| &entry.message)
            .collect()
    }

    /// 获取所有**可见**消息的 owned Arc 快照（跳过 excluded 标记的消息）
    ///
    /// 用于在事件边界（如 `RenderEvent::TurnCompleted`）向 TUI/ACP 消费方传递
    /// 权威 transcript 快照。Arc 浅克隆避免消息本体的深拷贝。
    pub fn visible_snapshot(&self) -> Arc<Vec<BaseMessage>> {
        let filtered: Vec<BaseMessage> = self
            .entries
            .iter()
            .filter(|entry| {
                let f = self.flags.get(&entry.message.id());
                match f {
                    None => true,
                    Some(flags) => !flags.excluded,
                }
            })
            .map(|entry| entry.message.clone())
            .collect();
        Arc::new(filtered)
    }

    /// 按 id 获取条目（O(1)）
    pub fn get(&self, id: MessageId) -> Option<&TranscriptEntry> {
        self.id_index.get(&id).map(|&idx| &self.entries[idx])
    }

    /// 获取消息标记（无标记时返回默认值）
    pub fn flags(&self, id: MessageId) -> MessageFlags {
        self.flags.get(&id).copied().unwrap_or_default()
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
        self.entries.push(TranscriptEntry { message });
        // 异步持久化
        self.send_persist(PersistOp::Append(self.entries[idx].message.clone()));
        id
    }

    /// 批量追加消息，返回所有 MessageId
    pub fn append_batch(&mut self, messages: Vec<BaseMessage>) -> Vec<MessageId> {
        let mut ids = Vec::with_capacity(messages.len());
        for msg in messages {
            let id = msg.id();
            let idx = self.entries.len();
            self.id_index.insert(id, idx);
            self.entries.push(TranscriptEntry { message: msg });
            ids.push(id);
            self.send_persist(PersistOp::Append(self.entries[idx].message.clone()));
        }
        ids
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
        self.entries.push(TranscriptEntry {
            message: staged.ai_message,
        });
        self.send_persist(PersistOp::Append(self.entries[ai_idx].message.clone()));

        // 写入 ToolResult 列表
        for tool_result in staged.tool_results {
            let id = tool_result.id();
            let idx = self.entries.len();
            self.id_index.insert(id, idx);
            self.entries.push(TranscriptEntry {
                message: tool_result,
            });
            self.send_persist(PersistOp::Append(self.entries[idx].message.clone()));
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

    /// 设置 truncated 标记（Micro compact）
    pub fn set_truncated(&mut self, id: MessageId, value: bool) {
        self.flags.entry(id).or_default().truncated = value;
        let flags = self.flags[&id];
        self.send_persist(PersistOp::UpdateFlags(id, flags));
    }

    /// 设置 excluded 标记（Full / Smart compact）
    pub fn set_excluded(&mut self, id: MessageId, value: bool) {
        self.flags.entry(id).or_default().excluded = value;
        let flags = self.flags[&id];
        self.send_persist(PersistOp::UpdateFlags(id, flags));
    }

    /// 清除指定消息的所有标记
    pub fn clear_flags(&mut self, id: MessageId) {
        self.flags.remove(&id);
        self.send_persist(PersistOp::UpdateFlags(id, MessageFlags::default()));
    }

    // ── 重建 ──────────────────────────────────────────────────────────────────

    /// 用新消息列表替换内部状态（Compact 专用）
    ///
    /// 消费 self，返回新 Transcript。保留 `ancestor_len`、持久化绑定等配置。
    /// `entries` 参数为 `(BaseMessage, MessageFlags)` 对，保留标记。
    pub fn rebuild(self, entries: Vec<(BaseMessage, MessageFlags)>) -> Self {
        let mut new_entries = Vec::with_capacity(entries.len());
        let mut new_index = HashMap::with_capacity(entries.len());
        let mut new_flags = HashMap::with_capacity(entries.len());

        for (idx, (msg, flags)) in entries.into_iter().enumerate() {
            let id = msg.id();
            new_index.insert(id, idx);
            new_entries.push(TranscriptEntry { message: msg });
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
            persist_tx: self.persist_tx.clone(),
            persist_handle: self.persist_handle.clone(),
            thread_id: self.thread_id.clone(),
            store: self.store.clone(),
        }
    }

    // ── Rewind ─────────────────────────────────────────────────────────────────

    /// 截断 Transcript 至指定消息（含）
    ///
    /// 同步收缩索引表、清空 staging。
    /// 若 id 不存在返回错误。
    pub fn rewind_to(&mut self, id: MessageId) -> Result<(), anyhow::Error> {
        let target_idx = self
            .id_index
            .get(&id)
            .copied()
            .ok_or_else(|| anyhow!("rewind target id {id:?} not found in transcript"))?;

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
        let remove_ids: Vec<MessageId> = self.entries[target_idx + 1..]
            .iter()
            .map(|e| e.message.id())
            .collect();

        // 截断 entries
        self.entries.truncate(target_idx + 1);

        // 收缩索引表
        for rid in &remove_ids {
            self.id_index.remove(rid);
            self.flags.remove(rid);
        }

        // 异步持久化 rewind
        self.send_persist(PersistOp::RewindTo(id));

        Ok(())
    }

    // ── 内部辅助 ────────────────────────────────────────────────────────────────

    /// 发送持久化操作到 writer task
    fn send_persist(&self, op: PersistOp) {
        if let Some(ref tx) = self.persist_tx {
            if let Err(e) = tx.send(op) {
                tracing::warn!("transcript persist send failed (channel closed): {e}");
            }
        }
    }

    /// 优雅关闭持久化 writer task
    pub fn shutdown_persistence(&self) {
        if let Some(ref handle) = self.persist_handle {
            handle.abort();
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
mod tests {
    use super::*;
    use crate::messages::MessageContent;

    fn make_human(text: &str) -> BaseMessage {
        BaseMessage::human(MessageContent::text(text.to_string()))
    }

    fn make_ai(text: &str) -> BaseMessage {
        BaseMessage::ai(MessageContent::text(text.to_string()))
    }

    fn make_tool_result(tool_call_id: &str, text: &str) -> BaseMessage {
        BaseMessage::tool_result(
            tool_call_id.to_string(),
            MessageContent::text(text.to_string()),
        )
    }

    // ── 基础构造 ──────────────────────────────────────────────────────────────

    #[test]
    fn test_new_transcript_is_empty() {
        let t = MessageTranscript::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert_eq!(t.ancestor_len(), 0);
    }

    #[test]
    fn test_with_ancestor_sets_boundary() {
        let a1 = make_human("ancestor-1");
        let a2 = make_human("ancestor-2");
        let t = MessageTranscript::new().with_ancestor(vec![a1.clone(), a2.clone()]);

        assert_eq!(t.len(), 2);
        assert_eq!(t.ancestor_len(), 2);
        assert!(t.get(a1.id()).is_some());
        assert!(t.get(a2.id()).is_some());
    }

    // ── ID 寻址 ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_id_indexing_o1_lookup() {
        let mut t = MessageTranscript::new();
        let m1 = make_human("msg-1");
        let m2 = make_human("msg-2");
        let m3 = make_human("msg-3");

        let id1 = t.append(m1);
        let id2 = t.append(m2);
        let id3 = t.append(m3);

        assert_eq!(t.len(), 3);
        // 所有 id 可找到
        assert!(t.get(id1).is_some());
        assert!(t.get(id2).is_some());
        assert!(t.get(id3).is_some());
        // 不存在的 id 返回 None
        let ghost_id = MessageId::new();
        assert!(t.get(ghost_id).is_none());
    }

    #[test]
    fn test_append_returns_correct_id() {
        let mut t = MessageTranscript::new();
        let msg = make_human("hello");
        let id = t.append(msg);
        // 返回的 id 应与消息内部 id 一致
        assert_eq!(t.get(id).unwrap().message.id(), id);
    }

    #[test]
    fn test_append_batch() {
        let mut t = MessageTranscript::new();
        let msgs = vec![make_human("a"), make_human("b"), make_human("c")];
        let ids = t.append_batch(msgs);

        assert_eq!(ids.len(), 3);
        assert_eq!(t.len(), 3);
        // 按 append 顺序存储
        assert_eq!(t.entries()[0].message.content(), "a");
        assert_eq!(t.entries()[1].message.content(), "b");
        assert_eq!(t.entries()[2].message.content(), "c");
    }

    // ── Staging 两阶段写入 ────────────────────────────────────────────────────

    #[test]
    fn test_staging_commit_atomic() {
        let mut t = MessageTranscript::new();
        // 先追加一条用户消息
        t.append(make_human("user question"));

        // Stage AI 消息
        let ai_msg = make_ai("thinking...");
        t.stage_ai_message(ai_msg);
        assert!(t.has_staged());
        // Staging 期间主列表不变
        assert_eq!(t.len(), 1);

        // Stage ToolResult
        t.stage_tool_result(make_tool_result("call_1", "result-1"));
        t.stage_tool_result(make_tool_result("call_2", "result-2"));

        // Commit
        t.commit_staged();
        assert!(!t.has_staged());
        // AI + 2 个 ToolResult = 3 条新消息
        assert_eq!(t.len(), 4);
        // 顺序：user → ai → tool1 → tool2
        assert_eq!(t.entries()[1].message.content(), "thinking...");
        assert_eq!(t.entries()[2].message.content(), "result-1");
        assert_eq!(t.entries()[3].message.content(), "result-2");
    }

    #[test]
    fn test_staging_discard() {
        let mut t = MessageTranscript::new();
        t.append(make_human("user question"));

        let ai_msg = make_ai("will be discarded");
        t.stage_ai_message(ai_msg);
        t.stage_tool_result(make_tool_result("call_1", "also discarded"));
        assert!(t.has_staged());

        t.discard_staged();
        assert!(!t.has_staged());
        // 主列表不变
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn test_stage_tool_result_without_ai_message_is_noop() {
        let mut t = MessageTranscript::new();
        t.stage_tool_result(make_tool_result("call_1", "ignored"));
        assert!(!t.has_staged(), "无 AI 消息时 tool_result 应被忽略");
    }

    #[test]
    fn test_stage_ai_message_overwrites_previous_staging() {
        let mut t = MessageTranscript::new();

        let ai1 = make_ai("first ai");
        t.stage_ai_message(ai1);
        t.stage_tool_result(make_tool_result("call_1", "result for first"));

        // 新的 AI 消息覆盖旧的 staging
        let ai2 = make_ai("second ai");
        t.stage_ai_message(ai2);
        // 旧的 tool_results 被丢弃
        t.stage_tool_result(make_tool_result("call_2", "result for second"));

        t.commit_staged();
        assert_eq!(t.len(), 2, "只有 ai2 + tool2，ai1 和 tool1 被丢弃");
        assert_eq!(t.entries()[0].message.content(), "second ai");
        assert_eq!(t.entries()[1].message.content(), "result for second");
    }

    #[test]
    fn test_commit_without_staging_is_noop() {
        let mut t = MessageTranscript::new();
        t.append(make_human("existing"));
        t.commit_staged(); // 无 staging，不应 panic
        assert_eq!(t.len(), 1);
    }

    // ── 标记系统 ───────────────────────────────────────────────────────────────

    #[test]
    fn test_truncated_flag() {
        let mut t = MessageTranscript::new();
        let id = t.append(make_human("truncatable"));
        assert_eq!(t.flags(id), MessageFlags::default());
        assert!(!t.flags(id).truncated);

        t.set_truncated(id, true);
        assert!(t.flags(id).truncated);
        assert!(!t.flags(id).excluded);

        t.set_truncated(id, false);
        assert!(!t.flags(id).truncated);
    }

    #[test]
    fn test_excluded_flag() {
        let mut t = MessageTranscript::new();
        let id = t.append(make_human("excludable"));

        t.set_excluded(id, true);
        assert!(t.flags(id).excluded);
        assert!(!t.flags(id).truncated);
    }

    #[test]
    fn test_clear_flags() {
        let mut t = MessageTranscript::new();
        let id = t.append(make_human("flagged"));
        t.set_truncated(id, true);
        t.set_excluded(id, true);

        t.clear_flags(id);
        let f = t.flags(id);
        assert!(!f.truncated);
        assert!(!f.excluded);
    }

    #[test]
    fn test_visible_messages_skips_excluded() {
        let mut t = MessageTranscript::new();
        let id1 = t.append(make_human("visible-1"));
        let id2 = t.append(make_human("will-be-excluded"));
        let id3 = t.append(make_human("visible-2"));

        t.set_excluded(id2, true);

        let visible = t.visible_messages();
        assert_eq!(visible.len(), 2, "excluded 消息应被过滤");
        assert_eq!(visible[0].id(), id1);
        assert_eq!(visible[1].id(), id3);
    }

    #[test]
    fn test_visible_messages_keeps_truncated() {
        let mut t = MessageTranscript::new();
        let id = t.append(make_human("truncated but visible"));
        t.set_truncated(id, true);

        let visible = t.visible_messages();
        assert_eq!(visible.len(), 1, "truncated 消息仍然可见");
    }

    // ── Ancestor 边界 ──────────────────────────────────────────────────────────

    #[test]
    fn test_ancestor_boundary_is_readonly_concept() {
        let a1 = make_human("ancestor");
        let own = make_human("own message");
        let mut t = MessageTranscript::new().with_ancestor(vec![a1]);

        t.append(own);
        assert_eq!(t.ancestor_len(), 1);
        assert_eq!(t.len(), 2);
    }

    // ── Rewind ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_rewind_to_truncates_correctly() {
        let mut t = MessageTranscript::new();
        let id1 = t.append(make_human("keep-1"));
        let id2 = t.append(make_human("keep-2"));
        let _id3 = t.append(make_human("will-remove-1"));
        let _id4 = t.append(make_human("will-remove-2"));

        t.rewind_to(id2).unwrap();
        assert_eq!(t.len(), 2, "rewind 后应只保留 id1 + id2");
        assert!(t.get(id1).is_some());
        assert!(t.get(id2).is_some());
    }

    #[test]
    fn test_rewind_clears_staging() {
        let mut t = MessageTranscript::new();
        let id = t.append(make_human("target"));
        t.append(make_human("after"));

        t.stage_ai_message(make_ai("staged ai"));
        assert!(t.has_staged());

        t.rewind_to(id).unwrap();
        assert!(!t.has_staged(), "rewind 应清空 staging");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn test_rewind_nonexistent_id_returns_error() {
        let mut t = MessageTranscript::new();
        t.append(make_human("only msg"));
        let ghost_id = MessageId::new();

        let result = t.rewind_to(ghost_id);
        assert!(result.is_err(), "rewind 不存在的 id 应返回错误");
    }

    #[test]
    fn test_rewind_into_ancestor_returns_error() {
        let a1 = make_human("ancestor");
        let mut t = MessageTranscript::new().with_ancestor(vec![a1.clone()]);
        t.append(make_human("own"));

        let result = t.rewind_to(a1.id());
        assert!(result.is_err(), "rewind 到祖先区域应返回错误");
    }

    // ── Rebuild ───────────────────────────────────────────────────────────────

    #[test]
    fn test_rebuild_preserves_flags() {
        let mut t = MessageTranscript::new();
        let id1 = t.append(make_human("msg-1"));
        let id2 = t.append(make_human("msg-2"));
        t.set_excluded(id1, true);

        // 重建：保留 id1 的 excluded 标记
        let entries = vec![
            (
                t.entries()[0].message.clone(),
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            ),
            (t.entries()[1].message.clone(), MessageFlags::default()),
        ];

        let t2 = t.rebuild(entries);
        assert_eq!(t2.len(), 2);
        assert!(t2.flags(id1).excluded, "rebuild 后标记应保留");
        assert!(!t2.flags(id2).excluded);
    }

    #[test]
    fn test_rebuild_preserves_ancestor_and_persistence() {
        let mut t = MessageTranscript::new().with_ancestor(vec![make_human("ancestor")]);
        t.append(make_human("own-1"));
        t.append(make_human("own-2"));

        let entries: Vec<(BaseMessage, MessageFlags)> = t
            .entries()
            .iter()
            .map(|e| (e.message.clone(), MessageFlags::default()))
            .collect();

        let t2 = t.rebuild(entries);
        assert_eq!(t2.ancestor_len(), 1, "rebuild 应保留 ancestor_len");
        assert_eq!(t2.len(), 3);
    }

    #[test]
    fn test_rebuild_clears_staging() {
        let mut t = MessageTranscript::new();
        t.append(make_human("msg"));
        t.stage_ai_message(make_ai("staged"));

        let entries = vec![(t.entries()[0].message.clone(), MessageFlags::default())];
        let t2 = t.rebuild(entries);
        assert!(!t2.has_staged(), "rebuild 应清空 staging");
    }
}
