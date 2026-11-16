//! Langfuse tracer.

use super::{
    fmt, new_uuid, now_rfc3339, try_add_or_warn_via_session, Debug, EventBody, Formatter,
    IngestionEvent, LangfuseConfig, LangfuseSessionLike, ObservationLevel, StageHandle,
    TurnErrorReason, VERSION,
};

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
    #[must_use]
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
    #[must_use]
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

    // ── 其他 langfuse v2 事件 ───────────────────────────────────────────────

    /// AI 推理内容 chunk
    pub fn on_ai_reasoning_chunk(&mut self, agent_id: &str, text: &str) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        // 首个分片即该 generation 的 completion 起点；同一 generation 只记第一次。
        self.generation
            .mark_completion_start(agent_id, now_rfc3339());
        tracing::debug!(
            target: "langfuse::tracer",
            trace_id = %self.trace_id,
            text_len = text.len(),
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

impl Debug for LangfuseTracer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LangfuseTracer")
            .field("session_id", &self.session_id)
            .field("trace_id", &self.trace_id)
            .field("agent_observation_id", &self.agent_observation_id)
            .field("user_id_present", &self.user_id.is_some())
            .field("compact_work_done", &self.compact_work_done)
            .field("agent_start_time_present", &self.agent_start_time.is_some())
            .field("agent_input_present", &self.agent_input.is_some())
            .field("last_error_class_present", &self.last_error_class.is_some())
            .finish_non_exhaustive()
    }
}
