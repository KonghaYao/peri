use super::*;

/// 后台任务完成的事件参数
pub(crate) struct BackgroundTaskResult {
    pub task_id: String,
    pub agent_name: String,
    pub success: bool,
    pub output: String,
    pub tool_calls_count: usize,
    pub duration_ms: u64,
    pub child_thread_id: Option<String>,
}

fn build_bg_display_notification(
    task_id: &str,
    agent_name: &str,
    success: bool,
    output: &str,
    tool_calls_count: usize,
    duration_ms: u64,
    lc: &crate::i18n::LcRegistry,
) -> String {
    let short_id = &task_id[..8.min(task_id.len())];
    if success {
        let _output_preview: String = output
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect();
        lc.tr_args(
            "app-bg-task-done",
            &[
                ("id".into(), short_id.into()),
                ("agent".into(), agent_name.into()),
                ("tools".into(), (tool_calls_count as i64).into()),
                ("duration".into(), (duration_ms as i64).into()),
            ],
        )
    } else {
        let err_preview: String = output.chars().take(80).collect();
        lc.tr_args(
            "app-bg-task-failed",
            &[
                ("id".into(), short_id.into()),
                ("agent".into(), agent_name.into()),
                ("error".into(), err_preview.into()),
            ],
        )
    }
}

impl App {
    pub(crate) fn handle_background_task_completed(
        &mut self,
        result: BackgroundTaskResult,
    ) -> (bool, bool, bool) {
        let BackgroundTaskResult {
            task_id,
            agent_name,
            success,
            output,
            tool_calls_count,
            duration_ms,
            child_thread_id,
        } = result;

        if let Some(ref ctid) = child_thread_id {
            if let Some(pos) = self
                .session_mgr
                .current_mut()
                .background_agents
                .iter()
                .position(|a| a.instance_id == *ctid)
            {
                self.session_mgr.current_mut().background_agents.remove(pos);
            }
        } else {
            if let Some(pos) = self
                .session_mgr
                .current_mut()
                .background_agents
                .iter()
                .position(|a| a.agent_name == agent_name)
            {
                self.session_mgr.current_mut().background_agents.remove(pos);
            }
        }

        let was_focused = child_thread_id.as_deref()
            == self
                .session_mgr
                .current_mut()
                .focused_instance_id
                .as_deref();

        if was_focused {
            self.session_mgr.current_mut().focused_instance_id = None;
            self.session_mgr.current_mut().ui.bg_bar_cursor = None;
            self.request_rebuild();
        }

        // Phase 2.3: 同步完成状态到 SubAgentStatusMap（v2 渲染时覆盖 DTO）
        {
            let key = child_thread_id
                .as_deref()
                .or(Some(task_id.as_str()))
                .filter(|s| !s.is_empty());
            if let Some(inst) = key {
                self.session_mgr
                    .current_mut()
                    .subagent_status
                    .complete_background(inst, output.clone(), !success, tool_calls_count);
            }
        }

        let short_id_state = &task_id[..8.min(task_id.len())];
        let state_notification = if success {
            self.services.lc.tr_args(
                "app-bg-task-done-with-result",
                &[
                    ("id".into(), short_id_state.into()),
                    ("agent".into(), agent_name.clone().into()),
                    ("tools".into(), (tool_calls_count as i64).into()),
                    ("duration".into(), (duration_ms as i64).into()),
                    ("result".into(), output.clone().into()),
                ],
            )
        } else {
            self.services.lc.tr_args(
                "app-bg-task-failed-with-error",
                &[
                    ("id".into(), short_id_state.into()),
                    ("agent".into(), agent_name.clone().into()),
                    ("error".into(), output.clone().into()),
                ],
            )
        };

        if self
            .session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .agent_done_pending
        {
            self.session_mgr.current_mut().agent.origin_messages.push(
                peri_agent::messages::BaseMessage::human(state_notification.as_str()),
            );
        }

        // Phase 2.6 step 6 — 删除 view_messages.SubAgentGroup iter_mut 突变 +
        // ToolBlock fallback。生产渲染完全通过 SubAgentStatusMap +
        // SessionSubAgentProbe 读取 bg 完成状态，不依赖 view_messages。
        // 上方 subagent_status.complete_background() 已是权威路径。
        let _ = (
            &task_id,
            &agent_name,
            success,
            &output,
            tool_calls_count,
            duration_ms,
        );

        if agent_name.starts_with("workflow:") {
            let workflow_name = agent_name.strip_prefix("workflow:").unwrap_or(&agent_name);
            let continuation_text = format!(
                "<system-reminder>\nWorkflow '{}' has completed. Please review the results from \
                 .claude/workflow-runs/{}/state.json.\n</system-reminder>",
                workflow_name, task_id,
            );

            let loading = self.session_mgr.current().ui.loading;
            self.session_mgr
                .current_mut()
                .messages
                .pending_messages
                .push(continuation_text);

            if !loading {
                return (true, false, true);
            }
            return (true, false, false);
        }

        let display_notification = build_bg_display_notification(
            &task_id,
            &agent_name,
            success,
            &output,
            tool_calls_count,
            duration_ms,
            &self.services.lc,
        );
        self.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pre_done_completions
            .push(display_notification);

        self.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pre_done_results
            .push(peri_agent::agent::events::BackgroundTaskResult {
                task_id: task_id.clone(),
                agent_name: agent_name.clone(),
                prompt_summary: String::new(),
                success,
                output,
                tool_calls_count,
                duration_ms,
                child_thread_id: child_thread_id.clone(),
            });

        if self
            .session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .agent_done_pending
            && self.session_mgr.current_mut().background_agents.is_empty()
        {
            self.session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .agent_done_pending = false;
            let _all_results: Vec<_> = self
                .session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .pre_done_results
                .drain(..)
                .collect();

            return (true, false, true);
        } else if !self
            .session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .agent_done_pending
            && self.session_mgr.current_mut().background_agents.is_empty()
        {
            tracing::info!(
                "background task completed before Done, buffering notification for deferred continuation"
            );
        }

        (true, false, false)
    }

    pub(crate) fn handle_bg_tool_step(&mut self, child_thread_id: &str) {
        let session = self.session_mgr.current_mut();
        if let Some(agent) = session
            .background_agents
            .iter_mut()
            .find(|a| a.instance_id == child_thread_id)
        {
            agent.tool_count += 1;
        }
        // Phase 2.3: 同步到 SubAgentStatusMap（供 v2 渲染覆盖）
        session.subagent_status.incr_tool_step(child_thread_id);
    }
}
