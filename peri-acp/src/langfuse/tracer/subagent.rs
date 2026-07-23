//! SubAgent 嵌套调用栈管理器。
//!
//! 管理多层 SubAgent 的层级关系：
//! - begin_subagent：推入新上下文（新 observation_id / agent_id / 独立 ToolBatch）
//! - end_subagent：弹出栈顶上下文，返回 SubagentEnd
//! - current_agent_id：返回栈顶 agent_id，栈空时 fallback 到主 agent
//! - current_tool_batch_mut：返回当前层 ToolBatch 的可变引用（Main 或 Sub）
//! - is_agent_tool_anywhere：检查 tool_call_id 是否在任意层被标记为 agent 工具

use super::tool_batch::{ToolBatch, ToolsBatchFlush};

pub(crate) struct SubAgentContext {
    pub observation_id: String,
    pub agent_id: String,
    pub start_time: String,
    pub input: serde_json::Value,
    pub tool_batch: ToolBatch,
    /// 子 agent 是否已实际启动（接收过至少一个 subagent 事件，如 StageStarted / LLMStarted）
    pub has_started: bool,
    /// bg subagent 场景下，on_tool_end 时暂存的 agent tool output，
    /// 等 subagent 真正启动并被清理时用于发送 ObservationCreate
    pub deferred_output: Option<String>,
}

pub(crate) struct SubagentEnd {
    pub observation_id: String,
    pub agent_id: String,
    pub start_time: String,
    pub input: serde_json::Value,
    /// bg subagent 场景下暂存的 agent tool output，供 on_turn_end 清理时使用
    pub deferred_output: Option<String>,
}

/// 主层 / 子层 ToolBatch 引用（双路径写入收口）。
pub(crate) enum ToolBatchRef<'a> {
    Main(&'a mut ToolBatch),
    Sub(&'a mut ToolBatch),
}

impl<'a> std::ops::Deref for ToolBatchRef<'a> {
    type Target = ToolBatch;

    fn deref(&self) -> &ToolBatch {
        match self {
            ToolBatchRef::Main(t) | ToolBatchRef::Sub(t) => t,
        }
    }
}

impl<'a> std::ops::DerefMut for ToolBatchRef<'a> {
    fn deref_mut(&mut self) -> &mut ToolBatch {
        match self {
            ToolBatchRef::Main(t) | ToolBatchRef::Sub(t) => t,
        }
    }
}

pub(crate) struct SubagentStack {
    stack: Vec<SubAgentContext>,
}

impl SubagentStack {
    pub(crate) fn new() -> Self {
        Self { stack: Vec::new() }
    }

    pub(crate) fn current_agent_id(&self, fallback_main: &str) -> String {
        self.stack
            .last()
            .map(|c| c.observation_id.clone())
            .unwrap_or_else(|| fallback_main.to_string())
    }

    pub(crate) fn current_tool_batch_mut<'a>(
        &'a mut self,
        main_tb: &'a mut ToolBatch,
    ) -> ToolBatchRef<'a> {
        match self.stack.last_mut() {
            Some(top) => ToolBatchRef::Sub(&mut top.tool_batch),
            None => ToolBatchRef::Main(main_tb),
        }
    }

    pub(crate) fn is_agent_tool_anywhere(&self, main_tb: &ToolBatch, tool_call_id: &str) -> bool {
        if main_tb.is_agent_tool(tool_call_id) {
            return true;
        }
        self.stack
            .iter()
            .any(|c| c.tool_batch.is_agent_tool(tool_call_id))
    }

    pub(crate) fn begin_subagent(&mut self, input: &serde_json::Value) {
        let observation_id = format!("obs_{}", uuid::Uuid::now_v7());
        let agent_id = format!("agent_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        self.stack.push(SubAgentContext {
            observation_id,
            agent_id,
            start_time,
            input: input.clone(),
            tool_batch: ToolBatch::new(),
            has_started: false,
            deferred_output: None,
        });
    }

    pub(crate) fn end_subagent(&mut self) -> Option<SubagentEnd> {
        let c = self.stack.pop()?;
        Some(SubagentEnd {
            observation_id: c.observation_id,
            agent_id: c.agent_id,
            start_time: c.start_time,
            input: c.input,
            deferred_output: c.deferred_output,
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// 标记栈顶 subagent 已实际启动。用于 bg subagent 恢复：
    /// on_tool_end 检测到 has_started=false → 不弹栈；
    /// 后续第一个 subagent 事件（StageStarted / LLMStarted）到达时调用此方法恢复活跃状态。
    pub(crate) fn mark_top_started(&mut self) {
        if let Some(top) = self.stack.last_mut() {
            top.has_started = true;
        }
    }

    /// 返回栈顶 subagent 的 has_started 状态。栈空返回 false。
    pub(crate) fn top_has_started(&self) -> bool {
        self.stack.last().map(|c| c.has_started).unwrap_or(false)
    }

    /// bg subagent 场景：将 agent tool output 记录到栈顶 context。
    /// 供 on_turn_end 清理时发送观察（ObservationCreate）使用。
    pub(crate) fn record_tool_output(&mut self, output: &str) {
        if let Some(top) = self.stack.last_mut() {
            top.deferred_output = Some(output.to_string());
        }
    }

    /// 刷新所有栈中 subagent 的 tool_batch，返回 flush 结果列表。
    /// 用于 on_turn_end 清理 bg subagent 残留的工具批次。
    pub(crate) fn flush_all_subagent_tool_batches(&mut self) -> Vec<ToolsBatchFlush> {
        self.stack
            .iter_mut()
            .map(|ctx| ctx.tool_batch.flush())
            .collect()
    }

    pub(crate) fn depth(&self) -> usize {
        self.stack.len()
    }
}

#[cfg(test)]
#[path = "subagent_test.rs"]
mod tests;
