//! Langfuse 单轮追踪器（per-turn）。
#![allow(dead_code)]
//!
//! 本模块采用 Layered + Module-per-Feature 模式拆分：
//!
//! - `mod.rs`（本文件）：Facade，定义 `LangfuseTracer` 结构体、构造器、Turn 生命周期
//!   与其余杂项 `on_*` 事件处理方法。持有 config + 5 个简单字段 + 7 个子对象。
//!   （context.rs 已删除，数据结构迁移至各子对象模块）
//! - `event_builder.rs`：基础设施层，统一时间戳、UUID、try_add + warn 样板。
//! - `usage.rs`：TokenUsage → langfuse_usage_details 转换 + 重试 metadata 组装。
//! - `sampling.rs`：采样决策器。
//! - `stages.rs`：ReAct 5 阶段 Span 管理。
//! - `middleware.rs`：中间件链追踪器。
//! - `generation.rs`：LLM Generation 生命周期追踪器。
//! - `tool_batch.rs`：工具调用批次管理器。
//! - `registry.rs`：SubAgent 身份注册表(agent_id 查表归属,替代旧 LIFO 栈)。
//! - `compact.rs`：Compact 操作 Span 追踪器。
//! - `llm_events.rs`：Facade 的 LLM Generation 事件处理方法(`on_llm_*`)。
//! - `tool_events.rs`：Facade 的工具调用事件处理方法 + 工具批次上报(`emit_tools_flush`)。
//! - `span_events.rs`：Facade 的 Stage/Workflow/Compact/Middleware Span 事件处理方法。
//! - `subagent_events.rs`：Facade 的 SubAgent registry 入口与 AGENT obs 上报。
//!
//! 所有事件通过 session trait 的 try_add() 同步入队，保证事件顺序与调用顺序一致，
//! 确保 Langfuse 层级关系正确（父 span 先于子 span 入队）。

pub(crate) mod compact;
mod event_builder;
mod generation;
mod llm_events;
pub(crate) mod middleware;
pub(crate) mod registry;
mod sampling;
mod span_events;
pub mod stages;
mod subagent_events;
mod tool_batch;
mod tool_events;
mod usage;

use super::config::LangfuseConfig;
use super::session_like::LangfuseSessionLike;
use crate::langfuse::tracer::stages::StageHandle;
use event_builder::{new_uuid, now_rfc3339, try_add_or_warn_via_session, VERSION};
use langfuse_client::types::session::SessionBody;
use langfuse_client::types::{EventBody, ObservationLevel, TraceBody};
use langfuse_client::{IngestionEvent, ObservationBody, ObservationType, SpanBody};
use peri_agent::agent::events_v2::TurnErrorReason;

pub struct LangfuseTracer {
    pub(crate) session: std::sync::Arc<dyn LangfuseSessionLike>,
    /// Langfuse session_id = 会话的 thread_id，用于在 Langfuse UI 中按会话分组
    pub(crate) session_id: String,
    /// 当前对话轮次的 Trace ID（提前生成，所有观测对象共享）
    ///
    /// [不变量] trace_id 在 new() 时一次性生成，整个 turn 内所有事件共享，
    /// 禁止重新生成（会破坏 Langfuse 层级）。
    pub(crate) trace_id: String,
    /// 主 Agent Observation 的 ID
    pub(crate) agent_observation_id: String,
    /// 累积的最终回答
    pub(crate) final_answer: String,
    /// 配置（采样率、ErrorSpan 策略等）
    pub(crate) config: LangfuseConfig,
    /// 自定义 user 维度（来自 LANGFUSE_USER_ID 或 settings.json）
    pub(crate) user_id: Option<String>,
    // 7 个子对象
    pub(crate) sampling: crate::langfuse::tracer::sampling::SamplingDecider,
    pub(crate) stages: crate::langfuse::tracer::stages::StageSpans,
    pub(crate) middleware: crate::langfuse::tracer::middleware::MiddlewareTracer,
    pub(crate) generation: crate::langfuse::tracer::generation::GenerationTracker,
    pub(crate) tool_batch: crate::langfuse::tracer::tool_batch::ToolBatch,
    pub(crate) subagent: crate::langfuse::tracer::registry::SubagentRegistry,
    pub(crate) compact: crate::langfuse::tracer::compact::CompactSpan,
    /// 乱序场景:gate 重放的 StageStarted 产生的 stage handle
    /// (StageStarted 被注册闸门缓存后重放,bridge 的 active_stage 收不到;
    /// StageEnded 分支查 active_stage 失败时到此处领取)
    pub(crate) replayed_stage_handles: std::collections::HashMap<String, StageHandle>,
    /// 当前 stage-compact 阶段中是否有实际 compact 工作（micro/full）
    pub(crate) compact_work_done: bool,
    /// agent-run observation 的开始时间（推迟到 on_turn_end 创建时设置）
    pub(crate) agent_start_time: Option<String>,
    /// agent-run observation 的 input（on_turn_start 时暂存，on_turn_end 创建时写入）
    pub(crate) agent_input: Option<String>,
    /// 最近一次 TurnError 的稳定分类；原始错误正文绝不写入 Langfuse。
    pub(crate) last_error_class: Option<TurnErrorReason>,
}

impl LangfuseTracer {
    /// 从共享 Session + 配置构造 per-turn Tracer
    pub fn new(
        session: std::sync::Arc<dyn LangfuseSessionLike>,
        session_id: String,
        config: LangfuseConfig,
    ) -> Self {
        let rate = config.trace_sampling;
        let user_id = config.user_id.clone();
        Self {
            session,
            session_id,
            trace_id: uuid::Uuid::now_v7().to_string(),
            agent_observation_id: uuid::Uuid::now_v7().to_string(),
            final_answer: String::new(),
            config,
            user_id,
            sampling: crate::langfuse::tracer::sampling::SamplingDecider::new(rate),
            stages: crate::langfuse::tracer::stages::StageSpans::new(),
            middleware: crate::langfuse::tracer::middleware::MiddlewareTracer::new(),
            generation: crate::langfuse::tracer::generation::GenerationTracker::new(),
            tool_batch: crate::langfuse::tracer::tool_batch::ToolBatch::new(),
            subagent: crate::langfuse::tracer::registry::SubagentRegistry::new(),
            compact: crate::langfuse::tracer::compact::CompactSpan::new(),
            replayed_stage_handles: std::collections::HashMap::new(),
            compact_work_done: false,
            agent_start_time: None,
            agent_input: None,
            last_error_class: None,
        }
    }

    /// 使用预生成的 turn_id 构造 Tracer（避免 UUID v7 碰撞风险）
    pub fn new_with_turn_id(
        session: std::sync::Arc<dyn LangfuseSessionLike>,
        session_id: String,
        turn_id: String,
        config: LangfuseConfig,
    ) -> Self {
        Self {
            trace_id: turn_id,
            ..Self::new(session, session_id, config)
        }
    }

    // ── Turn 生命周期 ──────────────────────────────────────────────────────

    /// TurnError 事件：仅捕获稳定枚举分类，避免将 provider/tool 错误正文上报。
    pub fn on_turn_error(&mut self, reason: TurnErrorReason) {
        self.last_error_class = Some(reason);
    }

    /// 对话轮次开始：创建 Trace 根 span + Session + 推迟 agent-run Observation。
    /// 如有 user_id 配置，在 TraceCreate/SessionCreate 中设置 user 维度。
    pub fn on_turn_start(&mut self, input: &str) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }

        let start_time = now_rfc3339();
        tracing::info!(
            trace_id = %self.trace_id,
            agent_obs_id = %self.agent_observation_id,
            "langfuse: on_trace_start called"
        );

        // 始终发送 TraceCreate 作为 OTEL 根 span（agent-run 将挂在此 span 下）
        let trace_body = TraceBody {
            id: Some(self.trace_id.clone()),
            user_id: self.user_id.clone(),
            name: Some(format!("turn {}", self.trace_id)),
            session_id: Some(self.session_id.clone()),
            version: Some(VERSION.to_string()),
            ..Default::default()
        };
        let trace_event = IngestionEvent::TraceCreate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body: trace_body,
            metadata: None,
        };
        try_add_or_warn_via_session(
            &*self.session,
            trace_event,
            &self.trace_id,
            "turn TraceCreate",
        );

        // 显式创建 session（Langfuse UI 按 session 分组）
        let session_body = SessionBody {
            id: self.session_id.clone(),
            user_id: self.user_id.clone(),
            version: Some(VERSION.to_string()),
            ..Default::default()
        };
        let session_event = IngestionEvent::SessionCreate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body: session_body,
            metadata: None,
        };
        try_add_or_warn_via_session(
            &*self.session,
            session_event,
            &self.trace_id,
            "SessionCreate",
        );

        // 推迟 agent-run ObservationCreate 到 on_turn_end，
        // 避免 OTEL span 不可变导致 end_time 无法更新 → 0s latency
        self.agent_start_time = Some(start_time);
        self.agent_input = Some(input.to_string());
    }

    /// 对话轮次结束：更新 agent-run Observation 输出和结束时间，并强制 flush。
    ///
    /// [不变量] 这是 Tracer 唯一的 async 路径（最终 flush）。所有其他事件
    /// 均通过 session.try_add() 同步入队，保证顺序。tokio::spawn 使 flush 异步化，
    /// 不阻塞调用方。
    ///
    /// ErrorSpan 机制：当轮次以 error 结束时，始终发送 ErrorTurn span
    /// （即使该轮次未被采样），确保错误可观测。
    pub fn on_turn_end(&mut self, error_output: Option<&str>) -> tokio::task::JoinHandle<()> {
        use std::sync::Arc;

        // 先 flush tools batch，发出 batch span + 所有工具 span
        let flush = self.tool_batch.flush();
        self.emit_tools_flush(flush);

        // 兜底:清理未收 Stop 的活跃 subagent(pending/gate/残留 invocation),
        // 关闭其 AGENT obs(metadata 携带 incomplete_reason)。
        let closed_list = self.subagent.cleanup_turn_end();
        for closed in closed_list {
            self.emit_subagent_close(closed);
        }
        self.replayed_stage_handles.clear();

        let is_error = error_output.is_some();
        let sampled = self.sampling.should_emit(&self.trace_id, &self.session_id);
        let error_class = self
            .last_error_class
            .take()
            .map(|reason| reason.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // ErrorSpan：错误时始终发送（即使未采样），确保错误可观测
        if is_error && self.config.error_span_always {
            let turn_id = self.trace_id.clone();
            let error_out =
                serde_json::json!({"error_class": &error_class, "error_schema_version": 1});

            if !sampled {
                // 未采样时创建合成 Trace（复用 trace_id），让 error span 有父 trace
                let trace_body = TraceBody {
                    id: Some(turn_id.clone()),
                    name: Some(format!("turn {}", turn_id)),
                    user_id: self.user_id.clone(),
                    input: None,
                    output: Some(error_out.clone()),
                    session_id: Some(self.session_id.clone()),
                    release: None,
                    version: Some(VERSION.to_string()),
                    public: None,
                    metadata: Some(serde_json::json!({
                        "synthetic_error": true,
                        "error_class": &error_class,
                        "error_schema_version": 1,
                    })),
                    tags: None,
                    environment: None,
                    timestamp: Some(now_rfc3339()),
                };
                let trace_event = IngestionEvent::TraceCreate {
                    id: new_uuid(),
                    timestamp: now_rfc3339(),
                    body: trace_body,
                    metadata: None,
                };
                try_add_or_warn_via_session(
                    &*self.session,
                    trace_event,
                    &turn_id,
                    "ErrorTurn synthetic TraceCreate",
                );
            }

            // Emit ErrorTurn Span
            let error_span_id = new_uuid();
            let span_body = SpanBody {
                id: Some(error_span_id.clone()),
                trace_id: Some(turn_id.clone()),
                name: Some("ErrorTurn".to_string()),
                start_time: Some(now_rfc3339()),
                end_time: Some(now_rfc3339()),
                input: None,
                output: Some(error_out),
                metadata: Some(serde_json::json!({
                    "is_synthetic": !sampled,
                    "was_sampled": sampled,
                    "turn_id": &turn_id,
                    "error_class": &error_class,
                    "error_schema_version": 1,
                })),
                level: Some(ObservationLevel::Error),
                status_message: None,
                version: Some(VERSION.to_string()),
                environment: None,
                parent_observation_id: Some(self.agent_observation_id.clone()),
                session_id: Some(self.session_id.clone()),
            };
            let span_event = IngestionEvent::SpanCreate {
                id: new_uuid(),
                timestamp: now_rfc3339(),
                body: span_body,
                metadata: None,
            };
            try_add_or_warn_via_session(
                &*self.session,
                span_event,
                &self.trace_id,
                "ErrorTurn SpanCreate",
            );
        }

        // 未采样且非 error span 已处理：提前退出
        if !sampled {
            self.sampling.cleanup_turn(&self.trace_id);
            return tokio::spawn(async {});
        }

        let session = Arc::clone(&self.session);
        let trace_id = self.trace_id.clone();
        let agent_observation_id = self.agent_observation_id.clone();
        let output = if error_output.is_some() {
            Some(serde_json::json!({"error_class": &error_class}))
        } else {
            None
        };

        self.sampling.cleanup_turn(&self.trace_id);

        // 取出推迟到现在的 start_time 和 input。
        let agent_start_time = self.agent_start_time.take();
        let agent_input = self.agent_input.take();

        // agent-run ObservationCreate 同步入队（不放进 spawn 任务）：
        // 保证 on_turn_end 返回时全部事件已入队，调用方随后显式 flush() 即可
        // 一次性送达（Batcher::flush 经 mpsc FIFO，先入队者先发送）。
        // 短生命周期进程（-p/print 模式）在 run_session_loop 返回后调用
        // session.flush()，不依赖 spawn 任务的调度时序，避免 trace 随进程退出丢失。
        let end_time = now_rfc3339();
        let obs_body = ObservationBody {
            id: Some(agent_observation_id.clone()),
            trace_id: Some(trace_id.clone()),
            r#type: ObservationType::Agent,
            name: Some("agent-run".to_string()),
            start_time: agent_start_time,
            end_time: Some(end_time.clone()),
            input: agent_input.map(|s| serde_json::json!(s)),
            output,
            parent_observation_id: Some(trace_id.clone()),
            version: Some(VERSION.to_string()),
            ..Default::default()
        };
        let obs_event = IngestionEvent::ObservationCreate {
            id: new_uuid(),
            timestamp: end_time,
            body: obs_body,
            metadata: None,
        };
        try_add_or_warn_via_session(
            &*session,
            obs_event,
            &trace_id,
            "agent-run ObservationCreate",
        );

        // 最终 flush 保持 fire-and-forget（不阻塞执行管线；pump_done 已先行发出），
        // 常驻进程（TUI/ACP server）无需等待；短生命周期进程由调用方显式 flush。
        tokio::spawn(async move {
            if session.flush().await.is_err() {
                tracing::warn!(trace_id = %trace_id, "langfuse: session flush failed");
            }
        })
    }

    // ── 其他 langfuse v2 事件 ───────────────────────────────────────────────

    /// AI 推理内容 chunk
    pub fn on_ai_reasoning_chunk(&mut self, _text: &str) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        tracing::debug!(
            target: "langfuse::tracer",
            trace_id = %self.trace_id,
            text_len = _text.len(),
            "ai_reasoning_chunk"
        );
    }

    /// 预算阈值命中：创建 Langfuse Event（Warning 级别），含阈值、百分比、token 用量
    pub fn on_budget_threshold_hit(
        &mut self,
        threshold: &str,
        pct: f64,
        tokens_in: u64,
        tokens_out: u64,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }

        let event_body = EventBody {
            id: Some(new_uuid()),
            trace_id: Some(self.trace_id.clone()),
            name: Some("budget-threshold-hit".to_string()),
            start_time: Some(now_rfc3339()),
            input: Some(serde_json::json!({
                "threshold": threshold,
                "current_pct": pct,
                "tokens_in": tokens_in,
                "tokens_out": tokens_out,
            })),
            output: None,
            metadata: Some(serde_json::json!({
                "event_type": "budget_warning",
                "severity": threshold,
            })),
            level: Some(ObservationLevel::Warning),
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(self.agent_observation_id.clone()),
        };
        let event = IngestionEvent::EventCreate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body: event_body,
            metadata: None,
        };
        try_add_or_warn_via_session(
            &*self.session,
            event,
            &self.trace_id,
            "BudgetThresholdHit EventCreate",
        );
    }

    /// langfuse v2：Session 级别事件
    pub fn on_session_start(&mut self, _frozen_summary: &serde_json::Value) {
        tracing::debug!(
            target: "langfuse::tracer",
            session_id = %self.session_id,
            "on_session_start（stub）"
        );
    }
}

#[cfg(test)]
#[path = "tracer_test.rs"]
mod tests;
