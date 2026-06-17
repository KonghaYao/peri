use super::*;

impl App {
    /// 记录一条历史（提交时调用）
    pub fn push_input_history(&mut self, text: String) {
        if self.session_mgr.current_mut().ui.input_history.first() == Some(&text) {
            return;
        }
        self.session_mgr
            .current_mut()
            .ui
            .input_history
            .insert(0, text);
        self.session_mgr
            .current_mut()
            .ui
            .input_history
            .truncate(1000);
        // 持久化到磁盘
        let history = &self.session_mgr.current_mut().ui.input_history;
        super::history_persistence::save_input_history(history);
    }

    /// Up 键：向上浏览历史（更早的消息）
    pub fn history_up(&mut self) {
        if self.session_mgr.current_mut().ui.input_history.is_empty() {
            return;
        }
        let lines = self
            .session_mgr
            .current_mut()
            .ui
            .textarea
            .lines()
            .join("\n");
        let history_index = self.session_mgr.current().ui.history_index;
        let input_history_len = self.session_mgr.current().ui.input_history.len();
        match history_index {
            None => {
                if !lines.trim().is_empty() {
                    self.session_mgr.current_mut().ui.draft_input = Some(lines);
                }
                self.session_mgr.current_mut().ui.history_index = Some(0);
            }
            Some(idx) if idx + 1 < input_history_len => {
                self.session_mgr.current_mut().ui.history_index = Some(idx + 1);
            }
            Some(_) => {}
        }
        self.restore_history_to_textarea();
    }

    /// Down 键：向下浏览历史（更新的消息）
    pub fn history_down(&mut self) {
        match self.session_mgr.current_mut().ui.history_index {
            Some(0) => {
                self.session_mgr.current_mut().ui.history_index = None;
                self.restore_draft();
            }
            Some(idx) => {
                self.session_mgr.current_mut().ui.history_index = Some(idx - 1);
                self.restore_history_to_textarea();
            }
            None => {}
        }
    }

    /// 退出历史浏览（任意输入字符时调用）
    pub fn exit_history(&mut self) {
        self.session_mgr.current_mut().ui.history_index = None;
        self.session_mgr.current_mut().ui.draft_input = None;
    }

    fn restore_history_to_textarea(&mut self) {
        if let Some(idx) = self.session_mgr.current_mut().ui.history_index {
            if let Some(text) = self
                .session_mgr
                .current_mut()
                .ui
                .input_history
                .get(idx)
                .cloned()
            {
                let textarea = &mut self.session_mgr.current_mut().ui.textarea;
                // 复用同一实例（clear + insert_str），避免整体替换 textarea 导致
                // ratatui buffer diff 无法清理前一帧 cursor_at_end 的 REVERSED 空格，
                // 在文本末尾留下残影光标块。
                textarea.clear();
                textarea.insert_str(&text);
            }
        }
    }

    fn restore_draft(&mut self) {
        let draft = self.session_mgr.current_mut().ui.draft_input.take();
        let textarea = &mut self.session_mgr.current_mut().ui.textarea;
        // 同上：复用同一实例避免残影
        textarea.clear();
        if let Some(draft) = draft {
            textarea.insert_str(&draft);
        }
    }
}
