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
//! - **session_context**：自有 `HashMap`，从 `ctx.session_context` 克隆
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

    /// 内部 recall 缓冲区，每个 hook 执行后 drain 到 ctx.recall_buffer
    recall_buffer: Vec<String>,

    /// 自有 TokenTracker（与当前 snapshot_to_agent_state 语义一致）
    token_tracker: TokenTracker,

    /// compact 边界标记（内部维护）
    ancestor_len: usize,

    /// session 上下文键值对（自有 HashMap，克隆自 ctx.session_context）
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
            .transcript
            .read()
            .visible_messages()
            .into_iter()
            .cloned()
            .collect();
        let session_context = ctx.session_context.read().clone();
        Self {
            ctx,
            messages_cache,
            recall_buffer: Vec::new(),
            token_tracker: ctx.token_tracker.read().clone(),
            ancestor_len: 0,
            session_context,
        }
    }
}

impl MiddlewareState for AgentContext<'_> {
    fn cwd(&self) -> &str {
        &self.ctx.turn.cwd
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
        self.ctx.transcript.write().append(message.clone());
        self.messages_cache.push(message);
    }

    /// 发出 warn 日志，仅插入 cache（不写入 transcript）。
    /// 此 API 在生产环境零调用。
    fn prepend_message(&mut self, message: BaseMessage) {
        tracing::warn!("AgentContext::prepend_message called — change NOT reflected in transcript");
        self.messages_cache.insert(0, message);
    }

    /// 发出 warn 日志，返回 cache 可变引用（不触及 transcript）。
    /// 此 API 在生产环境零调用。
    fn messages_mut(&mut self) -> &mut Vec<BaseMessage> {
        tracing::warn!("AgentContext::messages_mut called — changes NOT reflected in transcript");
        &mut self.messages_cache
    }

    fn current_step(&self) -> usize {
        self.ctx.turn.current_step()
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
        &self.ctx.queue
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::stages::StageContext;
    use crate::messages::MessageContent;
    use crate::session::store::FrozenContext;
    use crate::session::Session;
    use std::sync::Arc;

    fn make_context() -> StageContext {
        let cwd: Arc<str> = Arc::from("/tmp/test");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        StageContext::new(turn, session.transcript(), session.queue().clone())
    }

    #[test]
    fn test_from_stage_copies_visible_messages() {
        let ctx = make_context();
        ctx.transcript
            .write()
            .append(BaseMessage::human(MessageContent::text("hello")));

        let ac = AgentContext::from_stage(&ctx);
        assert_eq!(ac.messages().len(), 1);
        assert_eq!(ac.messages()[0].content(), "hello");
    }

    #[test]
    fn test_from_stage_excluded_messages_filtered() {
        let ctx = make_context();
        let id = ctx
            .transcript
            .write()
            .append(BaseMessage::human(MessageContent::text("excluded")));
        ctx.transcript.write().set_excluded(id, true);
        ctx.transcript
            .write()
            .append(BaseMessage::human(MessageContent::text("visible")));

        let ac = AgentContext::from_stage(&ctx);
        assert_eq!(
            ac.messages().len(),
            1,
            "excluded 消息不应进入 AgentContext 视野"
        );
        assert_eq!(ac.messages()[0].content(), "visible");
    }

    #[test]
    fn test_add_message_dual_writes_transcript_and_cache() {
        let ctx = make_context();
        ctx.transcript
            .write()
            .append(BaseMessage::human(MessageContent::text("old")));

        let mut ac = AgentContext::from_stage(&ctx);
        ac.add_message(BaseMessage::human(MessageContent::text("new")));

        // cache 应包含 new
        assert_eq!(ac.messages().len(), 2);
        assert_eq!(ac.messages()[1].content(), "new");

        // transcript 也应包含 new（双写同步）
        let transcript = ctx.transcript.read();
        assert_eq!(transcript.len(), 2, "transcript 应同时包含 old + new");
        assert_eq!(transcript.entries()[0].message.content(), "old");
        assert_eq!(transcript.entries()[1].message.content(), "new");
    }

    #[test]
    fn test_cwd_delegates_to_turn() {
        let ctx = make_context();
        let ac = AgentContext::from_stage(&ctx);
        assert_eq!(ac.cwd(), "/tmp/test");
    }

    #[test]
    fn test_current_step_delegates_to_turn() {
        let ctx = make_context();
        let ac = AgentContext::from_stage(&ctx);
        assert_eq!(ac.current_step(), 0);
    }

    #[test]
    fn test_set_cwd_is_noop() {
        let ctx = make_context();
        let mut ac = AgentContext::from_stage(&ctx);
        ac.set_cwd("/other".to_string());
        // turn.cwd 不受影响（no-op）
        assert_eq!(ac.cwd(), "/tmp/test");
    }

    #[test]
    fn test_set_current_step_is_noop() {
        let ctx = make_context();
        let mut ac = AgentContext::from_stage(&ctx);
        ac.set_current_step(42);
        // turn.current_step 不受影响（no-op）
        assert_eq!(ac.current_step(), 0);
    }

    #[test]
    fn test_get_set_context_on_owned_hashmap() {
        let ctx = make_context();
        {
            let mut guard = ctx.session_context.write();
            guard.insert("session_id".to_string(), "s1".to_string());
        }
        let mut ac = AgentContext::from_stage(&ctx);

        // get_context 读取 from_stage 时的快照
        assert_eq!(ac.get_context("session_id"), Some("s1"));

        // set_context 修改自有 HashMap
        ac.set_context("key".to_string(), "value".to_string());
        assert_eq!(ac.get_context("key"), Some("value"));

        // ctx.session_context 不受影响（自有克隆）
        let guard = ctx.session_context.read();
        assert_eq!(guard.get("key"), None);
    }

    #[test]
    fn test_token_tracker_is_default() {
        let ctx = make_context();
        let ac = AgentContext::from_stage(&ctx);
        assert_eq!(ac.token_tracker().total_input_tokens, 0);
        assert!(ac.token_tracker().last_usage.is_none());
    }

    #[test]
    fn test_token_tracker_mut_is_mutable() {
        let ctx = make_context();
        let mut ac = AgentContext::from_stage(&ctx);
        ac.token_tracker_mut().total_input_tokens = 100;
        assert_eq!(ac.token_tracker().total_input_tokens, 100);
    }

    #[test]
    fn test_push_and_drain_recall() {
        let ctx = make_context();
        let mut ac = AgentContext::from_stage(&ctx);

        ac.push_recall("recall-1".to_string());
        ac.push_recall("recall-2".to_string());

        let drained = ac.drain_recall();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0], "recall-1");
        assert_eq!(drained[1], "recall-2");

        // drain 后 buffer 清空
        assert!(ac.drain_recall().is_empty());
    }

    #[test]
    fn test_store_and_thread_id_are_none() {
        let ctx = make_context();
        let ac = AgentContext::from_stage(&ctx);
        assert!(ac.store().is_none());
        assert!(ac.own_thread_id().is_none());
    }

    #[test]
    fn test_v2_queue_is_shared() {
        let ctx = make_context();
        let ac = AgentContext::from_stage(&ctx);
        // 验证 queue 是同一个实例（通过地址比较或行为验证）
        assert!(ac.v2_queue().is_empty());
    }

    #[test]
    fn test_messages_mut_emits_warning() {
        let ctx = make_context();
        let mut ac = AgentContext::from_stage(&ctx);
        ac.add_message(BaseMessage::human(MessageContent::text("msg1")));

        // messages_mut 应只影响 cache，不写入 transcript
        let cache = ac.messages_mut();
        cache.push(BaseMessage::human(MessageContent::text("cache-only")));

        // transcript 不应有 cache-only 消息
        let transcript = ctx.transcript.read();
        assert_eq!(transcript.len(), 1, "messages_mut 不应写入 transcript");
        assert!(!transcript
            .entries()
            .iter()
            .any(|e| e.message.content() == "cache-only"));
    }

    #[test]
    fn test_prepend_message_emits_warning() {
        let ctx = make_context();
        let mut ac = AgentContext::from_stage(&ctx);
        ac.add_message(BaseMessage::human(MessageContent::text("msg1")));

        // prepend_message 应只影响 cache，不写入 transcript
        ac.prepend_message(BaseMessage::human(MessageContent::text("prepended")));

        assert_eq!(ac.messages().len(), 2);
        assert_eq!(ac.messages()[0].content(), "prepended");

        // transcript 不应有 prepended 消息
        let transcript = ctx.transcript.read();
        assert_eq!(transcript.len(), 1);
    }
}
