//! 生产 v2 middleware 状态适配。
//!
//! messages 是 hook 链开始时的可见消息快照；add_message 双写 transcript
//! 与缓存，replace_message 按稳定 ID 修改缓存，再由 before_agent runner
//! 在链结束后统一 reconcile（包括错误路径）。recall 由 runner drain。
//! cwd/step 委托 TurnContext，queue 与 local_tools 委托 session/runtime。
//! 不暴露无法回写的 token/context 快照或 cwd/step setter。

use crate::agent::stages::StageContext;
use crate::messages::{BaseMessage, MessageId};
use crate::middleware::state::MiddlewareState;
use crate::session::MessageQueue;

/// MiddlewareState 的 StageContext 薄封装
pub struct AgentContext<'a> {
    /// 委托给 StageContext（实时状态）
    ctx: &'a StageContext,

    /// 从 transcript.visible_messages() 克隆的消息缓存
    messages_cache: Vec<BaseMessage>,

    /// 首次 Receive 的输入身份，只供本次 before_agent 链读取。
    input_message_ids: Option<&'a [MessageId]>,

    /// 标记已有消息是否被替换（用于 before_agent runner reconcile）
    messages_modified: bool,

    /// 内部 recall 缓冲区，每个 hook 执行后 drain 到 ctx.recall_buffer
    recall_buffer: Vec<String>,
}

impl<'a> AgentContext<'a> {
    /// 从 StageContext 构造 AgentContext
    ///
    /// - 一次性克隆 transcript 的 visible_messages 到 messages_cache
    pub fn from_stage(ctx: &'a StageContext) -> Self {
        let messages_cache = ctx
            .session
            .transcript
            .read()
            .visible_messages()
            .into_iter()
            .cloned()
            .collect();
        Self {
            ctx,
            messages_cache,
            input_message_ids: None,
            messages_modified: false,
            recall_buffer: Vec::new(),
        }
    }

    /// 为本次输入准备绑定精确批次，Some(empty) 表示不处理历史用户消息。
    pub(crate) fn with_input_message_ids(mut self, input_message_ids: &'a [MessageId]) -> Self {
        self.input_message_ids = Some(input_message_ids);
        self
    }

    /// 获取消息缓存快照（供 runner reconcile 到 transcript 使用）
    pub fn messages_cache(&self) -> &[BaseMessage] {
        &self.messages_cache
    }

    /// 已有消息是否被替换（供 before_agent runner 决定是否需要 reconcile）
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

    fn messages(&self) -> &[BaseMessage] {
        &self.messages_cache
    }

    fn input_message_ids(&self) -> Option<&[MessageId]> {
        self.input_message_ids
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

    fn replace_message(&mut self, message: BaseMessage) -> bool {
        let Some(existing) = self
            .messages_cache
            .iter_mut()
            .find(|existing| existing.id() == message.id())
        else {
            return false;
        };
        *existing = message;
        self.messages_modified = true;
        true
    }

    fn current_step(&self) -> usize {
        self.ctx.session.turn.current_step()
    }

    fn push_recall(&mut self, item: String) {
        self.recall_buffer.push(item);
    }

    fn drain_recall(&mut self) -> Vec<String> {
        std::mem::take(&mut self.recall_buffer)
    }

    fn v2_queue(&self) -> &MessageQueue {
        &self.ctx.session.queue
    }

    fn inbox_handle(&self) -> Option<&peri_acp_types::session::InboxHandle> {
        self.ctx.async_ctx.inbox_handle.as_ref()
    }

    fn local_tools(&self) -> Option<&crate::agent::stages::SharedToolMap> {
        Some(&self.ctx.runtime.tools)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "agent_context_test.rs"]
mod tests;
