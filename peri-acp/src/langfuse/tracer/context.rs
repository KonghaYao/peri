//! Per-turn 追踪器的纯数据结构定义。
//!
//! 本文件仅承载数据结构（无业务逻辑），供 LangfuseTracer 及各 handler 模块共享。
//! 保持字段可见性为 `pub(crate)` 或 `pub(super)`，以便测试和 handler 访问。

use std::collections::HashMap;

use peri_agent::{messages::BaseMessage, tools::ToolDefinition};

/// 工具调用的中间缓冲数据（start 时存储，end 时取出组合成完整 span-create）
pub(crate) struct PendingTool {
    pub(crate) span_id: String,
    pub(crate) name: String,
    pub(crate) input: serde_json::Value,
    pub(crate) start_time: String,
    /// 父 span ID（= 所属批次的 tools_batch_span_id）
    pub(crate) parent_span_id: String,
}

/// LLM Generation 缓存数据（on_llm_start 写入，on_llm_request_payload 补充 raw_body，
/// on_llm_end 取出生成 GenerationCreate 事件）。
///
/// 取代旧元组 `(String, Vec<BaseMessage>, Vec<ToolDefinition>, String)`，
/// 具名字段降低新增字段时的漏改风险（validate agent 风险点 #1）。
#[derive(Default)]
pub(crate) struct GenerationCached {
    /// Langfuse Generation ID（new_uuid）
    pub(crate) gen_id: Option<String>,
    /// LLM 输入消息快照（fallback 序列化用）
    pub(crate) messages: Vec<BaseMessage>,
    /// LLM 工具定义快照（fallback 序列化用）
    pub(crate) tools: Vec<ToolDefinition>,
    /// RFC3339 开始时间戳
    pub(crate) start_time: Option<String>,
    /// Provider 实际请求体（on_llm_request_payload 写入；on_llm_end 优先用此字段，
    /// fallback 到 messages+tools 抽象序列化）
    pub(crate) raw_body: Option<serde_json::Value>,
}

/// SubAgent 追踪上下文
pub(crate) struct SubAgentContext {
    /// SubAgent 的 Observation ID
    pub(crate) observation_id: String,
    /// SubAgent 的 agent_id（如 "code-reviewer"）
    pub(crate) agent_id: String,
    /// SubAgent 开始时间（延迟到 end_subagent 时与 ObservationCreate 一起发送）
    pub(crate) start_time: String,
    /// SubAgent 输入（prompt 预览）
    pub(crate) input: serde_json::Value,
    /// 当前 subagent 下的 tools batch 信息
    pub(crate) tools_batch_span_id: Option<String>,
    pub(crate) tools_batch_start_time: Option<String>,
    pub(crate) tools_batch_end_time: Option<String>,
    /// SubAgent 下的工具调用缓冲
    pub(crate) pending_tools: HashMap<String, PendingTool>,
}

/// Compact Span 上下文（CompactStarted → CompactCompleted/Error 期间存续）
pub(crate) struct CompactSpanContext {
    /// Compact Span 的 Observation ID
    pub(crate) span_id: String,
    /// Compact 开始时间
    pub(crate) start_time: String,
}

/// 单次 LLM 重试记录
pub(crate) struct RetryAttempt {
    pub(crate) attempt: usize,
    pub(crate) max_attempts: usize,
    pub(crate) delay_ms: u64,
    pub(crate) error: String,
}
