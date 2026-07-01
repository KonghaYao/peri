use super::*;

/// Inline / skill/command 补全触发状态。
/// 参考 `AtMentionState::detect()` 模式：在光标前回溯查找 `/` token，
/// 要求 `/` 前为空白字符或行首以避免 `and/or` 等正常文本误触发。
#[derive(Default)]
pub struct SlashHintState {
    pub active: bool,
    /// `/` 符号在全文中的字节偏移
    pub token_start: usize,
    /// `/` 之后的文本（用于过滤候选，如 "code" 匹配 /code-review）
    pub prefix: String,
}

impl SlashHintState {
    /// 在 `text[..cursor_pos]` 中检测光标前最近的 `/` token。
    /// 要求 `/` 前为空白字符/行首，`/` 后为有效 skill/command 名字符（字母数字 `-` `_` `:` `.`）。
    /// 返回 `(prefix, slash_byte_offset)` 或 `None`。
    pub fn detect(text: &str, cursor_pos: usize) -> Option<(String, usize)> {
        if cursor_pos == 0 || cursor_pos > text.len() {
            return None;
        }
        let before_cursor = &text[..cursor_pos];
        let slash_pos = before_cursor.rfind('/')?;
        let after_slash = &before_cursor[slash_pos + '/'.len_utf8()..];

        // 检查 / 前是否为空白字符或行首
        if slash_pos > 0 {
            let char_before = before_cursor[..slash_pos].chars().next_back().unwrap();
            if !char_before.is_whitespace() && char_before != '\n' {
                return None;
            }
        }

        // / 后允许空（仅 / 时展示全部候选）或仅含有效名字符
        if !after_slash.is_empty()
            && !after_slash
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.')
        {
            return None;
        }

        Some((after_slash.to_string(), slash_pos))
    }

    pub fn activate(&mut self, prefix: String, token_start: usize) {
        self.active = true;
        self.prefix = prefix;
        self.token_start = token_start;
    }

    pub fn deactivate(&mut self) {
        self.active = false;
        self.prefix.clear();
    }
}

/// 统一候选项：命令、Skill 或 Agent 命令，与渲染侧 hints.rs 保持一致
enum HintItem {
    Cmd { name: String },
    Skill { name: String },
    AgentCmd { name: String },
}

impl HintItem {
    fn name(&self) -> &str {
        match self {
            HintItem::Cmd { name } => name,
            HintItem::Skill { name } => name,
            HintItem::AgentCmd { name } => name,
        }
    }
}

impl App {
    /// 构建统一排序后的候选项列表（与渲染侧一致）
    fn build_hint_items(&self) -> Vec<HintItem> {
        let slash_hint = &self.session_mgr.current().ui.slash_hint;
        if !slash_hint.active {
            return vec![];
        }
        let prefix = &slash_hint.prefix;
        let cmd_candidates: Vec<_> = self
            .session_mgr
            .current()
            .commands
            .command_registry
            .match_prefix(prefix, &self.services.lc);
        let skill_candidates: Vec<_> = self
            .session_mgr
            .current()
            .commands
            .skills
            .iter()
            .filter(|s| prefix.is_empty() || s.name.contains(prefix))
            .collect();
        // Agent commands from ACP AvailableCommandsUpdate (e.g. /compact)
        let agent_cmd_candidates: Vec<_> = self
            .session_mgr
            .current()
            .commands
            .agent_commands
            .iter()
            .filter(|n| prefix.is_empty() || n.contains(prefix))
            .collect();

        let mut items: Vec<HintItem> = Vec::new();
        for (name, _) in &cmd_candidates {
            items.push(HintItem::Cmd { name: name.clone() });
        }
        for skill in &skill_candidates {
            items.push(HintItem::Skill {
                name: skill.name.clone(),
            });
        }
        for name in &agent_cmd_candidates {
            items.push(HintItem::AgentCmd {
                name: (*name).clone(),
            });
        }
        items.sort_by(|a, b| {
            let a_starts = a.name().starts_with(prefix) as u8;
            let b_starts = b.name().starts_with(prefix) as u8;
            // 前缀匹配优先 > 命令 > Skill > AgentCmd > 字母序
            let a_rank = match a {
                HintItem::Cmd { .. } => 2,
                HintItem::Skill { .. } => 1,
                HintItem::AgentCmd { .. } => 0,
            };
            let b_rank = match b {
                HintItem::Cmd { .. } => 2,
                HintItem::Skill { .. } => 1,
                HintItem::AgentCmd { .. } => 0,
            };
            b_starts
                .cmp(&a_starts)
                .then_with(|| b_rank.cmp(&a_rank))
                .then_with(|| a.name().cmp(b.name()))
        });
        items
    }

    /// 获取当前提示浮层的候选数量
    pub fn hint_candidates_count(&self) -> usize {
        self.build_hint_items().len()
    }

    /// Tab/Enter 补全：选中当前光标处的候选项，仅替换 /token，保留消息其余内容。
    pub fn hint_complete(&mut self) {
        let selected_name = {
            let items = self.build_hint_items();
            let cursor = self.session_mgr.current().ui.hint_cursor.unwrap_or(0);
            items.get(cursor).map(|item| item.name().to_string())
        };

        if let Some(name) = selected_name {
            let slash_hint = &self.session_mgr.current().ui.slash_hint;
            let full_text: String = self.session_mgr.current().ui.textarea.lines().join("\n");
            let slash_pos = slash_hint.token_start;
            // token 总长度：/ + prefix（字符级精确计算）
            let token_len = 1 + slash_hint.prefix.len();
            let replacement = format!("/{} ", name);

            // 构造新文本：保留 / 之前和 token 之后的内容
            let mut new_text = String::with_capacity(full_text.len() + replacement.len());
            new_text.push_str(&full_text[..slash_pos]);
            new_text.push_str(&replacement);
            let after_end = slash_pos + token_len;
            if after_end < full_text.len() {
                new_text.push_str(&full_text[after_end..]);
            }

            let mut new_ta = build_textarea(false);
            new_ta.insert_str(&new_text);
            self.session_mgr.current_mut().ui.textarea = new_ta;

            // 关闭 slash hint
            self.session_mgr.current_mut().ui.slash_hint.deactivate();
            self.session_mgr.current_mut().ui.hint_cursor = None;
        }
    }
}
