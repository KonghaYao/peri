//! Langfuse 单轮追踪器（per-turn）。
//!
//! 本模块采用 Layered + Module-per-Feature 模式拆分：
//!
//! - `mod.rs`（本文件）：Facade，定义 `LangfuseTracer` 结构体与 `new()` 构造器，
//!   持有 11 个状态字段（trace_id / agent_observation_id / generation_data /
//!   pending_tools / final_answer 等）。所有 pub on_* 方法签名保持不变，下游
//!   `executor.rs` 调用点零改动。
//! - `context.rs`：纯数据结构（PendingTool / SubAgentContext / CompactSpanContext /
//!   RetryAttempt）。
//! - `event_builder.rs`：基础设施层，统一时间戳、UUID、try_add + warn 样板。
//! - `subagent_stack.rs`：SubAgent 嵌套栈操作（push/pop、current_agent_id、
//!   current_tools_context、flush_tools_batch）。
//! - `usage.rs`：TokenUsage → langfuse_usage_details 转换 + 重试 metadata 组装。
//! - `llm_handler.rs`：on_llm_start / on_llm_end / on_llm_retrying。
//! - `tool_handler.rs`：on_tool_start / on_tool_end / on_text_chunk。
//! - `compact_handler.rs`：on_compact_start / on_compact_end。
//! - `trace_lifecycle.rs`：on_trace_start / on_trace_end（async flush）。
//!
//! 持有对 LangfuseSession 的引用，复用 client/batcher。
//! 生命周期：从 execute_prompt 开始 → ExecutorEvent::Done/Error 时结束。
//!
//! 所有事件通过 `batcher.try_add()` 同步入队，保证事件顺序与调用顺序一致，
//! 确保 Langfuse 层级关系正确（父 span 先于子 span 入队）。

mod compact_handler;
mod context;
mod event_builder;
mod llm_handler;
mod subagent_stack;
mod tool_handler;
mod trace_lifecycle;
mod usage;

use std::collections::HashMap;

use super::session::LangfuseSession;
// 重新导出数据结构，便于测试通过 super::* 访问（保持向后兼容）
pub(crate) use context::{
    CompactSpanContext, GenerationCached, PendingTool, RetryAttempt, SubAgentContext,
};

pub struct LangfuseTracer {
    pub(crate) session: std::sync::Arc<LangfuseSession>,
    /// Langfuse session_id = 会话的 thread_id，用于在 Langfuse UI 中按会话分组
    pub(crate) session_id: String,
    /// 当前对话轮次的 Trace ID（提前生成，所有观测对象共享）
    ///
    /// [不变量] trace_id 在 new() 时一次性生成，整个 turn 内所有事件共享，
    /// 禁止重新生成（会破坏 Langfuse 层级）。
    pub(crate) trace_id: String,
    /// 主 Agent Observation 的 ID
    pub(crate) agent_observation_id: String,
    /// step → GenerationCached（gen_id / messages / tools / start_time / raw_body）
    ///
    /// raw_body 字段由 on_llm_request_payload 写入（紧随 on_llm_start 之后）；
    /// on_llm_end 时优先用 raw_body 作为 Generation input，fallback 到
    /// messages+tools 抽象序列化。
    pub(crate) generation_data: HashMap<usize, GenerationCached>,
    /// 工具调用缓冲数据：tool_call_id → PendingTool
    pub(crate) pending_tools: HashMap<String, PendingTool>,
    /// 当前批次工具组 Span ID
    pub(crate) tools_batch_span_id: Option<String>,
    /// 当前批次工具组开始时间
    pub(crate) tools_batch_start_time: Option<String>,
    /// 当前批次工具组最后一次 ToolEnd 时间
    pub(crate) tools_batch_end_time: Option<String>,
    /// 累积的最终回答
    pub(crate) final_answer: String,
    /// SubAgent 栈：保存当前活动的 subagent observation IDs
    /// 支持 subagent 嵌套调用（subagent 中再调用 subagent）
    pub(crate) subagent_stack: Vec<SubAgentContext>,
    /// Compact Span 上下文（非 None 表示正在 compact 操作中）
    pub(crate) compact_span: Option<CompactSpanContext>,
    /// 当前活跃的 LLM step 编号（用于将 LlmRetrying 关联到正确 generation）
    pub(crate) active_step: Option<usize>,
    /// 当前 step 的 LLM 重试记录（每次 on_llm_start 清空）
    pub(crate) retry_attempts: Vec<RetryAttempt>,
}

impl LangfuseTracer {
    /// 从共享 Session 构造 per-turn Tracer
    pub fn new(session: std::sync::Arc<LangfuseSession>, session_id: String) -> Self {
        Self {
            session,
            session_id,
            trace_id: uuid::Uuid::now_v7().to_string(),
            agent_observation_id: uuid::Uuid::now_v7().to_string(),
            generation_data: HashMap::new(),
            pending_tools: HashMap::new(),
            tools_batch_span_id: None,
            tools_batch_start_time: None,
            tools_batch_end_time: None,
            final_answer: String::new(),
            subagent_stack: Vec::new(),
            compact_span: None,
            active_step: None,
            retry_attempts: Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "tracer_test.rs"]
mod tests;
