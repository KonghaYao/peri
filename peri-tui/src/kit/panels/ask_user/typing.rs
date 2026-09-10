//! Text editing decisions for the active custom answer.

use super::form::FormState;
use peri_acp_types::event_data::AskUser;
use ratatui_kit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl FormState {
    pub(super) fn handle_typing_key(
        &mut self,
        key: &KeyEvent,
        pending: Option<&AskUser>,
        wrap_width: usize,
    ) -> bool {
        let st = &mut self.typing_state;
        match key.code {
            // Enter → 确认输入
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                let text = st.text.trim().to_string();
                if !text.is_empty() {
                    let q_idx = self.focused;
                    let ca = &mut self.custom_answers;
                    if q_idx >= ca.len() {
                        ca.resize(q_idx + 1, None);
                    }
                    ca[q_idx] = Some(text);
                }
                self.is_typing = false;
                true
            }
            // ESC → 取消输入
            KeyCode::Esc if key.modifiers == KeyModifiers::NONE => {
                self.is_typing = false;
                true
            }
            // Backspace
            KeyCode::Backspace if key.modifiers == KeyModifiers::NONE => {
                st.backspace();
                true
            }
            // Delete
            KeyCode::Delete if key.modifiers == KeyModifiers::NONE => {
                st.delete_forward();
                true
            }
            // Ctrl+W → 删词
            KeyCode::Char('w' | 'W') if key.modifiers == KeyModifiers::CONTROL => {
                st.delete_word_backward();
                true
            }
            // Ctrl+U → 清空行
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                st.clear();
                true
            }
            // Ctrl+A → 行首
            KeyCode::Char('a') if key.modifiers == KeyModifiers::CONTROL => {
                st.cursor_line_home();
                true
            }
            // Ctrl+E → 行尾
            KeyCode::Char('e') if key.modifiers == KeyModifiers::CONTROL => {
                st.cursor_line_end();
                true
            }
            // 左箭头
            KeyCode::Left if key.modifiers == KeyModifiers::NONE => {
                st.cursor_left();
                true
            }
            KeyCode::Left if key.modifiers == KeyModifiers::CONTROL => {
                st.cursor_word_left();
                true
            }
            // 右箭头
            KeyCode::Right if key.modifiers == KeyModifiers::NONE => {
                st.cursor_right();
                true
            }
            KeyCode::Right if key.modifiers == KeyModifiers::CONTROL => {
                st.cursor_word_right();
                true
            }
            // 上/下箭头：视觉行移动；到顶时回到选项列表
            KeyCode::Up if key.modifiers == KeyModifiers::NONE => {
                let moved = st.cursor_visual_up(wrap_width);
                if !moved {
                    // 已在最顶：退出 typing，回到预设选项
                    self.is_typing = false;
                    let q = pending
                        .as_ref()
                        .and_then(|au| au.questions.get(self.focused));
                    if let Some(q) = q {
                        self.focused_option = q.options.len().saturating_sub(1);
                    }
                }
                true
            }
            KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                let _ = st.cursor_visual_down(wrap_width);
                true
            }
            // Ctrl+Z → undo
            KeyCode::Char('z') if key.modifiers == KeyModifiers::CONTROL => {
                st.undo();
                true
            }
            // Ctrl+Shift+Z / Ctrl+Y → redo
            KeyCode::Char('Z') if key.modifiers == KeyModifiers::CONTROL => {
                st.redo();
                true
            }
            KeyCode::Char('y') if key.modifiers == KeyModifiers::CONTROL => {
                st.redo();
                true
            }
            // 可见字符插入
            KeyCode::Char(c)
                if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT =>
            {
                st.insert_char(c);
                true
            }
            _ => false,
        }
    }
}
