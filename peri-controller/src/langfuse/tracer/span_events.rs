use super::event_builder::{new_uuid, now_rfc3339, try_add_or_warn_via_session, VERSION};
use super::stages::MAIN_AGENT_KEY;
use super::LangfuseTracer;
use langfuse_client::types::ObservationLevel;
use langfuse_client::{IngestionEvent, SpanBody};
use peri_agent::agent::events::{
    CompactStrategy, CompactTrigger, MiddlewareHook, Stage, StageStatus,
};

impl LangfuseTracer {
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
    ///
    /// 结构约定：
    /// - name：Micro 策略记录为 `micro-compact`，其余（Full/Smart/Skip）记录为 `compact`，以区分类型
    /// - input：执行前状态（strategy/trigger/estimated_tokens_before/cache_hit_rate_before）
    /// - output：执行结果（summary/文件/技能/清理数/token 节省与剩余/escalation/outcome）
    pub fn on_compact_end(&mut self, info: crate::langfuse::tracer::compact::CompactEndInfo) {
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
        if duration_ms == 0 && !info.is_error {
            return;
        }

        // Micro compact 以独立类型记录（name 区分），Full/Smart/Skip 统一 compact
        let span_name = match ctx.strategy {
            CompactStrategy::Micro => "micro-compact",
            _ => "compact",
        };

        let output = if info.is_error {
            serde_json::json!({
                "error_class": "compact_failure",
                "message": info.error_message,
            })
        } else {
            serde_json::json!({
                "summary": info.summary,
                "files_count": info.files_count,
                "skills_count": info.skills_count,
                "micro_cleared": info.micro_cleared,
                "duration_ms": duration_ms,
                "estimated_tokens_saved": info.estimated_tokens_saved,
                "estimated_tokens_after": info.estimated_tokens_after,
                "full_escalation_reason": info.full_escalation_reason,
                "outcome": info.outcome,
            })
        };
        let level = if info.is_error {
            Some(ObservationLevel::Error)
        } else {
            None
        };

        let span_body = SpanBody {
            id: Some(ctx.span_id),
            trace_id: Some(self.trace_id.clone()),
            name: Some(span_name.to_string()),
            start_time: Some(ctx.start_time),
            end_time: Some(end_time.clone()),
            input: Some(serde_json::json!({
                "strategy": format!("{:?}", ctx.strategy),
                "trigger": format!("{:?}", ctx.trigger),
                "estimated_tokens_before": info.estimated_tokens_before,
                "cache_hit_rate_before": info.cache_hit_rate_before,
            })),
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
        let (handle, replaced) = self.stages.on_stage_start(
            MAIN_AGENT_KEY,
            stage,
            &self.trace_id,
            turn_id,
            &self.agent_observation_id,
        );
        // 工具批次归属 Act 阶段:ToolStart 先于 StageStarted(Act) 到达时,
        // batch parent 冻结在旧 stage(stage-reason),Act 开始后重挂到 stage-act
        if stage == Stage::Act {
            self.tool_batch.on_act_stage_start(&handle.span_id);
        }
        // 旧 stage 被覆盖:立即补发其合并 SpanCreate。若等待 StageEnded,
        // 乱序/重放丢失会导致 span 永不发送,工具 batch 的 parent 悬空(孤儿 batch)。
        if let Some(old) = replaced {
            self.emit_stage_span_close(&old, StageStatus::Done, None);
        }
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

        self.emit_stage_span_close(handle, status, receive_input);

        // Act stage 结束时按 owner flush 对应工具批次，保证每轮工具稳定挂在
        // `stage-act → tool-batch` 下。subagent 也必须逐 Act flush：若一直延迟到
        // AGENT obs 关闭，同一个 batch 会跨多个 Act，并被后续 on_act_stage_start
        // 反复重挂；最终若落到被 0ms 条件过滤的 Act，整批工具都会成为孤儿。
        if handle.stage == Stage::Act {
            let flush = match self.subagent.ownership(agent_id) {
                crate::langfuse::tracer::registry::Ownership::Main => Some(self.tool_batch.flush()),
                crate::langfuse::tracer::registry::Ownership::Subagent => {
                    Some(self.subagent.tool_batch_mut(agent_id).flush())
                }
                crate::langfuse::tracer::registry::Ownership::Unknown => None,
            };
            if let Some(flush) = flush {
                self.emit_tools_flush(flush);
            }
        }
        self.compact_work_done = false;
    }

    /// 发送 stage 的合并 SpanCreate（含 end_time）。
    ///
    /// 条件上报（v2 语义）：0ms stage 不上报；Compact 阶段无实际工作时不上报。
    /// 供 `on_stage_end` 与「旧 stage 被覆盖时立即补发」两条路径共用——
    /// 覆盖补发的 span 若与后续乱序到达的 StageEnded 重复发送，Langfuse 对
    /// 相同 observation id 的写入是 upsert，最终以较晚的 end_time/status 为准。
    pub(crate) fn emit_stage_span_close(
        &self,
        handle: &crate::langfuse::tracer::stages::StageHandle,
        status: StageStatus,
        receive_input: Option<serde_json::Value>,
    ) {
        // Compact stage：仅在实际执行了 micro/full compact 时才上报 span，
        // 否则跳过空 compact 阶段（无意义的 ~20ms span）
        if handle.stage == Stage::Compact && !self.compact_work_done {
            return;
        }

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
