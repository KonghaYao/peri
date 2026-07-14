//! Langfuse 单轮追踪器（per-turn）。
//!
//! 本模块采用 Layered + Module-per-Feature 模式拆分：
//!
//! - `mod.rs`（本文件）：Facade，定义 `LangfuseTracer` 结构体与全部
//!   `on_*` 事件处理方法。持有 config + 5 个简单字段 + 7 个子对象。
//! （context.rs 已删除，数据结构迁移至各子对象模块）
//! - `event_builder.rs`：基础设施层，统一时间戳、UUID、try_add + warn 样板。
//! - `usage.rs`：TokenUsage → langfuse_usage_details 转换 + 重试 metadata 组装。
//! - `sampling.rs`：采样决策器。
//! - `stages.rs`：ReAct 5 阶段 Span 管理。
//! - `middleware.rs`：中间件链追踪器。
//! - `generation.rs`：LLM Generation 生命周期追踪器。
//! - `tool_batch.rs`：工具调用批次管理器。
//! - `subagent.rs`：SubAgent 嵌套调用栈管理器。
//! - `compact.rs`：Compact 操作 Span 追踪器。
//!
//! 所有事件通过 session trait 的 try_add() 同步入队，保证事件顺序与调用顺序一致，
//! 确保 Langfuse 层级关系正确（父 span 先于子 span 入队）。

mod compact;
mod event_builder;
mod generation;
pub(crate) mod middleware;
mod sampling;
pub(crate) mod stages;
mod subagent;
mod tool_batch;
mod usage;

use super::config::LangfuseConfig;
use super::session_like::LangfuseSessionLike;
use event_builder::{new_uuid, now_rfc3339, try_add_or_warn_via_session, VERSION};
use langfuse_client::types::{ObservationLevel, TraceBody};
use langfuse_client::{GenerationBody, IngestionEvent, ObservationBody, ObservationType, SpanBody};
use peri_agent::agent::events::{
    CompactStrategy, CompactTrigger, MiddlewareHook, Stage, StageStatus,
};
use peri_agent::llm::types::TokenUsage;
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;

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
    // 7 个子对象
    pub(crate) sampling: crate::langfuse::tracer::sampling::SamplingDecider,
    pub(crate) stages: crate::langfuse::tracer::stages::StageSpans,
    pub(crate) middleware: crate::langfuse::tracer::middleware::MiddlewareTracer,
    pub(crate) generation: crate::langfuse::tracer::generation::GenerationTracker,
    pub(crate) tool_batch: crate::langfuse::tracer::tool_batch::ToolBatch,
    pub(crate) subagent: crate::langfuse::tracer::subagent::SubagentStack,
    pub(crate) compact: crate::langfuse::tracer::compact::CompactSpan,
}

impl LangfuseTracer {
    /// 从共享 Session + 配置构造 per-turn Tracer
    pub fn new(
        session: std::sync::Arc<dyn LangfuseSessionLike>,
        session_id: String,
        config: LangfuseConfig,
    ) -> Self {
        let rate = config.trace_sampling;
        Self {
            session,
            session_id,
            trace_id: uuid::Uuid::now_v7().to_string(),
            agent_observation_id: uuid::Uuid::now_v7().to_string(),
            final_answer: String::new(),
            config,
            sampling: crate::langfuse::tracer::sampling::SamplingDecider::new(rate),
            stages: crate::langfuse::tracer::stages::StageSpans::new(),
            middleware: crate::langfuse::tracer::middleware::MiddlewareTracer::new(),
            generation: crate::langfuse::tracer::generation::GenerationTracker::new(),
            tool_batch: crate::langfuse::tracer::tool_batch::ToolBatch::new(),
            subagent: crate::langfuse::tracer::subagent::SubagentStack::new(),
            compact: crate::langfuse::tracer::compact::CompactSpan::new(),
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

    /// 对话轮次开始：创建 agent-run Observation（根 observation）
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

        let body = ObservationBody {
            id: Some(self.agent_observation_id.clone()),
            trace_id: Some(self.trace_id.clone()),
            r#type: ObservationType::Agent,
            name: Some("agent-run".to_string()),
            start_time: Some(start_time),
            end_time: None,
            completion_start_time: None,
            parent_observation_id: None,
            input: Some(serde_json::json!(input)),
            output: None,
            metadata: None,
            model: None,
            model_parameters: None,
            level: None,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::ObservationCreate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body,
            metadata: None,
        };
        if let Err(e) = self.session.try_add(event) {
            tracing::warn!(error = %e, trace_id = %self.trace_id, "langfuse: agent-run observation 入队失败（背压丢弃）");
        }
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

        // 先 flush tools batch，确保所有工具 span 在 ObservationUpdate 之前入队
        let _tools_record = self.tool_batch.flush();

        let is_error = error_output.is_some();
        let sampled = self.sampling.should_emit(&self.trace_id, &self.session_id);

        // ErrorSpan：错误时始终发送（即使未采样），确保错误可观测
        if is_error && self.config.error_span_always {
            let error_msg = error_output.unwrap_or("unknown error").to_string();
            let turn_id = self.trace_id.clone();

            if !sampled {
                // 未采样时创建合成 Trace（复用 trace_id），让 error span 有父 trace
                let trace_body = TraceBody {
                    id: Some(turn_id.clone()),
                    name: Some(format!("turn {}", &turn_id)),
                    user_id: None,
                    input: None,
                    output: Some(serde_json::json!({"error": &error_msg})),
                    session_id: Some(self.session_id.clone()),
                    release: None,
                    version: Some(VERSION.to_string()),
                    public: None,
                    metadata: Some(serde_json::json!({"synthetic_error": true})),
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
                output: Some(serde_json::json!({"error": &error_msg})),
                metadata: Some(serde_json::json!({
                    "is_synthetic": !sampled,
                    "was_sampled": sampled,
                    "turn_id": &turn_id,
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
        let output = if let Some(err) = error_output {
            err.to_string()
        } else {
            std::mem::take(&mut self.final_answer)
        };

        self.sampling.cleanup_turn(&self.trace_id);

        tokio::spawn(async move {
            let end_time = now_rfc3339();

            let obs_body = ObservationBody {
                id: Some(agent_observation_id.clone()),
                trace_id: Some(trace_id.clone()),
                r#type: ObservationType::Agent,
                name: Some("agent-run".to_string()),
                output: Some(serde_json::json!(output)),
                end_time: Some(end_time.clone()),
                version: Some(VERSION.to_string()),
                ..Default::default()
            };
            let obs_event = IngestionEvent::ObservationUpdate {
                id: new_uuid(),
                timestamp: end_time,
                body: obs_body,
                metadata: None,
            };
            if let Err(e) = session.try_add(obs_event) {
                tracing::warn!(error = %e, trace_id = %trace_id, obs_id = %agent_observation_id, "langfuse: agent-run observation 更新失败");
            }
            if let Err(e) = session.flush().await {
                tracing::warn!(error = %e, trace_id = %trace_id, "langfuse: session flush 失败");
            }
        })
    }

    // ── LLM Generation 事件 ──────────────────────────────────────────────────

    /// LLM 调用开始
    pub fn on_llm_start(
        &mut self,
        step: usize,
        messages: &[BaseMessage],
        tools: &[ToolDefinition],
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        self.generation
            .on_llm_start(step, messages.to_vec(), tools.to_vec());
    }

    /// LLM 请求体接收：紧随 on_llm_start 之后，缓存 Provider 实际请求体
    pub fn on_llm_request_payload(&mut self, step: usize, body: std::sync::Arc<serde_json::Value>) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        self.generation.on_llm_request_payload(step, body);
    }

    /// LLM 调用结束：同步创建 Generation 事件
    pub fn on_llm_end(
        &mut self,
        step: usize,
        model: &str,
        _provider: &str,
        output: &str,
        usage: Option<&TokenUsage>,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }

        let gen_end = match self.generation.on_llm_end(step) {
            Some(g) => g,
            None => return,
        };

        let end_time = now_rfc3339();
        let usage_details: Option<std::collections::HashMap<String, i32>> =
            usage.map(usage::build_usage_details);
        let usage_map: Option<std::collections::HashMap<String, serde_json::Value>> =
            usage.map(|u| {
                let mut map = std::collections::HashMap::new();
                map.insert("input".to_string(), serde_json::json!(u.input_tokens));
                map.insert("output".to_string(), serde_json::json!(u.output_tokens));
                map.insert(
                    "total".to_string(),
                    serde_json::json!(u.input_tokens + u.output_tokens),
                );
                map
            });

        let current_agent_id = self.subagent.current_agent_id(&self.agent_observation_id);

        let gen_body = GenerationBody {
            id: Some(gen_end.gen_id),
            trace_id: Some(self.trace_id.clone()),
            name: Some(format!("step-{}", step)),
            start_time: Some(gen_end.start_time),
            end_time: Some(end_time.clone()),
            input: Some(gen_end.input_json),
            output: Some(serde_json::json!(output)),
            metadata: gen_end.retry_metadata,
            level: None,
            status_message: None,
            parent_observation_id: Some(current_agent_id),
            version: Some(VERSION.to_string()),
            environment: None,
            completion_start_time: None,
            model: Some(model.to_string()),
            model_parameters: None,
            usage_details,
            usage: usage_map,
            cost_details: None,
            prompt_name: None,
            prompt_version: None,
            session_id: Some(self.session_id.clone()),
        };

        let event = IngestionEvent::GenerationCreate {
            id: new_uuid(),
            timestamp: end_time,
            body: gen_body,
            metadata: None,
        };
        try_add_or_warn_via_session(
            &*self.session,
            event,
            &self.trace_id,
            "LLM GenerationCreate",
        );
    }

    /// LLM 重试：记录重试信息，最终在 on_llm_end 时写入 Generation metadata
    pub fn on_llm_retrying(
        &mut self,
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        error: &str,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        self.generation
            .on_llm_retrying(attempt, max_attempts, delay_ms, error);
    }

    // ── 工具调用事件 ────────────────────────────────────────────────────────

    /// TextChunk 事件：累积最终回答（不区分采样，始终累积）
    pub fn on_text_chunk(&mut self, chunk: &str) {
        self.final_answer.push_str(chunk);
    }

    /// 工具调用开始
    pub fn on_tool_start(&mut self, tool_call_id: &str, name: &str, input: &serde_json::Value) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let _record = self
            .tool_batch
            .on_tool_start(tool_call_id, name, input.clone());
        // 如果是 Agent 工具调用，压入 subagent 栈
        if name == "Agent" || name == "Task" {
            self.subagent.begin_subagent(input);
        }
    }

    /// 工具调用结束：同步创建 tool observation
    pub fn on_tool_end(&mut self, tool_call_id: &str, _output: &str, _is_error: bool) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        // 如果是 Agent 工具结束，弹出 subagent 栈
        if self.tool_batch.is_agent_tool(tool_call_id) {
            if let Some(end) = self.subagent.end_subagent() {
                // SubAgent ended: emit ObservationCreate for the subagent
                let body = ObservationBody {
                    id: Some(end.observation_id),
                    trace_id: Some(self.trace_id.clone()),
                    r#type: ObservationType::Agent,
                    name: Some(format!("subagent-{}", end.agent_id)),
                    start_time: Some(end.start_time),
                    end_time: Some(now_rfc3339()),
                    completion_start_time: None,
                    parent_observation_id: Some(self.agent_observation_id.clone()),
                    input: Some(end.input),
                    output: Some(serde_json::json!(_output)),
                    metadata: None,
                    model: None,
                    model_parameters: None,
                    level: None,
                    status_message: None,
                    version: Some(VERSION.to_string()),
                    environment: None,
                    session_id: Some(self.session_id.clone()),
                };
                let event = IngestionEvent::ObservationCreate {
                    id: new_uuid(),
                    timestamp: now_rfc3339(),
                    body,
                    metadata: None,
                };
                try_add_or_warn_via_session(
                    &*self.session,
                    event,
                    &self.trace_id,
                    "SubAgent ObservationCreate",
                );
            }
        }
        let _pending = self.tool_batch.on_tool_end(tool_call_id);
    }

    // ── Compact 事件 ────────────────────────────────────────────────────────

    /// Compact 开始：创建 compact Span
    pub fn on_compact_start(&mut self) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        // Use default strategy/trigger for backward compat
        let strategy = CompactStrategy::Full;
        let trigger = CompactTrigger::Auto;
        let start = self.compact.on_start(strategy, trigger);
        let span_body = SpanBody {
            id: Some(start.span_id),
            trace_id: Some(self.trace_id.clone()),
            name: Some("compact".to_string()),
            start_time: Some(start.start_time),
            end_time: None,
            input: None,
            output: None,
            metadata: None,
            level: None,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(self.agent_observation_id.clone()),
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::SpanCreate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body: span_body,
            metadata: None,
        };
        try_add_or_warn_via_session(&*self.session, event, &self.trace_id, "Compact SpanCreate");
    }

    /// Compact 完成/错误：更新 compact Span
    pub fn on_compact_end(
        &mut self,
        summary: &str,
        files_count: usize,
        skills_count: usize,
        micro_cleared: usize,
        is_error: bool,
        error_message: &str,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let ctx = match self.compact.on_end() {
            Some(c) => c,
            None => return,
        };

        let end_time = now_rfc3339();
        let output = if is_error {
            serde_json::json!({"error": error_message})
        } else {
            serde_json::json!({
                "summary": summary,
                "files_count": files_count,
                "skills_count": skills_count,
                "micro_cleared": micro_cleared,
            })
        };
        let level = if is_error {
            Some(ObservationLevel::Error)
        } else {
            None
        };

        let span_body = SpanBody {
            id: Some(ctx.span_id),
            trace_id: Some(self.trace_id.clone()),
            name: Some("compact".to_string()),
            start_time: Some(ctx.start_time),
            end_time: Some(end_time.clone()),
            input: None,
            output: Some(output),
            metadata: Some(serde_json::json!({
                "strategy": format!("{:?}", ctx.strategy),
                "trigger": format!("{:?}", ctx.trigger),
            })),
            level,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(self.agent_observation_id.clone()),
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::SpanUpdate {
            id: new_uuid(),
            timestamp: end_time,
            body: span_body,
            metadata: None,
        };
        try_add_or_warn_via_session(&*self.session, event, &self.trace_id, "Compact SpanUpdate");
    }

    // ── Stage 5 阶段 Span 事件 ──────────────────────────────────────────────

    /// Stage 开始：创建 stage span
    pub fn on_stage_start(&mut self, stage: Stage, turn_id: &str) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let handle =
            self.stages
                .on_stage_start(stage, &self.trace_id, turn_id, &self.agent_observation_id);

        let span_body = SpanBody {
            id: Some(handle.span_id),
            trace_id: Some(handle.trace_id),
            name: Some(format!("stage-{:?}", handle.stage).to_lowercase()),
            start_time: Some(handle.start_time),
            end_time: None,
            input: None,
            output: None,
            metadata: None,
            level: None,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(handle.parent_observation_id),
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::SpanCreate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body: span_body,
            metadata: None,
        };
        try_add_or_warn_via_session(&*self.session, event, &self.trace_id, "Stage SpanCreate");
    }

    /// Stage 结束：更新 stage span
    pub(crate) fn on_stage_end(
        &mut self,
        handle: &crate::langfuse::tracer::stages::StageHandle,
        status: StageStatus,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        self.stages.on_stage_end(handle, status);

        let end_time = now_rfc3339();
        let level = match status {
            StageStatus::Error => Some(ObservationLevel::Error),
            _ => Some(ObservationLevel::Default),
        };
        let span_body = SpanBody {
            id: Some(handle.span_id.clone()),
            trace_id: Some(handle.trace_id.clone()),
            name: Some(format!("stage-{:?}", handle.stage).to_lowercase()),
            start_time: Some(handle.start_time.clone()),
            end_time: Some(end_time.clone()),
            input: None,
            output: Some(serde_json::json!({"status": format!("{:?}", status)})),
            metadata: None,
            level,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(handle.parent_observation_id.clone()),
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::SpanUpdate {
            id: new_uuid(),
            timestamp: end_time,
            body: span_body,
            metadata: None,
        };
        try_add_or_warn_via_session(&*self.session, event, &self.trace_id, "Stage SpanUpdate");
    }

    /// 消息队列排空（Receive 阶段）
    pub fn on_mq_drained(&mut self, prompt: usize, defer: usize, info: usize) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        self.stages.on_mq_drained(prompt, defer, info);
    }

    /// Workflow 开始（Act 阶段）
    pub fn on_workflow_start(&mut self, workflow_id: &str, plan: &str) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let record = self.stages.on_workflow_start(workflow_id, plan);
        if record.span_id.is_empty() {
            return;
        }
        let span_body = SpanBody {
            id: Some(record.span_id),
            trace_id: Some(self.trace_id.clone()),
            name: Some(format!("workflow-{}", workflow_id)),
            start_time: Some(now_rfc3339()),
            end_time: None,
            input: Some(serde_json::json!({"plan": plan})),
            output: None,
            metadata: None,
            level: None,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(self.agent_observation_id.clone()),
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::SpanCreate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body: span_body,
            metadata: None,
        };
        try_add_or_warn_via_session(&*self.session, event, &self.trace_id, "Workflow SpanCreate");
    }

    /// Workflow 结束（Act 阶段）
    pub fn on_workflow_end(&mut self, workflow_id: &str, agents_spawned: usize, tool_calls: usize) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let record = match self
            .stages
            .on_workflow_end(workflow_id, agents_spawned, tool_calls)
        {
            Some(r) => r,
            None => return,
        };
        let end_time = now_rfc3339();
        let span_body = SpanBody {
            id: Some(record.span_id),
            trace_id: Some(self.trace_id.clone()),
            name: Some(format!("workflow-{}", workflow_id)),
            start_time: None, // start_time from WorkflowStartRecord not retained
            end_time: Some(end_time.clone()),
            input: None,
            output: Some(serde_json::json!({
                "agents_spawned": record.agents_spawned,
                "tool_calls": record.tool_calls,
            })),
            metadata: None,
            level: None,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(self.agent_observation_id.clone()),
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::SpanUpdate {
            id: new_uuid(),
            timestamp: end_time,
            body: span_body,
            metadata: None,
        };
        try_add_or_warn_via_session(&*self.session, event, &self.trace_id, "Workflow SpanUpdate");
    }

    // ── 中间件链事件 ────────────────────────────────────────────────────────

    /// 中间件开始
    pub fn on_middleware_start(&mut self, name: &str, hook: MiddlewareHook) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let handle = self.middleware.on_start(name, hook);

        let span_body = SpanBody {
            id: Some(handle.span_id),
            trace_id: Some(self.trace_id.clone()),
            name: Some(format!("mw-{}", handle.name)),
            start_time: Some(now_rfc3339()),
            end_time: None,
            input: Some(serde_json::json!({"hook": format!("{:?}", handle.hook)})),
            output: None,
            metadata: None,
            level: None,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(self.agent_observation_id.clone()),
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::SpanCreate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body: span_body,
            metadata: None,
        };
        try_add_or_warn_via_session(
            &*self.session,
            event,
            &self.trace_id,
            "Middleware SpanCreate",
        );
    }

    /// 中间件结束
    pub(crate) fn on_middleware_end(
        &mut self,
        handle: &crate::langfuse::tracer::middleware::MiddlewareSpanHandle,
        status: StageStatus,
        error: Option<String>,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let record = match self.middleware.on_end(handle, status, error) {
            Some(r) => r,
            None => return,
        };

        let end_time = now_rfc3339();
        let level = match record.status {
            StageStatus::Error => Some(ObservationLevel::Error),
            _ => Some(ObservationLevel::Default),
        };
        let mut output_json = serde_json::json!({
            "hook": format!("{:?}", record.hook),
            "status": format!("{:?}", record.status),
        });
        if let Some(ref err) = record.error {
            output_json["error"] = serde_json::json!(err);
        }
        let span_body = SpanBody {
            id: Some(record.span_id),
            trace_id: Some(self.trace_id.clone()),
            name: Some(format!("mw-{}", record.name)),
            start_time: Some(record.start_time),
            end_time: Some(end_time.clone()),
            input: None,
            output: Some(output_json),
            metadata: None,
            level,
            status_message: record.error,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(self.agent_observation_id.clone()),
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::SpanUpdate {
            id: new_uuid(),
            timestamp: end_time,
            body: span_body,
            metadata: None,
        };
        try_add_or_warn_via_session(
            &*self.session,
            event,
            &self.trace_id,
            "Middleware SpanUpdate",
        );
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

    /// 预算阈值命中
    pub fn on_budget_threshold_hit(
        &mut self,
        _threshold: &str,
        _pct: f64,
        _tokens_in: u64,
        _tokens_out: u64,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        tracing::debug!(
            target: "langfuse::tracer",
            trace_id = %self.trace_id,
            threshold = %_threshold,
            pct = %_pct,
            tokens_in = %_tokens_in,
            tokens_out = %_tokens_out,
            "budget_threshold_hit"
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

    // ── SubAgent 辅助方法 ──────────────────────────────────────────────────

    /// 查询 `tool_call_id` 是否对应 Agent 工具调用
    pub(crate) fn is_agent_tool(&self, tool_call_id: &str) -> bool {
        self.subagent
            .is_agent_tool_anywhere(&self.tool_batch, tool_call_id)
    }

    /// 获取当前活动的 agent observation ID
    pub(crate) fn current_agent_id(&self) -> String {
        self.subagent.current_agent_id(&self.agent_observation_id)
    }

    /// 创建 SubAgent 上下文并压入 subagent 栈
    pub(crate) fn begin_subagent(&mut self, input: &serde_json::Value) {
        self.subagent.begin_subagent(input);
    }

    /// 完成当前 SubAgent Observation：先发 ObservationCreate，再弹出栈
    pub(crate) fn end_subagent(&mut self, result: &str, is_error: bool) {
        if let Some(end) = self.subagent.end_subagent() {
            let level = if is_error {
                Some(ObservationLevel::Error)
            } else {
                None
            };
            let body = ObservationBody {
                id: Some(end.observation_id),
                trace_id: Some(self.trace_id.clone()),
                r#type: ObservationType::Agent,
                name: Some(format!("subagent-{}", end.agent_id)),
                start_time: Some(end.start_time),
                end_time: Some(now_rfc3339()),
                completion_start_time: None,
                parent_observation_id: Some(self.agent_observation_id.clone()),
                input: Some(end.input),
                output: Some(serde_json::json!(result)),
                metadata: None,
                model: None,
                model_parameters: None,
                level,
                status_message: None,
                version: Some(VERSION.to_string()),
                environment: None,
                session_id: Some(self.session_id.clone()),
            };
            let event = IngestionEvent::ObservationCreate {
                id: new_uuid(),
                timestamp: now_rfc3339(),
                body,
                metadata: None,
            };
            try_add_or_warn_via_session(
                &*self.session,
                event,
                &self.trace_id,
                "SubAgent ObservationCreate",
            );
        }
    }

    /// 提交当前批次 Tools Span（end subagent first, then flush tool batch）
    pub(crate) fn flush_tools_batch(&mut self) {
        let _ = self.subagent.end_subagent();
        let _ = self.tool_batch.flush();
    }
}

#[cfg(test)]
#[path = "tracer_test.rs"]
mod tests;
