//! Turn 生命周期编排；全部观测同步入队后才返回最终 flush 任务。

use super::event_builder::{new_uuid, now_rfc3339, try_add_or_warn_via_session, VERSION};
use super::turn_error::{failure_error_class, failure_output};
use super::turn_fallback::GenerationFallbackStatus;
use super::LangfuseTracer;
use langfuse_client::types::session::SessionBody;
use langfuse_client::types::TraceBody;
use langfuse_client::{IngestionEvent, ObservationBody, ObservationType};
use peri_acp_types::session::TurnTelemetryOutcome;

impl LangfuseTracer {
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
    /// ErrorEvent 机制：当轮次以 error 结束时，始终发送 ErrorTurn event
    /// （即使该轮次未被采样），确保错误可观测。错误是"时点标记"而非一段
    /// 工作区间，故用 Event 类型（无 end_time 语义），不产生误导性的 0ms span。
    pub fn on_turn_end(&mut self, outcome: TurnTelemetryOutcome) -> tokio::task::JoinHandle<()> {
        use std::sync::Arc;

        let fallback_status = GenerationFallbackStatus::for_outcome(&outcome);
        let failure = fallback_status.failure;
        let error_class = failure
            .map(failure_error_class)
            .or_else(|| {
                self.last_error_class
                    .take()
                    .map(|reason| reason.to_string())
            })
            .unwrap_or_else(|| fallback_status.error_class.clone());
        let is_error = failure.is_some();

        self.close_stage_parents();

        self.close_abandoned_generations(&fallback_status, &error_class);

        // 先 flush tools batch，发出 batch span + 所有工具 span
        let flush = self.tool_batch.flush();
        self.emit_tools_flush(flush);

        // 兜底:清理未收 Stop 的活跃 subagent(pending/gate/残留 invocation),
        // 关闭其 AGENT obs(metadata 携带 incomplete_reason)。
        let closed_list = self.subagent.cleanup_turn_end();
        for closed in closed_list {
            self.emit_subagent_close(closed);
        }

        let sampled = self.sampling.should_emit(&self.trace_id, &self.session_id);

        // ErrorSpan：错误时始终发送（即使未采样），确保错误可观测
        if is_error && self.config.error_span_always {
            self.emit_error_turn(sampled, failure, &error_class);
        }

        // 未采样的非错误 turn 无事件可发送；未采样 fatal 已补发 synthetic
        // trace/error event，仍须立即 flush，不能依赖定时批处理或进程寿命。
        if !sampled {
            self.sampling.cleanup_turn(&self.trace_id);
            if is_error && self.config.error_span_always {
                let session = Arc::clone(&self.session);
                let trace_id = self.trace_id.clone();
                return tokio::spawn(async move {
                    if session.flush().await.is_err() {
                        tracing::warn!(trace_id = %trace_id, "langfuse: session flush failed");
                    }
                });
            }
            return tokio::spawn(async {});
        }

        let session = Arc::clone(&self.session);
        let trace_id = self.trace_id.clone();
        let agent_observation_id = self.agent_observation_id.clone();
        let output = if is_error {
            Some(failure_output(
                failure.expect("is_error requires failure"),
                &error_class,
            ))
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
}
