//! SubAgent state tracking — token usage updates + subagent start events.
//! Extracted from original agent_ops.rs (2026-05-20 split).

use super::super::*;

impl App {
    pub(super) fn handle_token_usage_update(
        &mut self,
        usage: peri_acp::event::TokenUsageDto,
    ) -> (bool, bool, bool) {
        if self.session_mgr.current_mut().agent.subagent_depth > 0 {
            return (true, false, false);
        }

        let pa_usage = peri_agent::llm::types::TokenUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            request_id: usage.request_id.clone(),
        };

        self.session_mgr
            .current_mut()
            .agent
            .session_token_tracker
            .accumulate(&pa_usage);

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
                self.apply_add_message(vm);
            }
        }
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
            // P5: Background subagents don't increment subagent_depth
            // (they run in parallel, not nested within the main agent flow)
        } else {
            self.session_mgr.current_mut().agent.subagent_depth += 1;
        }

        // Phase 2.3: 同步注册到 SubAgentStatusMap（v2 渲染时覆盖 DTO 静态字段）
        self.session_mgr.current_mut().subagent_status.start(
            instance_id.clone(),
            agent_id.clone(),
            task_preview.clone(),
            is_background,
        );

        // P5: Create SubAgentGroup VM directly instead of through pipeline
        let vm = MessageViewModel::SubAgentGroup {
            agent_id: agent_id.clone(),
            instance_id: Some(instance_id.clone()),
            task_preview: task_preview.clone(),
            is_running: true,
            is_background,
            total_steps: 0,
            recent_messages: Vec::new(),
            collapsed: false,
            bg_hash: None,
            final_result: None,
            is_error: false,
            batch_agents: Vec::new(),
            content_hash: 0,
        };
        self.apply_add_message(vm);

        self.request_rebuild();
        (true, false, false)
    }
}
