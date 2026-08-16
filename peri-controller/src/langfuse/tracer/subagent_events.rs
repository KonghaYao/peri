use super::event_builder::{new_uuid, now_rfc3339, try_add_or_warn_via_session, VERSION};
use super::registry::{self, GateEvent, Ownership};
use super::stages::StageHandle;
use super::LangfuseTracer;
use langfuse_client::types::ObservationLevel;
use langfuse_client::{IngestionEvent, ObservationBody, ObservationType};
use peri_agent::agent::events::Stage;

impl LangfuseTracer {
    // ── SubAgent 身份注册表(registry)入口 ────────────────────────────────────

    /// 注入主 agent 身份(bridge1 构造时调用;bridge2/workflow 不注入 → None fallback)
    pub(crate) fn set_main_agent_id(&mut self, id: String) {
        self.subagent.set_main_agent_id(id);
    }

    /// 内容事件归属:返回 (归属域, parent observation id)。
    /// 归属链:该 agent 的活跃 stage span → 该 agent 的 AGENT obs → 主 agent obs。
    /// None = 未知 agent(未注册且非主)→ 调用方走注册闸门/跳过,禁止挂主 agent。
    pub(super) fn content_owner(&self, agent_id: &str) -> Option<(Ownership, String)> {
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
    pub(super) fn llm_parent(&self, agent_id: &str) -> Option<String> {
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
        // 工具批次归属 Act 阶段:ToolStart 先于 StageStarted(Act) 到达时,
        // batch parent 冻结在旧 stage(stage-reason),Act 开始后重挂到 stage-act
        if stage == Stage::Act {
            match self.subagent.ownership(agent_id) {
                Ownership::Main => self.tool_batch.on_act_stage_start(&handle.span_id),
                Ownership::Subagent => {
                    self.subagent
                        .tool_batch_mut(agent_id)
                        .on_act_stage_start(&handle.span_id);
                }
                Ownership::Unknown => {}
            }
        }
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
    pub(super) fn handle_join_outcome(&mut self, outcome: registry::SubagentStartOutcome) {
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
    pub(super) fn emit_subagent_close(&self, closed: registry::ClosedSubagent) {
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
