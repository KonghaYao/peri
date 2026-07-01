//! 输入区域 #[component] 组件。
//!
//! S8：完整输入体验——
//! - **多行 buffer**：Shift/Alt+Enter 换行；渲染按行拆分；高度动态扩展（3~40% 屏幕）
//! - **history**：Up/Down 浏览 `INPUT_HISTORY` atom；Esc 或回到栈底恢复编辑态
//! - **@mention**：输入 @ 触发 AT_MENTION_ACTIVE；popup 显示在输入框上方
//! - **slash**：行首 / 触发 SLASH_HINT_ACTIVE；popup 显示在输入框上方
//! - **提交**：Enter 提交，submit_consumer 消费 + push_history
//
// element! 宏展开为 `XxxProps { ... ..Default::default() }`，全字段已指定时
// clippy 触发 needless_update 警告。该警告来自宏展开而非用户代码，模块级抑制。
#![allow(clippy::needless_update)]

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Borders, Paragraph},
    },
};

use crate::kit::atoms::{
    ACP_STATE, AT_MENTION_ACTIVE, FILE_LIST, INPUT_BUFFER, MENTION_PREFIX, MENTION_SELECTED_INDEX,
    SLASH_HINT_ACTIVE, SLASH_PREFIX, SLASH_SELECTED_INDEX, SUBMIT_TX,
};
use crate::kit::input_history::{history_down, history_up, push_history};
use crate::kit::mention_popup::MentionPopup;
use crate::kit::slash_completion::SlashCompletion;
use crate::kit::theme;

/// 静态 slash 命令列表——补全提示用。
///
/// 设计原则：所有可能的命令（ACP server 内置 + 历史保留）都列出，让用户能
/// 看到 discoverability。命令实际执行逻辑在 ACP server 端或后续 RPC 调用。
///
/// 分类（仅注释，UI 不分组）：
/// - **ACP server 内置**：/bg /clear /compact /rewind
/// - **会话控制**：/help /quit /init /resume /continue /bug
/// - **配置类**：/mode /yolo /model /login /permissions
/// - **状态查看**：/cost /context /status
/// - **面板入口**：/agents /threads /mcp /cron /tasks /memory /hooks /config /plugins /lsp
/// - **子 Agent / 技能**：/subagent /workflow /skill
const SLASH_COMMANDS: &[&str] = &[
    // ACP server 内置
    "/bg",
    "/clear",
    "/compact",
    "/rewind",
    // 会话控制
    "/help",
    "/quit",
    "/init",
    "/resume",
    "/continue",
    "/bug",
    // 配置类
    "/mode",
    "/yolo",
    "/model",
    "/login",
    "/permissions",
    // 状态查看
    "/cost",
    "/context",
    "/status",
    // 面板入口
    "/agents",
    "/threads",
    "/mcp",
    "/cron",
    "/tasks",
    "/memory",
    "/hooks",
    "/config",
    "/plugins",
    "/lsp",
    // 子 Agent / 技能
    "/subagent",
    "/workflow",
    "/skill",
];

/// 输入状态
#[derive(Clone, Default)]
struct EditorState {
    text: String,
    /// 字符索引（保证在字符边界上）
    cursor: usize,
}

impl EditorState {
    /// 在光标位置插入字符，返回新的光标位置
    fn insert_char(&mut self, ch: char) {
        let byte_idx = Self::char_to_byte(&self.text, self.cursor);
        self.text.insert(byte_idx, ch);
        self.cursor += 1;
    }

    /// 在光标位置插入字符串
    fn insert_str(&mut self, s: &str) {
        let byte_idx = Self::char_to_byte(&self.text, self.cursor);
        self.text.insert_str(byte_idx, s);
        self.cursor += s.chars().count();
    }

    /// 删除光标前的字符
    fn backspace(&mut self) {
        if self.cursor > 0 {
            let byte_idx = Self::char_to_byte(&self.text, self.cursor);
            let prev_byte = Self::char_to_byte(&self.text, self.cursor - 1);
            self.text.drain(prev_byte..byte_idx);
            self.cursor -= 1;
        }
    }

    /// 左移光标
    fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// 右移光标
    fn cursor_right(&mut self) {
        if self.cursor < self.len() {
            self.cursor += 1;
        }
    }

    /// 光标到文本首
    fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    /// 光标到文本尾
    fn cursor_end(&mut self) {
        self.cursor = self.len();
    }

    /// 删除光标前一个词（Ctrl+W）
    fn delete_word_backward(&mut self) {
        let byte_idx = Self::char_to_byte(&self.text, self.cursor);
        // 跳过光标前的空白
        let mut pos = byte_idx;
        while pos > 0 {
            let prev = self.text[..pos].chars().last().unwrap();
            if !prev.is_whitespace() {
                break;
            }
            pos -= prev.len_utf8();
            self.cursor -= 1;
        }
        // 删除到词首
        while pos > 0 {
            let prev = self.text[..pos].chars().last().unwrap();
            if prev.is_whitespace() {
                break;
            }
            pos -= prev.len_utf8();
            self.cursor -= 1;
        }
        self.text.drain(pos..byte_idx);
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// 替换整个文本并把光标放到末尾（历史导航用）
    fn replace_all(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.chars().count();
    }

    /// 清除文本
    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// 字符索引 → 字节偏移
    fn char_to_byte(s: &str, char_idx: usize) -> usize {
        s.char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    }
}

#[derive(Default, Props)]
pub struct InputAreaProps {
    pub loading: bool,
}

#[component]
pub fn InputArea(props: &InputAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 单一编辑状态——闭包编辑 + 渲染读取共享同一实例
    let state = hooks.use_state(EditorState::default);

    hooks.use_local_events(move |event: Event| match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let is_shift = key.modifiers.contains(KeyModifiers::SHIFT);
            let is_alt = key.modifiers.contains(KeyModifiers::ALT);

            // 当前是否激活了 @mention / slash（激活时方向键给 popup 用）
            let mention_active = AT_MENTION_ACTIVE.get().map(|a| *a.read()).unwrap_or(false);
            let slash_active = SLASH_HINT_ACTIVE.get().map(|a| *a.read()).unwrap_or(false);

            match key.code {
                // ── 提交 ──（仅在不激活 popup 时按 Enter 提交）
                KeyCode::Enter if !is_shift && !is_alt && !mention_active && !slash_active => {
                    let mut s = state.write();
                    let submitted = std::mem::take(&mut s.text);
                    s.cursor = 0;
                    if !submitted.trim().is_empty() {
                        // 入历史栈
                        push_history(&submitted);
                        // 关键：loading 时入 INPUT_BUFFER，否则 SUBMIT_TX 直接提交。
                        // 通过 ACP_STATE.is_loading 判断当前 agent 是否运行——
                        // 这是 ratatui-kit 跨闭包共享 loading 状态的官方方式。
                        let is_loading = ACP_STATE
                            .get()
                            .map(|a| a.read().is_loading)
                            .unwrap_or(false);
                        if is_loading {
                            if let Some(buf) = INPUT_BUFFER.get() {
                                let mut guard = buf.write();
                                guard.push_back(submitted);
                                // 上限 32 条，超出从头部丢弃
                                while guard.len() > 32 {
                                    guard.pop_front();
                                }
                            }
                        } else if let Some(tx) = SUBMIT_TX.get() {
                            let _ = tx.send(submitted);
                        }
                        // 关闭可能的 @mention/slash
                        if let Some(a) = AT_MENTION_ACTIVE.get() {
                            *a.write() = false;
                        }
                        if let Some(a) = SLASH_HINT_ACTIVE.get() {
                            *a.write() = false;
                        }
                    }
                }

                // Shift/Alt+Enter：换行（多行 buffer）
                KeyCode::Enter if (is_shift || is_alt) && !mention_active && !slash_active => {
                    state.write().insert_char('\n');
                }

                // ── popup 激活时方向键 / Enter 给 popup ──
                // 这里 popup 自身的 use_local_events 会消费 Up/Down；
                // Enter/Esc 我们在 InputArea 层处理：选择第一项 / 取消
                KeyCode::Enter if mention_active => {
                    // I18-C：读取 popup 选中项索引，选取真实文件名（而非仅 prefix）
                    let prefix = MENTION_PREFIX
                        .get()
                        .map(|a| a.read().clone())
                        .unwrap_or_default();
                    let candidates = filter_files_for_mention(&prefix);
                    let sel_idx = MENTION_SELECTED_INDEX.get().map(|a| *a.read()).unwrap_or(0);
                    let replacement = candidates.get(sel_idx).cloned().unwrap_or_default();
                    // 找到 @ 字符位置并替换其后所有 mention prefix
                    let mut s = state.write();
                    if let Some(at_byte) = s.text.rfind('@') {
                        let after_at_byte = at_byte + 1;
                        // 删除 @ 后的所有非空白字符（即旧的 prefix）
                        let keep_until_byte = s.text[after_at_byte..]
                            .char_indices()
                            .take_while(|(_, c)| !c.is_whitespace())
                            .last()
                            .map(|(i, c)| after_at_byte + i + c.len_utf8())
                            .unwrap_or(after_at_byte);
                        s.text.drain(after_at_byte..keep_until_byte);
                        // 插入替换文本
                        s.text.insert_str(after_at_byte, &replacement);
                        s.cursor = s.text.chars().count();
                    }
                    drop(s);
                    if let Some(a) = AT_MENTION_ACTIVE.get() {
                        *a.write() = false;
                    }
                    if let Some(a) = MENTION_PREFIX.get() {
                        a.write().clear();
                    }
                    // 重置选中索引，下次开 popup 默认第 0 项
                    if let Some(a) = MENTION_SELECTED_INDEX.get() {
                        *a.write() = 0;
                    }
                }
                KeyCode::Enter if slash_active => {
                    let prefix = SLASH_PREFIX
                        .get()
                        .map(|a| a.read().clone())
                        .unwrap_or_default();
                    // I18-C：按 popup 相同过滤逻辑获取真实选中命令
                    let prefix_lower = prefix.to_lowercase();
                    let filtered: Vec<String> = SLASH_COMMANDS
                        .iter()
                        .map(|s| s.to_string())
                        .filter(|cmd| {
                            prefix_lower.is_empty() || cmd.to_lowercase().starts_with(&prefix_lower)
                        })
                        .collect();
                    let sel_idx = SLASH_SELECTED_INDEX.get().map(|a| *a.read()).unwrap_or(0);
                    let cmd = filtered.get(sel_idx).cloned();
                    let mut s = state.write();
                    // 替换整个 editor 内容为命令
                    if let Some(cmd) = cmd {
                        s.replace_all(cmd.clone());
                        // 立即提交命令——同样检查 loading 入 buffer
                        drop(s);
                        let final_text = state.read().text.clone();
                        if !final_text.trim().is_empty() {
                            push_history(&final_text);
                            let is_loading = ACP_STATE
                                .get()
                                .map(|a| a.read().is_loading)
                                .unwrap_or(false);
                            if is_loading {
                                if let Some(buf) = INPUT_BUFFER.get() {
                                    let mut guard = buf.write();
                                    guard.push_back(final_text);
                                    while guard.len() > 32 {
                                        guard.pop_front();
                                    }
                                }
                            } else if let Some(tx) = SUBMIT_TX.get() {
                                let _ = tx.send(final_text);
                            }
                            state.write().clear();
                        }
                    } else {
                        drop(s);
                    }
                    if let Some(a) = SLASH_HINT_ACTIVE.get() {
                        *a.write() = false;
                    }
                    if let Some(a) = SLASH_PREFIX.get() {
                        a.write().clear();
                    }
                    // 重置选中索引
                    if let Some(a) = SLASH_SELECTED_INDEX.get() {
                        *a.write() = 0;
                    }
                }

                // ── 编辑快捷键 ──
                KeyCode::Char('w') if is_ctrl => {
                    state.write().delete_word_backward();
                }
                KeyCode::Char('u') if is_ctrl => {
                    state.write().clear();
                }
                KeyCode::Backspace if !mention_active && !slash_active => {
                    let mut s = state.write();
                    s.backspace();
                    // 若删完后文本不以 / 开头，关闭 slash 提示
                    if let Some(a) = SLASH_HINT_ACTIVE.get()
                        && *a.read()
                        && !s.text.starts_with('/')
                    {
                        *a.write() = false;
                    }
                }
                KeyCode::Backspace => {
                    // popup 激活时退格——更新 prefix
                    let mut s = state.write();
                    s.backspace();
                    update_popup_prefix(&s.text);
                }
                KeyCode::Left if !mention_active && !slash_active => {
                    state.write().cursor_left();
                }
                KeyCode::Right if !mention_active && !slash_active => {
                    state.write().cursor_right();
                }
                // ── history 导航（仅在不激活 popup 且无 Ctrl 修饰时）──
                // I18-B：必须排除 Ctrl+Up/Down/Home/End——这些键留给 message_area 滚动。
                // use_local_events 是广播式，InputArea 和 MessageArea 都会收到同一事件。
                KeyCode::Up if !is_ctrl && !mention_active && !slash_active => {
                    if let Some(historical) = history_up() {
                        state.write().replace_all(historical);
                    }
                }
                KeyCode::Down if !is_ctrl && !mention_active && !slash_active => {
                    match history_down() {
                        Some(historical) => state.write().replace_all(historical),
                        None => {
                            // 回到编辑态——保留当前文本（草稿语义）
                        }
                    }
                }
                KeyCode::Home if !is_ctrl && !mention_active && !slash_active => {
                    state.write().cursor_home();
                }
                KeyCode::End if !is_ctrl && !mention_active && !slash_active => {
                    state.write().cursor_end();
                }
                // Esc 在不激活 popup 时清空文本（激活 popup 时由上层关闭 popup）
                KeyCode::Esc if !mention_active && !slash_active => {
                    state.write().clear();
                }

                // ── 字符输入 ──
                KeyCode::Char(ch) if !is_ctrl && !is_alt => {
                    let mut s = state.write();
                    s.insert_char(ch);
                    // 触发 @mention / slash 提示
                    update_popup_prefix(&s.text);
                }

                _ => {}
            }
        }
        Event::Paste(paste_text) => {
            let mut s = state.write();
            s.insert_str(&paste_text);
            update_popup_prefix(&s.text);
        }
        _ => {}
    });
    let editor = state.read().clone();
    let text = editor.text.clone();
    let cursor = editor.cursor;
    let loading = props.loading;

    // 当前激活状态（驱动 popup 渲染）
    let mention_active = AT_MENTION_ACTIVE.get().map(|a| *a.read()).unwrap_or(false);
    let slash_active = SLASH_HINT_ACTIVE.get().map(|a| *a.read()).unwrap_or(false);
    let mention_prefix = MENTION_PREFIX
        .get()
        .map(|a| a.read().clone())
        .unwrap_or_default();
    let slash_prefix = SLASH_PREFIX
        .get()
        .map(|a| a.read().clone())
        .unwrap_or_default();

    // 多行渲染——按 \n 拆分，每行作为独立 Line，光标高亮放在对应行
    let lines = render_multiline_with_cursor(&text, cursor, loading);

    // 计算高度：3 行基础 + 文本行数；最大 12 行
    let line_count = text.matches('\n').count() + 1;
    let editor_height = (line_count as u16 + 2).min(12);

    let slash_commands: Vec<String> = SLASH_COMMANDS.iter().map(|s| s.to_string()).collect();

    // popup 高度建议——避免高度计算复杂，固定 8 行
    let _ = editor_height;

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            #(if slash_active {
                element!(SlashCompletion(
                    prefix: slash_prefix.clone(),
                    commands: slash_commands.clone(),
                    on_select: Handler::from(|_: String| {}),
                    on_cancel: Handler::from(|_: ()| {}),
                )).into_any()
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0))).into_any()
            })
            #(if mention_active {
                element!(MentionPopup(
                    prefix: mention_prefix.clone(),
                    items: filter_files_for_mention(&mention_prefix),
                    on_select: Handler::from(|_: String| {}),
                    on_cancel: Handler::from(|_: ()| {}),
                )).into_any()
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0))).into_any()
            })
            View(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Length(editor_height),
            ) {
                Text(text: Paragraph::new(lines).block(
                    if loading {
                        Block::default()
                            .borders(Borders::TOP)
                            .border_style(ratatui::style::Style::new().fg(theme::MUTED))
                    } else {
                        Block::default().borders(Borders::TOP)
                    }
                ))
            }
        }
    )
}

/// 从 `FILE_LIST` atom 读出 cwd 文件列表，按 `prefix` 过滤，最多 20 条。
///
/// 大小写不敏感的子串匹配——这样 `@auth` 能匹配 `auth.rs` / `oauth.rs` /
/// `authenticated.md` 等。结果按"prefix 开头优先"排序，提升命中率。
fn filter_files_for_mention(prefix: &str) -> Vec<String> {
    let files = FILE_LIST
        .get()
        .map(|a| a.read().clone())
        .unwrap_or_default();
    if prefix.is_empty() {
        return files.into_iter().take(20).collect();
    }
    let prefix_lower = prefix.to_lowercase();
    let mut matches: Vec<String> = files
        .iter()
        .filter(|f| f.to_lowercase().contains(&prefix_lower))
        .cloned()
        .collect();
    // prefix 开头的优先
    matches.sort_by_key(|f| !f.to_lowercase().starts_with(&prefix_lower));
    matches.truncate(20);
    matches
}

/// 根据 editor 当前文本更新 @mention / slash 提示状态。
///
/// - `/` 在行首：开启 slash 提示，prefix = 第一个非空白/制表符之后到光标的内容
/// - `@` 在最近词中：开启 @mention，prefix = @ 之后的字符
fn update_popup_prefix(text: &str) {
    // slash：仅当整段文本以 / 开头且无空格时
    let slash_active_now = text.starts_with('/') && !text[1..].contains(' ');
    if let Some(a) = SLASH_HINT_ACTIVE.get() {
        *a.write() = slash_active_now;
    }
    if let Some(a) = SLASH_PREFIX.get() {
        if slash_active_now {
            // prefix = 去掉 / 后的所有字符
            *a.write() = text[1..].to_string();
        } else {
            a.write().clear();
        }
    }

    // @mention：找最后一个 @，若其后到文本末尾无空白则激活
    let mention_active_now = if let Some(at_idx) = text.rfind('@') {
        let after = &text[at_idx + 1..];
        !after.is_empty() && !after.contains(char::is_whitespace) && after != "@"
    } else {
        false
    };
    if let Some(a) = AT_MENTION_ACTIVE.get() {
        *a.write() = mention_active_now;
    }
    if let Some(a) = MENTION_PREFIX.get() {
        if mention_active_now {
            if let Some(at_idx) = text.rfind('@') {
                *a.write() = text[at_idx + 1..].to_string();
            }
        } else {
            a.write().clear();
        }
    }
}

/// 把文本按 \n 拆成多行 Line，光标以反转色高亮。
fn render_multiline_with_cursor(text: &str, cursor: usize, loading: bool) -> Vec<Line<'static>> {
    let cursor_style = Style::default()
        .fg(Color::Rgb(0, 0, 0))
        .bg(theme::TEXT)
        .add_modifier(Modifier::BOLD);

    if text.is_empty() {
        return vec![if loading {
            Line::from("")
        } else {
            Line::from(vec![Span::styled("\u{258C}", cursor_style)])
        }];
    }

    // 把光标位置映射到 (line_idx, col_idx)
    let mut chars_before_cursor = 0usize;
    let mut done = false;
    let mut target_line = 0usize;
    let mut target_col = 0usize;
    for (li, line) in text.split('\n').enumerate() {
        let line_chars = line.chars().count();
        if !done && chars_before_cursor + line_chars >= cursor {
            target_line = li;
            target_col = cursor - chars_before_cursor;
            done = true;
            break;
        }
        chars_before_cursor += line_chars + 1; // +1 for \n
        if chars_before_cursor > cursor + 1 {
            break;
        }
    }
    if !done {
        // 光标在文本末尾
        let total_lines: Vec<&str> = text.split('\n').collect();
        target_line = total_lines.len() - 1;
        target_col = total_lines.last().map(|l| l.chars().count()).unwrap_or(0);
    }

    let mut result: Vec<Line<'static>> = Vec::new();
    for (li, line) in text.split('\n').enumerate() {
        if li == target_line {
            let col_byte = EditorState::char_to_byte(line, target_col);
            let mut spans: Vec<Span<'static>> = Vec::new();
            if col_byte > 0 {
                spans.push(Span::raw(line[..col_byte].to_string()));
            }
            if col_byte < line.len() {
                // 高亮当前字符
                let next_end = line[col_byte..]
                    .chars()
                    .next()
                    .map(|c| col_byte + c.len_utf8())
                    .unwrap_or(line.len());
                spans.push(Span::styled(
                    line[col_byte..next_end].to_string(),
                    cursor_style,
                ));
                if next_end < line.len() {
                    spans.push(Span::raw(line[next_end..].to_string()));
                }
            } else {
                // 光标在行尾
                spans.push(Span::styled(" ", cursor_style));
            }
            result.push(Line::from(spans));
        } else {
            result.push(Line::from(line.to_string()));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_editor_state_replace_all_sets_cursor_to_end() {
        let mut s = EditorState::default();
        s.replace_all("hello".to_string());
        assert_eq!(s.text, "hello");
        assert_eq!(s.cursor, 5);
    }

    #[test]
    fn test_editor_state_clear() {
        let mut s = EditorState {
            text: "abc".into(),
            cursor: 2,
        };
        s.clear();
        assert!(s.text.is_empty());
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn test_char_to_byte_boundaries() {
        // ASCII
        assert_eq!(EditorState::char_to_byte("hello", 0), 0);
        assert_eq!(EditorState::char_to_byte("hello", 3), 3);
        assert_eq!(EditorState::char_to_byte("hello", 5), 5);
        assert_eq!(EditorState::char_to_byte("hello", 99), 5); // 越界回退

        // CJK
        assert_eq!(EditorState::char_to_byte("你好", 0), 0);
        assert_eq!(EditorState::char_to_byte("你好", 1), 3); // '你' 占 3 字节
        assert_eq!(EditorState::char_to_byte("你好", 2), 6);
    }

    #[test]
    fn test_render_multiline_empty_shows_cursor() {
        let lines = render_multiline_with_cursor("", 0, false);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_render_multiline_splits_newlines() {
        let lines = render_multiline_with_cursor("a\nb\nc", 0, false);
        assert_eq!(lines.len(), 3);
    }

    fn reset_popup_atoms() {
        if let Some(a) = AT_MENTION_ACTIVE.get() {
            *a.write() = false;
        }
        if let Some(a) = SLASH_HINT_ACTIVE.get() {
            *a.write() = false;
        }
        if let Some(a) = MENTION_PREFIX.get() {
            a.write().clear();
        }
        if let Some(a) = SLASH_PREFIX.get() {
            a.write().clear();
        }
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_slash_at_start() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        update_popup_prefix("/hel");
        assert!(!*AT_MENTION_ACTIVE.get().unwrap().read());
        assert!(*SLASH_HINT_ACTIVE.get().unwrap().read());
        assert_eq!(SLASH_PREFIX.get().unwrap().read().as_str(), "hel");
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_slash_with_space_disables() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        update_popup_prefix("/help me");
        assert!(!*SLASH_HINT_ACTIVE.get().unwrap().read());
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_mention_trigger() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        update_popup_prefix("see @auth");
        assert!(*AT_MENTION_ACTIVE.get().unwrap().read());
        assert_eq!(MENTION_PREFIX.get().unwrap().read().as_str(), "auth");
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_mention_with_space_disables() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        update_popup_prefix("see @auth service");
        assert!(!*AT_MENTION_ACTIVE.get().unwrap().read());
    }

    /// C2 回归测试：filter_files_for_mention 在 prefix 为空时返回前 20 条。
    #[test]
    #[serial]
    fn test_filter_files_empty_prefix_returns_top_20() {
        crate::kit::atoms::init_atoms();
        // 写 25 个文件
        if let Some(atom) = FILE_LIST.get() {
            let mut list: Vec<String> = (0..25).map(|i| format!("file{i}.rs")).collect();
            list.sort();
            *atom.write() = list;
        }
        let result = filter_files_for_mention("");
        assert_eq!(result.len(), 20);
    }

    /// C2 回归测试：filter_files_for_mention 按大小写不敏感子串过滤。
    #[test]
    #[serial]
    fn test_filter_files_substring_case_insensitive() {
        crate::kit::atoms::init_atoms();
        if let Some(atom) = FILE_LIST.get() {
            *atom.write() = vec![
                "auth.rs".into(),
                "oauth.rs".into(),
                "OAUTH.md".into(),
                "utils.rs".into(),
            ];
        }
        let result = filter_files_for_mention("AUTH");
        // 三个含 auth/AUTH 的文件应被过滤出来（大小写不敏感）
        assert_eq!(result.len(), 3);
        assert!(result.contains(&"auth.rs".to_string()));
        assert!(result.contains(&"oauth.rs".to_string()));
        assert!(result.contains(&"OAUTH.md".to_string()));
    }

    /// C2 回归测试：prefix 开头的文件优先于子串匹配的。
    #[test]
    #[serial]
    fn test_filter_files_prefix_start_priority() {
        crate::kit::atoms::init_atoms();
        if let Some(atom) = FILE_LIST.get() {
            *atom.write() = vec![
                "myauth.rs".into(), // 子串匹配
                "auth.rs".into(),   // 开头匹配，应优先
                "oauth.rs".into(),  // 子串匹配
            ];
        }
        let result = filter_files_for_mention("auth");
        assert_eq!(result.first().unwrap(), "auth.rs");
    }
}
