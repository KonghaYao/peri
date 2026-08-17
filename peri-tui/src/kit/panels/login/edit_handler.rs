use crate::kit::atoms::PERI_CONFIG_HANDLE;
use ratatui_kit::crossterm::event::{KeyCode, KeyModifiers};

use super::config_store::save_login_edit;
use super::{LoginEditField, LoginEditState, LoginPanelMode};

// ── Edit 模式辅助函数 ─────────────────────────────────────────────────────────

/// 从 PERI_CONFIG_HANDLE 读取完整 provider 配置并初始化编辑状态。
pub(super) fn enter_login_edit_mode(
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginEditField,
    edit_cursor: &mut usize,
    provider_id: &str,
) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let cfg = handle.read();
    if let Some(config) = cfg.config.providers.iter().find(|p| p.id == provider_id) {
        *edit_state = Some(LoginEditState::from_provider_config(config));
        *edit_focus = LoginEditField::ProviderType;
        *edit_cursor = 0;
    }
    drop(cfg);
}

/// 编辑模式下的按键处理。
///
/// 文本编辑（字符、退格、删除、光标移动、Ctrl+W）、字段导航（↑/↓）、
/// 确认按钮 Enter 保存、Esc 放弃、Ctrl+S 快捷保存。
pub(super) fn handle_login_edit_keys(
    mode: &mut LoginPanelMode,
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginEditField,
    edit_cursor: &mut usize,
    key: &ratatui_kit::crossterm::event::KeyEvent,
) {
    let Some(es) = edit_state else {
        *mode = LoginPanelMode::Browse;
        return;
    };

    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // 先处理文本编辑按键（所有字段共用）
    let text_handled = handle_login_text_input(es, *edit_focus, edit_cursor, key);
    if text_handled {
        return;
    }

    // ProviderType toggle（Left/Right/Space 切换，参考 setup_wizard 模式）
    if *edit_focus == LoginEditField::ProviderType {
        match key.code {
            KeyCode::Left | KeyCode::Right if !is_ctrl => {
                es.provider_type = match es.provider_type.as_str() {
                    "anthropic" => "openai".to_string(),
                    _ => "anthropic".to_string(),
                };
                return;
            }
            KeyCode::Char(' ') if !is_ctrl => {
                es.provider_type = match es.provider_type.as_str() {
                    "anthropic" => "openai".to_string(),
                    _ => "anthropic".to_string(),
                };
                return;
            }
            _ => {}
        }
    }

    // 导航 / 确认 / 放弃
    match key.code {
        KeyCode::Up if !is_ctrl => {
            *edit_focus = edit_focus.prev();
            *edit_cursor = if *edit_focus == LoginEditField::ProviderType {
                0
            } else {
                es.field_value(*edit_focus).chars().count()
            };
        }
        KeyCode::Down if !is_ctrl => {
            *edit_focus = edit_focus.next();
            *edit_cursor = if *edit_focus == LoginEditField::ProviderType {
                0
            } else {
                es.field_value(*edit_focus).chars().count()
            };
        }
        KeyCode::Enter => {
            // Enter：聚焦在确认按钮时保存（校验通过才回到 Browse，参考 setup_wizard）
            if *edit_focus == LoginEditField::Confirm && save_login_edit(es) {
                *mode = LoginPanelMode::Browse;
                *edit_state = None;
            }
        }
        KeyCode::Esc => {
            // 放弃编辑，回到 Browse
            *mode = LoginPanelMode::Browse;
            *edit_state = None;
        }
        KeyCode::Char('s') if is_ctrl && save_login_edit(es) => {
            // Ctrl+S 保存（保留快捷键，校验通过才回到 Browse）
            *mode = LoginPanelMode::Browse;
            *edit_state = None;
        }
        _ => {}
    }
}

/// 编辑字段的文本输入处理（与 setup_wizard 的 handle_text_input 模式一致）
fn handle_login_text_input(
    state: &mut LoginEditState,
    field: LoginEditField,
    edit_cursor: &mut usize,
    key: &ratatui_kit::crossterm::event::KeyEvent,
) -> bool {
    // ProviderType 是 toggle、Confirm 是按钮，均不接受文本输入
    if field == LoginEditField::ProviderType || field == LoginEditField::Confirm {
        return false;
    }

    use KeyCode::*;
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    let val = state.field_value_mut(field);
    let chars: Vec<char> = val.chars().collect();

    match key.code {
        Char(ch) if !is_ctrl => {
            let pos = (*edit_cursor).min(chars.len());
            let prefix: String = chars[..pos].iter().collect();
            let suffix: String = chars[pos..].iter().collect();
            *val = format!("{}{}{}", prefix, ch, suffix);
            *edit_cursor = pos + 1;
            true
        }
        Backspace if !is_ctrl => {
            if *edit_cursor > 0 && *edit_cursor <= chars.len() {
                let prefix: String = chars[..*edit_cursor - 1].iter().collect();
                let suffix: String = chars[*edit_cursor..].iter().collect();
                *val = format!("{}{}", prefix, suffix);
                *edit_cursor -= 1;
            } else if *edit_cursor > chars.len() && !chars.is_empty() {
                let prefix: String = chars[..chars.len() - 1].iter().collect();
                *val = prefix;
                *edit_cursor = chars.len() - 1;
            }
            true
        }
        Delete => {
            if *edit_cursor < chars.len() {
                let prefix: String = chars[..*edit_cursor].iter().collect();
                let suffix: String = chars[*edit_cursor + 1..].iter().collect();
                *val = format!("{}{}", prefix, suffix);
            }
            true
        }
        Left if !is_ctrl => {
            if *edit_cursor > 0 {
                *edit_cursor -= 1;
            }
            true
        }
        Right if !is_ctrl => {
            let max_pos = chars.len();
            if *edit_cursor < max_pos {
                *edit_cursor += 1;
            }
            true
        }
        Home if !is_ctrl => {
            *edit_cursor = 0;
            true
        }
        End if !is_ctrl => {
            *edit_cursor = chars.len();
            true
        }
        Char('w') if is_ctrl => {
            // Ctrl+W: 删除前一个词
            let pos = (*edit_cursor).min(chars.len());
            if pos == 0 {
                return true;
            }
            let mut end = pos;
            while end > 0 && chars[end - 1].is_whitespace() {
                end -= 1;
            }
            while end > 0 && !chars[end - 1].is_whitespace() {
                end -= 1;
            }
            let prefix: String = chars[..end].iter().collect();
            let suffix: String = chars[pos..].iter().collect();
            *edit_cursor = end;
            *val = format!("{}{}", prefix, suffix);
            true
        }
        _ => false,
    }
}

/// 粘贴到当前编辑字段的光标位置。
pub(super) fn handle_login_paste(
    edit_cursor: &mut usize,
    state: &mut LoginEditState,
    field: LoginEditField,
    paste_text: &str,
) {
    const MAX_PASTE_CHARS: usize = 10_000;
    let normalized = paste_text.replace("\r\n", "\n").replace('\r', "\n");
    let truncated: String = normalized.chars().take(MAX_PASTE_CHARS).collect();
    if normalized.chars().count() != truncated.chars().count() {
        tracing::warn!(
            "login panel: paste truncated from {} to {MAX_PASTE_CHARS} chars (CJK-safe)",
            normalized.chars().count()
        );
    }

    let val = state.field_value_mut(field);
    let chars: Vec<char> = val.chars().collect();
    let pos = (*edit_cursor).min(chars.len());
    let prefix: String = chars[..pos].iter().collect();
    let suffix: String = chars[pos..].iter().collect();
    let paste_len = truncated.chars().count();
    *val = format!("{}{}{}", prefix, truncated, suffix);
    *edit_cursor = pos + paste_len;
}
