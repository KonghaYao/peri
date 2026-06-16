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

    /// 滚动到底部（恢复 follow 模式）
    pub fn scroll_to_bottom(&mut self) {
        self.session_mgr.current_mut().ui.scroll_offset = u16::MAX;
        self.session_mgr.current_mut().ui.scroll_follow = true;
    }

    /// 滚动到顶部
    pub fn scroll_to_top(&mut self) {
        self.session_mgr.current_mut().ui.scroll_offset = 0;
        self.session_mgr.current_mut().ui.scroll_follow = false;
    }

    /// 展开/折叠所有工具调用消息
    pub fn toggle_collapsed_messages(&mut self) {
        self.session_mgr.current_mut().ui.show_tool_messages =
            !self.session_mgr.current_mut().ui.show_tool_messages;
        let show_tool_messages = self.session_mgr.current().ui.show_tool_messages;
        let _ = self
            .session_mgr
            .current_mut()
            .messages
            .render_tx
            .try_send(RenderEvent::ToggleToolMessages(show_tool_messages));
    }

    /// 切换 Write/Edit 工具结果内联 diff 的显隐
    pub fn toggle_diff(&mut self) {
        let new_visible = !self.session_mgr.current_mut().ui.diff_visible;
        self.session_mgr.current_mut().ui.diff_visible = new_visible;

        // ToggleDiff 会清空 hash 缓存并触发全量重渲染
        let _ = self
            .session_mgr
            .current()
            .messages
            .render_tx
            .try_send(RenderEvent::ToggleDiff(new_visible));
    }

    /// 添加一个图片附件到待发送列表
    pub fn add_pending_attachment(&mut self, att: PendingAttachment) {
        self.session_mgr
            .current_mut()
            .metadata
            .pending_attachments
            .push(att);
    }

    /// 删除最后一个图片附件
    pub fn pop_pending_attachment(&mut self) {
        self.session_mgr
            .current_mut()
            .metadata
            .pending_attachments
            .pop();
    }

    // ─── Thread 操作 ──────────────────────────────────────────────────────────

    /// 重置 AgentComm 会话状态（token tracker、重试、subagent 等）
    /// 在 open_thread / new_thread 时调用，确保切换 thread 后上下文干净
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

    /// 恢复历史 thread：加载消息，关闭 browser
    pub fn open_thread(&mut self, thread_id: ThreadId) {
        let store = self.services.thread_store.clone();
        let tid = thread_id.clone();
        let base_msgs = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(store.load_context(&tid))
                .unwrap_or_default()
        });
        self.session_mgr
            .current_mut()
            .messages
            .view_messages
            .clear();
        self.session_mgr
            .current_mut()
            .messages
            .ephemeral_notes
            .clear();
        self.session_mgr.current_mut().agent.origin_messages = base_msgs.clone();

        // 使用统一管线转换：与流式路径共享同一个 messages_to_view_models()
        let mut view_msgs = message_pipeline::MessagePipeline::messages_to_view_models(
            &base_msgs,
            &self.services.cwd,
        );
        // 历史恢复时聚合连续的已完成 SubAgentGroup 为批次汇总
        message_pipeline::aggregate_batch_groups(&mut view_msgs);
        self.session_mgr.current_mut().messages.view_messages = view_msgs;

        // 同步 Pipeline 内部状态，确保后续流式事件能正确续接
        self.session_mgr.current_mut().messages.pipeline.clear();
        self.session_mgr
            .current_mut()
            .messages
            .pipeline
            .restore_completed(base_msgs.clone());

        let thread_id_str = thread_id.to_string();
        self.session_mgr.current_mut().current_thread_id = Some(thread_id);
        // 同步 ACP 服务器端 session 状态：确保 state.history 包含当前 thread 的消息，
        // 这样 /compact 命令和后续 prompt 能正确读到完整历史
        if let Some(ref acp_client) = self.acp_client {
            let client = acp_client.clone();
            let cwd = self.services.cwd.clone();
            let model = self.services.model_name.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    match client.load_session(&thread_id_str, &cwd, Some(&model)).await {
                        Ok(sid) => tracing::info!(session_id = %sid, "open_thread: ACP session synced"),
                        Err(e) => tracing::warn!(error = %e, "open_thread: ACP session sync failed (compact may not work until first prompt)"),
                    }
                })
            });
        }
        self.session_mgr
            .current_mut()
            .session_panels
            .close_if(PanelKind::ThreadBrowser);
        self.session_mgr
            .current_mut()
            .metadata
            .pending_attachments
            .clear();
        self.session_mgr.current_mut().langfuse.langfuse_session = None;
        self.session_mgr.current_mut().todo_items.clear();

        self.reset_agent_session();
        // 回收释放的内存给 OS
        crate::alloc_config::alloc_collect();

        // 恢复 sticky header：找到 thread 中最后一条 Human 消息
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

        // 通知渲染线程加载历史消息
        let vms = self.session_mgr.current().messages.view_messages.clone();
        let _ = self
            .session_mgr
            .current_mut()
            .messages
            .render_tx
            .try_send(RenderEvent::Rebuild(vms));
    }

    pub fn open_thread_with_feedback(&mut self, thread_id: ThreadId) {
        self.open_thread(thread_id);
    }

    /// 新建 thread：清空消息，关闭 browser（thread id 在首次发送时创建）
    pub fn new_thread(&mut self) {
        // Fire SessionEnd hooks before clearing session state
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

        self.session_mgr
            .current_mut()
            .messages
            .view_messages
            .clear();
        self.session_mgr
            .current_mut()
            .messages
            .view_messages
            .shrink_to_fit();
        self.session_mgr
            .current_mut()
            .messages
            .ephemeral_notes
            .clear();
        self.session_mgr.current_mut().agent.origin_messages.clear();
        self.session_mgr
            .current_mut()
            .agent
            .origin_messages
            .shrink_to_fit();
        self.session_mgr.current_mut().messages.pipeline.clear();
        self.session_mgr
            .current_mut()
            .messages
            .pipeline
            .shrink_to_fit();
        self.session_mgr.current_mut().current_thread_id = None;
        self.session_mgr.current_mut().todo_items.clear();
        self.session_mgr
            .current_mut()
            .metadata
            .pending_attachments
            .clear();
        self.session_mgr
            .current_mut()
            .session_panels
            .close_if(PanelKind::ThreadBrowser);
        self.session_mgr.current_mut().langfuse.langfuse_session = None;
        self.session_mgr.current_mut().metadata.last_human_message = None;
        self.session_mgr.current_mut().messages.last_submitted_text = None;
        self.session_mgr.current_mut().metadata.pre_submit_state_len = 0;

        self.reset_agent_session();

        // 通过 ACP 协议创建新 session，清空 server 端 history
        if let Some(ref acp_client) = self.acp_client {
            let client = acp_client.clone();
            let cwd = self.services.cwd.clone();
            let model = self.services.model_name.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    match client.new_session(&cwd, Some(&model)).await {
                        Ok(sid) => tracing::info!(session_id = %sid, "new_thread: ACP new_session succeeded"),
                        Err(e) => tracing::warn!(error = %e, "new_thread: ACP new_session failed"),
                    }
                })
            });
        }
        // 回收释放的内存给 OS
        crate::alloc_config::alloc_collect();

        let _ = self
            .session_mgr
            .current_mut()
            .messages
            .render_tx
            .try_send(RenderEvent::Clear);

        // 归还已释放内存页给 OS
        crate::alloc_config::alloc_collect();
    }

    /// 打开 thread 浏览面板（通过命令触发）
    pub fn open_thread_browser(&mut self) {
        let store = self.services.thread_store.clone();
        let cwd = self.services.cwd.clone();
        let threads = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(store.list_threads())
                .unwrap_or_default()
        });
        let filtered: Vec<_> = threads.into_iter().filter(|t| t.cwd == cwd).collect();

        // 检测当前 cwd 的 git 分支
        let branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&self.services.cwd)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());

        let browser = ThreadBrowser::new(filtered, self.services.thread_store.clone(), branch);
        self.open_panel(PanelState::ThreadBrowser(Box::new(browser)));
    }
}

#[cfg(test)]
mod tests {
    use crate::thread::ThreadMeta;
    include!("thread_ops_test.rs");
}
