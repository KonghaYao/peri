//! SubAgent state tracking — token usage updates + subagent start events.
//! Extracted from original agent_ops.rs (2026-05-20 split).

use super::super::*;
use crate::app::{message_pipeline::PipelineAction, App};

impl App {
    pub(super) fn handle_token_usage_update(
        &mut self,
        usage: peri_acp::event::TokenUsageDto,
    ) -> (bool, bool, bool) {
        // SubAgent 的 TokenUsageUpdate 不应污染父 agent 的 tracker
        if self.session_mgr.current_mut().agent.subagent_depth > 0 {
            return (true, false, false);
        }

        // 转回 peri_agent 类型供 tracker 使用（短期：tracker 接口与 peri_agent 强绑定）
        let pa_usage = peri_agent::llm::types::TokenUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            request_id: usage.request_id.clone(),
        };

        // 累积到会话追踪器
        self.session_mgr
            .current_mut()
            .agent
            .session_token_tracker
            .accumulate(&pa_usage);

        // 缓存率检查：当次命中率低于 80% 时显示黄色提示
        let rate = self
            .session_mgr
            .current_mut()
            .agent
            .session_token_tracker
            .cache_hit_rate();
        if rate < 0.8 {
            let sid = self.session_mgr.current().metadata.session_id.to_string();
            let tracker = &self.session_mgr.current_mut().agent.session_token_tracker;
            tracing::warn!(
                input = tracker.total_input_tokens,
                cache_read = tracker.total_cache_read_tokens,
                rate_pct = rate * 100.0,
                "prompt cache hit rate below threshold"
            );
            peri_agent::metrics::emit(
                "trap.cache_anomaly",
                serde_json::json!({
                    "rate": rate,
                    "threshold": 0.80,
                    "request_id": tracker.last_request_id.as_deref().unwrap_or("-"),
                    "total_input_tokens": tracker.total_input_tokens,
                    "total_cache_read_tokens": tracker.total_cache_read_tokens,
                }),
                Some(&sid),
                None,
            );
            // 检查配置：show_cache_warning 为 false 时跳过消息流展示
            if self.services.peri_config.read().config.show_cache_warning {
                let percentage = (rate * 100.0) as u32;
                let req_id = tracker.last_request_id.as_deref().unwrap_or("-");
                let msg = format!(
                    "⚠ {}",
                    self.services.lc.tr_args(
                        "app-prompt-cache-low",
                        &[
                            ("rate".into(), (percentage as i64).into()),
                            ("req".into(), req_id.to_string().into()),
                        ]
                    )
                );
                let vm = MessageViewModel::system(msg);
                self.apply_pipeline_action(PipelineAction::AddMessage(vm));
            }
        }
        // 更新 spinner 的 token 显示（仅当次调用的 token，不累计）
        let current_tokens = pa_usage.input_tokens as usize + pa_usage.output_tokens as usize;
        self.session_mgr
            .current_mut()
            .spinner_state
            .set_token_count(current_tokens);
        (true, false, false)
    }

    pub(super) fn handle_subagent_start(
        &mut self,
        agent_id: String,
        instance_id: String,
        task_preview: String,
        is_background: bool,
    ) -> (bool, bool, bool) {
        if is_background {
            use super::super::chat_session::RunningBgAgent;
            self.session_mgr
                .current_mut()
                .background_agents
                .push(RunningBgAgent {
                    agent_name: agent_id.clone(),
                    instance_id: instance_id.clone(),
                    started_at: std::time::Instant::now(),
                    tool_count: 0,
                });
        }
        self.session_mgr.current_mut().agent.subagent_depth += 1;
        // Pipeline：创建 SubAgentGroup VM
        let actions = self
            .session_mgr
            .current_mut()
            .messages
            .pipeline
            .handle_event(AgentEvent::SubAgentStart {
                agent_id,
                instance_id,
                task_preview,
                is_background,
            });
        for action in actions {
            self.apply_pipeline_action(action);
        }
        self.request_rebuild();
        (true, false, false)
    }
}
