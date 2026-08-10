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
//! - `registry.rs`：SubAgent 身份注册表(agent_id 查表归属,替代旧 LIFO 栈)。
//! - `compact.rs`：Compact 操作 Span 追踪器。
//!
//! 所有事件通过 session trait 的 try_add() 同步入队，保证事件顺序与调用顺序一致，
//! 确保 Langfuse 层级关系正确（父 span 先于子 span 入队）。

mod compact;
mod event_builder;
mod generation;
pub(crate) mod middleware;
pub(crate) mod registry;
mod sampling;
pub mod stages;
mod tool_batch;
mod usage;

use super::config::LangfuseConfig;
use super::session_like::LangfuseSessionLike;
use crate::langfuse::tracer::registry::{GateEvent, Ownership};
use crate::langfuse::tracer::stages::{StageHandle, MAIN_AGENT_KEY};
use event_builder::{new_uuid, now_rfc3339, try_add_or_warn_via_session, VERSION};
use langfuse_client::types::session::SessionBody;
use langfuse_client::types::{EventBody, ObservationLevel, TraceBody};
use langfuse_client::{GenerationBody, IngestionEvent, ObservationBody, ObservationType, SpanBody};
use peri_agent::agent::events::{
    CompactStrategy, CompactTrigger, MiddlewareHook, Stage, StageStatus,
};
use peri_agent::agent::events_v2::TurnErrorReason;
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
    pub fn on_turn_start(&mut self, _input: &str) {
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

        // 取出推迟到现在的 start_time。
        let agent_start_time = self.agent_start_time.take();

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
            input: None,
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

    // ── LLM Generation 事件 ──────────────────────────────────────────────────

    /// LLM 调用开始
    pub fn on_llm_start(
        &mut self,
        agent_id: &str,
        step: usize,
        messages: &[BaseMessage],
        tools: &[ToolDefinition],
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let _ = self.on_llm_start_inner(agent_id, step, messages, tools);
    }

    /// LLM 调用开始(业务主体;供 gate 重放复用,不重复采样检查)。
    /// 返回 false = 事件被注册闸门缓存或丢弃。
    fn on_llm_start_inner(
        &mut self,
        agent_id: &str,
        step: usize,
        messages: &[BaseMessage],
        tools: &[ToolDefinition],
    ) -> bool {
        match self.subagent.ownership(agent_id) {
            Ownership::Main | Ownership::Subagent => {
                self.generation
                    .on_llm_start(agent_id, step, messages.to_vec(), tools.to_vec());
                true
            }
            Ownership::Unknown => {
                // 未知 agent(Start 未到/已 incomplete)→ 注册闸门缓存,等 join 重放
                if self.subagent.try_gate(GateEvent::LlmCallStart {
                    agent_id: agent_id.to_string(),
                    step,
                    messages: messages.to_vec(),
                    tools: tools.to_vec(),
                }) {
                    tracing::debug!(
                        target: "langfuse::subagent",
                        %agent_id,
                        "on_llm_start: 未知 agent,事件入注册闸门缓存"
                    );
                    return false;
                }
                false
            }
        }
    }

    /// LLM 请求体接收：紧随 on_llm_start 之后，缓存 Provider 实际请求体
    pub fn on_llm_request_payload(
        &mut self,
        agent_id: &str,
        step: usize,
        body: std::sync::Arc<serde_json::Value>,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        self.generation.on_llm_request_payload(agent_id, step, body);
    }

    /// LLM 调用结束：同步创建 Generation 事件
    #[allow(clippy::too_many_arguments)] // agent_id 隔离并行 subagent，语义清晰不拆结构体
    pub fn on_llm_end(
        &mut self,
        agent_id: &str,
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

        let gen_end = match self.generation.on_llm_end(agent_id, step) {
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

        // 优先使用当前活跃 stage span 作为父 observation（按 agent 隔离：
        // 并行 subagent 各自持有自己的 stage slot，不会取到其他 agent 的 span）。
        // 归属链:该 agent 的活跃 stage → 该 agent 的 AGENT obs → 主 agent obs。
        // 禁止降级挂主 agent:未知 agent(未注册且非 main)直接跳过。
        let parent_id = match self.llm_parent(agent_id) {
            Some(p) => p,
            None => {
                tracing::warn!(
                    target: "langfuse::subagent",
                    %agent_id,
                    "on_llm_end: agent 未注册且非主 agent,跳过 generation 上报"
                );
                return;
            }
        };

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

        // LLM 失败路径只保留固定分类，避免将 provider 原始错误写入 statusMessage 或 output。
        let (level, status_message, generation_output) = if output.starts_with("ERROR: ") {
            (
                Some(ObservationLevel::Error),
                Some("provider_or_stream_failure".to_string()),
                Some(serde_json::json!({"error_class": "provider_or_stream_failure"})),
            )
        } else {
            (None, None, Some(parse_output(output)))
        };

        let gen_body = GenerationBody {
            id: Some(gen_end.gen_id),
            trace_id: Some(self.trace_id.clone()),
            name: Some(format!("step-{}", step)),
            start_time: Some(gen_end.start_time),
            end_time: Some(end_time.clone()),
            input: None,
            output: generation_output,
            metadata: Some(meta),
            level,
            status_message,
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

    /// LLM 重试：记录重试信息，最终在 on_llm_end 时写入 Generation metadata。
    /// `agent_id`/`step` 标识所属 generation（v1 路径由 bridge 推断归属）。
    pub fn on_llm_retrying(
        &mut self,
        agent_id: &str,
        step: usize,
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        error: &str,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        self.generation
            .on_llm_retrying(agent_id, step, attempt, max_attempts, delay_ms, error);
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
    pub fn on_tool_start(
        &mut self,
        agent_id: &str,
        tool_call_id: &str,
        name: &str,
        input: &serde_json::Value,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let _ = self.on_tool_start_inner(agent_id, tool_call_id, name, input);
    }

    /// 工具调用开始(业务主体;供 gate 重放复用)。返回 false = 事件被注册闸门缓存/丢弃。
    fn on_tool_start_inner(
        &mut self,
        agent_id: &str,
        tool_call_id: &str,
        name: &str,
        input: &serde_json::Value,
    ) -> bool {
        // 归属链:该 agent 的活跃 stage → 该 agent 的 AGENT obs → 主 agent obs。
        // 未知 agent 走注册闸门(缓存等 Start join 后重放),不挂主 agent。
        let (owner, parent_id) = match self.content_owner(agent_id) {
            Some(x) => x,
            None => {
                self.subagent.try_gate(GateEvent::ToolStart {
                    agent_id: agent_id.to_string(),
                    tool_call_id: tool_call_id.to_string(),
                    name: name.to_string(),
                    input: input.clone(),
                });
                return false;
            }
        };
        // Agent 工具:写入 owner 自己的 ToolBatch + 登记 invocation。
        // 不创建任何 AGENT obs——生命周期由 SubagentStart/Stop 驱动。
        let is_agent_tool = name == "Agent" || name == "Task";
        match owner {
            Ownership::Main => {
                self.tool_batch
                    .on_tool_start(tool_call_id, name, input.clone(), &parent_id);
            }
            Ownership::Subagent => {
                let tb = self.subagent.tool_batch_mut(agent_id);
                tb.on_tool_start(tool_call_id, name, input.clone(), &parent_id);
            }
            Ownership::Unknown => return false,
        }
        if is_agent_tool {
            if let Some(outcome) =
                self.subagent
                    .register_invocation(agent_id, tool_call_id, input, &parent_id)
            {
                self.handle_join_outcome(outcome);
            }
        }
        true
    }

    /// 工具调用结束：同步创建 tool observation
    ///
    /// `agent_id` 用于按 owner 路由到正确的 ToolBatch 并关联 invocation。
    /// Agent 工具的 ToolEnded 只结束父工具记录 + 更新 invocation,
    /// **不再创建/关闭 AGENT obs**(生命周期由 SubagentStart/Stop 驱动)。
    pub fn on_tool_end(
        &mut self,
        agent_id: &str,
        tool_call_id: &str,
        output: &str,
        is_error: bool,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let _ = self.on_tool_end_inner(agent_id, tool_call_id, output, is_error);
    }

    /// 工具调用结束(业务主体;供 gate 重放复用)
    fn on_tool_end_inner(
        &mut self,
        agent_id: &str,
        tool_call_id: &str,
        output: &str,
        is_error: bool,
    ) -> bool {
        match self.subagent.ownership(agent_id) {
            Ownership::Main | Ownership::Subagent => {
                let main_domain = matches!(self.subagent.ownership(agent_id), Ownership::Main);
                if main_domain {
                    self.tool_batch.on_tool_end(tool_call_id, output, is_error);
                } else {
                    let tb = self.subagent.tool_batch_mut(agent_id);
                    tb.on_tool_end(tool_call_id, output, is_error);
                }
                // Agent 工具 invocation:结束父工具记录、更新 invocation;
                // 两信号齐备(Stop + ToolEnded)时回收 → 关闭 AGENT obs + flush child batch。
                // 绝不 end_subagent。
                if self.subagent.has_invocation(agent_id, tool_call_id) {
                    if let Some(closed) = self.subagent.on_invocation_tool_end(
                        agent_id,
                        tool_call_id,
                        output,
                        is_error,
                    ) {
                        self.emit_subagent_close(closed);
                    }
                }
                true
            }
            Ownership::Unknown => {
                self.subagent.try_gate(GateEvent::ToolEnd {
                    agent_id: agent_id.to_string(),
                    tool_call_id: tool_call_id.to_string(),
                    output: output.to_string(),
                    is_error,
                });
                false
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
        _error_message: &str,
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
            serde_json::json!({"error_class": "compact_failure"})
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
    ///
    /// v1 直调路径（无 agent_id 事件来源）：使用固定 `MAIN_AGENT_KEY` slot，
    /// 与 v2 ObserveEvent 路径（按事件 agent_id 隔离）互不干扰。
    pub fn on_stage_start(&mut self, stage: Stage, turn_id: &str) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let _handle = self.stages.on_stage_start(
            MAIN_AGENT_KEY,
            stage,
            &self.trace_id,
            turn_id,
            &self.agent_observation_id,
        );
        // SpanCreate 延迟到 on_stage_end：仅在 duration > 0 时发送
    }

    /// Stage 结束：若 duration > 0 则发送 SpanCreate（合并 start+end），否则静默跳过。
    /// 实现 v2 spec §1.2 条件上报：0ms stage span 不上报。
    pub(crate) fn on_stage_end(
        &mut self,
        agent_id: &str,
        handle: &crate::langfuse::tracer::stages::StageHandle,
        status: StageStatus,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        // 在 on_stage_end 清空 active 前捕获 Receive 阶段的 mq_counts，
        // 否则 span body 构造时 active 已为 None，排空计数全部丢失。
        let receive_input = if handle.stage == Stage::Receive {
            self.stages
                .mq_counts(agent_id)
                .map(|(prompt, defer, info)| {
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

        self.stages.on_stage_end(agent_id, handle, status);

        // Act stage 结束时自动 flush 主 agent 工具批次（确保工具挂在正确的 act 下，
        // 而非全部堆在第一个 act 中）。仅当该 stage 属于主 agent 域时 flush——
        // subagent 的 Act 结束不应影响主 batch(主 batch 中的 Agent 工具可能尚未结束,
        // flush() 的 pending_tools.clear() 会导致丢失)。
        // subagent 的工具批次由 AGENT obs 关闭(Stop/ToolEnded 双信号)时独立 flush。
        if handle.stage == Stage::Act && self.subagent.is_main_agent(agent_id) {
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
    pub fn on_mq_drained(&mut self, agent_id: &str, prompt: usize, defer: usize, info: usize) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        self.stages.on_mq_drained(agent_id, prompt, defer, info);
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
        let output_json = serde_json::json!({
            "hook": format!("{:?}", record.hook),
            "status": format!("{:?}", record.status),
            "duration_ms": duration_ms,
            "error_class": record.is_error.then_some("middleware_failure"),
        });
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
            status_message: record.is_error.then_some("middleware_failure".to_string()),
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

    // ── SubAgent 身份注册表(registry)入口 ────────────────────────────────────

    /// 注入主 agent 身份(bridge1 构造时调用;bridge2/workflow 不注入 → None fallback)
    pub(crate) fn set_main_agent_id(&mut self, id: String) {
        self.subagent.set_main_agent_id(id);
    }

    /// 内容事件归属:返回 (归属域, parent observation id)。
    /// 归属链:该 agent 的活跃 stage span → 该 agent 的 AGENT obs → 主 agent obs。
    /// None = 未知 agent(未注册且非主)→ 调用方走注册闸门/跳过,禁止挂主 agent。
    fn content_owner(&self, agent_id: &str) -> Option<(Ownership, String)> {
        if let Some(h) = self.stages.active_handle(agent_id) {
            let owner = match self.subagent.ownership(agent_id) {
                Ownership::Subagent => Ownership::Subagent,
                _ => Ownership::Main,
            };
            return Some((owner, h.span_id.clone()));
        }
        if let Some(obs) = self.subagent.observation_id_of(agent_id) {
            return Some((Ownership::Subagent, obs));
        }
        if self.subagent.is_main_agent(agent_id) {
            return Some((Ownership::Main, self.agent_observation_id.clone()));
        }
        None
    }

    /// generation 的 parent:该 agent 的活跃 stage → 该 agent 的 AGENT obs → 主 agent obs。
    /// None = 未知 agent(禁止降级主 agent)。
    fn llm_parent(&self, agent_id: &str) -> Option<String> {
        if let Some(h) = self.stages.active_handle(agent_id) {
            return Some(h.span_id.clone());
        }
        if let Some(obs) = self.subagent.observation_id_of(agent_id) {
            return Some(obs);
        }
        if self.subagent.is_main_agent(agent_id) {
            return Some(self.agent_observation_id.clone());
        }
        None
    }

    /// bridge 的 StageStarted 分支入口:按事件 agent_id 决策 parent。
    /// 未知 agent → 入注册闸门缓存(等 Start join 后重放)或跳过;返回 None 时
    /// bridge 不创建 stage handle。乱序重放产生的 handle 存入
    /// `replayed_stage_handles`,由 StageEnded 分支领取。
    pub(crate) fn on_stage_start_gated(
        &mut self,
        agent_id: &str,
        stage: Stage,
        turn_id: &str,
    ) -> Option<StageHandle> {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return None;
        }
        let parent = match self.content_owner(agent_id) {
            Some((_, p)) => p,
            None => {
                self.subagent.try_gate(GateEvent::StageStarted {
                    agent_id: agent_id.to_string(),
                    stage,
                    turn_id: turn_id.to_string(),
                });
                return None;
            }
        };
        let handle = self
            .stages
            .on_stage_start(agent_id, stage, &self.trace_id, turn_id, &parent);
        Some(handle)
    }

    /// StageEnded 分支领取乱序重放的 stage handle(active_stage 未命中时)
    pub(crate) fn take_replayed_stage_handle(&mut self, agent_id: &str) -> Option<StageHandle> {
        self.replayed_stage_handles.remove(agent_id)
    }

    /// SubagentStart:驱动 AGENT obs 创建(join 成功后 emit ObservationCreate open),
    /// 并重放该 child 被注册闸门缓存的内容事件。
    pub(crate) fn on_subagent_start(
        &mut self,
        parent_agent_id: &str,
        child_agent_id: &str,
        agent_name: &str,
        is_background: bool,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let outcome = self.subagent.on_subagent_start(
            parent_agent_id,
            child_agent_id,
            agent_name,
            is_background,
        );
        self.handle_join_outcome(outcome);
    }

    /// SubagentStop:驱动 AGENT obs 关闭(两信号齐备时 emit ObservationUpdate + flush)
    pub(crate) fn on_subagent_stop(
        &mut self,
        parent_agent_id: &str,
        child_agent_id: &str,
        result: &str,
        is_error: bool,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        if let Some(closed) =
            self.subagent
                .on_subagent_stop(parent_agent_id, child_agent_id, result, is_error)
        {
            self.emit_subagent_close(closed);
        }
    }

    /// 处理 join 结果:emit AGENT obs open → 重放 gate 事件 → 可能立即关闭
    fn handle_join_outcome(&mut self, outcome: registry::SubagentStartOutcome) {
        let registry::SubagentStartOutcome::Joined {
            obs,
            replayed,
            immediately_close,
        } = outcome
        else {
            return; // Pending / Duplicate 无 obs 动作
        };
        self.emit_subagent_obs_start(&obs);
        for ev in replayed {
            match ev {
                GateEvent::StageStarted {
                    agent_id,
                    stage,
                    turn_id,
                } => {
                    if let Some(h) = self.on_stage_start_gated(&agent_id, stage, &turn_id) {
                        // 乱序重放:bridge 的 active_stage 未参与,handle 由 StageEnded 领取
                        self.replayed_stage_handles.insert(agent_id, h);
                    }
                }
                GateEvent::LlmCallStart {
                    agent_id,
                    step,
                    messages,
                    tools,
                } => {
                    self.on_llm_start_inner(&agent_id, step, &messages, &tools);
                }
                GateEvent::ToolStart {
                    agent_id,
                    tool_call_id,
                    name,
                    input,
                } => {
                    self.on_tool_start_inner(&agent_id, &tool_call_id, &name, &input);
                }
                GateEvent::ToolEnd {
                    agent_id,
                    tool_call_id,
                    output,
                    is_error,
                } => {
                    self.on_tool_end_inner(&agent_id, &tool_call_id, &output, is_error);
                }
            }
        }
        if let Some(closed) = immediately_close {
            self.emit_subagent_close(closed);
        }
    }

    /// AGENT obs 创建(open):ObservationCreate,无 end_time。
    /// start 时刻 = Start join 时刻(≤ 最早 child 事件,17ms 空壳场景不复现)。
    fn emit_subagent_obs_start(&self, obs: &registry::AgentObsStart) {
        let body = ObservationBody {
            id: Some(obs.observation_id.clone()),
            trace_id: Some(self.trace_id.clone()),
            r#type: ObservationType::Agent,
            name: Some(format!("subagent-{}", obs.agent_name)),
            start_time: Some(obs.start_time.clone()),
            end_time: None,
            completion_start_time: None,
            parent_observation_id: Some(obs.parent_observation_id.clone()),
            input: obs.input.clone(),
            output: None,
            // 与 ErrorTurn span 的 metadata 格式对齐(trace_id == turn_id)
            metadata: Some(serde_json::json!({
                "is_synthetic": false,
                "was_sampled": true,
                "turn_id": self.trace_id.clone(),
            })),
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

    /// AGENT obs 关闭:flush child tool_batch + ObservationUpdate(带 end_time/output)。
    /// end 时刻 = Stop 时刻;output = Stop result(空则父工具 deferred_output)。
    fn emit_subagent_close(&self, closed: registry::ClosedSubagent) {
        // 先 flush child 的工具批次(工具 span 挂在 child 的 batch/stage 下)
        self.emit_tools_flush(closed.flush);
        let level = if closed.is_error {
            Some(ObservationLevel::Error)
        } else {
            None
        };
        // 成功/失败统一写 text(成功不再丢 output);错误时附加 error_class
        let mut output = serde_json::json!({"text": closed.output});
        if closed.is_error {
            output["error_class"] = serde_json::json!("subagent_failure");
        }
        // 与 ErrorTurn span 的 metadata 格式对齐(trace_id == turn_id)
        let mut metadata = serde_json::json!({
            "is_synthetic": false,
            "was_sampled": true,
            "turn_id": self.trace_id.clone(),
        });
        if let Some(reason) = &closed.incomplete_reason {
            metadata["incomplete_reason"] = serde_json::json!(format!("{:?}", reason));
        }
        let body = ObservationBody {
            id: Some(closed.observation_id),
            trace_id: Some(self.trace_id.clone()),
            r#type: ObservationType::Agent,
            name: Some(format!("subagent-{}", closed.agent_name)),
            start_time: Some(closed.start_time),
            end_time: Some(closed.stop_time),
            completion_start_time: None,
            parent_observation_id: Some(closed.parent_observation_id),
            input: closed.input.clone(),
            output: Some(output),
            metadata: Some(metadata),
            model: None,
            model_parameters: None,
            usage: None,
            level,
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            session_id: Some(self.session_id.clone()),
        };
        let event = IngestionEvent::ObservationUpdate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body,
            metadata: None,
        };
        try_add_or_warn_via_session(
            &*self.session,
            event,
            &self.trace_id,
            "SubAgent ObservationUpdate",
        );
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
                    "tool_count": flush.tools.len(),
                    "failed_tools": flush.tools.iter().filter(|tool| tool.is_error).count(),
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
                    output: Some(if tool.is_error {
                        serde_json::json!({"error_class": "tool_failure"})
                    } else {
                        serde_json::json!(tool.output)
                    }),
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
