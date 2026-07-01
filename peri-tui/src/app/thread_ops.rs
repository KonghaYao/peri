use super::*;

impl App {
    pub fn scroll_up(&mut self) {
        self.session_mgr.current_mut().ui.scroll_offset = self
            .session_mgr
            .current_mut()
            .ui
            .scroll_offset
            .saturating_sub(3);
        self.session_mgr.current_mut().ui.scroll_follow = false;
    }

    pub fn scroll_down(&mut self) {
        self.session_mgr.current_mut().ui.scroll_offset = self
            .session_mgr
            .current_mut()
            .ui
            .scroll_offset
            .saturating_add(3);
        self.session_mgr.current_mut().ui.scroll_follow = false;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.session_mgr.current_mut().ui.scroll_offset = u16::MAX;
        self.session_mgr.current_mut().ui.scroll_follow = true;
    }

    pub fn scroll_to_top(&mut self) {
        self.session_mgr.current_mut().ui.scroll_offset = 0;
        self.session_mgr.current_mut().ui.scroll_follow = false;
    }

    pub fn toggle_collapsed_messages(&mut self) {
        self.session_mgr.current_mut().ui.show_tool_messages =
            !self.session_mgr.current_mut().ui.show_tool_messages;
        // P5: sync rendering, no render_tx needed — toggled by draw()
    }

    pub fn toggle_diff(&mut self) {
        self.session_mgr.current_mut().ui.diff_visible =
            !self.session_mgr.current_mut().ui.diff_visible;
        // P5: sync rendering, diff visibility toggled by draw()
    }

    pub fn add_pending_attachment(&mut self, att: PendingAttachment) {
        self.session_mgr
            .current_mut()
            .metadata
            .pending_attachments
            .push(att);
    }

    pub fn pop_pending_attachment(&mut self) {
        self.session_mgr
            .current_mut()
            .metadata
            .pending_attachments
            .pop();
    }

    // ─── Thread 操作 ──────────────────────────────────────────────────────────

    fn reset_agent_session(&mut self) {
        self.session_mgr
            .current_mut()
            .agent
            .session_token_tracker
            .reset();
        self.session_mgr.current_mut().agent.retry_status = None;
        self.session_mgr.current_mut().agent.subagent_depth = 0;
        self.session_mgr.current_mut().agent.task_start_time = None;
        self.session_mgr.current_mut().agent.last_task_duration = None;
        self.session_mgr.current_mut().agent.agent_id = None;
        self.session_mgr.current_mut().agent.interaction_prompt = None;
        self.session_mgr.current_mut().agent.pending_hitl_items = None;
        self.session_mgr.current_mut().agent.pending_ask_user = None;
        self.session_mgr.current_mut().agent.cancel_token = None;
        self.session_mgr.current_mut().messages.last_submitted_text = None;
        self.session_mgr.current_mut().spinner_state.reset();
    }

    pub fn open_thread(&mut self, thread_id: ThreadId) {
        let store = self.services.thread_store.clone();
        let tid = thread_id.clone();
        let base_msgs = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(store.load_context(&tid))
                .unwrap_or_default()
        });
        self.session_mgr.current_mut().agent.origin_messages = base_msgs.clone();

        // Phase 2.6 step 7e.9: view_messages assignment removed — dead code.
        // Production rendering reads from v2 state.view (populated by ACP
        // ViewCommit during load_session below).

        self.session_mgr.current_mut().messages.message_cache = None;

        let thread_id_str = thread_id.to_string();
        self.session_mgr.current_mut().current_thread_id = Some(thread_id);
        if let Some(ref acp_client) = self.acp_client {
            let client = acp_client.clone();
            let cwd = self.services.cwd.clone();
            let model = self.services.model_name.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        client.load_session(&thread_id_str, &cwd, Some(&model)),
                    )
                    .await
                    {
                        Ok(Ok(sid)) => tracing::info!(session_id = %sid, "open_thread: ACP session synced"),
                        Ok(Err(e)) => tracing::warn!(error = %e, "open_thread: ACP session sync failed (compact may not work until first prompt)"),
                        Err(_elapsed) => {
                            tracing::warn!("open_thread: ACP session sync timed out after 5s");
                        }
                    }
                })
            });
        }
        self.session_mgr
            .current_mut()
            .metadata
            .pending_attachments
            .clear();
        self.session_mgr.current_mut().langfuse.langfuse_session = None;
        self.session_mgr.current_mut().todo_items.clear();

        self.reset_agent_session();
        crate::alloc_config::alloc_collect();

        self.session_mgr.current_mut().metadata.last_human_message = base_msgs
            .iter()
            .filter_map(|m| {
                if let BaseMessage::Human { content, .. } = m {
                    let text = content.text_content();
                    if text.trim().is_empty() {
                        None
                    } else {
                        Some(text)
                    }
                } else {
                    None
                }
            })
            .next_back();

        // P5: sync rendering, no render_tx needed
    }

    pub fn open_thread_with_feedback(&mut self, thread_id: ThreadId) {
        self.open_thread(thread_id);
    }

    pub fn new_thread(&mut self) {
        {
            let mut hooks = self
                .services
                .plugin_data
                .as_ref()
                .map(|pd| pd.all_hooks.clone())
                .unwrap_or_default();
            hooks.extend(peri_middlewares::hooks::loader::load_global_settings_hooks());
            hooks.extend(peri_middlewares::hooks::loader::load_settings_local_hooks(
                &self.services.cwd,
            ));
            if !hooks.is_empty() {
                let cwd = self.services.cwd.clone();
                let provider_name = self.services.provider_name.clone();
                tokio::spawn(async move {
                    peri_middlewares::hooks::middleware::fire_standalone_lifecycle_hooks(
                        &hooks,
                        peri_middlewares::hooks::types::HookEvent::SessionEnd,
                        &cwd,
                        "",
                        "",
                        &provider_name,
                        None,
                        Some("clear"),
                    )
                    .await;
                });
            }
        }

        self.session_mgr.current_mut().agent.origin_messages.clear();
        self.session_mgr
            .current_mut()
            .agent
            .origin_messages
            .shrink_to_fit();
        self.session_mgr.current_mut().current_thread_id = None;
        self.session_mgr.current_mut().todo_items.clear();
        self.session_mgr
            .current_mut()
            .metadata
            .pending_attachments
            .clear();
        self.session_mgr.current_mut().langfuse.langfuse_session = None;
        self.session_mgr.current_mut().metadata.last_human_message = None;
        self.session_mgr.current_mut().messages.last_submitted_text = None;
        self.session_mgr.current_mut().metadata.pre_submit_state_len = 0;
        // Phase 2.3: 清空 SubAgent 运行时状态映射（与 view_messages.clear 同步）
        self.session_mgr.current_mut().subagent_status.clear();

        self.reset_agent_session();

        if let Some(ref acp_client) = self.acp_client {
            let client = acp_client.clone();
            let cwd = self.services.cwd.clone();
            let model = self.services.model_name.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        client.new_session(&cwd, Some(&model)),
                    )
                    .await
                    {
                        Ok(Ok(sid)) => tracing::info!(session_id = %sid, "new_thread: ACP new_session succeeded"),
                        Ok(Err(e)) => tracing::warn!(error = %e, "new_thread: ACP new_session failed"),
                        Err(_elapsed) => {
                            tracing::warn!("new_thread: ACP new_session timed out after 5s");
                        }
                    }
                })
            });
        }
        crate::alloc_config::alloc_collect();
        // P5: sync rendering, no render_tx Clear needed
    }
}

#[cfg(test)]
mod tests {
    use crate::thread::ThreadMeta;
    include!("thread_ops_test.rs");
}
