//! MessagePipeline 生命周期管理。

use peri_agent::messages::BaseMessage;

use crate::ui::message_view::MessageViewModel;

impl super::MessagePipeline {
    /// 标记当前 AI 轮次结束
    pub fn done(&mut self) {
        self.finalize_current_ai();
        // finalize 后重置 finalized 标志，允许下一轮迭代在同一 partial 上继续流式
        if let Some(partial) = self.partial.as_mut() {
            partial.finalized = false;
        }
        // v2 路径下没有携带消息列表的完整 StateSnapshot 事件，工具调用状态
        // （`partial.tool_calls` + `partial.completed_tools`）是 Done 后工具显示的
        // 唯一来源。Done 时不清空 partial，让 `build_tail_vms` 仍能渲染本轮工具，
        // 直到下一轮 `begin_round()` 清空。与 `frozen_subagent_vms` 保留语义一致。
        self.adaptive_policy.reset();
        self.force_flush_block();
        self.throttle_last_fire = None;
        self.active_batch = None;
        self.drain_subagent_stack();
    }

    /// 中断：finalize 当前状态并清理残留
    pub fn interrupt(&mut self) {
        self.finalize_current_ai();
        if let Some(partial) = self.partial.as_mut() {
            partial.finalized = false;
        }
        self.partial = None;
        self.adaptive_policy.reset();
        self.force_flush_block();
        self.throttle_last_fire = None;
        self.active_batch = None;
        self.drain_subagent_stack();
    }

    pub fn clear(&mut self) {
        self.transcript.clear();
        self.partial = None;
        self.subagent_stack.clear();
        self.frozen_subagent_vms.clear();
        self.active_batch = None;
    }

    /// 清空并释放所有内部 buffer 的 capacity
    pub fn shrink_to_fit(&mut self) {
        self.transcript.shrink_to_fit();
        if let Some(partial) = self.partial.as_mut() {
            partial.text.shrink_to_fit();
            partial.reasoning.shrink_to_fit();
            partial.tool_calls.shrink_to_fit();
            partial.pending_tools.shrink_to_fit();
            partial.completed_tools.shrink_to_fit();
        }
        self.subagent_stack.shrink_to_fit();
        self.frozen_subagent_vms.shrink_to_fit();
    }

    /// 当前迭代是否有可见的流式内容
    pub fn has_streaming_content(&self) -> bool {
        self.partial
            .as_ref()
            .is_some_and(|p| p.has_streaming_content())
    }

    /// 当前迭代是否有待处理的 tool_calls
    pub fn has_pending_tool_calls(&self) -> bool {
        self.partial
            .as_ref()
            .is_some_and(|p| p.has_pending_tool_calls())
    }

    /// 是否在 SubAgent 执行中
    pub fn in_subagent(&self) -> bool {
        // 后台 agent 不会阻塞父 agent 的 Done 事件
        self.subagent_stack
            .last()
            .is_some_and(|s| s.is_running && !s.is_background)
    }

    /// 本轮是否已收到过 TurnCommitted / set_completed 提交信号
    pub fn has_snapshot_this_round(&self) -> bool {
        self.has_committed_this_round
    }

    /// 诊断用：返回 frozen_subagent_vms 的数量
    pub fn frozen_subagent_vms_count(&self) -> usize {
        self.frozen_subagent_vms.len()
    }

    /// 可变访问 frozen_subagent_vms（供 handle_background_task_completed 同步更新状态）
    pub fn frozen_subagent_vms_mut(&mut self) -> &mut Vec<MessageViewModel> {
        &mut self.frozen_subagent_vms
    }

    // ── 轮次管理 ──────────────────────────────────────────────────────────────

    /// 标记新一轮对话开始。由 submit_message() 调用。
    pub fn begin_round(&mut self) {
        self.transcript_len_at_round_start = self.transcript.len();
        self.has_committed_this_round = false;
        self.adaptive_policy.reset();
        self.throttle_last_fire = None;
        // 清空上一轮的 frozen_subagent_vms，防止跨轮次累积导致新轮次的
        // SubAgentGroup 按位置错误匹配到旧轮的 frozen VM（而非本轮的）。
        self.frozen_subagent_vms.clear();
        // v2 路径下没有携带消息列表的完整 StateSnapshot，`begin_round` 是新一轮
        // 开始的唯一信号。必须清空上一轮残留的 partial（v1 路径下 `commit_iteration`
        // 已先清空，此处幂等）。`done()` 不清空 partial，故必须在 `begin_round`
        // 兜底，否则上一轮的 AssistantBubble + 工具会混入新一轮 tail_vms。
        self.partial = None;
    }

    /// 获取规范 transcript（用于持久化）
    pub fn completed_messages(&self) -> &[BaseMessage] {
        &self.transcript
    }

    /// 迭代边界提交：将 transcript 替换为权威快照，清空 partial。
    ///
    /// 由 v2 stages 在每次 Act 阶段结束时通过 `TurnCommitted` 事件触发。
    /// 「替换」而非「extend」——因为 v2 的 `finalized_messages` 是全量快照，
    /// 不是增量。这消除了旧 `set_completed` 被多次调用累积消息的隐患。
    pub fn commit_iteration(&mut self, messages: Vec<BaseMessage>) {
        self.transcript = messages;
        self.partial = None;
        self.has_committed_this_round = true;
    }

    /// v1 兼容别名：等价于 `commit_iteration`。
    ///
    /// 旧路径（持久化 / v1 executor）仍可能通过 `StateSnapshot` 触发；
    /// 保留此方法名避免下游大量改动，内部直接走新路径。
    pub fn set_completed(&mut self, messages: Vec<BaseMessage>) {
        self.commit_iteration(messages);
    }

    /// 返回 transcript 的条数和估算堆内存（字节），供 /gc 诊断用
    pub fn completed_stats(&self) -> (usize, usize) {
        let count = self.transcript.len();
        let bytes =
            super::super::super::command::core::gc::estimate_messages_heap(&self.transcript);
        (count, bytes)
    }

    /// 从外部加载全量 BaseMessages（历史恢复 / compact 后覆盖），
    /// 与 `commit_iteration` 同构：替换 transcript + 清空 partial。
    pub fn restore_completed(&mut self, messages: Vec<BaseMessage>) {
        self.transcript = messages;
        self.transcript_len_at_round_start = self.transcript.len();
        self.has_committed_this_round = false;
        self.partial = None;
    }
}
