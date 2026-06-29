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
        self.session_mgr
            .current_mut()
            .messages
            .push_system_note(content);
    }

    /// P5: Direct VM push without PipelineAction indirection.
    pub(crate) fn apply_add_message(&mut self, vm: MessageViewModel) {
        let session = self.session_mgr.current_mut();
        let anchor = session.messages.view_messages.len();
        session.messages.ephemeral_notes.push((anchor, vm.clone()));
        session.messages.view_messages.push(vm);
    }

    /// P5: Direct view_messages rebuild without PipelineAction indirection.
    /// Preserves ephemeral note anchors and handles UserBubble deduplication.
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

        // 保存 ephemeral_notes 中锚点在 tail 范围内的
        let mut saved_notes: Vec<(usize, MessageViewModel)> = session
            .messages
            .ephemeral_notes
            .drain(..)
            .filter(|(anchor, _)| *anchor >= prefix_len)
            .filter(|(_, vm)| !matches!(vm, MessageViewModel::UserBubble { .. }))
            .collect();

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

        // 按锚点位置插入 saved_notes
        saved_notes.sort_by_key(|(anchor, _)| *anchor);
        for (anchor, vm) in saved_notes {
            let tail_len = session.messages.view_messages.len() - prefix_len;
            let insert_pos = (anchor - prefix_len).min(tail_len) + prefix_len;
            session
                .messages
                .view_messages
                .insert(insert_pos, vm.clone());
            session.messages.ephemeral_notes.push((insert_pos, vm));
        }
    }
}
