//! Agent polling functions — poll_agent, poll_panic_notifications, poll_workflow_runs.
//! Extracted from original agent_ops.rs (2026-05-20 split).
//!
//! v2 queue drain, cron trigger drain, and channel notification drain removed:
//! Agent now owns async event responsibility (stages/end.rs + ACP executor).

use crate::app::App;

impl App {
    /// Drain ACP notification channel.  Returns `true` if the UI needs a redraw.
    ///
    /// `view_slice` is the current v2 state.view snapshot (captured by the caller
    /// in main_loop) — passed through to handle_acp_notification → handle_agent_event
    /// → handle_interrupted so interrupt paths can scan v2 ViewModels instead of
    /// v1 view_messages.
    pub fn poll_agent(&mut self, view_slice: &[peri_acp_types::view_model::ViewModel]) -> bool {
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

        loop {
            // Try ACP notification channel
            let acp_result = self
                .session_mgr
                .current_mut()
                .agent
                .acp_notification_rx
                .as_mut()
                .map(|rx| rx.try_recv());
            if let Some(Ok(notif)) = acp_result {
                let (ev_updated, should_break, should_return) =
                    self.handle_acp_notification(notif, view_slice);
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
                // v2: Workflow panel refresh handled by state machine via PanelReadContext
                self.global_ui
                    .workflow_tracker
                    .replace_runs(snapshots.clone());
                updated = true;
            }
        }
        updated
    }
}
