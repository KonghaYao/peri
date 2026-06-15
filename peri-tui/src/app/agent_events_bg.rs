use super::{message_pipeline::PipelineAction, *};
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

/// 构建后台任务完成的显示通知文本（截断版，供 UI 展示）
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
        let output_preview: String = output
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect();
        let _preview = if output.chars().count() > 80 || output.lines().count() > 1 {
            format!("{}...", output_preview)
        } else {
            output_preview
        };
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
        // 优先按 child_thread_id 匹配 background_agents，回退到 agent_name
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
            // 回退：按 agent_name 匹配
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

        // 聚焦检查：用 child_thread_id 直接比较 focused_instance_id
        let was_focused = child_thread_id.as_deref()
            == self
                .session_mgr
                .current_mut()
                .focused_instance_id
                .as_deref();

        // 聚焦检查：如果被移除的是当前聚焦的 agent，退出聚焦
        if was_focused {
            self.session_mgr.current_mut().focused_instance_id = None;
            self.session_mgr.current_mut().ui.bg_bar_cursor = None;
            self.request_rebuild();
        }

        tracing::info!(
            task_id = %task_id,
            agent_name = %agent_name,
            child_thread_id = ?child_thread_id,
            success = success,
            bg_count_before = self.session_mgr.current_mut().background_agents.len() + 1,
            bg_count_after = self.session_mgr.current_mut().background_agents.len(),
            agent_done_pending = self.session_mgr.current_mut().agent.bg_task_state.agent_done_pending,
            "[bg-diag] TUI: handle_background_task_completed called"
        );

        // 用于 LLM 上下文的纯文本通知
        let short_id = &task_id[..8.min(task_id.len())];
        let state_notification = if success {
            self.services.lc.tr_args(
                "app-bg-task-done-with-result",
                &[
                    ("id".into(), short_id.into()),
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
                    ("id".into(), short_id.into()),
                    ("agent".into(), agent_name.clone().into()),
                    ("error".into(), output.clone().into()),
                ],
            )
        };

        // 将通知加入 origin_messages，使下一轮 agent 执行可见。
        // 仅在 executor 已结束（bg_task_state.agent_done_pending）时直接 push 作为兜底；
        // executor 运行期间的通知由 drain_notifications → StateSnapshot 路径写入，
        // 此处 push 会导致 origin_messages 中出现重复消息。
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

        // 尝试在 view_messages 中找到匹配的 SubAgentGroup 并更新。
        let short_id = &task_id[..8.min(task_id.len())];
        let mut found_and_updated = false;
        let session = &mut self.session_mgr.current_mut();

        // 第一遍：按 instance_id 精确匹配（child_thread_id → SubAgentGroup.instance_id）
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

        // 第二遍（兜底）：按 agent_name 匹配 is_running 的 SubAgentGroup
        // 同名并发场景：优先匹配 final_result 为空的 group（尚未被更新），
        // 防止多个同名 bg agent 的 completion 事件反复匹配同一个 group。
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
                            break; // 精确匹配：尚未被更新的 group
                        }
                        // 兜底：group 正在运行但已被更新（不应发生）
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

        // 同步更新管线状态（SubAgentState + frozen VM），
        // 确保 reconcile/rebuild 产生的 SubAgentGroup 携带完成状态。
        // 不依赖 child_thread_id——无则用 agent_name 兜底匹配。
        self.session_mgr
            .current_mut()
            .messages
            .pipeline
            .notify_bg_completed(
                child_thread_id.as_deref(),
                &agent_name,
                &output,
                success,
                tool_calls_count,
            );

        if found_and_updated {
            // 成功更新 SubAgentGroup，触发 RebuildAll
            self.request_rebuild();
        } else {
            // 未找到匹配的 SubAgentGroup，回退到创建 ToolBlock（兼容现有行为）
            let display_name = format!("bg:{}", agent_name);
            // 输出截断为单行（取第一行，再截取前 80 字符）
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
                *collapsed = true; // 始终折叠，摘要已在 header 中
                vm.recompute_hash();
            }
            self.apply_pipeline_action(PipelineAction::AddMessage(vm));
        }

        // 诊断日志：记录 BackgroundTaskCompleted 处理后的 view_messages 中 SubAgentGroup 数量
        {
            let subagent_count = self
                .session_mgr
                .current_mut()
                .messages
                .view_messages
                .iter()
                .filter(|vm| vm.is_subagent_group())
                .count();
            let frozen_count = self
                .session_mgr
                .current_mut()
                .messages
                .pipeline
                .frozen_subagent_vms_count();
            tracing::debug!(
                task_id = %&task_id[..8.min(task_id.len())],
                agent_name = %agent_name,
                child_thread_id = ?child_thread_id,
                subagent_count_in_view = subagent_count,
                frozen_count,
                background_agents_count = self.session_mgr.current_mut().background_agents.len(),
                bg_task_state.agent_done_pending = self.session_mgr.current_mut().agent.bg_task_state.agent_done_pending,
                "[bg-diag] after BackgroundTaskCompleted"
            );
        }

        // 累积当前完成通知到 bg_task_state.pre_done_completions（显示文本）
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

        // 累积结构化结果到 bg_task_state.pre_done_results（供 auto-continuation 注入合成消息）
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

        // 如果 agent 已完成（Done）且所有后台任务都已完成，关闭通道并自动提交 continuation
        if self
            .session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .agent_done_pending
            && self.session_mgr.current_mut().background_agents.is_empty()
        {
            tracing::info!("all background tasks completed, auto-submitting continuation");
            self.session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .agent_done_pending = false;
            // 使用结构化结果（而非显示文本）驱动 continuation
            let all_results: Vec<_> = self
                .session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .pre_done_results
                .drain(..)
                .collect();
            self.session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .pending_continuation = Some(all_results);

            return (true, false, true);
        } else if !self
            .session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .agent_done_pending
            && self.session_mgr.current_mut().background_agents.is_empty()
        {
            // 竞态修复：agent 尚未 Done，但所有后台任务已完成。
            // 暂存通知已在上方 push，待 Done 处理时检查此字段并设置 bg_task_state.pending_continuation。
            tracing::info!(
                "background task completed before Done, buffering notification for deferred continuation"
            );
        }

        (true, false, false)
    }

    /// 后台 agent 工具调用进度更新（BgToolStep 事件）
    /// 递增 RunningBgAgent.tool_count，用于 bg_agent_bar 实时显示
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
