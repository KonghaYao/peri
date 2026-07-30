//! AgentContext — MiddlewareState 的薄封装实现，桥接 v2 StageContext ↔ v1 middleware
//!
//! ## 背景
//!
//! v2 stages 以 `MessageTranscript` 为唯一消息真相源，v1 middleware 通过 `MiddlewareState`
//! trait 操作消息。当前 `AgentState` 在每次 middleware hook 时都需要 snapshot→restore，
//! restore 阶段整体 rebuild transcript（O(n) 全量 entries + id_index + flags 重建）。
//!
//! ## 设计
//!
//! `AgentContext` 是 `StageContext` 的薄封装：
//!
//! - **messages_cache**：`from_stage()` 时从 transcript 克隆 visible_messages（一次性开销）
//! - **recall_buffer**：内部缓冲区，每个 hook 执行后由 runner drain 到 `ctx.recall_buffer`
//! - **token_tracker**：自有 `TokenTracker::default()`，与当前 `snapshot_to_agent_state` 语义一致
//! - **session_context**：自有 `HashMap`，从 `ctx.session.session_context` 克隆
//!
//! 与旧 `AgentState` 方案的关键区别：**不再 restore**——middleware 通过 `add_message()`
//! 直接双写 transcript + cache，消除 `restore_from_agent_state.rebuild()` 的 O(n) 开销。
//!
//! ## 语义说明
//!
//! - `messages_mut()` / `prepend_message()`：发出 `tracing::warn!`，仅修改 cache，不写入 transcript
//!   （这两个 API 在生产环境零调用，保留以便测试兼容）
//! - `set_cwd()` / `set_current_step()`：no-op（v2 中由 TurnContext 管理）
//! - `store()` / `own_thread_id()`：返回 None（与 `snapshot_to_agent_state` 语义一致）

use std::collections::HashMap;
use std::sync::Arc;

use crate::agent::stages::StageContext;
use crate::agent::token::TokenTracker;
use crate::messages::BaseMessage;
use crate::middleware::state::MiddlewareState;
use crate::session::MessageQueue;
use crate::thread::{ThreadId, ThreadStore};

/// MiddlewareState 的 StageContext 薄封装
pub struct AgentContext<'a> {
    /// 委托给 StageContext（实时状态）
    ctx: &'a StageContext,

    /// 从 transcript.visible_messages() 克隆的消息缓存
    messages_cache: Vec<BaseMessage>,

    /// 标记 messages_mut() 是否被调用（用于 runner reconcile）
    messages_modified: bool,

    /// 内部 recall 缓冲区，每个 hook 执行后 drain 到 ctx.recall_buffer
    recall_buffer: Vec<String>,

    /// 自有 TokenTracker（与当前 snapshot_to_agent_state 语义一致）
    token_tracker: TokenTracker,

    /// compact 边界标记（内部维护）
    ancestor_len: usize,

    /// session 上下文键值对（自有 HashMap，克隆自 ctx.session.session_context）
    session_context: HashMap<String, String>,
}

impl<'a> AgentContext<'a> {
    /// 从 StageContext 构造 AgentContext
    ///
    /// - 一次性克隆 transcript 的 visible_messages 到 messages_cache
    /// - 克隆 session_context（自有 HashMap，get_context 无需持锁）
    /// - TokenTracker 为默认值（P0 #2 将迁移到 StageContext）
    pub fn from_stage(ctx: &'a StageContext) -> Self {
        let messages_cache = ctx
            .session
            .transcript
            .read()
            .visible_messages()
            .into_iter()
            .cloned()
            .collect();
        let session_context = ctx.session.session_context.read().clone();
        Self {
            ctx,
            messages_cache,
            messages_modified: false,
            recall_buffer: Vec::new(),
            token_tracker: ctx.compact.token_tracker.read().clone(),
            ancestor_len: 0,
            session_context,
        }
    }

    /// 获取消息缓存快照（供 runner reconcile 到 transcript 使用）
    pub fn messages_cache(&self) -> &[BaseMessage] {
        &self.messages_cache
    }

    /// messages_mut() 是否被调用过（供 runner 决定是否需要 reconcile）
    pub fn messages_modified(&self) -> bool {
        self.messages_modified
    }

    /// 将缓存变更同步回 transcript（调用 replace_by_id 逐条更新）
    pub fn reconcile_to_transcript(
        &self,
        transcript: &mut crate::session::transcript::MessageTranscript,
    ) {
        if !self.messages_modified {
            return;
        }
        for msg in &self.messages_cache {
            transcript.replace_by_id(msg.clone());
        }
    }
}

impl MiddlewareState for AgentContext<'_> {
    fn cwd(&self) -> &str {
        &self.ctx.session.turn.cwd
    }

    fn set_cwd(&mut self, _cwd: String) {
        // no-op：v2 中 cwd 由 TurnContext 管理，middleware 不可修改
    }

    fn messages(&self) -> &[BaseMessage] {
        &self.messages_cache
    }

    /// 双写 transcript + cache。
    ///
    /// INVARIANT：transcript.append 和 cache.push 必须同时成功或同时失败。
    /// 当前 `Vec::push` 在内存耗尽外不会失败，因此无需 rollback。
    fn add_message(&mut self, message: BaseMessage) {
        // INVARIANT: transcript.append 和 cache.push 必须同时成功或同时失败
        self.ctx.session.transcript.write().append(message.clone());
        self.messages_cache.push(message);
    }

    /// 发出 warn 日志，仅插入 cache（不写入 transcript）。
    /// 此 API 在生产环境零调用。
    fn prepend_message(&mut self, message: BaseMessage) {
        tracing::warn!("AgentContext::prepend_message called — change NOT reflected in transcript");
        self.messages_cache.insert(0, message);
    }

    /// 发出 warn 日志，返回 cache 可变引用（不触及 transcript）。
    /// 调用方负责在 hook 执行后通过 runner 将变更 reconcile 到 transcript。
    fn messages_mut(&mut self) -> &mut Vec<BaseMessage> {
        self.messages_modified = true;
        &mut self.messages_cache
    }

    fn current_step(&self) -> usize {
        self.ctx.session.turn.current_step()
    }

    fn set_current_step(&mut self, _step: usize) {
        // no-op：v2 中 step 由 TurnContext 管理，middleware 不可修改
    }

    fn get_context(&self, key: &str) -> Option<&str> {
        self.session_context.get(key).map(|s| s.as_str())
    }

    fn set_context(&mut self, key: String, value: String) {
        self.session_context.insert(key, value);
    }

    fn token_tracker(&self) -> &TokenTracker {
        &self.token_tracker
    }

    fn token_tracker_mut(&mut self) -> &mut TokenTracker {
        &mut self.token_tracker
    }

    fn push_recall(&mut self, item: String) {
        self.recall_buffer.push(item);
    }

    fn drain_recall(&mut self) -> Vec<String> {
        std::mem::take(&mut self.recall_buffer)
    }

    fn ancestor_len(&self) -> usize {
        self.ancestor_len
    }

    fn store(&self) -> Option<&Arc<dyn ThreadStore>> {
        None
    }

    fn own_thread_id(&self) -> Option<&ThreadId> {
        None
    }

    fn v2_queue(&self) -> &MessageQueue {
        &self.ctx.session.queue
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "agent_context_test.rs"]
mod tests;
