//! Langfuse 单轮追踪器（per-turn）。
#![allow(dead_code)]
//!
//! 本模块采用 Layered + Module-per-Feature 模式拆分：
//!
//! - `mod.rs`（本文件）：Facade，定义 `LangfuseTracer` 结构体与全部
//!   `on_*` 事件处理方法。持有 config + 5 个简单字段 + 7 个子对象。
//!   （context.rs 已删除，数据结构迁移至各子对象模块）
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
use langfuse_client::types::session::SessionBody;
use langfuse_client::types::{EventBody, ObservationLevel, TraceBody};
use langfuse_client::{GenerationBody, IngestionEvent, ObservationBody, ObservationType, SpanBody};
use peri_agent::agent::events::{
    CompactStrategy, CompactTrigger, MiddlewareHook, Stage, StageStatus,
};
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;
use peri_model::TokenUsage;

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
    pub(crate) subagent: crate::langfuse::tracer::subagent::SubagentStack,
    pub(crate) compact: crate::langfuse::tracer::compact::CompactSpan,
    /// 当前 stage-compact 阶段中是否有实际 compact 工作（micro/full）
    pub(crate) compact_work_done: bool,
    /// agent-run observation 的开始时间（推迟到 on_turn_end 创建时设置）
    pub(crate) agent_start_time: Option<String>,
    /// agent-run observation 的输入（推迟到 on_turn_end 创建时设置）
    pub(crate) agent_input: Option<String>,
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
            subagent: crate::langfuse::tracer::subagent::SubagentStack::new(),
            compact: crate::langfuse::tracer::compact::CompactSpan::new(),
            compact_work_done: false,
            agent_start_time: None,
            agent_input: None,
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

        // Flush 所有 subagent 栈中的 tool_batch（bg subagent 延迟清理）
        let subagent_flushes = self.subagent.flush_all_subagent_tool_batches();
        for sub_flush in subagent_flushes {
            self.emit_tools_flush(sub_flush);
        }

        // 结束子 agent 栈（如有残余），对 bg subagent 发出 ObservationCreate
        while let Some(end) = self.subagent.end_subagent() {
            let output_str = end.deferred_output.unwrap_or_default();
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
                output: Some(serde_json::json!(strip_child_thread_id(&output_str))),
                metadata: None,
                model: None,
                model_parameters: None,
                usage: None,
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
                "SubAgent ObservationCreate (deferred cleanup)",
            );
        }

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
                    name: Some(format!("turn {}", turn_id)),
                    user_id: self.user_id.clone(),
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

        // 取出推迟到现在的 start_time 和 input（on_turn_start 时存储）
        let agent_start_time = self.agent_start_time.take();
        let agent_input = self.agent_input.take();

        tokio::spawn(async move {
            let end_time = now_rfc3339();

            let obs_body = ObservationBody {
                id: Some(agent_observation_id.clone()),
                trace_id: Some(trace_id.clone()),
                r#type: ObservationType::Agent,
                name: Some("agent-run".to_string()),
                start_time: agent_start_time,
                end_time: Some(end_time.clone()),
                input: agent_input.map(|s| serde_json::json!(s)),
                output: Some(serde_json::json!(output)),
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
            if let Err(e) = session.try_add(obs_event) {
                tracing::warn!(error = %e, trace_id = %trace_id, obs_id = %agent_observation_id, "langfuse: agent-run observation 创建失败");
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
        // SubAgent 栈非空时，标记栈顶 subagent 已启动
        // （bg subagent：LLM 调用也是 subagent 启动信号）
        self.subagent.mark_top_started();
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
        request_id: Option<&str>,
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
                // 缓存 token 必须加入 usage map，否则 OTEL 转换后
                // langfuse.observation.usage_details 不含 cache，Tokens 面板不显示
                if let Some(cache_read) = u.cache_read_input_tokens {
                    map.insert(
                        "cache_read_input_tokens".to_string(),
                        serde_json::json!(cache_read),
                    );
                }
                if let Some(cache_create) = u.cache_creation_input_tokens {
                    map.insert(
                        "cache_creation_input_tokens".to_string(),
                        serde_json::json!(cache_create),
                    );
                }
                map
            });

        // 优先使用当前活跃 stage span 作为父 observation
        // Reason stage → Generation 挂在 stage-reason 下
        let parent_id = self
            .stages
            .active_handle()
            .map(|h| h.span_id.clone())
            .or_else(|| {
                // fallback: subagent stack 非空但 stage 尚未创建（竞态/时序问题）
                if !self.subagent.is_empty() {
                    Some(self.subagent.current_agent_id(&self.agent_observation_id))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| self.agent_observation_id.clone());

        // 合并 retry metadata + token 用量到 metadata 字段（Langfuse UI 可见）
        let mut meta = gen_end.retry_metadata.unwrap_or(serde_json::json!({}));
        let meta_obj = meta.as_object_mut();
        if let Some(u) = usage {
            if let Some(obj) = meta_obj {
                obj.insert("model".to_string(), serde_json::json!(model));
                obj.insert(
                    "input_tokens".to_string(),
                    serde_json::json!(u.input_tokens),
                );
                obj.insert(
                    "output_tokens".to_string(),
                    serde_json::json!(u.output_tokens),
                );
                obj.insert(
                    "cache_read_input_tokens".to_string(),
                    serde_json::json!(u.cache_read_input_tokens),
                );
                obj.insert(
                    "cache_creation_input_tokens".to_string(),
                    serde_json::json!(u.cache_creation_input_tokens),
                );
                obj.insert(
                    "total_tokens".to_string(),
                    serde_json::json!(u.input_tokens + u.output_tokens),
                );
                // 历史 TokenUsage 曾携带 first_token_time（TTFB 指标），
                // 但 v2 路径（ObserveEvent::LlmCallEnd）迁移前就恒为 None；peri_model::TokenUsage
                // 不包含该字段。TTFB 随旧 LLM facade 一并退役，此处不再计算。
            }
        } else if let Some(obj) = meta_obj {
            obj.insert("model".to_string(), serde_json::json!(model));
        }
        // provider request_id 无条件写入 metadata（与 usage 独立，用于关联 provider 侧日志）
        if let Some(req_id) = request_id {
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("request_id".to_string(), serde_json::json!(req_id));
            }
        }

        let gen_body = GenerationBody {
            id: Some(gen_end.gen_id),
            trace_id: Some(self.trace_id.clone()),
            name: Some(format!("step-{}", step)),
            start_time: Some(gen_end.start_time),
            end_time: Some(end_time.clone()),
            input: Some(gen_end.input_json),
            output: Some(parse_output(output)),
            metadata: Some(meta),
            level: None,
            status_message: None,
            parent_observation_id: Some(parent_id),
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

        // 缓存命中率警告：input > 10k tokens 且 cache_read / input < 20% 时创建 Event
        if let Some(u) = usage {
            self.emit_cache_warning_if_needed(step, u);
        }
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

    /// 缓存命中率过低时创建 Warning Event
    fn emit_cache_warning_if_needed(&mut self, step: usize, usage: &TokenUsage) {
        let input_tokens = usage.input_tokens as f64;
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0) as f64;

        // 仅在输入 token > 10000 且缓存命中率 < 20% 时告警
        if input_tokens < 10000.0 {
            return;
        }
        let hit_rate = if input_tokens > 0.0 {
            cache_read / input_tokens
        } else {
            1.0
        };
        if hit_rate >= 0.2 {
            return;
        }

        let event_body = EventBody {
            id: Some(new_uuid()),
            trace_id: Some(self.trace_id.clone()),
            name: Some("cache-hit-rate-low".to_string()),
            start_time: Some(now_rfc3339()),
            input: Some(serde_json::json!({
                "step": step,
                "input_tokens": usage.input_tokens,
                "cache_read_input_tokens": usage.cache_read_input_tokens,
                "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                "hit_rate": hit_rate,
            })),
            output: None,
            metadata: Some(serde_json::json!({
                "event_type": "cache_warning",
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
            "CacheWarning EventCreate",
        );
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
        // 捕获当前活跃 stage span_id 作为 tool-batch 的父节点
        // （必须在 on_tool_start 时获取，因为 emit_tools_flush 可能在 stage 结束后才调用）
        let parent_id = self
            .stages
            .active_handle()
            .map(|h| h.span_id.clone())
            .or_else(|| {
                // fallback: subagent stack 非空但 stage 尚未创建（竞态/时序问题）
                if !self.subagent.is_empty() {
                    Some(self.subagent.current_agent_id(&self.agent_observation_id))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| self.agent_observation_id.clone());
        // Agent 工具：先写入主 agent 的 ToolBatch，再 push subagent context。
        // Fix A: 这样 Agent 工具的 parent_observation_id 正确指向主 agent 的 act span，
        // 后续 subagent 工具会创建新的 sub batch，parent 指向 subagent 的 act span。
        // WARNING: Fix B（on_tool_end 路由）必须同时应用，否则 Agent 工具完成记录会静默丢失。
        let is_agent_tool = name == "Agent" || name == "Task";
        if is_agent_tool {
            // Fix A: Write Agent/Task tool to MAIN's ToolBatch BEFORE begin_subagent.
            let _record =
                self.tool_batch
                    .on_tool_start(tool_call_id, name, input.clone(), &parent_id);
            self.subagent.begin_subagent(input);
        } else {
            // 非 Agent 工具：路由到正确的 ToolBatch（subagent 栈非空时写入 subagent 的）
            let mut tb_ref = self.subagent.current_tool_batch_mut(&mut self.tool_batch);
            let _record = tb_ref.on_tool_start(tool_call_id, name, input.clone(), &parent_id);
        }
    }

    /// 工具调用结束：同步创建 tool observation
    pub fn on_tool_end(&mut self, tool_call_id: &str, output: &str, is_error: bool) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        // 检查是否为 Agent 工具（在路由前检查，路由后 pending_tools 中会移除）
        let is_agent = self
            .subagent
            .is_agent_tool_anywhere(&self.tool_batch, tool_call_id);

        // 路由到正确的 ToolBatch 结束工具记录
        // （必须在 subagent 弹栈之前完成，避免工具数据丢失）
        // Fix B: Agent 工具写入主 batch，必须路由到主 batch 完成；非 Agent 工具走栈路由。
        {
            if is_agent {
                let _pending = self.tool_batch.on_tool_end(tool_call_id, output, is_error);
            } else {
                let mut tb_ref = self.subagent.current_tool_batch_mut(&mut self.tool_batch);
                let _pending = tb_ref.on_tool_end(tool_call_id, output, is_error);
            }
        }

        if is_agent {
            if self.subagent.top_has_started() {
                // fork 情况：subagent 已运行完毕，弹栈前先 flush subagent 的 tool_batch
                let sub_flush = {
                    let mut tb_ref = self.subagent.current_tool_batch_mut(&mut self.tool_batch);
                    tb_ref.flush()
                };
                self.emit_tools_flush(sub_flush);
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
                        parent_observation_id: Some(
                            self.stages
                                .active_handle()
                                .map(|h| h.span_id.clone())
                                .unwrap_or_else(|| self.agent_observation_id.clone()),
                        ),
                        input: Some(end.input),
                        output: Some(serde_json::json!(strip_child_thread_id(output))),
                        metadata: None,
                        model: None,
                        model_parameters: None,
                        usage: None,
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
            } else {
                // bg 情况：subagent 尚未启动，不弹栈、不发射 observation
                // subagent 保留在栈上，等实际启动后恢复活跃，turn_end 时统一清理
                // 记录 tool output 到栈顶 context（供最终 observation 使用）
                self.subagent.record_tool_output(output);
            }
        }
    }

    // ── Compact 事件 ────────────────────────────────────────────────────────

    /// Compact 开始：注册 compact span（SpanCreate 延迟到 on_compact_end 发送，使用真实策略/触发方式）
    pub fn on_compact_start(&mut self, strategy: CompactStrategy, trigger: CompactTrigger) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let _start = self.compact.on_start(strategy, trigger);
        self.compact_work_done = true;
        // SpanCreate 延迟到 on_compact_end：仅在 duration > 0 时发送
    }

    /// Compact 完成/错误：若 duration > 0 则发送 SpanCreate（合并 start+end），否则跳过。
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
        let duration_ms = calculate_duration_ms(&ctx.start_time, &end_time);

        // 0ms compact span 不上报
        if duration_ms == 0 && !is_error {
            return;
        }

        let output = if is_error {
            serde_json::json!({"error": error_message})
        } else {
            serde_json::json!({
                "summary": summary,
                "files_count": files_count,
                "skills_count": skills_count,
                "micro_cleared": micro_cleared,
                "duration_ms": duration_ms,
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
                "duration_ms": duration_ms,
            })),
            level,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(self.agent_observation_id.clone()),
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::SpanCreate {
            id: new_uuid(),
            timestamp: end_time,
            body: span_body,
            metadata: None,
        };
        try_add_or_warn_via_session(&*self.session, event, &self.trace_id, "Compact SpanCreate");
    }

    // ── Stage 5 阶段 Span 事件 ──────────────────────────────────────────────

    /// Stage 开始：注册 stage span（SpanCreate 延迟到 on_stage_end 发送，
    /// 仅在 duration > 0 时上报，实现 v2 条件上报语义）
    pub fn on_stage_start(&mut self, stage: Stage, turn_id: &str) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        // SubAgent 栈非空时，标记栈顶 subagent 已启动
        // （bg subagent：StageStarted 是第一个 subagent 事件，标志着真正开始）
        self.subagent.mark_top_started();
        let _handle =
            self.stages
                .on_stage_start(stage, &self.trace_id, turn_id, &self.agent_observation_id);
        // SpanCreate 延迟到 on_stage_end：仅在 duration > 0 时发送
    }

    /// Stage 结束：若 duration > 0 则发送 SpanCreate（合并 start+end），否则静默跳过。
    /// 实现 v2 spec §1.2 条件上报：0ms stage span 不上报。
    pub(crate) fn on_stage_end(
        &mut self,
        handle: &crate::langfuse::tracer::stages::StageHandle,
        status: StageStatus,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        // 在 on_stage_end 清空 active 前捕获 Receive 阶段的 mq_counts，
        // 否则 span body 构造时 active 已为 None，排空计数全部丢失。
        let receive_input = if handle.stage == Stage::Receive {
            self.stages.mq_counts().map(|(prompt, defer, info)| {
                serde_json::json!({
                    "messages_drained": {
                        "prompt": prompt,
                        "defer": defer,
                        "info": info,
                        "total": prompt + defer + info,
                    }
                })
            })
        } else {
            None
        };

        self.stages.on_stage_end(handle, status);

        // Act stage 结束时自动 flush 主 agent 工具批次（确保工具挂在正确的 act 下，
        // 而非全部堆在第一个 act 中）。仅当子 agent 栈为空时 flush——子 agent Act 结束
        // 不应影响主 batch（主 batch 中的 Agent 工具可能尚未结束，flush() 的
        // pending_tools.clear() 会导致丢失）。
        // 子 agent 的工具批次由 on_tool_end("Agent") fork 路径独立 flush（top_has_started 守卫）。
        if handle.stage == Stage::Act && self.subagent.is_empty() {
            let flush = self.tool_batch.flush();
            self.emit_tools_flush(flush);
        }

        // Compact stage：仅在实际执行了 micro/full compact 时才上报 span，
        // 否则跳过空 compact 阶段（无意义的 ~20ms span）
        if handle.stage == Stage::Compact && !self.compact_work_done {
            return;
        }
        self.compact_work_done = false;

        let end_time = now_rfc3339();
        let duration_ms = calculate_duration_ms(&handle.start_time, &end_time);

        // v2 条件上报：0ms 不做 Span，跳过
        if duration_ms == 0 {
            return;
        }

        let level = match status {
            StageStatus::Error => Some(ObservationLevel::Error),
            _ => Some(ObservationLevel::Default),
        };
        // 合并 SpanCreate + SpanUpdate 为单个 SpanCreate（含 end_time）
        let span_body = SpanBody {
            id: Some(handle.span_id.clone()),
            trace_id: Some(handle.trace_id.clone()),
            name: Some(format!("stage-{:?}", handle.stage).to_lowercase()),
            start_time: Some(handle.start_time.clone()),
            end_time: Some(end_time.clone()),
            input: receive_input,
            output: Some(serde_json::json!({
                "status": format!("{:?}", status),
                "duration_ms": duration_ms,
            })),
            metadata: None,
            level,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(handle.parent_observation_id.clone()),
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::SpanCreate {
            id: new_uuid(),
            timestamp: end_time,
            body: span_body,
            metadata: None,
        };
        try_add_or_warn_via_session(&*self.session, event, &self.trace_id, "Stage SpanCreate");
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

    /// 中间件开始：注册 span（SpanCreate 延迟到 on_middleware_end 发送）
    pub fn on_middleware_start(&mut self, name: &str, hook: MiddlewareHook) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let _handle = self.middleware.on_start(name, hook);
        // SpanCreate 延迟到 on_middleware_end：仅在 duration > 0 时发送
    }

    /// 中间件结束：若 duration > 0 则发送 SpanCreate（合并 start+end），否则静默跳过。
    /// 大多数中间件执行时间 < 1ms，跳过可大幅减少噪音 span。
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
        let duration_ms = calculate_duration_ms(&record.start_time, &end_time);

        // 0ms middleware span 不上报（绝大多数中间件 < 1ms）
        if duration_ms == 0 {
            return;
        }

        let level = match record.status {
            StageStatus::Error => Some(ObservationLevel::Error),
            _ => Some(ObservationLevel::Default),
        };
        let mut output_json = serde_json::json!({
            "hook": format!("{:?}", record.hook),
            "status": format!("{:?}", record.status),
            "duration_ms": duration_ms,
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
        let event = IngestionEvent::SpanCreate {
            id: new_uuid(),
            timestamp: end_time,
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
                output: Some(serde_json::json!(strip_child_thread_id(result))),
                metadata: None,
                model: None,
                model_parameters: None,
                usage: None,
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
        let flush = self.tool_batch.flush();
        self.emit_tools_flush(flush);
    }

    /// 将 ToolsBatchFlush 转换为 Langfuse SpanCreate 事件并入队
    fn emit_tools_flush(&self, flush: tool_batch::ToolsBatchFlush) {
        if let Some(ref batch) = flush.batch {
            // 使用 on_tool_start 时捕获的 stage span_id（而非运行时动态查找）
            let parent_id = &flush.parent_observation_id;

            // 构建 batch span 的 input（工具名称列表和数量）
            let batch_input = serde_json::json!({
                "tool_count": flush.tools.len(),
                "tools": flush.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
            });

            // 构建 batch span 的 output（汇总各工具执行结果）
            let batch_output = {
                let start_ms = chrono::DateTime::parse_from_rfc3339(&batch.batch_start_time).ok();
                let end_ms = chrono::DateTime::parse_from_rfc3339(&batch.batch_end_time).ok();
                let duration_ms = match (start_ms, end_ms) {
                    (Some(s), Some(e)) => {
                        e.signed_duration_since(s).num_milliseconds().max(0) as u64
                    }
                    _ => 0,
                };
                serde_json::json!({
                    "duration_ms": duration_ms,
                    "tool_results": flush.tools.iter().map(|t| serde_json::json!({
                        "name": t.name,
                        "is_error": t.is_error,
                        "output_preview": if t.output.len() > 200 {
                            let preview: String = t.output.chars().take(200).collect();
                            format!("{}...", preview)
                        } else {
                            t.output.clone()
                        },
                    })).collect::<Vec<_>>(),
                })
            };

            // 批量工具父 span（tool-batch）
            let batch_body = SpanBody {
                id: Some(batch.batch_span_id.clone()),
                trace_id: Some(self.trace_id.clone()),
                name: Some("tool-batch".to_string()),
                start_time: Some(batch.batch_start_time.clone()),
                end_time: Some(batch.batch_end_time.clone()),
                input: Some(batch_input),
                output: Some(batch_output),
                parent_observation_id: Some(parent_id.clone()),
                version: Some(VERSION.to_string()),
                session_id: Some(self.session_id.clone()),
                ..Default::default()
            };
            let batch_event = IngestionEvent::SpanCreate {
                id: new_uuid(),
                timestamp: batch.batch_end_time.clone(),
                body: batch_body,
                metadata: None,
            };
            try_add_or_warn_via_session(
                &*self.session,
                batch_event,
                &self.trace_id,
                "tool-batch SpanCreate",
            );

            // 每个工具以 ObservationCreate + ObservationType::Tool 上报
            for tool in &flush.tools {
                let level = if tool.is_error {
                    Some(ObservationLevel::Error)
                } else {
                    None
                };
                let obs_body = ObservationBody {
                    id: Some(tool.span_id.clone()),
                    trace_id: Some(self.trace_id.clone()),
                    r#type: ObservationType::Tool,
                    name: Some(tool.name.clone()),
                    start_time: Some(tool.start_time.clone()),
                    end_time: Some(tool.end_time.clone()),
                    input: Some(tool.input.clone()),
                    output: Some(serde_json::json!(tool.output)),
                    parent_observation_id: Some(batch.batch_span_id.clone()),
                    level,
                    version: Some(VERSION.to_string()),
                    session_id: Some(self.session_id.clone()),
                    ..Default::default()
                };
                let tool_event = IngestionEvent::ObservationCreate {
                    id: new_uuid(),
                    timestamp: tool.end_time.clone(),
                    body: obs_body,
                    metadata: None,
                };
                try_add_or_warn_via_session(
                    &*self.session,
                    tool_event,
                    &self.trace_id,
                    "tool ObservationCreate",
                );
            }
        }
    }
}

/// 计算两 RFC3339 时间戳之间的毫秒差。
/// parse 失败时返回 0（保守：不上报 0ms span）。
fn calculate_duration_ms(start: &str, end: &str) -> u64 {
    use chrono::TimeZone;
    let s = chrono::DateTime::parse_from_rfc3339(start)
        .unwrap_or_else(|_| chrono::Utc.timestamp_opt(0, 0).unwrap().into());
    let e = chrono::DateTime::parse_from_rfc3339(end)
        .unwrap_or_else(|_| chrono::Utc.timestamp_opt(0, 0).unwrap().into());
    let dur = e.signed_duration_since(s);
    dur.num_milliseconds().max(0) as u64
}

/// 解析 LlmCallEnd output 为 JSON Value。
/// 若 output 是合法 JSON object，返回解析后的 Value（保持结构化）；
/// 否则包装为 `{"text": output}` 纯文本（向后兼容非结构化旧数据）。
fn parse_output(output: &str) -> serde_json::Value {
    // 尝试解析为 JSON Value
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(output) {
        if val.is_object() {
            return val;
        }
    }
    // fallback: 将纯文本包装为 {text: ...}
    serde_json::json!({"text": output})
}

/// 从 subagent 输出文本中剥离 `child_thread_id: <uuid>\n` 前缀。
/// 若输出以前缀开头，返回剥离后的剩余内容；否则返回原输出。
fn strip_child_thread_id(output: &str) -> &str {
    // 匹配模式: "child_thread_id: <uuid>\n"
    if let Some(rest) = output.strip_prefix("child_thread_id: ") {
        // 找到第一个换行符后的内容
        if let Some(newline_pos) = rest.find('\n') {
            return &rest[newline_pos + 1..];
        }
    }
    output
}

#[cfg(test)]
#[path = "tracer_test.rs"]
mod tests;
