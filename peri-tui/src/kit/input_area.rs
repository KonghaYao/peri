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
use std::sync::{Arc, Mutex};

use crate::kit::atoms::{
    ACP_STATE, AT_MENTION_ACTIVE, FILE_LIST, INPUT_AREA_ESC_PREFIX, INPUT_BUFFER, MENTION_PREFIX,
    MENTION_SELECTED_INDEX, SLASH_HINT_ACTIVE, SLASH_PREFIX, SLASH_SELECTED_INDEX, SUBMIT_TX,
};
use crate::kit::input_history::{history_down, history_up, push_history};
use crate::kit::mention_popup::MentionPopup;
use crate::kit::panel_registry::{
    PANELS, open_panel, panel_for_slash_command, slash_command_for_panel,
};
use crate::kit::slash_completion::{SlashActionKind, SlashCompletion, SlashCompletionItem};
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
const REMOTE_SLASH_COMMANDS: &[(&str, &str)] = &[
    ("bg", "Run the next prompt as a background task"),
    ("clear", "Clear current thread messages"),
    ("compact", "Compact conversation context"),
    ("rewind", "Delete the most recent user turn"),
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

    /// 左移到上一个词边界
    fn cursor_word_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut new_cursor = self.cursor;
        while new_cursor > 0 {
            let prev = self.char_at(new_cursor - 1);
            if !prev.is_whitespace() {
                break;
            }
            new_cursor -= 1;
        }
        while new_cursor > 0 {
            let prev = self.char_at(new_cursor - 1);
            if prev.is_whitespace() {
                break;
            }
            new_cursor -= 1;
        }
        self.cursor = new_cursor;
    }

    /// 右移到下一个词边界
    fn cursor_word_right(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        let mut new_cursor = self.cursor;
        while new_cursor < self.len() {
            let ch = self.char_at(new_cursor);
            if ch.is_whitespace() {
                break;
            }
            new_cursor += 1;
        }
        while new_cursor < self.len() {
            let ch = self.char_at(new_cursor);
            if !ch.is_whitespace() {
                break;
            }
            new_cursor += 1;
        }
        self.cursor = new_cursor;
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

    fn char_at(&self, char_idx: usize) -> char {
        self.text
            .chars()
            .nth(char_idx)
            .expect("cursor must stay within character bounds")
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

    hooks.use_event_handler(
        EventScope::Current,
        EventPriority::Normal,
        move |event| match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let is_shift = key.modifiers.contains(KeyModifiers::SHIFT);
                let is_alt = key.modifiers.contains(KeyModifiers::ALT);

                if matches!(key.code, KeyCode::Esc) {
                    INPUT_AREA_ESC_PREFIX.set(is_alt);
                } else {
                    INPUT_AREA_ESC_PREFIX.set(false);
                }

                // 当前是否激活了 @mention / slash（激活时方向键给 popup 用）
                let mention_active = *AT_MENTION_ACTIVE.state().read();
                let slash_active = *SLASH_HINT_ACTIVE.state().read();

                let result = match key.code {
                    // ── 提交 ──（仅在不激活 popup 时按 Enter 提交）
                    KeyCode::Enter if !is_shift && !is_alt && !mention_active && !slash_active => {
                        let mut s = state.write();
                        let submitted = std::mem::take(&mut s.text);
                        s.cursor = 0;
                        drop(s);

                        submit_text(submitted);
                        reset_mention_popup();
                        reset_slash_popup();
                        EventResult::Consumed
                    }

                    // Shift/Alt+Enter：换行（多行 buffer）
                    KeyCode::Enter if (is_shift || is_alt) && !mention_active && !slash_active => {
                        state.write().insert_char('\n');
                        EventResult::Consumed
                    }

                    // ── 编辑快捷键 ──
                    KeyCode::Char('w') if is_ctrl => {
                        state.write().delete_word_backward();
                        EventResult::Consumed
                    }
                    KeyCode::Char('u') if is_ctrl => {
                        state.write().clear();
                        EventResult::Consumed
                    }
                    KeyCode::Backspace if !mention_active && !slash_active => {
                        let mut s = state.write();
                        s.backspace();
                        if *SLASH_HINT_ACTIVE.state().read() && !s.text.starts_with('/') {
                            *SLASH_HINT_ACTIVE.state().write() = false;
                        }
                        EventResult::Consumed
                    }
                    KeyCode::Backspace => {
                        let mut s = state.write();
                        s.backspace();
                        update_popup_prefix(&s.text);
                        EventResult::Consumed
                    }
                    KeyCode::Left if is_alt && !is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_word_left();
                        EventResult::Consumed
                    }
                    KeyCode::Right if is_alt && !is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_word_right();
                        EventResult::Consumed
                    }
                    KeyCode::Left if !mention_active && !slash_active => {
                        state.write().cursor_left();
                        EventResult::Consumed
                    }
                    KeyCode::Right if !mention_active && !slash_active => {
                        state.write().cursor_right();
                        EventResult::Consumed
                    }
                    // ── history 导航（仅在不激活 popup 且无 Ctrl 修饰时）──
                    // I18-B：必须排除 Ctrl+Up/Down/Home/End——这些键留给 message_area 滚动。
                    // 事件现在分别由 InputArea / MessageArea 用 `use_event_handler` 注册，
                    // 因此这里显式避开 Ctrl+ 组合，避免与消息区滚动键冲突。
                    KeyCode::Up if !is_ctrl && !mention_active && !slash_active => {
                        tracing::info!(?key, "input area consumed history up");
                        if let Some(historical) = history_up() {
                            state.write().replace_all(historical);
                        }
                        EventResult::Consumed
                    }
                    KeyCode::Down if !is_ctrl && !mention_active && !slash_active => {
                        tracing::info!(?key, "input area consumed history down");
                        if let Some(historical) = history_down() {
                            state.write().replace_all(historical);
                        }
                        EventResult::Consumed
                    }
                    KeyCode::Home if !is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_home();
                        EventResult::Consumed
                    }
                    KeyCode::End if !is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_end();
                        EventResult::Consumed
                    }
                    // I19-D：Esc 不再清空草稿——用户多行输入时误按 Esc 会丢失全部内容。
                    // 双击 Esc 由 event_handlers.rs 上层处理（触发 RewindPopup），
                    // 用户想清空输入框用 Ctrl+U。popup 激活时 Esc 由 event_handlers 关 popup。
                    KeyCode::Esc if !mention_active && !slash_active => EventResult::Ignored,

                    // ── 字符输入 ──
                    KeyCode::Char(ch) if !is_ctrl && !is_alt => {
                        let mut s = state.write();
                        s.insert_char(ch);
                        update_popup_prefix(&s.text);
                        EventResult::Consumed
                    }

                    _ => EventResult::Ignored,
                };

                if !is_alt {
                    INPUT_AREA_ESC_PREFIX.set(false);
                }
                result
            }
            Event::Paste(paste_text) => {
                // I22-A：paste 大小上限——防止用户误粘 10MB 日志冻结终端。
                // 10_000 chars 足够覆盖正常长 paste（代码片段、命令输出）；
                // 超出截断并 log warn 提示（用户可改用文件追加方式）。
                const MAX_PASTE_CHARS: usize = 10_000;
                let char_count = paste_text.chars().count();
                let truncated: String = paste_text.chars().take(MAX_PASTE_CHARS).collect();
                if char_count > MAX_PASTE_CHARS {
                    tracing::warn!(
                        original_chars = char_count,
                        capped_at = MAX_PASTE_CHARS,
                        "InputArea: paste 截断——超出 10K char 上限"
                    );
                }
                let mut s = state.write();
                s.insert_str(&truncated);
                update_popup_prefix(&s.text);
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        },
    );
    let editor = state.read().clone();
    let text = editor.text.clone();
    let cursor = editor.cursor;
    let loading = props.loading;

    // 当前激活状态（驱动 popup 渲染）
    let mention_active = *AT_MENTION_ACTIVE.state().read();
    let slash_active = *SLASH_HINT_ACTIVE.state().read();
    let mention_prefix = MENTION_PREFIX.state().read().clone();
    let slash_prefix = SLASH_PREFIX.state().read().clone();

    // 多行渲染——按 \n 拆分，每行作为独立 Line，光标高亮放在对应行
    let lines = render_multiline_with_cursor(&text, cursor, loading);

    // 计算 composer 本体高度；popup 额外占位，避免被输入区自身裁切。
    let line_count = text.matches('\n').count() + 1;
    let editor_rows = (line_count as u16).clamp(1, 10);
    let composer_height = editor_rows + 2;

    let slash_items = build_slash_items();
    let mention_select_state = state.clone();
    let slash_select_state = state.clone();

    let slash_popup_height = if slash_active {
        popup_height(slash_items.len())
    } else {
        0
    };
    let mention_popup_height = if mention_active {
        popup_height(filter_files_for_mention(&mention_prefix).len())
    } else {
        0
    };
    let total_height = composer_height + slash_popup_height + mention_popup_height;

    let composer_lines = build_composer_lines(lines, loading);
    let mention_items = filter_files_for_mention(&mention_prefix);

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(total_height),
        ) {
            { if slash_active {
                element!(SlashCompletion(
                    prefix: slash_prefix.clone(),
                    items: slash_items.clone(),
                    on_select: Arc::new(Mutex::new(Handler::from(move |item: SlashCompletionItem| {
                        match item.kind {
                            SlashActionKind::Panel => {
                                if let Some(kind) = panel_for_slash_command(&item.insert_text) {
                                    open_panel(kind);
                                }
                            }
                            SlashActionKind::Command => {
                                let mut editor = slash_select_state.write();
                                apply_slash_selection(&mut editor, &item.insert_text);
                            }
                        }
                        reset_slash_popup();
                    }))),
                    on_cancel: Arc::new(Mutex::new(Handler::from(|_: ()| {
                        reset_slash_popup();
                    }))),
                )).into_any()
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0))).into_any()
            } }
            { if mention_active {
                element!(MentionPopup(
                    prefix: mention_prefix.clone(),
                    items: mention_items.clone(),
                    on_select: Arc::new(Mutex::new(Handler::from(move |replacement: String| {
                        let mut editor = mention_select_state.write();
                        replace_last_mention(&mut editor, &replacement);
                        reset_mention_popup();
                    }))),
                    on_cancel: Arc::new(Mutex::new(Handler::from(|_: ()| {
                        reset_mention_popup();
                    }))),
                )).into_any()
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0))).into_any()
            } }
            View(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Length(composer_height),
            ) {
                Text(text: Paragraph::new(composer_lines).block(build_composer_block(loading)))
            }
        }
    )
}

fn build_composer_block(loading: bool) -> Block<'static> {
    let border_color = if loading {
        theme::MUTED
    } else {
        theme::BORDER_ACTIVE
    };

    Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color))
}

fn build_composer_lines(editor_lines: Vec<Line<'static>>, loading: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(editor_lines.len().max(1));
    let prompt_style = Style::default()
        .fg(if loading { theme::MUTED } else { theme::ACCENT })
        .add_modifier(Modifier::BOLD);

    if editor_lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" ❯ ", prompt_style),
            Span::raw(""),
        ]));
        return lines;
    }

    for (index, line) in editor_lines.into_iter().enumerate() {
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        if index == 0 {
            spans.push(Span::styled(" ❯ ", prompt_style));
        } else {
            spans.push(Span::styled("   ", Style::default().fg(theme::DIM)));
        }
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }

    lines
}

fn popup_height(item_count: usize) -> u16 {
    (item_count.max(1) + 2).min(10) as u16
}

fn submit_text(submitted: String) {
    if submitted.trim().is_empty() {
        return;
    }

    push_history(&submitted);
    let is_loading = ACP_STATE.state().read().is_loading;
    if is_loading {
        let input_buffer = INPUT_BUFFER.state();
        let mut guard = input_buffer.write();
        guard.push_back(submitted);
        while guard.len() > 32 {
            guard.pop_front();
        }
    } else if let Some(tx) = SUBMIT_TX.get() {
        let _ = tx.send(submitted);
    }
}

fn reset_mention_popup() {
    *AT_MENTION_ACTIVE.state().write() = false;
    MENTION_PREFIX.state().write().clear();
    *MENTION_SELECTED_INDEX.state().write() = 0;
}

fn reset_slash_popup() {
    *SLASH_HINT_ACTIVE.state().write() = false;
    SLASH_PREFIX.state().write().clear();
    *SLASH_SELECTED_INDEX.state().write() = 0;
}

fn replace_last_mention(state: &mut EditorState, replacement: &str) {
    if let Some(at_byte) = state.text.rfind('@') {
        let after_at_byte = at_byte + 1;
        let keep_until_byte = state.text[after_at_byte..]
            .char_indices()
            .take_while(|(_, c)| !c.is_whitespace())
            .last()
            .map(|(i, c)| after_at_byte + i + c.len_utf8())
            .unwrap_or(after_at_byte);
        state.text.drain(after_at_byte..keep_until_byte);
        state.text.insert_str(after_at_byte, replacement);
        state.cursor = state.text.chars().count();
    }
}

fn apply_slash_selection(state: &mut EditorState, cmd: &str) {
    state.replace_all(format!("/{cmd} "));
}

fn build_slash_items() -> Vec<SlashCompletionItem> {
    let mut items = Vec::with_capacity(PANELS.len() + REMOTE_SLASH_COMMANDS.len());
    for panel in PANELS {
        let slash_name = slash_command_for_panel(panel.kind).into_owned();
        items.push(SlashCompletionItem {
            label: slash_name.clone(),
            insert_text: slash_name,
            description: panel.description.to_string(),
            kind: SlashActionKind::Panel,
        });
    }
    for (name, description) in REMOTE_SLASH_COMMANDS {
        items.push(SlashCompletionItem {
            label: (*name).to_string(),
            insert_text: (*name).to_string(),
            description: (*description).to_string(),
            kind: SlashActionKind::Command,
        });
    }
    items
}

/// 从 `FILE_LIST` atom 读出 cwd 文件列表，按 `prefix` 过滤，最多 20 条。
///
/// 大小写不敏感的子串匹配——这样 `@auth` 能匹配 `auth.rs` / `oauth.rs` /
/// `authenticated.md` 等。结果按"prefix 开头优先"排序，提升命中率。
fn filter_files_for_mention(prefix: &str) -> Vec<String> {
    let files = FILE_LIST.state().read().clone();
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
    *SLASH_HINT_ACTIVE.state().write() = slash_active_now;
    if slash_active_now {
        *SLASH_PREFIX.state().write() = text[1..].to_string();
    } else {
        SLASH_PREFIX.state().write().clear();
    }

    // @mention：找最后一个 @，若其后到文本末尾无空白则激活
    let mention_active_now = if let Some(at_idx) = text.rfind('@') {
        let after = &text[at_idx + 1..];
        !after.is_empty() && !after.contains(char::is_whitespace) && after != "@"
    } else {
        false
    };
    *AT_MENTION_ACTIVE.state().write() = mention_active_now;
    if mention_active_now {
        if let Some(at_idx) = text.rfind('@') {
            *MENTION_PREFIX.state().write() = text[at_idx + 1..].to_string();
        }
    } else {
        MENTION_PREFIX.state().write().clear();
    }
}

/// 把文本按 \n 拆成多行 Line，光标以反转色高亮。
///
/// I22-B：渲染窗口上限——只渲染光标附近的 MAX_RENDER_LINES 行。
/// 原实现遍历整个 text 拆分并生成 Line，paste 大文本时每帧分配 O(n) Vec。
/// 现改为以光标行为中心，仅渲染 MAX_RENDER_LINES 行（与 editor_height 上限一致），
/// 渲染成本由 O(总行数) 降为 O(12)。光标始终落在渲染窗口内。
fn render_multiline_with_cursor(text: &str, cursor: usize, loading: bool) -> Vec<Line<'static>> {
    let cursor_style = Style::default()
        .fg(Color::Rgb(0, 0, 0))
        .bg(theme::TEXT)
        .add_modifier(Modifier::BOLD);

    // I22-B：渲染窗口上限。editor_height 最大 12，所以渲染 >12 行是浪费。
    const MAX_RENDER_LINES: usize = 12;

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

    // I22-B：计算渲染窗口 [start, end)，确保光标行包含在内。
    // 当总行数 <= MAX_RENDER_LINES 时展示全部；否则以光标行为中心构建窗口，
    // 并在 end 被末尾钳位后向上扩展 start，保证窗口始终占满 MAX_RENDER_LINES 行
    // （修复验证报告的"光标在末尾时窗口缩到 7 行"shrinkage bug）。
    let total_line_count = text.matches('\n').count() + 1;
    let (start, end) = if total_line_count <= MAX_RENDER_LINES {
        (0, total_line_count)
    } else {
        let half_window = MAX_RENDER_LINES / 2;
        let center_start = target_line.saturating_sub(half_window);
        let end = (center_start + MAX_RENDER_LINES).min(total_line_count);
        let start = end.saturating_sub(MAX_RENDER_LINES);
        (start, end)
    };

    let mut result: Vec<Line<'static>> = Vec::with_capacity(end - start);
    for (li, line) in text.split('\n').enumerate().skip(start).take(end - start) {
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
        *AT_MENTION_ACTIVE.state().write() = false;
        *SLASH_HINT_ACTIVE.state().write() = false;
        MENTION_PREFIX.state().write().clear();
        SLASH_PREFIX.state().write().clear();
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_slash_at_start() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        update_popup_prefix("/hel");
        assert!(!*AT_MENTION_ACTIVE.state().read());
        assert!(*SLASH_HINT_ACTIVE.state().read());
        assert_eq!(SLASH_PREFIX.state().read().as_str(), "hel");
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_slash_with_space_disables() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        update_popup_prefix("/help me");
        assert!(!*SLASH_HINT_ACTIVE.state().read());
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_mention_trigger() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        update_popup_prefix("see @auth");
        assert!(*AT_MENTION_ACTIVE.state().read());
        assert_eq!(MENTION_PREFIX.state().read().as_str(), "auth");
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_mention_with_space_disables() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        update_popup_prefix("see @auth service");
        assert!(!*AT_MENTION_ACTIVE.state().read());
    }

    /// C2 回归测试：filter_files_for_mention 在 prefix 为空时返回前 20 条。
    #[test]
    #[serial]
    fn test_filter_files_empty_prefix_returns_top_20() {
        crate::kit::atoms::init_atoms();
        // 写 25 个文件
        {
            let state = FILE_LIST.state();
            let mut list = state.write();
            *list = (0..25).map(|i| format!("file{i}.rs")).collect();
            list.sort();
        }
        let result = filter_files_for_mention("");
        assert_eq!(result.len(), 20);
    }

    /// C2 回归测试：filter_files_for_mention 按大小写不敏感子串过滤。
    #[test]
    #[serial]
    fn test_filter_files_substring_case_insensitive() {
        crate::kit::atoms::init_atoms();
        *FILE_LIST.state().write() = vec![
            "auth.rs".into(),
            "oauth.rs".into(),
            "OAUTH.md".into(),
            "utils.rs".into(),
        ];
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
        *FILE_LIST.state().write() = vec![
            "myauth.rs".into(), // 子串匹配
            "auth.rs".into(),   // 开头匹配，应优先
            "oauth.rs".into(),  // 子串匹配
        ];
        let result = filter_files_for_mention("auth");
        assert_eq!(result.first().unwrap(), "auth.rs");
    }
}
