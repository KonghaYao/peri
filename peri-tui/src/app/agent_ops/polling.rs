//! Agent polling functions — poll_agent, poll_background_events, poll_cron_triggers.
//! Extracted from original agent_ops.rs (2026-05-20 split).

use crate::app::App;

impl App {
    pub fn poll_agent(&mut self) -> bool {
        // Cancel 超时安全网：5 秒后仍未收到 Interrupted/Done，强制清理
        if let Some(cancel_at) = self.session_mgr.current_mut().agent.cancel_sent_at {
            if cancel_at.elapsed() > std::time::Duration::from_secs(5)
                && self.session_mgr.current_mut().ui.loading
            {
                tracing::warn!(
                    "cancel timeout: 5s elapsed without Interrupted/Done, force cleanup"
                );
                self.session_mgr.current_mut().agent.cancel_sent_at = None;
                self.cleanup_agent_state(None);
                return true;
            }
        }

        // Check for events from ACP notification channel (primary path)
        let has_acp = self
            .session_mgr
            .current_mut()
            .agent
            .acp_notification_rx
            .is_some();

        if !has_acp {
            return false;
        }

        let mut updated = false;

        // 节流检查（每帧开始时，确保上一批 chunk 的尾部也被显示）
        {
            let prefix_len = self.session_mgr.current_mut().messages.round_start_vm_idx;
            if let Some(action) = self
                .session_mgr
                .current_mut()
                .messages
                .pipeline
                .check_throttle(prefix_len)
            {
                self.apply_pipeline_action(action);
                updated = true;
            }
        }

        // Agent 空闲时，优先消费 pending_messages（用户输入缓存 + Workflow 完成
        // 通知；二者都需 agent review 语义，与异步事件优先级不同）。
        // cron/channel/bg_results 等纯异步事件由下方 v2 queue drain 路径处理。
        // 优先级：bg continuation（pending_continuation）已由 handle_done/handle_error
        // 中的 flush 路径处理，此处只处理 idle 时的 pending_messages。
        if !self.session_mgr.current().ui.loading
            && !self
                .session_mgr
                .current()
                .messages
                .pending_messages
                .is_empty()
        {
            self.flush_pending_messages();
            return true;
        }

        // Agent 空闲且无 pending 用户输入时，drain 共享 v2 MessageQueue 的
        // Prompt/Defer 消息（cron/channel 等异步事件），合并文本调 submit_message
        // 触发新一轮（接收方主动续跑）。stages 内部 drain_for_receive 只在 agent
        // 运行时（loading=true）执行，与本处的 idle-only drain 互斥，无冲突。
        // submit_message 同步执行，会立即 set_loading(true)，故 return true 后下一帧
        // !loading 检查自然失败，避免重入。
        if !self.session_mgr.current().ui.loading {
            if let Some(queue) = self.v2_queue_for_current() {
                if let Some(awakened) = queue.drain_for_end() {
                    let combined: String = awakened
                        .iter()
                        .map(|m| m.message.content())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if !combined.is_empty() {
                        tracing::info!(
                            count = awakened.len(),
                            sources = ?awakened.iter().map(|m| &m.source).collect::<Vec<_>>(),
                            "v2 queue drained, submitting as new prompt"
                        );
                        self.submit_message(combined);
                        return true;
                    } else {
                        tracing::warn!(
                            count = awakened.len(),
                            "v2 queue drained but combined text empty; messages dropped"
                        );
                    }
                }
            }
        }

        loop {
            // Try ACP notification channel first (new path)
            let acp_result = self
                .session_mgr
                .current_mut()
                .agent
                .acp_notification_rx
                .as_mut()
                .map(|rx| rx.try_recv());
            if let Some(Ok(notif)) = acp_result {
                let (ev_updated, should_break, should_return) = self.handle_acp_notification(notif);
                if ev_updated {
                    updated = true;
                }
                if should_return {
                    return true;
                }
                if should_break {
                    break;
                }
                continue;
            }
            break;
        }

        // 当 loading=true 时（如 compact 中），即使没有新事件也返回 true，
        // 确保 spinner 动画持续渲染而非冻结
        let loading = self.session_mgr.current_mut().ui.loading;
        if loading {
            return true;
        }

        // Poll channel notifications
        self.poll_channel_notifications();

        updated
    }

    /// 每帧调用：消费 channel 消息通知。
    /// 异步触发通过 v2_queue_for_current() 注入共享 v2 MessageQueue（Defer kind）。
    /// ACP session 未就绪时 graceful drop。
    fn poll_channel_notifications(&mut self) {
        let mut channel_notifications = Vec::new();
        {
            let session = &mut self.session_mgr.current_mut();
            if let Some(ref mut rx) = session.messages.channel_notification_rx {
                while let Ok(notif) = rx.try_recv() {
                    channel_notifications.push(notif);
                }
            }
        }

        for notif in channel_notifications {
            // 异步到达，用 Defer kind：本轮 Receive 跳过，End 阶段唤醒新 turn
            // （与 executor.rs 的 bg_results 注入语义一致）
            match self.v2_queue_for_current() {
                Some(queue) => {
                    let payload = format!(
                        "<system-reminder><channel source=\"{}\" chat_id=\"{}\">{}</channel></system-reminder>",
                        notif.source, notif.chat_id, notif.text
                    );
                    queue.push(peri_agent::session::queue::QueuedMessage::defer(
                        peri_agent::session::queue::MessageSource::ChannelMessage,
                        peri_agent::messages::BaseMessage::human(
                            peri_agent::messages::MessageContent::text(payload),
                        ),
                    ));
                    tracing::info!(
                        source = %notif.source,
                        chat_id = %notif.chat_id,
                        "channel notification injected to v2 queue"
                    );
                }
                None => {
                    // ACP session 尚未建立，drain 以避免背压，丢弃
                    tracing::debug!(
                        source = %notif.source,
                        "channel notification dropped (no v2 queue; ACP session not ready)"
                    );
                }
            }
        }
    }

    /// 每帧调用：消费后台事件通道（MCP OAuth 等异步任务发送的事件），返回是否有 UI 更新
    pub fn poll_background_events(&mut self) -> bool {
        let events: Vec<_> = match self.services.bg_event_rx.as_mut() {
            Some(rx) => {
                let mut evts = Vec::new();
                loop {
                    match rx.try_recv() {
                        Ok(event) => evts.push(event),
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                            self.services.bg_event_rx = None;
                            break;
                        }
                    }
                }
                evts
            }
            None => return false,
        };
        let mut updated = false;
        for event in events {
            let (ev_updated, _should_break, should_return) = self.handle_agent_event(event);
            if ev_updated {
                updated = true;
            }
            if should_return {
                return true;
            }
        }
        updated
    }

    /// 轮询 panic hook 通知通道，返回是否有新消息。
    /// panic 消息通过 tracing::error! 写入日志，同时通过通道通知 TUI 显示。
    pub fn poll_panic_notifications(&mut self) -> bool {
        // 先收集所有消息，释放 services 的借用
        let messages: Vec<String> = {
            let rx = match self.services.panic_notify_rx.as_mut() {
                Some(rx) => rx,
                None => return false,
            };
            let mut msgs = Vec::new();
            loop {
                match rx.try_recv() {
                    Ok(msg) => msgs.push(msg),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        self.services.panic_notify_rx = None;
                        break;
                    }
                }
            }
            msgs
        };
        let updated = !messages.is_empty();
        for msg in messages {
            self.push_system_note(msg);
            self.request_rebuild();
        }
        updated
    }

    /// 每帧调用：检查 cron 触发事件。
    /// 异步触发通过 v2_queue_for_current() 注入共享 v2 MessageQueue（Defer kind）。
    /// ACP session 未就绪时 graceful drop。
    pub fn poll_cron_triggers(&mut self) {
        let cron_triggers: Vec<_> = self
            .services
            .cron
            .trigger_rx
            .as_mut()
            .map(|rx| {
                let mut triggers = Vec::new();
                while let Ok(trigger) = rx.try_recv() {
                    triggers.push(trigger);
                }
                triggers
            })
            .unwrap_or_default();
        for trigger in cron_triggers {
            // Cron 是用户预设的主动触发，用 goal steering 语义（<goal-message>）
            match self.v2_queue_for_current() {
                Some(queue) => {
                    let payload = format!(
                        "<goal-message>Cron triggered: {}</goal-message>",
                        trigger.prompt
                    );
                    queue.push(peri_agent::session::queue::QueuedMessage::defer(
                        peri_agent::session::queue::MessageSource::CronTrigger,
                        peri_agent::messages::BaseMessage::human(
                            peri_agent::messages::MessageContent::text(payload),
                        ),
                    ));
                    tracing::info!(
                        prompt = %trigger.prompt,
                        "cron trigger injected to v2 queue"
                    );
                }
                None => {
                    tracing::debug!(
                        prompt = %trigger.prompt,
                        "cron trigger dropped (no v2 queue; ACP session not ready)"
                    );
                }
            }
        }
    }

    /// 每帧调用：@ mention 已改为同步刷新，此处为 no-op（保留接口兼容）
    pub fn poll_at_mention(&mut self) -> bool {
        false
    }

    /// 排空 workflow 轮询通道，更新 tracker（状态栏读）与 panel（若打开）。
    ///
    /// polling 生命周期跟 panel 解耦：panel 关闭不停止 polling，
    /// 这样状态栏的 workflow 计数仍能持续刷新。
    /// polling 在 open_workflows_panel 首次打开后启动，session 切换时由
    /// `workflow_poll_kill` 被 drop 而自然退出。
    pub fn poll_workflow_runs(&mut self) -> bool {
        let mut updated = false;
        if let Some(ref mut rx) = self.workflow_poll_rx {
            while let Ok(snapshots) = rx.try_recv() {
                // 先写 tracker（状态栏源），保证 panel 未打开时也有数据
                self.global_ui
                    .workflow_tracker
                    .replace_runs(snapshots.clone());
                // panel 打开则同步刷新
                if let Some(panel) = self
                    .global_panels
                    .get_mut::<crate::app::workflow_panel::WorkflowPanel>()
                {
                    panel.update_runs(snapshots);
                }
                updated = true;
            }
        }
        updated
    }
}
