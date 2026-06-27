//! Pipeline 状态类型：PartialAiMessage 与辅助结构。
//!
//! `PartialAiMessage` 是「当前 ReAct 迭代进行中的增量」容器，合并了原 5 个
//! `current_ai_*` 字段。它在流式事件到达时由 `partial_mut()` 懒初始化，
//! 在 `commit_iteration` 边界被整体丢弃——下一轮迭代会重新分配新的实例。

use std::collections::HashMap;

use peri_agent::messages::ToolCallRequest;

// ─── PendingTool / CompletedTool（从 mod.rs 迁移） ────────────────────────────

/// 已开始但未结束的工具调用
pub(crate) struct PendingTool {
    /// 用于工具调用匹配，reconcile 阶段读取
    #[allow(dead_code)]
    pub tool_call_id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// ToolEnd 后、TurnCommitted 前的工具结果（用于在 reconcile gap 期间显示）
pub(crate) struct CompletedTool {
    pub tool_call_id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub output: String,
    pub is_error: bool,
}

// ─── PartialAiMessage ────────────────────────────────────────────────────────

/// 当前 ReAct 迭代进行中的 AI 消息增量。
///
/// 在流式事件到达时由 `MessagePipeline::partial_mut()` 懒初始化。每次
/// `commit_iteration(messages)` 在迭代边界（`TurnCommitted` 事件）触发时，
/// 整体丢弃此结构——下一轮迭代会重新分配一个新的空实例。
///
/// 字段语义：
/// - `text` / `reasoning`：当前迭代的流式 LLM 输出
/// - `tool_calls`：当前迭代的 ToolCallRequest 列表（顺序即时间线）
/// - `pending_tools`：已 ToolStart 但未 ToolEnd 的工具（key = tool_call_id）
/// - `completed_tools`：已 ToolEnd 但 TurnCommitted 尚未到达的工具结果
/// - `finalized`：当前迭代是否已 finalize（ToolStart 后即标记）
#[derive(Default)]
pub(crate) struct PartialAiMessage {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCallRequest>,
    pub pending_tools: HashMap<String, PendingTool>,
    pub completed_tools: Vec<CompletedTool>,
    pub finalized: bool,
}

impl PartialAiMessage {
    /// 是否有可见的流式内容（文本或推理）
    pub fn has_streaming_content(&self) -> bool {
        !self.text.trim().is_empty() || !self.reasoning.is_empty()
    }

    /// 是否有待处理的 tool_calls
    pub fn has_pending_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }

    /// 标记当前迭代已 finalize（ToolStart 后调用，防止重复 finalize）
    pub fn finalize(&mut self) {
        self.finalized = true;
    }

    /// 是否有任意内容（用于判断 finalize 时是否需要写入 transcript）
    pub fn has_any_content(&self) -> bool {
        !self.text.trim().is_empty() || !self.reasoning.is_empty() || !self.tool_calls.is_empty()
    }
}
