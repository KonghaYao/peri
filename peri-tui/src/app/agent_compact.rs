use peri_agent::messages::BaseMessage; // P4b: type-dependency

use super::*;

impl App {
    pub(crate) fn handle_compact_started(&mut self) -> (bool, bool, bool) {
        self.session_mgr.current_mut().focused_instance_id = None;
        self.session_mgr.current_mut().ui.bg_bar_cursor = None;
        self.session_mgr.current_mut().ui.text_selection.clear();
        self.set_loading(true);
        self.push_system_note(self.services.lc.tr("app-compact-started"));
        (true, false, false)
    }

    pub(crate) fn handle_compact_completed(
        &mut self,
        _summary: String,
        files: Vec<peri_acp::event::CompactFileInfoDto>,
        skills: Vec<String>,
        micro_cleared: usize,
        messages: Vec<BaseMessage>,
    ) -> (bool, bool, bool) {
        if micro_cleared > 0 {
            self.session_mgr.current_mut().agent.origin_messages = messages;
            self.push_system_note(self.services.lc.tr_args(
                "app-compact-auto-cleared",
                &[("count".into(), (micro_cleared as i64).into())],
            ));
            return (true, false, false);
        }

        self.session_mgr.current_mut().ui.text_selection.clear();

        let mut label_lines = vec![format!("✻ {}", self.services.lc.tr("app-compact-done"))];
        for f in &files {
            label_lines.push(format!("  ⎿  Read {} ({} lines)", f.path, f.lines));
        }
        if !skills.is_empty() {
            label_lines.push(format!("  ⎿  Skill: {}", skills.join(", ")));
        }
        let compact_label = label_lines.join("\n");

        self.session_mgr.current_mut().agent.origin_messages = messages.clone();

        // P5: replace view_messages (SystemNote anchor tracking retired in Phase 2.5)
        let view_msgs = vec![MessageViewModel::system(compact_label)];
        self.session_mgr.current_mut().messages.round_start_vm_idx = 0;
        self.apply_rebuild_all(0, view_msgs);

        (true, false, false)
    }

    pub(crate) fn handle_compact_error(&mut self, msg: String) -> (bool, bool, bool) {
        self.set_loading(false);
        self.push_system_note(
            self.services
                .lc
                .tr_args("app-compact-failed", &[("error".into(), msg.into())]),
        );

        (true, false, false)
    }

    pub(crate) fn handle_rewind_completed(
        &mut self,
        summary: String,
        messages: Vec<BaseMessage>,
    ) -> (bool, bool, bool) {
        self.session_mgr.current_mut().agent.origin_messages = messages.clone();

        // P5: replace view_messages (SystemNote anchor tracking retired in Phase 2.5)
        let cwd = self.services.cwd.clone();
        let mut view_msgs = super::messages_to_view_models(&messages, &cwd);
        let label = format!("↩ {summary}");
        view_msgs.push(MessageViewModel::system(label));
        self.session_mgr.current_mut().messages.round_start_vm_idx = 0;
        self.apply_rebuild_all(0, view_msgs);

        if let Some(text) = self.session_mgr.current_mut().ui.pending_rewind_text.take() {
            self.session_mgr.current_mut().ui.textarea.insert_str(&text);
        }

        (true, false, false)
    }
}
