//! UI 状态操作 + Thread 操作（精简版，S13c-4c 重生）。
//!
//! 原 thread_ops.rs 大量引用 ChatSession.agent 字段（reset_agent_session /
//! origin_messages 等），整片删除后由本文件提供 kit 路径仍需要的 UI 操作
//! 和 thread 恢复入口。

use crate::app::App;
use crate::thread::ThreadId;

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
    }

    pub fn toggle_diff(&mut self) {
        self.session_mgr.current_mut().ui.diff_visible =
            !self.session_mgr.current_mut().ui.diff_visible;
    }

    pub fn add_pending_attachment(&mut self, path: String) {
        self.session_mgr
            .current_mut()
            .metadata
            .pending_attachments
            .push(path);
    }

    pub fn pop_pending_attachment(&mut self) {
        self.session_mgr
            .current_mut()
            .metadata
            .pending_attachments
            .pop();
    }

    /// 恢复指定 thread：仅设置 current_thread_id。
    ///
    /// (S13c-4c) 原 thread_ops 大量 agent.origin_messages / retry_status /
    /// subagent_depth 重置已删除——kit 路径下，thread 恢复由 ACP server 端
    /// `session/load` 完成，TUI 仅持有 thread_id 用于后续 submit_consumer
    /// 触发 `client.load_session(tid, ...)`。
    pub fn open_thread(&mut self, thread_id: ThreadId) {
        self.session_mgr.current_mut().current_thread_id = Some(thread_id);
    }
}
