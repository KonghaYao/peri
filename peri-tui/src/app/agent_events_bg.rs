use super::*;
use crate::ui::message_view::MessageViewModel;

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

        let short_id = &task_id[..8.min(task_id.len())];
        let mut found_and_updated = false;
        let session = &mut self.session_mgr.current_mut();

        if let Some(ref ctid) = child_thread_id {
            for vm in &mut session.messages.view_messages {
                if let MessageViewModel::SubAgentGroup {
                    instance_id,
                    is_running,
                    is_background,
                    total_steps,
                    bg_hash: _,
                    final_result,
                    is_error,
                    ..
                } = vm
                {
                    if *is_background
                        && *is_running
                        && instance_id.as_deref() == Some(ctid.as_str())
                    {
                        *is_running = false;
                        *final_result = Some(output.clone());
                        *is_error = !success;
                        *total_steps = tool_calls_count;
                        vm.recompute_hash();
                        found_and_updated = true;
                        break;
                    }
                }
            }
        }

        if !found_and_updated {
            let mut best_idx: Option<usize> = None;
            for (idx, vm) in session.messages.view_messages.iter().enumerate() {
                if let MessageViewModel::SubAgentGroup {
                    agent_id,
                    is_running,
                    is_background,
                    final_result,
                    ..
                } = vm
                {
                    if *is_background && *is_running && agent_id == &agent_name {
                        if final_result.is_none() {
                            best_idx = Some(idx);
                            break;
                        }
                        if best_idx.is_none() {
                            best_idx = Some(idx);
                        }
                    }
                }
            }
            if let Some(idx) = best_idx {
                let vm = &mut session.messages.view_messages[idx];
                if let MessageViewModel::SubAgentGroup {
                    is_running,
                    total_steps,
                    final_result,
                    is_error,
                    ..
                } = vm
                {
                    *is_running = false;
                    *final_result = Some(output.clone());
                    *is_error = !success;
                    *total_steps = tool_calls_count;
                    vm.recompute_hash();
                    found_and_updated = true;
                }
            }
        }

        // P5: No pipeline.notify_bg_completed() — SubAgentGroup state updated directly above

        if found_and_updated {
            self.request_rebuild();
        } else {
            let display_name = format!("bg:{}", agent_name);
            let first_line = output.lines().next().unwrap_or("");
            let one_line = if first_line.chars().count() > 80 {
                let truncated: String = first_line.chars().take(80).collect();
                format!("{}...", truncated)
            } else if first_line.is_empty() && !output.is_empty() {
                String::from("(empty)")
            } else {
                first_line.to_string()
            };
            let header_info = if success {
                format!(
                    "{} completed ({} calls, {}ms): {}",
                    short_id, tool_calls_count, duration_ms, one_line
                )
            } else {
                format!("{} failed: {}", short_id, one_line)
            };
            let mut vm =
                MessageViewModel::tool_block(display_name.clone(), header_info, None, !success);
            if let MessageViewModel::ToolBlock { collapsed, .. } = &mut vm {
                *collapsed = true;
                vm.recompute_hash();
            }
            self.apply_add_message(vm);
        }

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
    }
}
