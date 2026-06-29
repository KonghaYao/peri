use super::*;

impl App {
    pub fn submit_message(&mut self, input: String) {
        if input.trim().is_empty() {
            return;
        }

        // P5: /streaming command removed with pipeline deletion

        self.session_mgr.current_mut().metadata.pre_submit_state_len =
            self.session_mgr.current_mut().agent.origin_messages.len();

        self.push_input_history(input.clone());

        let attachments =
            std::mem::take(&mut self.session_mgr.current_mut().metadata.pending_attachments);

        let display = if attachments.is_empty() {
            input.clone()
        } else {
            self.services.lc.tr_args(
                "app-submit-attachments",
                &[
                    ("input".into(), input.clone().into()),
                    ("count".into(), (attachments.len() as i64).into()),
                ],
            )
        };

        let message_content = if attachments.is_empty() {
            peri_agent::messages::MessageContent::text(input.clone())
        } else {
            let mut blocks = vec![peri_agent::messages::ContentBlock::text(input.clone())];
            for att in attachments {
                blocks.push(peri_agent::messages::ContentBlock::image_base64(
                    &att.media_type,
                    &att.base64_data,
                ));
            }
            peri_agent::messages::MessageContent::Blocks(blocks)
        };

        // P5: No pipeline.begin_round() — round tracking via round_start_vm_idx

        let user_vm = MessageViewModel::user(display.clone());
        self.apply_add_message(user_vm);
        self.session_mgr.current_mut().messages.round_start_vm_idx =
            self.session_mgr.current_mut().messages.view_messages.len();
        self.session_mgr.current_mut().metadata.last_human_message = Some(display);
        self.session_mgr.current_mut().messages.last_submitted_text = Some(input.clone());
        self.set_loading(true);
        self.session_mgr.current_mut().ui.scroll_offset = u16::MAX;
        self.session_mgr.current_mut().ui.scroll_follow = true;
        self.session_mgr.current_mut().todo_items.clear();

        self.session_mgr.current_mut().agent.task_start_time = Some(std::time::Instant::now());
        self.session_mgr.current_mut().agent.last_task_duration = None;
        if self
            .session_mgr
            .current_mut()
            .agent
            .session_start_time
            .is_none()
        {
            self.session_mgr.current_mut().agent.session_start_time =
                Some(std::time::Instant::now());
        }

        let provider = {
            let cfg_guard = self.services.peri_config.read();
            agent::LlmProvider::from_config(&cfg_guard)
        };
        let provider = match provider.or_else(agent::LlmProvider::from_env) {
            Some(p) => p,
            None => {
                self.apply_add_message(MessageViewModel::system(
                    self.services.lc.tr("app-no-provider-submit"),
                ));
                self.set_loading(false);
                return;
            }
        };

        {
            let mut model_cw = provider.context_window();
            if self
                .services
                .peri_config
                .read()
                .config
                .context_1m
                .unwrap_or(false)
            {
                model_cw = 1_000_000;
            }
            if model_cw > 0 && self.session_mgr.current_mut().agent.context_window != model_cw {
                self.session_mgr.current_mut().agent.context_window = model_cw;
            }
        }

        self.session_mgr.current_mut().agent.subagent_depth = 0;
        self.session_mgr.current_mut().agent.agent_replied = false;
        self.session_mgr.current_mut().agent.reconcile_already_done = false;

        let pending_bg_results: Vec<crate::app::agent_comm::BgTaskResult> = self
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
            .reset_for_new_round();
        self.session_mgr.current_mut().agent.lsp_diagnostics.reset();

        let cwd = self.services.cwd.clone();
        if let Some(ref acp_client) = self.acp_client {
            let acp_client_clone = acp_client.clone();
            let model_clone = self.services.model_name.clone();
            let message_content_clone = message_content.clone();
            let cwd_clone = cwd.clone();
            let existing_thread_id = self.session_mgr.current_mut().current_thread_id.clone();

            tokio::spawn(async move {
                let client = acp_client_clone;
                if !client.has_session() {
                    if let Some(ref tid) = existing_thread_id {
                        match client
                            .load_session(tid, &cwd_clone, Some(&model_clone))
                            .await
                        {
                            Ok(sid) => {
                                tracing::info!(session_id = %sid, "ACP submit: load_session succeeded")
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "ACP submit: load_session FAILED");
                                return;
                            }
                        }
                    } else {
                        match client.new_session(&cwd_clone, Some(&model_clone)).await {
                            Ok(sid) => {
                                tracing::info!(session_id = %sid, "ACP submit: new_session succeeded")
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "ACP submit: new_session FAILED");
                                return;
                            }
                        }
                    }
                }
                if pending_bg_results.is_empty() {
                    match client.prompt(&message_content_clone).await {
                        Ok(()) => tracing::info!("ACP submit: prompt completed"),
                        Err(e) => tracing::error!(error = %e, "ACP submit: prompt FAILED"),
                    }
                } else {
                    match client
                        .prompt_with_bg_results(&message_content_clone, pending_bg_results)
                        .await
                    {
                        Ok(()) => {
                            tracing::info!("ACP submit: prompt_with_bg_results completed")
                        }
                        Err(e) => tracing::error!(
                            error = %e,
                            "ACP submit: prompt_with_bg_results FAILED"
                        ),
                    }
                }
            });
        } else {
            tracing::error!("ACP client not initialized, cannot submit agent");
            self.apply_add_message(MessageViewModel::system(
                self.services.lc.tr("app-no-provider-submit"),
            ));
            self.set_loading(false);
        }
    }

    pub(crate) fn flush_pending_messages(&mut self) {
        if let Some(msg) = self
            .session_mgr
            .current_mut()
            .messages
            .pending_messages
            .first()
            .cloned()
        {
            self.session_mgr
                .current_mut()
                .messages
                .pending_messages
                .remove(0);
            self.submit_message(msg);
        }
    }
}
