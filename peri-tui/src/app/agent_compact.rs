use peri_agent::messages::BaseMessage;

use super::{message_pipeline::PipelineAction, *};

impl App {
    pub(crate) fn handle_compact_started(&mut self) -> (bool, bool, bool) {
        // 退出聚焦模式（如有）
        self.session_mgr.current_mut().focused_instance_id = None;
        self.session_mgr.current_mut().ui.bg_bar_cursor = None;

        // 清理 text_selection：compact 将重建所有消息，旧 visual 坐标失效
        self.session_mgr.current_mut().ui.text_selection.clear();

        // 显示 loading 状态（spinner + 禁用输入）
        self.set_loading(true);
        let vm = MessageViewModel::system(self.services.lc.tr("app-compact-started"));
        self.apply_pipeline_action(PipelineAction::AddMessage(vm));
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
            // Micro-compact: 更新内部状态，保留 pipeline 显示
            self.session_mgr.current_mut().agent.origin_messages = messages;
            let vm = MessageViewModel::system(self.services.lc.tr_args(
                "app-compact-auto-cleared",
                &[("count".into(), (micro_cleared as i64).into())],
            ));
            self.apply_pipeline_action(PipelineAction::AddMessage(vm));
            return (true, false, false);
        }

        // Full compact: 清理 pipeline + 更新内部状态
        // loading 不在此结束——auto-compact 和 manual compact 统一由 Done 事件结束 loading。
        // Manual compact 是 CommandKind::Immediate，executor 执行后调用 push_done() 发送 Done。
        // Auto-compact 在 ReAct 循环内，Done 在循环结束时自然到达。

        // 清理 text_selection：RebuildAll 后 wrap_map 完全重建，旧坐标失效
        self.session_mgr.current_mut().ui.text_selection.clear();

        let mut label_lines = vec![format!("✻ {}", self.services.lc.tr("app-compact-done"))];
        for f in &files {
            label_lines.push(format!("  ⎿  Read {} ({} lines)", f.path, f.lines));
        }
        if !skills.is_empty() {
            label_lines.push(format!("  ⎿  Skill: {}", skills.join(", ")));
        }
        let compact_label = label_lines.join("\n");

        // 更新内部状态消息（供下一次 prompt 使用）
        self.session_mgr.current_mut().agent.origin_messages = messages.clone();

        // 清空 pipeline 内部状态 + 用压缩后消息恢复
        self.session_mgr.current_mut().messages.pipeline.clear();
        self.session_mgr
            .current_mut()
            .messages
            .pipeline
            .restore_completed(messages);

        // 清除 ephemeral_notes，防止 compact 前的系统通知残留
        self.session_mgr
            .current_mut()
            .messages
            .ephemeral_notes
            .clear();

        // 清空 view_messages，只显示 compact 通知
        let view_msgs = vec![MessageViewModel::system(compact_label)];
        // resubmit 后 view_messages 被清空重建，round_start_vm_idx 必须重置
        // 否则第二轮 agent 事件的 request_rebuild() 使用旧值会越界并导致 VM 累积
        self.session_mgr.current_mut().messages.round_start_vm_idx = 0;
        self.apply_pipeline_action(PipelineAction::RebuildAll {
            prefix_len: 0,
            tail_vms: view_msgs,
        });

        (true, false, false)
    }

    pub(crate) fn handle_compact_error(&mut self, msg: String) -> (bool, bool, bool) {
        self.set_loading(false);
        let vm = MessageViewModel::system(
            self.services
                .lc
                .tr_args("app-compact-failed", &[("error".into(), msg.into())]),
        );
        self.apply_pipeline_action(PipelineAction::AddMessage(vm));

        (true, false, false)
    }

    /// Rewind 完成：更新消息历史，显示回退通知。
    ///
    /// 与 compact 不同：保留到目标用户消息（含），不显示文件/skill 信息，
    /// 直接使用 rewind 命令传入的摘要文本。
    pub(crate) fn handle_rewind_completed(
        &mut self,
        summary: String,
        messages: Vec<BaseMessage>,
    ) -> (bool, bool, bool) {
        // 更新内部状态消息
        self.session_mgr.current_mut().agent.origin_messages = messages.clone();

        // 清空 pipeline + 用回退后消息恢复
        self.session_mgr.current_mut().messages.pipeline.clear();
        self.session_mgr
            .current_mut()
            .messages
            .pipeline
            .restore_completed(messages.clone());
        self.session_mgr
            .current_mut()
            .messages
            .ephemeral_notes
            .clear();

        // 将保留的消息渲染为 VMs，追加 rewind 通知
        let cwd = self.services.cwd.clone();
        let mut view_msgs =
            super::message_pipeline::MessagePipeline::messages_to_view_models(&messages, &cwd);
        let label = format!("↩ {summary}");
        view_msgs.push(MessageViewModel::system(label));
        self.session_mgr.current_mut().messages.round_start_vm_idx = 0;
        self.apply_pipeline_action(PipelineAction::RebuildAll {
            prefix_len: 0,
            tail_vms: view_msgs,
        });

        // 将被撤回的用户消息文本回填到输入框
        if let Some(text) = self.session_mgr.current_mut().ui.pending_rewind_text.take() {
            self.session_mgr.current_mut().ui.textarea.insert_str(&text);
        }

        (true, false, false)
    }
}
