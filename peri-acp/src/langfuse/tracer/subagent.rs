//! SubAgent 嵌套调用栈管理器。
//!
//! 管理多层 SubAgent 的层级关系：
//! - begin_subagent：推入新上下文（新 observation_id / agent_id / 独立 ToolBatch）
//! - end_subagent：弹出栈顶上下文，返回 SubagentEnd
//! - current_agent_id：返回栈顶 agent_id，栈空时 fallback 到主 agent
//! - current_tool_batch_mut：返回当前层 ToolBatch 的可变引用（Main 或 Sub）
//! - is_agent_tool_anywhere：检查 tool_call_id 是否在任意层被标记为 agent 工具

use super::tool_batch::ToolBatch;

pub(crate) struct SubAgentContext {
    pub observation_id: String,
    pub agent_id: String,
    pub start_time: String,
    pub input: serde_json::Value,
    pub tool_batch: ToolBatch,
}

pub(crate) struct SubagentEnd {
    pub observation_id: String,
    pub agent_id: String,
    pub start_time: String,
    pub input: serde_json::Value,
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

    pub(crate) fn is_agent_tool_anywhere(
        &self,
        main_tb: &ToolBatch,
        tool_call_id: &str,
    ) -> bool {
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
        });
    }

    pub(crate) fn end_subagent(&mut self) -> Option<SubagentEnd> {
        let c = self.stack.pop()?;
        Some(SubagentEnd {
            observation_id: c.observation_id,
            agent_id: c.agent_id,
            start_time: c.start_time,
            input: c.input,
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    pub(crate) fn depth(&self) -> usize {
        self.stack.len()
    }
}

#[cfg(test)]
#[path = "subagent_test.rs"]
mod tests;
