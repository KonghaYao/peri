//! 输入区域 #[component] 组件。
//!
//! 使用 String 存储文本状态 + 手动处理键盘事件（tui_textarea 非 Send+Sync，
//! 无法放入 ratatui-kit use_state）。渲染时用 Paragraph 显示文本 + 光标指示符。

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

use crate::ui::theme;

/// 输入状态
#[derive(Clone, Default)]
struct EditorState {
    text: String,
    /// 字节偏移量（保证在字符边界上）
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

    /// 删除光标处的字符
    fn delete(&mut self) {
        if self.cursor < self.len() {
            let start = Self::char_to_byte(&self.text, self.cursor);
            let end = Self::char_to_byte(&self.text, self.cursor + 1);
            self.text.drain(start..end);
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

    /// 光标到行首
    fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    /// 光标到行尾
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
    // 创建单一编辑状态（闭包编辑 + 渲染读取共享同一实例）
    let state = hooks.use_state(EditorState::default);

    hooks.use_local_events({
        let state = state;
        move |event: Event| match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let is_shift = key.modifiers.contains(KeyModifiers::SHIFT);
                let is_alt = key.modifiers.contains(KeyModifiers::ALT);

                match key.code {
                    // ── 提交 ──
                    KeyCode::Enter if !is_shift && !is_alt => {
                        let mut s = state.write();
                        let submitted = s.text.clone();
                        if !submitted.trim().is_empty() {
                            // 写入提交 Atom（connect Phase 8 with ACP）
                            if let Some(pending) = crate::kit::atoms::SUBMIT_PENDING.get() {
                                *pending.write() = true;
                            }
                            if let Some(submit_text) = crate::kit::atoms::SUBMIT_TEXT.get() {
                                *submit_text.write() = submitted;
                            }
                            s.clear();
                        }
                    }

                    // Shift/Alt+Enter: 换行
                    KeyCode::Enter => {
                        state.write().insert_char('\n');
                    }

                    // ── 编辑快捷键 ──
                    KeyCode::Char('w') if is_ctrl => {
                        state.write().delete_word_backward();
                    }
                    KeyCode::Char('u') if is_ctrl => {
                        state.write().clear();
                    }
                    KeyCode::Backspace => {
                        state.write().backspace();
                    }
                    KeyCode::Delete => {
                        state.write().delete();
                    }
                    KeyCode::Left => {
                        state.write().cursor_left();
                    }
                    KeyCode::Right => {
                        state.write().cursor_right();
                    }
                    KeyCode::Up => {
                        // Phase 9: history up
                    }
                    KeyCode::Down => {
                        // Phase 9: history down
                    }
                    KeyCode::Home => {
                        state.write().cursor_home();
                    }
                    KeyCode::End => {
                        state.write().cursor_end();
                    }
                    KeyCode::Esc => {
                        state.write().clear();
                    }

                    // ── 字符输入 ──
                    KeyCode::Char(ch) if !is_ctrl && !is_alt => {
                        state.write().insert_char(ch);
                    }

                    _ => {}
                }
            }
            Event::Paste(paste_text) => {
                state.write().insert_str(&paste_text);
            }
            _ => {}
        }
    });

    let editor = state.read().clone();
    let text = editor.text.clone();
    let cursor = editor.cursor;
    let loading = props.loading;

    // 构建带光标指示的渲染文本
    let cursor_style = Style::default()
        .fg(Color::Rgb(0, 0, 0))
        .bg(theme::TEXT)
        .add_modifier(Modifier::BOLD);

    let line = if text.is_empty() {
        if !loading {
            // 空输入框：显示闪烁的光标
            Line::from(vec![Span::styled("\u{258C}", cursor_style)])
        } else {
            Line::from("")
        }
    } else {
        let mut cursor_byte = EditorState::char_to_byte(&text, cursor);
        if cursor_byte > text.len() {
            cursor_byte = text.len();
        }

        let mut spans = Vec::new();
        if cursor_byte > 0 {
            spans.push(Span::raw(text[..cursor_byte].to_string()));
        }
        // 光标指示：用反转样式高亮当前字符，或在末尾插入块状光标
        if cursor_byte < text.len() {
            let next_char_end = text[cursor_byte..]
                .chars()
                .next()
                .map(|c| cursor_byte + c.len_utf8())
                .unwrap_or(text.len());
            spans.push(Span::styled(
                text[cursor_byte..next_char_end].to_string(),
                cursor_style,
            ));
            if next_char_end < text.len() {
                spans.push(Span::raw(text[next_char_end..].to_string()));
            }
        } else {
            // 光标在文本末尾
            spans.push(Span::styled(" ", cursor_style));
        }
        Line::from(spans)
    };

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(3),
        ) {
            Text(text: Paragraph::new(line).block(
                if loading {
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(ratatui::style::Style::new().fg(theme::MUTED))
                } else {
                    Block::default().borders(Borders::TOP)
                }
            ))
        }
    )
}
