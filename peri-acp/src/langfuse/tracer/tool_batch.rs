//! 同 turn 工具调用批次管理器。
//!
//! 将同一 LLM 响应中的所有工具调用聚合为一个 batch span。
//! - on_tool_start：lazy 创建 batch span，记录单个工具调用
//! - on_tool_end：标记单个工具调用结束
//! - flush：取出完整的 batch span 记录

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(crate) struct PendingTool {
    pub name: String,
    pub input: serde_json::Value,
    pub span_id: String,
    pub start_time: String,
    pub is_agent: bool,
}

pub(crate) struct ToolStartRecord {
    pub tool_span_id: String,
    pub tool_start_time: String,
    pub parent_span_id: String, // batch_span_id 或 agent_id（lazy 创建时）
}

pub(crate) struct ToolsBatchRecord {
    pub batch_span_id: String,
    pub batch_start_time: String,
    pub batch_end_time: String,
}

pub(crate) struct ToolBatch {
    pending_tools: HashMap<String, PendingTool>,
    batch_span_id: Option<String>,
    batch_start_time: Option<String>,
    batch_end_time: Option<String>,
}

impl ToolBatch {
    pub(crate) fn new() -> Self {
        Self {
            pending_tools: HashMap::new(),
            batch_span_id: None,
            batch_start_time: None,
            batch_end_time: None,
        }
    }

    pub(crate) fn on_tool_start(
        &mut self,
        tool_call_id: &str,
        name: &str,
        input: serde_json::Value,
    ) -> ToolStartRecord {
        let now = chrono::Utc::now().to_rfc3339();
        // lazy 创建 batch span
        if self.batch_span_id.is_none() {
            self.batch_span_id = Some(format!("batch_{}", uuid::Uuid::now_v7()));
            self.batch_start_time = Some(now.clone());
        }
        let tool_span_id = format!("obs_{}", uuid::Uuid::now_v7());
        let is_agent = name == "Agent" || name == "Task";
        let parent = self.batch_span_id.clone().unwrap();
        self.pending_tools.insert(
            tool_call_id.to_string(),
            PendingTool {
                name: name.to_string(),
                input,
                span_id: tool_span_id.clone(),
                start_time: now.clone(),
                is_agent,
            },
        );
        ToolStartRecord {
            tool_span_id,
            tool_start_time: now,
            parent_span_id: parent,
        }
    }

    pub(crate) fn on_tool_end(&mut self, tool_call_id: &str) -> Option<PendingTool> {
        self.pending_tools.remove(tool_call_id)
    }

    pub(crate) fn record_end_time(&mut self, end_time: String) {
        self.batch_end_time = Some(end_time);
    }

    pub(crate) fn flush(&mut self) -> Option<ToolsBatchRecord> {
        let span_id = self.batch_span_id.take()?;
        let start = self.batch_start_time.take()?;
        let end = self
            .batch_end_time
            .take()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        Some(ToolsBatchRecord {
            batch_span_id: span_id,
            batch_start_time: start,
            batch_end_time: end,
        })
    }

    pub(crate) fn is_agent_tool(&self, tool_call_id: &str) -> bool {
        self.pending_tools
            .get(tool_call_id)
            .map(|p| p.is_agent)
            .unwrap_or(false)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pending_tools.is_empty()
    }
}

#[cfg(test)]
#[path = "tool_batch_test.rs"]
mod tests;
