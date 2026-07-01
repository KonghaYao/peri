use super::*;

impl App {
    /// P5: Synchronous rendering from state machine State.view + current_turn.
    /// render_rebuild is now a no-op — rendering happens immediately in draw().
    pub(crate) fn render_rebuild(&self) {
        // P5: no-op, sync rendering from state machine
    }

    /// P5: request_rebuild is now a no-op — sync rendering from state machine.
    pub(crate) fn request_rebuild(&mut self) {
        // P5: no-op, sync rendering from state machine
    }

    /// 添加系统通知并记录锚点位置。
    pub(crate) fn push_system_note(&mut self, content: String) {
        let session = self.session_mgr.current_mut();
        session.messages.push_system_note(content);
        session.messages.message_cache = None;
    }

    /// Phase 2.6 step 7e: seed a v2 `ViewModel` into the headless test
    /// render source.
    ///
    /// Headless tests cannot access `state_machine::State.view` (local
    /// variable in `main_loop::run`). This helper pushes VMs to
    /// `MessageState.v2_test_views`, which `HeadlessHandle::render()`
    /// reads as the primary render source. Replaces `apply_add_message`.
    #[cfg(test)]
    pub(crate) fn seed_v2_vm(&mut self, vm: peri_acp_types::view_model::ViewModel) {
        let session = self.session_mgr.current_mut();
        session.messages.v2_test_views.push(vm);
        session.messages.message_cache = None;
    }

    /// Convenience: seed a UserBubble with the given text.
    #[cfg(test)]
    pub(crate) fn seed_v2_user_bubble(&mut self, text: &str) {
        self.seed_v2_vm(peri_acp_types::view_model::ViewModel::UserBubble(
            peri_acp_types::view_model::UserBubbleData {
                text: text.to_string(),
            },
        ));
    }

    /// Convenience: seed an AssistantBubble with the given text.
    #[cfg(test)]
    pub(crate) fn seed_v2_assistant_bubble(&mut self, text: &str) {
        self.seed_v2_vm(peri_acp_types::view_model::ViewModel::AssistantBubble(
            peri_acp_types::view_model::AssistantBubbleData {
                text: text.to_string(),
                reasoning: None,
                tool_card_ids: Vec::new(),
            },
        ));
    }

    /// Convenience: seed a ToolCard.
    #[cfg(test)]
    pub(crate) fn seed_v2_tool_card(&mut self, tool_name: &str, input_summary: &str) {
        self.seed_v2_vm(peri_acp_types::view_model::ViewModel::ToolCard(
            peri_acp_types::view_model::ToolCardData {
                tool_id: format!("tool-{}", tool_name),
                tool_name: tool_name.to_string(),
                input_summary: input_summary.to_string(),
                output_summary: String::new(),
                is_error: false,
                diff: None,
            },
        ));
    }
}
