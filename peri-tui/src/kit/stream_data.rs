//! 流式事件数据容器——TUI 内部类型，不共享给 ACP 层。
//!
//! Phase 0: 从 `peri-acp-types/src/event_data.rs` §4.1 内部化，消除跨 crate 共享。

use serde_json::Value;

/// `"text-chunk"` — 增量文本块。
#[derive(Debug, Clone)]
pub struct TuiTextChunk {
    pub text: String,
    /// ACP 协议 `agent_message_chunk` 携带的 `messageId`。
    /// 同一 message 的所有 chunk 共享同一 ID。变化时表示新消息开始。
    pub message_id: Option<String>,
    /// 当事件来自子 agent 时存在。
    pub agent_id: Option<String>,
}

/// `"reasoning-chunk"` — 增量推理/思考文本块。
#[derive(Debug, Clone)]
pub struct TuiReasoningChunk {
    pub text: String,
    /// ACP 协议 `agent_thought_chunk` 携带的 `messageId`。
    /// 与 `TuiTextChunk.message_id` 语义相同。
    pub message_id: Option<String>,
    /// 当事件来自子 agent 时存在。
    pub agent_id: Option<String>,
}

/// `"tool-started"` — 创建进行中的工具卡片。
#[derive(Debug, Clone)]
pub struct TuiToolStarted {
    pub tool_id: String,
    pub tool_name: String,
    pub input_summary: String,
    /// 原始结构化输入；仅供专属语义卡片消费，禁止从工具输出反推。
    pub raw_input: Value,
    /// 该工具所属 assistant 消息的稳定身份（ACP `_meta.peri.originMessageId`）。
    ///
    /// 纯工具响应轮次没有文本/推理 chunk，只有该字段能给出真实归属；超预算
    /// 工具卡据此进入可淘汰段。`None` 表示服务端未提供，客户端回退到当前
    /// 流式 assistant 身份，不得用 turn 身份冒充。
    pub origin_message_id: Option<String>,
    /// 当事件来自子 agent 时存在。
    pub agent_id: Option<String>,
}

/// `"tool-ended"` — 填充工具卡片结果。
#[derive(Debug, Clone)]
pub struct TuiToolEnded {
    pub tool_id: String,
    pub output_summary: String,
    pub is_error: bool,
    /// 当事件来自子 agent 时存在。
    pub agent_id: Option<String>,
}
