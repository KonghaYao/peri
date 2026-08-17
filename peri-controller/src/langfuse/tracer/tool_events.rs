use super::event_builder::{new_uuid, try_add_or_warn_via_session, VERSION};
use super::registry::{GateEvent, Ownership};
use super::tool_batch;
use super::LangfuseTracer;
use langfuse_client::types::ObservationLevel;
use langfuse_client::{IngestionEvent, ObservationBody, ObservationType, SpanBody};

impl LangfuseTracer {
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
    pub(super) fn on_tool_start_inner(
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
                self.subagent
                    .touch_content_time(agent_id, &chrono::Utc::now().to_rfc3339());
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
    pub(super) fn on_tool_end_inner(
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

    /// 将 ToolsBatchFlush 转换为 Langfuse SpanCreate 事件并入队
    pub(super) fn emit_tools_flush(&self, flush: tool_batch::ToolsBatchFlush) {
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
