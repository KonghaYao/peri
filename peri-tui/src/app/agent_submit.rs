use super::{message_pipeline::PipelineAction, *};

impl App {
    pub fn submit_message(&mut self, input: String) {
        if input.trim().is_empty() {
            return;
        }

        // ── TUI 本地命令拦截：/streaming ──
        if let Some(args) = input.strip_prefix("/streaming") {
            self.handle_streaming_command(args.trim());
            return;
        }

        // 记录提交前的状态长度，用于中断时回滚 origin_messages
        self.session_mgr.current_mut().metadata.pre_submit_state_len =
            self.session_mgr.current_mut().agent.origin_messages.len();

        self.push_input_history(input.clone());

        // 消费待发送附件
        let attachments =
            std::mem::take(&mut self.session_mgr.current_mut().metadata.pending_attachments);

        // 构建用于显示的文字（附件摘要追加在末尾）
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

        // 构建发送给 LLM 的 MessageContent（含附件图片 blocks）
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
        self.session_mgr
            .current_mut()
            .messages
            .pipeline
            .begin_round();
        let user_vm = MessageViewModel::user(display.clone());
        self.apply_pipeline_action(PipelineAction::AddMessage(user_vm));
        // round_start_vm_idx 在 UserBubble 推入之后设置，
        // 确保 RebuildAll 不会截掉当前轮次的用户消息
        self.session_mgr.current_mut().messages.round_start_vm_idx =
            self.session_mgr.current_mut().messages.view_messages.len();
        self.session_mgr.current_mut().metadata.last_human_message = Some(display);
        self.session_mgr.current_mut().messages.last_submitted_text = Some(input.clone());
        self.set_loading(true);
        self.session_mgr.current_mut().ui.scroll_offset = u16::MAX;
        self.session_mgr.current_mut().ui.scroll_follow = true;
        self.session_mgr.current_mut().todo_items.clear();

        // 开始计时新任务
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
                self.apply_pipeline_action(PipelineAction::AddMessage(MessageViewModel::system(
                    self.services.lc.tr("app-no-provider-submit"),
                )));
                self.set_loading(false);
                return;
            }
        };

        // 从 Provider 模型获取正确的 context_window（解决第三方 Provider 默认 200k 不准确问题）
        // 若启用 1M 上下文模式，则覆盖为 1,000,000
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
                tracing::debug!(
                    old = self.session_mgr.current_mut().agent.context_window,
                    new = model_cw,
                    "context_window updated from provider model"
                );
                self.session_mgr.current_mut().agent.context_window = model_cw;
            }
        }

        // 防御性重置：上次 agent 任务若 SubAgentEnd 因通道溢出被丢弃，
        // subagent_depth 会永久 > 0，导致所有后续 TokenUsageUpdate 被过滤（ctx 显示为 0）
        self.session_mgr.current_mut().agent.subagent_depth = 0;
        self.session_mgr.current_mut().agent.agent_replied = false;
        self.session_mgr.current_mut().agent.reconcile_already_done = false;
        // take 待消费的 bg 结果（agent loading 期间 bg 完成时累积在 pre_done_results，
        // 由本轮主动 submit 合并到 bgResults 参数）。必须在 reset_for_new_round 之前 take
        // ——reset 不清空 pre_done_results，但显式 drain 保证本轮独占消费，避免跨轮累积。
        let pending_bg_results: Vec<crate::app::agent_comm::BgTaskResult> = self
            .session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .pre_done_results
            .drain(..)
            .collect();
        // 清理后台任务 continuation 状态（用户主动发消息时覆盖自动 continuation）
        self.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .reset_for_new_round();
        // 重置 LSP 诊断计数
        self.session_mgr.current_mut().agent.lsp_diagnostics.reset();

        // ── ACP-based agent submission (replaces direct run_universal_agent spawn) ──
        let cwd = self.services.cwd.clone();
        if let Some(ref acp_client) = self.acp_client {
            // Clone what we need for the async task
            let acp_client_clone = acp_client.clone();
            let model_clone = self.services.model_name.clone();
            let message_content_clone = message_content.clone();
            let cwd_clone = cwd.clone();
            // 恢复的历史 thread_id：存在时用 load_session 加载历史上下文
            let existing_thread_id = self.session_mgr.current_mut().current_thread_id.clone();

            // 用户主动输入通过 ACP prompt 协议传输；cron/channel 异步触发通过
            // v2_queue_for_current() 注入共享 v2 MessageQueue（见 polling.rs）。
            // 若本轮 take 到待消费 bg 结果，走 prompt_with_bg_results 携带 bgResults。

            // Spawn the ACP calls as a background task — NEVER block the TUI event loop.
            // Events will arrive via acp_notification_rx and be processed by poll_agent().
            tokio::spawn(async move {
                let client = acp_client_clone;
                if !client.has_session() {
                    if let Some(ref tid) = existing_thread_id {
                        tracing::info!(thread_id = %tid, "ACP submit: loading existing session...");
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
                        tracing::info!("ACP submit: no session, calling new_session...");
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
                    tracing::info!("ACP submit: calling prompt...");
                    match client.prompt(&message_content_clone).await {
                        Ok(()) => tracing::info!("ACP submit: prompt completed"),
                        Err(e) => tracing::error!(error = %e, "ACP submit: prompt FAILED"),
                    }
                } else {
                    tracing::info!(
                        count = pending_bg_results.len(),
                        "ACP submit: calling prompt_with_bg_results (merging pre_done_results)"
                    );
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
            // Fallback: ACP client not available, show error
            tracing::error!("ACP client not initialized, cannot submit agent");
            self.apply_pipeline_action(PipelineAction::AddMessage(MessageViewModel::system(
                self.services.lc.tr("app-no-provider-submit"),
            )));
            self.set_loading(false);
        }
    }

    /// 后台任务完成后的自动续跑入口。
    ///
    /// 与 `submit_message` 的关键差异：
    /// - **不构造用户消息**：bg 续跑无用户输入，不 push UserBubble、不写
    ///   input_history、不处理 attachments。
    /// - **走 ACP `prompt_with_bg_results`**：携带结构化 `bgResults` 参数，让 server
    ///   端 `executor.rs` 把结果 push 到 v2 MessageQueue（Defer kind）。
    ///
    /// 状态管理与 submit_message 对齐（pipeline.begin_round、set_loading(true)、
    /// subagent_depth=0、agent_replied=false、reconcile_already_done=false、
    /// bg_task_state.reset_for_new_round、lsp_diagnostics.reset）。
    ///
    /// 调用方（polling.rs）必须先 `take()` `pending_continuation` 再调用本方法——
    /// reset_for_new_round 会清空 pending_continuation。
    pub fn submit_continuation_with_bg_results(
        &mut self,
        results: Vec<crate::app::agent_comm::BgTaskResult>,
    ) {
        if results.is_empty() {
            return;
        }

        // 标记新一轮开始（不 push UserBubble——这是自动续跑，无用户输入）。
        // round_start_vm_idx = 当前 view_messages.len()，rebuild 时 prefix_len 即此值。
        self.session_mgr
            .current_mut()
            .messages
            .pipeline
            .begin_round();
        self.session_mgr.current_mut().messages.round_start_vm_idx =
            self.session_mgr.current_mut().messages.view_messages.len();
        self.set_loading(true);

        // 任务计时
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
                self.apply_pipeline_action(PipelineAction::AddMessage(MessageViewModel::system(
                    self.services.lc.tr("app-no-provider-submit"),
                )));
                self.set_loading(false);
                return;
            }
        };

        // context_window 同步（防止漂移）
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

        // 状态重置（与 submit_message 一致）
        self.session_mgr.current_mut().agent.subagent_depth = 0;
        self.session_mgr.current_mut().agent.agent_replied = false;
        self.session_mgr.current_mut().agent.reconcile_already_done = false;
        self.session_mgr
            .current_mut()
            .agent
            .bg_task_state
            .reset_for_new_round();
        self.session_mgr.current_mut().agent.lsp_diagnostics.reset();

        // ACP 调用：走 prompt_with_bg_results（携带 bgResults 参数）。
        // 自动续跑无用户输入，构造固定 content 作为新一轮 user message。
        if let Some(ref acp_client) = self.acp_client {
            let acp_client_clone = acp_client.clone();
            let model_clone = self.services.model_name.clone();
            let cwd_clone = self.services.cwd.clone();
            // 防御：bg 续跑到达时 session 必然已存在（先有 submit_message → Done → bg 完成
            // → continuation）。但保留 load_session 包装以应对边缘时序。
            let existing_thread_id = self.session_mgr.current_mut().current_thread_id.clone();
            let continuation_content = peri_agent::messages::MessageContent::text(
                "Background agents completed. Please review the results.",
            );
            tokio::spawn(async move {
                let client = acp_client_clone;
                if !client.has_session() {
                    if let Some(ref tid) = existing_thread_id {
                        tracing::info!(
                            thread_id = %tid,
                            "ACP bg-continuation: loading existing session..."
                        );
                        if let Err(e) = client
                            .load_session(tid, &cwd_clone, Some(&model_clone))
                            .await
                        {
                            tracing::error!(error = %e, "ACP bg-continuation: load_session FAILED");
                            return;
                        }
                    } else {
                        tracing::error!(
                            "ACP bg-continuation: no session and no thread_id — cannot proceed"
                        );
                        return;
                    }
                }
                tracing::info!("ACP bg-continuation: calling prompt_with_bg_results...");
                match client
                    .prompt_with_bg_results(&continuation_content, results)
                    .await
                {
                    Ok(()) => {
                        tracing::info!("ACP bg-continuation: prompt_with_bg_results completed")
                    }
                    Err(e) => tracing::error!(
                        error = %e,
                        "ACP bg-continuation: prompt_with_bg_results FAILED"
                    ),
                }
            });
        } else {
            tracing::error!("ACP client not initialized, cannot submit bg continuation");
            self.set_loading(false);
        }
    }

    /// Loading 期间用户缓存消息的自动提交：
    /// 从 pending_messages 中取出一条，调用 submit_message 提交。
    ///
    /// 本字段是用户输入缓存路径（用户在 loading 期间主动输入）。
    /// 异步事件触发（cron/channel/workflow/bg_results 等）走 v2 queue +
    /// polling.rs drain 路径（`v2_queue_for_current()` + `drain_for_end()`），
    /// 与本字段机制独立、互不干扰。
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

    /// 处理 `/streaming` 本地命令：查看或切换流式渲染模式。
    fn handle_streaming_command(&mut self, args: &str) {
        use crate::app::message_pipeline::StreamingMode;

        let (mode, label) = match args {
            "" => {
                let current = self
                    .session_mgr
                    .current()
                    .messages
                    .pipeline
                    .streaming_mode();
                let mode_str = match current {
                    StreamingMode::Streaming => "Streaming",
                    StreamingMode::Block => "Block",
                    StreamingMode::None => "None",
                };
                let msg = format!(
                    "当前渲染模式：{}（可选：streaming / block / none）",
                    mode_str
                );
                self.apply_pipeline_action(PipelineAction::AddMessage(MessageViewModel::system(
                    msg,
                )));
                return;
            }
            "streaming" => (StreamingMode::Streaming, "Streaming"),
            "block" => (StreamingMode::Block, "Block"),
            "none" => (StreamingMode::None, "None"),
            _ => {
                self.apply_pipeline_action(PipelineAction::AddMessage(MessageViewModel::system(
                    "用法：/streaming [streaming|block|none]".to_string(),
                )));
                return;
            }
        };

        self.session_mgr
            .current_mut()
            .messages
            .pipeline
            .set_streaming_mode(mode);

        // 如果有 block buffer 残留需要 flush
        if self
            .session_mgr
            .current()
            .messages
            .pipeline
            .has_pending_block_flush()
        {
            let prefix = self.session_mgr.current().messages.round_start_vm_idx;
            if let Some(action) = self
                .session_mgr
                .current_mut()
                .messages
                .pipeline
                .check_throttle(prefix)
            {
                self.apply_pipeline_action(action);
            }
        }

        let msg = format!("渲染模式已切换为：{}", label);
        self.apply_pipeline_action(PipelineAction::AddMessage(MessageViewModel::system(msg)));
    }
}
