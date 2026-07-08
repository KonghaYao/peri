//! 流式事件数据容器——TUI 内部类型，不共享给 ACP 层。
//!
//! Phase 0: 从 `peri-acp-types/src/event_data.rs` §4.1 内部化，消除跨 crate 共享。

/// `"text-chunk"` — 增量文本块。
#[derive(Debug, Clone)]
pub struct TuiTextChunk {
    pub text: String,
    /// 当事件来自子 agent 时存在。
    pub agent_id: Option<String>,
}

/// `"reasoning-chunk"` — 增量推理/思考文本块。
#[derive(Debug, Clone)]
pub struct TuiReasoningChunk {
    pub text: String,
    /// 当事件来自子 agent 时存在。
    pub agent_id: Option<String>,
}

/// `"tool-started"` — 创建进行中的工具卡片。
#[derive(Debug, Clone)]
pub struct TuiToolStarted {
    pub tool_id: String,
    pub tool_name: String,
    pub input_summary: String,
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
