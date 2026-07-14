//! 同 turn 工具调用批次管理器。
//!
//! 将同一 LLM 响应中的所有工具调用聚合为一个 batch span。
//! - on_tool_start：lazy 创建 batch span，记录单个工具调用
//! - on_tool_end：标记单个工具调用结束，移入 completed_tools
//! - flush：返回 batch + 所有已完成工具，供调用方发送 SpanCreate 事件

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(crate) struct PendingTool {
    pub name: String,
    pub input: serde_json::Value,
    pub span_id: String,
    pub start_time: String,
    pub is_agent: bool,
}

/// 已完成工具记录（供 flush 时一并发出 SpanCreate）
#[derive(Debug, Clone)]
pub(crate) struct CompletedTool {
    pub name: String,
    pub input: serde_json::Value,
    pub output: String,
    pub span_id: String,
    pub start_time: String,
    pub end_time: String,
    pub is_agent: bool,
    pub is_error: bool,
}

pub(crate) struct ToolStartRecord {
    pub tool_span_id: String,
    pub tool_start_time: String,
    pub parent_span_id: String, // batch_span_id
}

pub(crate) struct ToolsBatchRecord {
    pub batch_span_id: String,
    pub batch_start_time: String,
    pub batch_end_time: String,
}

/// flush 返回的完整工具批次（含 batch span 信息 + 所有已完成工具）
pub(crate) struct ToolsBatchFlush {
    pub batch: Option<ToolsBatchRecord>,
    pub tools: Vec<CompletedTool>,
    /// batch span 的父 observation ID（首次 on_tool_start 时捕获的 stage-act span_id）
    pub parent_observation_id: String,
}

pub(crate) struct ToolBatch {
    pending_tools: HashMap<String, PendingTool>,
    completed_tools: Vec<CompletedTool>,
    batch_span_id: Option<String>,
    batch_start_time: Option<String>,
    batch_end_time: Option<String>,
    parent_observation_id: Option<String>,
}

impl ToolBatch {
    pub(crate) fn new() -> Self {
        Self {
            pending_tools: HashMap::new(),
            completed_tools: Vec::new(),
            batch_span_id: None,
            batch_start_time: None,
            batch_end_time: None,
            parent_observation_id: None,
        }
    }

    pub(crate) fn on_tool_start(
        &mut self,
        tool_call_id: &str,
        name: &str,
        input: serde_json::Value,
        parent_observation_id: &str,
    ) -> ToolStartRecord {
        let now = chrono::Utc::now().to_rfc3339();
        // lazy 创建 batch span，同时记录当前 stage 的 parent span_id
        if self.batch_span_id.is_none() {
            self.batch_span_id = Some(format!("batch_{}", uuid::Uuid::now_v7()));
            self.batch_start_time = Some(now.clone());
            self.parent_observation_id = Some(parent_observation_id.to_string());
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

    /// 工具调用结束：将 PendingTool 从待处理中移除，存入 completed_tools。
    /// 同时自动记录 batch_end_time（最后一个工具结束的时间）
    pub(crate) fn on_tool_end(
        &mut self,
        tool_call_id: &str,
        output: &str,
        is_error: bool,
    ) -> Option<CompletedTool> {
        self.pending_tools.remove(tool_call_id).map(|pt| {
            let now = chrono::Utc::now().to_rfc3339();
            self.batch_end_time = Some(now.clone());
            let ct = CompletedTool {
                name: pt.name,
                input: pt.input,
                output: output.to_string(),
                span_id: pt.span_id,
                start_time: pt.start_time,
                end_time: now,
                is_agent: pt.is_agent,
                is_error,
            };
            self.completed_tools.push(ct.clone());
            ct
        })
    }

    pub(crate) fn record_end_time(&mut self, end_time: String) {
        self.batch_end_time = Some(end_time);
    }

    pub(crate) fn flush(&mut self) -> ToolsBatchFlush {
        let batch = self.batch_span_id.take().map(|id| {
            let start = self
                .batch_start_time
                .take()
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
            let end = self
                .batch_end_time
                .take()
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
            ToolsBatchRecord {
                batch_span_id: id,
                batch_start_time: start,
                batch_end_time: end,
            }
        });
        let tools = std::mem::take(&mut self.completed_tools);
        let parent_id = self.parent_observation_id.take().unwrap_or_default();
        self.pending_tools.clear();
        ToolsBatchFlush {
            batch,
            tools,
            parent_observation_id: parent_id,
        }
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
