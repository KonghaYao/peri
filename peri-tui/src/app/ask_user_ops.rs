use super::*;

impl App {
    pub fn ask_user_next_tab(&mut self) {
        if let Some(InteractionPrompt::Questions(p)) = self
            .session_mgr
            .current_mut()
            .agent
            .interaction_prompt
            .as_mut()
        {
            p.next_tab();
        }
    }

    pub fn ask_user_prev_tab(&mut self) {
        if let Some(InteractionPrompt::Questions(p)) = self
            .session_mgr
            .current_mut()
            .agent
            .interaction_prompt
            .as_mut()
        {
            p.prev_tab();
        }
    }

    pub fn ask_user_move(&mut self, delta: isize) {
        if let Some(InteractionPrompt::Questions(p)) = self
            .session_mgr
            .current_mut()
            .agent
            .interaction_prompt
            .as_mut()
        {
            p.current().move_option_cursor(delta);
            // 光标跟随滚动：用渲染时构建的实际行号映射
            let cursor_opt = p.current().option_cursor.max(0) as usize;
            let cursor_row = p
                .option_row_map
                .get(cursor_opt)
                .copied()
                .unwrap_or_default();
            let visible_h = p.scrollbar_metrics.map(|m| m.bar_area.height).unwrap_or(20);
            p.scroll_offset = ensure_cursor_visible(cursor_row, p.scroll_offset, visible_h);
        }
    }

    /// 页面级滚动（Ctrl+U 上翻 / Ctrl+D 下翻 / 鼠标滚轮）
    pub fn ask_user_scroll(&mut self, lines: i16) {
        if let Some(InteractionPrompt::Questions(p)) = self
            .session_mgr
            .current_mut()
            .agent
            .interaction_prompt
            .as_mut()
        {
            if lines > 0 {
                p.scroll_offset = p.scroll_offset.saturating_add(lines as u16);
            } else {
                p.scroll_offset = p.scroll_offset.saturating_sub((-lines) as u16);
            }
            // 钳位到最大偏移
            if let Some(m) = p.scrollbar_metrics {
                p.scroll_offset = p.scroll_offset.min(m.max_offset);
            }
            // 光标不随滚动移动——用户用 Up/Down 移动光标，光标移动时自动滚动到可见位置
        }
    }

    pub fn ask_user_toggle(&mut self) {
        if let Some(InteractionPrompt::Questions(p)) = self
            .session_mgr
            .current_mut()
            .agent
            .interaction_prompt
            .as_mut()
        {
            p.current().toggle_current();
        }
    }

    pub fn ask_user_edit_key(&mut self, input: tui_textarea::Input) {
        if let Some(InteractionPrompt::Questions(p)) = self
            .session_mgr
            .current_mut()
            .agent
            .interaction_prompt
            .as_mut()
        {
            let q = p.current();
            if q.in_custom_input {
                q.custom_input.input(input);
            }
        }
    }

    /// Enter：确认当前问题。若全部问题均已确认则提交并关闭弹窗。
    /// 若当前问题没有选中任何选项（且不在自定义输入模式），自动选中光标所在选项。
    pub fn ask_user_confirm(&mut self) {
        let all_done = {
            let p = match self
                .session_mgr
                .current_mut()
                .agent
                .interaction_prompt
                .as_mut()
            {
                Some(InteractionPrompt::Questions(p)) => p,
                _ => return,
            };
            let q = &mut p.questions[p.active_tab];
            // 没有选中任何选项且不在自定义输入模式：自动选中当前光标行
            if !q.in_custom_input
                && !q.selected.iter().any(|&v| v)
                && q.custom_input.value().trim().is_empty()
            {
                q.toggle_current();
            }
            p.confirm_current()
        };

        if all_done {
            self.session_mgr.current_mut().agent.pending_ask_user = None;
            if let Some(InteractionPrompt::Questions(p)) = self
                .session_mgr
                .current_mut()
                .agent
                .interaction_prompt
                .take()
            {
                // 在消息流中显示用户的回答
                let answers: Vec<(String, String)> = p
                    .questions
                    .iter()
                    .map(|q| (q.data.header.clone(), q.answer()))
                    .collect();
                let answer_lines: Vec<String> = answers
                    .iter()
                    .map(|(header, answer)| format!("[{}] {}", header, answer))
                    .collect();
                // Cron #24 P1 #2: 路由到 v2 state.view（之前直接 push 到
                // view_messages 导致生产渲染不可见）。main_loop 会在下一帧
                // 通过 Event::PushUserBubble 将此文本送入 SM。
                self.session_mgr
                    .current_mut()
                    .messages
                    .push_user_bubble(answer_lines.join("\n"));

                // ACP 模式：通过 transport 回传结构化响应
                let acp_request_id = self
                    .session_mgr
                    .current_mut()
                    .agent
                    .pending_acp_request_id
                    .take();
                if let Some(request_id) = acp_request_id {
                    let acp_client = match self.acp_client {
                        Some(ref c) => c.clone(),
                        None => {
                            p.confirm();
                            return;
                        }
                    };
                    // Build CreateElicitationResponse: { action: "accept", content: { prop_id: value } }
                    let content: serde_json::Map<String, serde_json::Value> = p
                        .questions
                        .iter()
                        .map(|q| {
                            let selected_labels: Vec<String> = q
                                .selected
                                .iter()
                                .enumerate()
                                .filter(|(_, &v)| v)
                                .map(|(i, _)| q.data.options[i].label.clone())
                                .collect();
                            let value = if q.data.multi_select {
                                serde_json::Value::Array(
                                    selected_labels
                                        .into_iter()
                                        .map(serde_json::Value::String)
                                        .collect(),
                                )
                            } else {
                                let text = selected_labels
                                    .into_iter()
                                    .next()
                                    .or_else(|| {
                                        let s = q.custom_input.value().trim().to_string();
                                        if s.is_empty() {
                                            None
                                        } else {
                                            Some(s)
                                        }
                                    })
                                    .unwrap_or_default();
                                serde_json::Value::String(text)
                            };
                            (q.data.tool_call_id.clone(), value)
                        })
                        .collect();
                    let response = serde_json::json!({
                        "action": "accept",
                        "content": content
                    });
                    tokio::spawn(async move {
                        if let Err(e) = acp_client.send_response(request_id, Ok(response)).await {
                            tracing::error!(error = %e, "ACP elicitation response send failed");
                        }
                    });
                }

                p.confirm();
            }
        }
    }
}
