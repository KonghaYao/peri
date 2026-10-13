use super::event_builder::{
    new_uuid, now_rfc3339, try_add_or_warn_via_session, try_add_prepared_or_warn_via_session,
    VERSION,
};
use super::generation::PendingTerminalState;
use super::input_snapshot::{llm_input_source, mark_rejected_input, GenerationInput};
use super::registry::{GateEvent, Ownership};
use super::usage;
use super::LangfuseTracer;
use langfuse_client::snapshot::{snapshot_value, SnapshotSource};
use langfuse_client::types::{EventBody, ObservationLevel};
use langfuse_client::{GenerationBody, IngestionEvent};
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;
use peri_model::TokenUsage;
use std::sync::Arc;

impl LangfuseTracer {
    // ── LLM Generation 事件 ──────────────────────────────────────────────────

    /// 冻结输入来源；被拒时返回 [`GenerationInput::Rejected`]（batcher 已计入该 generation
    /// 拒收），不缓存原文、不等待额度（契约 §3.6）。
    fn freeze_input(&self, source: Arc<dyn SnapshotSource>) -> GenerationInput {
        match self.session.try_freeze(source) {
            Ok(frozen) => GenerationInput::Frozen(frozen),
            Err(error) => {
                tracing::warn!(
                    target: "langfuse::tracer",
                    trace_id = %self.trace_id,
                    %error,
                    "LLM input snapshot rejected; generation input is not reported"
                );
                GenerationInput::Rejected
            }
        }
    }

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
        self.on_llm_start_inner(agent_id, step, messages, tools);
    }

    /// LLM 调用开始(业务主体;供 gate 重放复用,不重复采样检查)。
    /// 返回 false = 事件被注册闸门缓存或丢弃。
    pub(super) fn on_llm_start_inner(
        &mut self,
        agent_id: &str,
        step: usize,
        messages: &[BaseMessage],
        tools: &[ToolDefinition],
    ) -> bool {
        // 快照计费先于归属判定与注册闸门：闸门只保存已冻结来源。
        let input = self.freeze_input(llm_input_source(
            &Arc::new(messages.to_vec()),
            &Arc::new(tools.to_vec()),
        ));
        self.on_llm_start_snapshot_inner(agent_id, step, input)
    }

    pub(crate) fn on_llm_start_snapshot(
        &mut self,
        agent_id: &str,
        step: usize,
        messages: Arc<Vec<BaseMessage>>,
        tools: &[ToolDefinition],
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let input = self.freeze_input(llm_input_source(&messages, &Arc::new(tools.to_vec())));
        self.on_llm_start_snapshot_inner(agent_id, step, input);
    }

    /// 归属分派；`input` 是已在闸门之前完成计费的冻结来源。
    pub(super) fn on_llm_start_snapshot_inner(
        &mut self,
        agent_id: &str,
        step: usize,
        input: GenerationInput,
    ) -> bool {
        match self.subagent.ownership(agent_id) {
            Ownership::Main => {
                self.generation.on_llm_start(agent_id, step, input);
                true
            }
            Ownership::Subagent => {
                self.generation.on_llm_start(agent_id, step, input);
                self.subagent
                    .touch_content_time(agent_id, &peri_time::now_utc_rfc3339());
                true
            }
            Ownership::Unknown => {
                // 未知 agent(Start 未到/已 incomplete)→ 注册闸门缓存,等 join 重放
                if self.subagent.try_gate(GateEvent::LlmCallStart {
                    agent_id: agent_id.to_string(),
                    step,
                    input,
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

    /// LLM 请求体接收：紧随 on_llm_start 之后，用 Provider 实际请求体替换 fallback。
    /// 冻结仍先于闸门；冻结被拒时只标记，不改用旧 fallback 冒充实际请求。
    pub fn on_llm_request_payload(
        &mut self,
        agent_id: &str,
        step: usize,
        body: std::sync::Arc<serde_json::Value>,
    ) {
        if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
            return;
        }
        let tracked = self.generation.contains(agent_id, step);
        if !tracked && self.subagent.ownership(agent_id) != Ownership::Unknown {
            return; // 保留既有静默 no-op
        }
        let input = self.freeze_input(snapshot_value(&body));
        if tracked {
            self.generation
                .on_llm_request_payload(agent_id, step, input);
        } else {
            self.subagent.try_gate(GateEvent::LlmRequestPayload {
                agent_id: agent_id.to_string(),
                step,
                input,
            });
        }
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

        let start_missing = !self.generation.contains(agent_id, step);
        if start_missing {
            tracing::warn!(
                target: "langfuse::generation",
                %agent_id,
                step,
                "on_llm_end: 缺少 LlmCallStart，创建 synthetic generation"
            );
            let input = self.freeze_input(llm_input_source(&Arc::default(), &Arc::default()));
            self.generation.on_llm_start(agent_id, step, input);
        }

        // 必须先解析 parent 再消费 tracker state。未知 ownership 可能由可丢的
        // SubagentStart 导致；完整终态保留到 turn-end fallback。
        let parent_id = match self.llm_parent(agent_id) {
            Some(parent_id) => parent_id,
            None => {
                self.generation.preserve_terminal(
                    agent_id,
                    step,
                    PendingTerminalState {
                        model: model.to_string(),
                        output: output.to_string(),
                        usage: usage.cloned(),
                        request_id: request_id.map(str::to_string),
                    },
                );
                tracing::warn!(
                    target: "langfuse::subagent",
                    %agent_id,
                    step,
                    "on_llm_end: agent ownership 暂不可解析，保留完整终态等待 turn-end 兜底"
                );
                return;
            }
        };

        let gen_end = self
            .generation
            .on_llm_end(agent_id, step)
            .expect("generation existence checked before removal");

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

        // parent 已在消费 tracker state 前解析，避免 ownership 暂不可用时永久丢失。

        // 合并 retry metadata + token 用量到 metadata 字段（Langfuse UI 可见）
        let mut meta = gen_end.retry_metadata.unwrap_or(serde_json::json!({}));
        if gen_end.input.is_rejected() {
            // 冻结被拒：明确标记，不把旧 fallback 当作实际请求上报（契约 §3.6）
            mark_rejected_input(&mut meta);
        }
        if start_missing {
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("start_missing".to_string(), serde_json::json!(true));
                obj.insert("lifecycle_incomplete".to_string(), serde_json::json!(true));
            }
        }
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

        let (level, status_message, generation_output) = if output.starts_with("ERROR: ") {
            (
                Some(ObservationLevel::Error),
                Some(output.to_string()),
                Some(
                    serde_json::json!({"error_class": "provider_or_stream_failure", "text": output}),
                ),
            )
        } else {
            (None, None, Some(parse_output(output)))
        };

        // input 不再进 body：冻结来源随事件移动到 worker（结束不等 IO，也不克隆原文）
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
        // 冻结被拒（`Rejected`）时事件不带来源，是直接事件：同一准入实现的直接入口才是它的
        // 交付路径（`try_add_prepared` 只承载携带来源的事件）；拒收事实已由 metadata 标记。
        match gen_end.input.into_source() {
            Some(input) => try_add_prepared_or_warn_via_session(
                &*self.session,
                event,
                Some(input),
                None,
                &self.trace_id,
                "LLM GenerationCreate",
            ),
            None => try_add_or_warn_via_session(
                &*self.session,
                event,
                &self.trace_id,
                "LLM GenerationCreate",
            ),
        }

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
