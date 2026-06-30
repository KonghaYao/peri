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

    /// P5: Direct VM push without PipelineAction indirection.
    pub(crate) fn apply_add_message(&mut self, vm: MessageViewModel) {
        let session = self.session_mgr.current_mut();
        session.messages.view_messages.push(vm);
        // Invalidate render cache — view_messages mutated.
        session.messages.message_cache = None;
    }

    /// P5: Direct view_messages rebuild without PipelineAction indirection.
    /// Preserves UserBubble deduplication. SystemNote anchor tracking was
    /// retired in Phase 2.5 — v2 state.view (production render source)
    /// handles SystemNote via `pending_v2_notes → Event::PushSystemNote`.
    pub(crate) fn apply_rebuild_all(&mut self, prefix_len: usize, tail_vms: Vec<MessageViewModel>) {
        let session = self.session_mgr.current_mut();
        let view_len = session.messages.view_messages.len();
        let prefix_len = if prefix_len > view_len {
            tracing::error!(
                prefix_len,
                view_len,
                round_start_vm_idx = session.messages.round_start_vm_idx,
                "RebuildAll prefix_len 越界，已钳位到 view_messages.len()"
            );
            view_len
        } else {
            prefix_len
        };

        // drain 尾部
        session.messages.view_messages.drain(prefix_len..);

        // 去重：UserBubble 在不同路径重复创建时去重
        let mut tail = tail_vms;
        if prefix_len > 0 && !tail.is_empty() {
            let prefix_last = session.messages.view_messages.get(prefix_len - 1);
            if let Some(MessageViewModel::UserBubble {
                content: prefix_content,
                ..
            }) = prefix_last
            {
                if let Some(MessageViewModel::UserBubble {
                    content: tail_content,
                    ..
                }) = tail.first()
                {
                    if prefix_content == tail_content {
                        tail.remove(0);
                    }
                }
            }
        }

        session.messages.view_messages.extend(tail);

        // Invalidate render cache: view_messages was modified (drain + extend).
        // Without this, render_messages() reuses stale cache because needs_rebuild
        // only checks width changes, not view_messages mutations.
        session.messages.message_cache = None;
    }
}
