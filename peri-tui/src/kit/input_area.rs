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
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Borders, Paragraph},
    },
};
use std::sync::{Arc, Mutex};
use unicode_width::UnicodeWidthChar;

use parking_lot::RwLock;
use std::sync::OnceLock;

use crate::kit::atoms::PredictionState;
use crate::kit::atoms::ViewModelsSnapshot;
use crate::kit::atoms::{
    ACP_STATE, AT_MENTION_ACTIVE, AVAILABLE_SLASH_COMMANDS, FILE_LIST, INPUT_AREA_ESC_PREFIX,
    INPUT_BUFFER, MENTION_PREFIX, MENTION_SELECTED_INDEX, PREDICTION, RENDER_CACHE, SKILL_NAMES,
    SLASH_HINT_ACTIVE, SLASH_PREFIX, SLASH_SELECTED_INDEX, SUBMIT_TX, VIEW_MODELS,
};
use crate::kit::focus_router::input_accepts_key;
use crate::kit::input_history::{history_down, history_up, push_history, reset_history_cursor};
use crate::kit::mention_popup::MentionPopup;
use crate::kit::panel_registry::{PANELS, open_panel, panel_for_slash_command};
use crate::kit::render_bridge::{self, RenderedEntry, VmKey};
use crate::kit::slash_completion::{SlashActionKind, SlashCompletion, SlashCompletionItem};
use crate::kit::theme;
use peri_acp_types::view_model::{UserBubbleData, ViewModel, hash_str};

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

    /// 删除光标后的字符
    fn delete_forward(&mut self) {
        if self.cursor < self.len() {
            let byte_idx = Self::char_to_byte(&self.text, self.cursor);
            let next_byte = Self::char_to_byte(&self.text, self.cursor + 1);
            self.text.drain(byte_idx..next_byte);
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

    /// 删除光标后的一个词（Alt+Delete）
    fn delete_word_forward(&mut self) {
        let start_byte = Self::char_to_byte(&self.text, self.cursor);
        let mut pos = start_byte;

        while pos < self.text.len() {
            let ch = self.text[pos..]
                .chars()
                .next()
                .expect("pos must be char boundary");
            if !ch.is_whitespace() {
                break;
            }
            pos += ch.len_utf8();
        }
        while pos < self.text.len() {
            let ch = self.text[pos..]
                .chars()
                .next()
                .expect("pos must be char boundary");
            if ch.is_whitespace() {
                break;
            }
            pos += ch.len_utf8();
        }

        if pos > start_byte {
            self.text.drain(start_byte..pos);
        }
    }

    /// 当前光标所在的 (line_idx, col_idx)
    fn cursor_line_col(&self) -> (usize, usize) {
        Self::cursor_line_col_for(&self.text, self.cursor)
    }

    fn cursor_line_col_for(text: &str, cursor: usize) -> (usize, usize) {
        let mut chars_before_line = 0usize;
        for (line_idx, line) in text.split('\n').enumerate() {
            let line_chars = line.chars().count();
            if chars_before_line + line_chars >= cursor {
                return (line_idx, cursor - chars_before_line);
            }
            chars_before_line += line_chars + 1;
        }
        let last_line = text.split('\n').last().unwrap_or("");
        (text.matches('\n').count(), last_line.chars().count())
    }

    fn line_col_to_cursor(text: &str, target_line: usize, target_col: usize) -> usize {
        let mut cursor = 0usize;
        for (line_idx, line) in text.split('\n').enumerate() {
            let line_chars = line.chars().count();
            if line_idx == target_line {
                return cursor + target_col.min(line_chars);
            }
            cursor += line_chars + 1;
        }
        text.chars().count()
    }

    /// 上移一行，返回是否真的做了多行移动。
    fn cursor_line_up(&mut self) -> bool {
        let (line, col) = self.cursor_line_col();
        if line == 0 {
            return false;
        }
        self.cursor = Self::line_col_to_cursor(&self.text, line - 1, col);
        true
    }

    /// 下移一行，返回是否真的做了多行移动。
    fn cursor_line_down(&mut self) -> bool {
        let (line, col) = self.cursor_line_col();
        let last_line = self.text.matches('\n').count();
        if line >= last_line {
            return false;
        }
        self.cursor = Self::line_col_to_cursor(&self.text, line + 1, col);
        true
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// 把当前字符光标转换为字节偏移。
    fn cursor_byte(&self) -> usize {
        Self::char_to_byte(&self.text, self.cursor)
    }

    /// 当前行首。
    fn cursor_line_home(&mut self) {
        let (line, _) = self.cursor_line_col();
        self.cursor = Self::line_col_to_cursor(&self.text, line, 0);
    }

    /// 当前行尾。
    fn cursor_line_end(&mut self) {
        let (line, _) = self.cursor_line_col();
        let line_len = self
            .text
            .split('\n')
            .nth(line)
            .unwrap_or("")
            .chars()
            .count();
        self.cursor = Self::line_col_to_cursor(&self.text, line, line_len);
    }

    /// 替换字符区间，并把光标放在替换文本末尾。
    fn replace_char_range(&mut self, start: usize, end: usize, replacement: &str) {
        let start = start.min(self.len());
        let end = end.min(self.len()).max(start);
        let start_byte = Self::char_to_byte(&self.text, start);
        let end_byte = Self::char_to_byte(&self.text, end);
        self.text.replace_range(start_byte..end_byte, replacement);
        self.cursor = start + replacement.chars().count();
    }

    /// 返回当前完整文本的引用。
    fn all_text(&self) -> String {
        self.text.clone()
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

/// 计算字符串前 char_idx 个字符的显示列宽度（CJK 字符占 2 列）。
fn display_width_before(s: &str, char_idx: usize) -> usize {
    s.chars()
        .take(char_idx)
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

#[derive(Default, Props)]
pub struct InputAreaProps {
    pub loading: bool,
    pub hidden: bool,
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
                if !input_accepts_key(&key) {
                    return EventResult::Ignored;
                }
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
                        *PREDICTION.state().write() = PredictionState::default();
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
                        reset_mention_popup();
                        reset_slash_popup();
                        EventResult::Consumed
                    }
                    KeyCode::Char('a') if is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_line_home();
                        EventResult::Consumed
                    }
                    KeyCode::Char('e') if is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_line_end();
                        EventResult::Consumed
                    }
                    KeyCode::Char('b') if is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_left();
                        EventResult::Consumed
                    }
                    KeyCode::Char('f') if is_ctrl && !mention_active && !slash_active => {
                        state.write().cursor_right();
                        EventResult::Consumed
                    }
                    KeyCode::Char('h') if is_ctrl && !mention_active && !slash_active => {
                        let mut s = state.write();
                        s.backspace();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('d') if is_ctrl && !mention_active && !slash_active => {
                        let mut s = state.write();
                        s.delete_forward();
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Char('b')
                        if is_alt && !is_ctrl && !mention_active && !slash_active =>
                    {
                        state.write().cursor_word_left();
                        EventResult::Consumed
                    }
                    KeyCode::Char('f')
                        if is_alt && !is_ctrl && !mention_active && !slash_active =>
                    {
                        state.write().cursor_word_right();
                        EventResult::Consumed
                    }
                    KeyCode::Backspace if !mention_active && !slash_active => {
                        let mut s = state.write();
                        if is_alt {
                            s.delete_word_backward();
                        } else {
                            s.backspace();
                        }
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Backspace => {
                        let mut s = state.write();
                        if is_alt {
                            s.delete_word_backward();
                        } else {
                            s.backspace();
                        }
                        update_popup_prefix(&s);
                        EventResult::Consumed
                    }
                    KeyCode::Delete if !mention_active && !slash_active => {
                        let mut s = state.write();
                        if is_alt {
                            s.delete_word_forward();
                        } else {
                            s.delete_forward();
                        }
                        update_popup_prefix(&s);
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
                        tracing::info!(?key, "input area consumed up");
                        let moved = state.write().cursor_line_up();
                        if !moved {
                            let current = state.read().all_text();
                            if let Some(historical) = history_up(Some(&current)) {
                                state.write().replace_all(historical);
                            }
                        }
                        EventResult::Consumed
                    }
                    KeyCode::Down if !is_ctrl && !mention_active && !slash_active => {
                        tracing::info!(?key, "input area consumed down");
                        let moved = state.write().cursor_line_down();
                        if !moved && let Some(historical) = history_down() {
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
                        update_popup_prefix(&s);
                        *PREDICTION.state().write() = PredictionState::default();
                        EventResult::Consumed
                    }

                    // ── 预测文本接受（Tab）──
                    KeyCode::Tab => {
                        let pred = PREDICTION.state();
                        if !pred.read().text.is_empty() {
                            let text = pred.read().text.clone();
                            *pred.write() = PredictionState::default();
                            state.write().replace_all(text);
                            reset_mention_popup();
                            reset_slash_popup();
                            return EventResult::Consumed;
                        }
                        EventResult::Ignored
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
                // 部分终端（VSCode、iTerm2）在 Bracketed Paste 中使用 \r 作为
                // 换行分隔符；render_multiline_with_cursor 只按 \n 拆分行，
                // 未归一化的 \r 会导致换行在渲染时不可见。
                let normalized = paste_text.replace("\r\n", "\n").replace('\r', "\n");
                let char_count = normalized.chars().count();
                let truncated: String = normalized.chars().take(MAX_PASTE_CHARS).collect();
                if char_count > MAX_PASTE_CHARS {
                    tracing::warn!(
                        original_chars = char_count,
                        capped_at = MAX_PASTE_CHARS,
                        "InputArea: paste 截断——超出 10K char 上限"
                    );
                }
                let mut s = state.write();
                s.insert_str(&truncated);
                update_popup_prefix(&s);
                *PREDICTION.state().write() = PredictionState::default();
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        },
    );
    let editor = state.read().clone();
    let hidden = props.hidden;
    let text = editor.text.clone();
    let cursor = editor.cursor;
    let loading = props.loading;

    // 当前激活状态（驱动 popup 渲染）
    let mention_active = *AT_MENTION_ACTIVE.state().read();
    let slash_active = *SLASH_HINT_ACTIVE.state().read();
    // 只在 popup 激活时才读/克隆 prefix 和 items，避免每帧不必要的 atom 读 + 堆分配
    let mention_prefix = if mention_active {
        MENTION_PREFIX.state().read().clone()
    } else {
        String::new()
    };
    let slash_prefix = if slash_active {
        SLASH_PREFIX.state().read().clone()
    } else {
        String::new()
    };

    // 多行渲染——按 \n 拆分，每行作为独立 Line，光标高亮放在对应行
    let lines = render_multiline_with_cursor(&text, cursor, loading);

    // 计算 composer 本体高度；popup 额外占位，避免被输入区自身裁切。
    let line_count = text.matches('\n').count() + 1;
    let editor_rows = (line_count as u16).clamp(1, 10);
    let composer_height = editor_rows + 2;

    // 只在 slash 激活时克隆整个 item 列表——非激活态跳过 50+ item × 3 String 的堆分配
    let slash_items = if slash_active {
        get_cached_slash_items()
    } else {
        Vec::new()
    };
    let mention_select_state = state.clone();
    let slash_select_state = state.clone();

    let slash_popup_height = if slash_active {
        popup_height(slash_items.len())
    } else {
        0
    };
    // 只在 mention 激活时读 FILE_LIST + 过滤——非激活态跳过 200+ 文件名的 atom 读和分配
    let mention_items = if mention_active {
        filter_files_for_mention(&mention_prefix)
    } else {
        Vec::new()
    };
    let mention_popup_height = if mention_active {
        popup_height(mention_items.len())
    } else {
        0
    };
    let overlay_height = slash_popup_height.max(mention_popup_height);
    // 预测文本（buffer 为空且 prediction 非空时显示为灰色占位符）
    let pred_text = hooks.use_atom(&PREDICTION).read().text.clone();
    let show_prediction = !hidden && !pred_text.is_empty() && text.is_empty();
    let total_height = if hidden {
        0
    } else {
        composer_height + overlay_height + if show_prediction { 1 } else { 0 }
    };

    let composer_lines = build_composer_lines(lines, loading);

    // 显式背景色：防止 Paragraph 文本缩短时旧内容残留（ghosting）。
    // 未设背景时 ratatui 仅渲染文本 span，超出新文本的列保留终端原有像素。
    let composer_paragraph = Paragraph::new(composer_lines)
        .block(build_composer_block(loading))
        .style(Style::default().bg(theme::semantic().surface.default));

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(total_height),
        ) {
            { if !hidden && slash_active {
                element!(SlashCompletion(
                    prefix: slash_prefix.clone(),
                    items: slash_items.clone(),
                    on_select: Arc::new(Mutex::new(Handler::from(move |item: SlashCompletionItem| {
                        match item.kind {
                            SlashActionKind::Panel => {
                                if let Some(kind) = panel_for_slash_command(&item.insert_text) {
                                    // 清空输入框再打开面板
                                    let mut editor = slash_select_state.write();
                                    editor.text.clear();
                                    editor.cursor = 0;
                                    open_panel(kind);
                                }
                            }
                            SlashActionKind::Command | SlashActionKind::Skill => {
                                // S16：command/skill 先检查是否映射到面板（如 /history → ThreadBrowser）
                                if let Some(kind) = panel_for_slash_command(&item.insert_text) {
                                    // 清空输入框再打开面板
                                    let mut editor = slash_select_state.write();
                                    editor.text.clear();
                                    editor.cursor = 0;
                                    open_panel(kind);
                                } else {
                                    let mut editor = slash_select_state.write();
                                    apply_slash_selection(&mut editor, &item.insert_text);
                                }
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
            { if !hidden && mention_active {
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
            { if !hidden {
                element!(
                    View(
                        flex_direction: Direction::Vertical,
                        width: Constraint::Fill(1),
                        height: Constraint::Length(composer_height),
                    ) {
                        Text(text: composer_paragraph)
                    }
                ).into_any()
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0))).into_any()
            } }
            { if show_prediction {
                let pred_line = Line::from(Span::styled(
                    format!("  {}", pred_text),
                    Style::default().fg(theme::component().statusbar.muted),
                ));
                element!(
                    View(width: Constraint::Fill(1), height: Constraint::Length(1)) {
                        Text(text: Paragraph::new(pred_line))
                    }
                ).into_any()
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0))).into_any()
            } }
        }
    )
}

fn input_tokens() -> &'static theme::InputTokens {
    &theme::component().input
}

fn build_composer_block(loading: bool) -> Block<'static> {
    let tokens = input_tokens();
    let border_color = if loading {
        tokens.border_loading
    } else {
        tokens.border
    };

    Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color))
}

fn build_composer_lines(editor_lines: Vec<Line<'static>>, loading: bool) -> Vec<Line<'static>> {
    let tokens = input_tokens();
    let mut lines = Vec::with_capacity(editor_lines.len().max(1));
    let prompt_style = Style::default()
        .fg(if loading {
            tokens.prompt_loading
        } else {
            tokens.prompt
        })
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
            spans.push(Span::styled(
                "   ",
                Style::default().fg(tokens.continuation),
            ));
        }
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }

    lines
}

fn popup_height(item_count: usize) -> u16 {
    (item_count.max(1) as u16 + 2).min(theme::component().popup.inline_height)
}

fn submit_text(submitted: String) {
    if submitted.trim().is_empty() {
        return;
    }

    push_history(&submitted);
    reset_history_cursor();
    let trimmed = submitted.trim().to_string();
    let is_loading = ACP_STATE.state().read().is_loading;
    if is_loading {
        append_local_user_bubble(&trimmed);
        let input_buffer = INPUT_BUFFER.state();
        let mut guard = input_buffer.write();
        guard.push_back(trimmed);
        while guard.len() > 32 {
            guard.pop_front();
        }
    } else if let Some(tx) = SUBMIT_TX.get() {
        append_local_user_bubble(&trimmed);
        let _ = tx.send(trimmed);

        // S16：提交后立即设为 loading，避免按键到首条流式事件间的空白窗口期。
        let acp = ACP_STATE.state();
        let mut guard = acp.write();
        guard.is_loading = true;
    }
}

fn append_local_user_bubble(text: &str) {
    let user_vm = ViewModel::UserBubble(UserBubbleData {
        text: text.to_string(),
        content_hash: hash_str(text),
        is_system_reminder: false,
    });
    let snapshot = {
        let vms = VIEW_MODELS.state();
        let snapshot = vms.read();
        let mut combined: Vec<ViewModel> = Vec::with_capacity(snapshot.committed.len() + 1);
        combined.extend(snapshot.committed.iter().cloned());
        combined.push(user_vm.clone());
        let new_snapshot = ViewModelsSnapshot {
            committed: Arc::from(combined),
            current_turn: Arc::clone(&snapshot.current_turn),
        };
        drop(snapshot);
        *vms.write() = new_snapshot.clone();
        new_snapshot
    };
    sync_render_cache(&snapshot);
}

fn sync_render_cache(snapshot: &ViewModelsSnapshot) {
    let width = 80;
    let mut cache = RENDER_CACHE.state().read().clone();

    // 增量模式：只追加最新一条 UserBubble，避免全量 markdown 重解析
    if !cache.entries.is_empty() {
        if let Some(last_vm) = snapshot.committed.last() {
            let new_index = snapshot.committed.len().saturating_sub(1);
            let key = VmKey::Committed(new_index);
            let lines = crate::kit::view_render::render_v2_vm(last_vm, width, false);
            let height = render_bridge::visual_height(&lines, width);
            cache.entries.push((
                key,
                RenderedEntry {
                    height,
                    lines: Arc::from(lines),
                },
            ));
        }
    } else {
        // Fallback：cache 为空时走全量构建
        let mut entries =
            Vec::with_capacity(snapshot.committed.len() + snapshot.current_turn.len());
        append_render_entries(&mut entries, &snapshot.committed, width, 0, true);
        append_render_entries(&mut entries, &snapshot.current_turn, width, 0, false);
        cache.entries = entries;
    }

    // 重建 cumulative_heights
    cache.cumulative_heights.clear();
    let mut sum = 0usize;
    for (_, entry) in &cache.entries {
        sum = sum.saturating_add(entry.height);
        cache.cumulative_heights.push(sum);
    }
    let all_lines: Vec<ratatui::text::Line<'static>> = cache
        .entries
        .iter()
        .flat_map(|(_, entry)| entry.lines.iter())
        .cloned()
        .collect();
    cache.wrap_map = render_bridge::build_wrap_map(&all_lines, width as u16);
    *RENDER_CACHE.state().write() = cache;
}

fn append_render_entries(
    entries: &mut Vec<(VmKey, RenderedEntry)>,
    items: &[ViewModel],
    width: usize,
    start_index: usize,
    committed: bool,
) {
    for (offset, vm) in items.iter().enumerate() {
        let key = if committed {
            VmKey::Committed(start_index + offset)
        } else {
            VmKey::CurrentTurn(offset)
        };
        let lines = crate::kit::view_render::render_v2_vm(vm, width, false);
        let height = render_bridge::visual_height(&lines, width);
        entries.push((
            key,
            RenderedEntry {
                height,
                lines: Arc::from(lines),
            },
        ));
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
    let replacement = format!("/{cmd} ");
    if let Some((_, token_start_byte)) = detect_slash_token(&state.text, state.cursor_byte()) {
        let token_start = state.text[..token_start_byte].chars().count();
        let token_end = state.cursor;
        state.replace_char_range(token_start, token_end, &replacement);
    } else {
        state.replace_all(replacement);
    }
}

fn build_slash_items() -> Vec<SlashCompletionItem> {
    let remote = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    let skill_names: std::collections::HashSet<String> =
        SKILL_NAMES.state().read().iter().cloned().collect();
    let mut items = Vec::with_capacity(PANELS.len() + remote.len());
    for panel in PANELS {
        let slash_name = panel.slash_command.to_string();
        items.push(SlashCompletionItem {
            label_lowercase: slash_name.to_lowercase(),
            label: slash_name.clone(),
            insert_text: slash_name,
            description: panel.description.to_string(),
            kind: SlashActionKind::Panel,
        });
    }
    for (name, description) in &remote {
        // S16：根据 SKILL_NAMES 区分 Skill vs Command
        let kind = if skill_names.contains(name) {
            SlashActionKind::Skill
        } else {
            SlashActionKind::Command
        };
        items.push(SlashCompletionItem {
            label: name.clone(),
            insert_text: name.clone(),
            description: description.clone(),
            kind,
            label_lowercase: name.to_lowercase(),
        });
    }
    // 字母序排序——只排一次，组件端不再重排
    items.sort_by(|a, b| a.label_lowercase.cmp(&b.label_lowercase));
    items
}

/// 缓存 `build_slash_items()` 的结果，仅在 ACP 推送新命令时刷新。
static SLASH_ITEMS_CACHE: OnceLock<RwLock<Vec<SlashCompletionItem>>> = OnceLock::new();

fn slash_items_cache() -> &'static RwLock<Vec<SlashCompletionItem>> {
    SLASH_ITEMS_CACHE.get_or_init(|| RwLock::new(build_slash_items()))
}

/// 刷新斜杠命令缓存——由 acp_notifier 在收到新命令后调用。
pub(crate) fn refresh_slash_items() {
    *slash_items_cache().write() = build_slash_items();
}

fn get_cached_slash_items() -> Vec<SlashCompletionItem> {
    slash_items_cache().read().clone()
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

/// 根据 editor 当前文本和光标更新 @mention / slash 提示状态。
///
/// - `/` token：参考 peri-main，向光标前回溯最近的 `/`，要求 `/` 前为空白或行首。
/// - `@` 在最近词中：开启 @mention，prefix = @ 之后的字符。
fn update_popup_prefix(state: &EditorState) {
    let cursor_byte = state.cursor_byte();
    if let Some((prefix, _)) = detect_slash_token(&state.text, cursor_byte) {
        *SLASH_HINT_ACTIVE.state().write() = true;
        *SLASH_PREFIX.state().write() = prefix;
    } else {
        *SLASH_HINT_ACTIVE.state().write() = false;
        SLASH_PREFIX.state().write().clear();
    }

    let before_cursor = &state.text[..cursor_byte];
    let mention_active_now = if let Some(at_idx) = before_cursor.rfind('@') {
        let after = &before_cursor[at_idx + 1..];
        !after.is_empty() && !after.contains(char::is_whitespace) && after != "@"
    } else {
        false
    };
    *AT_MENTION_ACTIVE.state().write() = mention_active_now;
    if mention_active_now {
        if let Some(at_idx) = before_cursor.rfind('@') {
            *MENTION_PREFIX.state().write() = before_cursor[at_idx + 1..].to_string();
        }
    } else {
        MENTION_PREFIX.state().write().clear();
    }
}

/// 在 `text[..cursor_byte]` 中检测光标前最近的 `/` token。
fn detect_slash_token(text: &str, cursor_byte: usize) -> Option<(String, usize)> {
    if cursor_byte == 0 || cursor_byte > text.len() || !text.is_char_boundary(cursor_byte) {
        return None;
    }
    let before_cursor = &text[..cursor_byte];
    let slash_pos = before_cursor.rfind('/')?;
    let after_slash = &before_cursor[slash_pos + '/'.len_utf8()..];

    if slash_pos > 0 {
        let char_before = before_cursor[..slash_pos].chars().next_back()?;
        if !char_before.is_whitespace() {
            return None;
        }
    }

    if !after_slash.is_empty()
        && !after_slash
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.')
    {
        return None;
    }

    Some((after_slash.to_string(), slash_pos))
}

/// 把文本按 \n 拆成多行 Line，光标以反转色高亮。
///
/// I22-B：渲染窗口上限——只渲染光标附近的 MAX_RENDER_LINES 行。
/// 原实现遍历整个 text 拆分并生成 Line，paste 大文本时每帧分配 O(n) Vec。
/// 现改为以光标行为中心，仅渲染 MAX_RENDER_LINES 行（与 editor_height 上限一致），
/// 渲染成本由 O(总行数) 降为 O(12)。光标始终落在渲染窗口内。
fn render_multiline_with_cursor(text: &str, cursor: usize, loading: bool) -> Vec<Line<'static>> {
    let tokens = input_tokens();
    let cursor_style = Style::default()
        .fg(tokens.cursor_fg)
        .bg(tokens.cursor_bg)
        .add_modifier(Modifier::BOLD);

    // I22-B：渲染窗口上限。editor_height 最大 12，所以渲染 >12 行是浪费。
    const MAX_RENDER_LINES: usize = 12;

    if text.is_empty() {
        return vec![if loading {
            Line::from("")
        } else {
            // 空态光标：styled space 与行尾光标保持一致，避免 ▓ 块与终端默认光标重叠产生"双光标"
            Line::from(vec![Span::styled(" ", cursor_style)])
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
            // 用 unicode-width 计算光标所在显示列，确保 CJK 双宽字符定位正确。
            // 与 text_selection.rs:visual_col_to_byte_offset 同策略。
            let visual_col = display_width_before(line, target_col);
            let mut col = 0usize;
            let mut cut_byte = 0usize;
            for (i, ch) in line.char_indices() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if col + cw > visual_col {
                    break;
                }
                col += cw;
                cut_byte = i + ch.len_utf8();
            }
            let mut spans: Vec<Span<'static>> = Vec::new();
            if cut_byte > 0 {
                spans.push(Span::raw(line[..cut_byte].to_string()));
            }
            if cut_byte < line.len() {
                // 反色高亮光标所在字符（用户期望的"字反色"行为）
                let next_end = line[cut_byte..]
                    .chars()
                    .next()
                    .map(|c| cut_byte + c.len_utf8())
                    .unwrap_or(line.len());
                spans.push(Span::styled(
                    line[cut_byte..next_end].to_string(),
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
    fn test_editor_delete_forward_removes_char_after_cursor() {
        let mut s = EditorState {
            text: "ab你c".into(),
            cursor: 2,
        };
        s.delete_forward();
        assert_eq!(s.text, "abc");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn test_editor_delete_word_forward_removes_next_word() {
        let mut s = EditorState {
            text: "hello   world next".into(),
            cursor: 5,
        };
        s.delete_word_forward();
        assert_eq!(s.text, "hello next");
        assert_eq!(s.cursor, 5);
    }

    #[test]
    fn test_editor_cursor_word_left_and_right() {
        let mut s = EditorState {
            text: "hello world next".into(),
            cursor: 0,
        };
        s.cursor_word_right();
        assert_eq!(s.cursor, 6);
        s.cursor_word_right();
        assert_eq!(s.cursor, 12);
        s.cursor_word_left();
        assert_eq!(s.cursor, 6);
    }

    #[test]
    fn test_editor_cursor_line_up_and_down() {
        let mut s = EditorState {
            text: "abc\nd\nefgh".into(),
            cursor: 2,
        };
        assert!(!s.cursor_line_up());
        assert!(s.cursor_line_down());
        assert_eq!(s.cursor, 5);
        assert!(s.cursor_line_down());
        assert_eq!(s.cursor, 7);
        assert!(s.cursor_line_up());
        assert_eq!(s.cursor, 5);
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
    fn test_editor_cursor_line_home_and_end() {
        let mut s = EditorState {
            text: "abc\nde你f".into(),
            cursor: 6,
        };
        s.cursor_line_home();
        assert_eq!(s.cursor, 4);
        s.cursor_line_end();
        assert_eq!(s.cursor, 8);
    }

    #[test]
    fn test_apply_slash_selection_replaces_only_current_token() {
        let mut s = EditorState::default();
        s.insert_str("run /hel after");
        s.cursor = 8;
        apply_slash_selection(&mut s, "help");
        assert_eq!(s.text, "run /help  after");
        assert_eq!(s.cursor, 10);
    }

    #[test]
    fn test_apply_slash_selection_preserves_cjk_before_token() {
        let mut s = EditorState::default();
        s.insert_str("你好 /he 后面");
        s.cursor = 6;
        apply_slash_selection(&mut s, "help");
        assert_eq!(s.text, "你好 /help  后面");
        assert_eq!(s.cursor, 9);
    }

    #[test]
    fn test_detect_slash_token_rejects_path_or_comment() {
        assert!(detect_slash_token("src/foo", 7).is_none());
        assert!(detect_slash_token("//", 2).is_none());
    }

    #[test]
    fn test_detect_slash_token_accepts_line_start() {
        assert_eq!(
            detect_slash_token("hello\n/com", 10),
            Some(("com".to_string(), 6))
        );
    }

    #[test]
    fn test_render_multiline_empty_shows_cursor() {
        let lines = render_multiline_with_cursor("", 0, false);
        assert_eq!(lines.len(), 1);
        // 空态：单行，光标为反色 space（字符反色风格）
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, " ");
    }

    #[test]
    fn test_render_multiline_empty_loading_shows_blank() {
        let lines = render_multiline_with_cursor("", 0, true);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans.is_empty() || lines[0].spans.iter().all(|s| s.content.is_empty()));
    }

    #[test]
    fn test_render_multiline_cjk_cursor_mid_line() {
        // "你好世界" (4 CJK chars, 8 display cols), cursor 在位置 2（"好"之后即"世"上）
        let lines = render_multiline_with_cursor("你好世界", 2, false);
        assert_eq!(lines.len(), 1);
        // spans: [Span("你好"), Span("世", cursor_style), Span("界")]
        assert_eq!(lines[0].spans.len(), 3);
        assert_eq!(lines[0].spans[0].content, "你好");
        // 光标字符应为反色高亮
        assert!(
            !lines[0].spans[1].style.bg.is_none() || !lines[0].spans[1].style.fg.is_none(),
            "cursor span should have non-default style (reversed fg/bg)"
        );
        assert_eq!(lines[0].spans[2].content, "界");
    }

    #[test]
    fn test_render_multiline_cjk_cursor_at_start() {
        // "你好", cursor 在位置 0（"你"上）
        let lines = render_multiline_with_cursor("你好", 0, false);
        assert_eq!(lines.len(), 1);
        // spans: [Span("你", cursor_style), Span("好")]
        assert_eq!(lines[0].spans.len(), 2);
        assert!(
            !lines[0].spans[0].style.bg.is_none(),
            "cursor span should have background (reversed)"
        );
        assert_eq!(lines[0].spans[1].content, "好");
    }

    #[test]
    fn test_render_multiline_cjk_cursor_at_end() {
        // "你好", cursor 在位置 2（末尾）
        let lines = render_multiline_with_cursor("你好", 2, false);
        assert_eq!(lines.len(), 1);
        // spans: [Span("你好"), Span(" ", cursor_style)]
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].content, "你好");
        assert_eq!(lines[0].spans[1].content, " ");
    }

    #[test]
    fn test_render_multiline_cjk_cursor_second_line() {
        // "abc\n你好", cursor 在位置 5（第二行"好"上）
        let lines = render_multiline_with_cursor("abc\n你好", 5, false);
        assert_eq!(lines.len(), 2);
        // 第一行无光标
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content, "abc");
        // 第二行：["你", Span("好", cursor_style)]
        assert_eq!(lines[1].spans.len(), 2);
        assert_eq!(lines[1].spans[0].content, "你");
        assert!(
            !lines[1].spans[1].style.bg.is_none(),
            "cursor span should have background"
        );
    }

    #[test]
    fn test_display_width_before_cjk() {
        assert_eq!(display_width_before("abc", 2), 2);
        assert_eq!(display_width_before("abc", 0), 0);
        assert_eq!(display_width_before("你好世界", 2), 4); // 2 CJK chars = 4 cols
        assert_eq!(display_width_before("你好世界", 1), 2);
        assert_eq!(display_width_before("你好", 3), 4); // 超出 char 数返回全宽
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
    fn test_update_popup_prefix_slash_token_at_cursor() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        let mut s = EditorState::default();
        s.insert_str("say /hel");
        update_popup_prefix(&s);
        assert!(!*AT_MENTION_ACTIVE.state().read());
        assert!(*SLASH_HINT_ACTIVE.state().read());
        assert_eq!(SLASH_PREFIX.state().read().as_str(), "hel");
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_slash_with_space_disables_after_token() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        let mut s = EditorState::default();
        s.insert_str("/help me");
        update_popup_prefix(&s);
        assert!(!*SLASH_HINT_ACTIVE.state().read());
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_mention_trigger() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        let mut s = EditorState::default();
        s.insert_str("see @auth");
        update_popup_prefix(&s);
        assert!(*AT_MENTION_ACTIVE.state().read());
        assert_eq!(MENTION_PREFIX.state().read().as_str(), "auth");
    }

    #[test]
    #[serial]
    fn test_update_popup_prefix_mention_with_space_disables() {
        crate::kit::atoms::init_atoms();
        reset_popup_atoms();
        let mut s = EditorState::default();
        s.insert_str("see @auth service");
        update_popup_prefix(&s);
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
