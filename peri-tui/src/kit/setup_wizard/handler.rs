use crate::app::setup_wizard::*;
use crate::i18n;
use crate::kit::atoms::{self, SETUP_WIZARD};
use crate::kit::panel_mouse::{ListLayout, hit_item, hit_row};
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui_kit::prelude::EventResult;

fn get_raw_field_value(state: &SetupWizardState) -> String {
    let mp = match state.active_provider_ref() {
        Some(mp) => mp,
        None => return String::new(),
    };
    match state.form_focus {
        FormField::ProviderId => mp.provider_id.clone(),
        FormField::BaseUrl => mp.base_url.clone(),
        FormField::ApiKey => mp.api_key.clone(),
        FormField::FableModel => mp.aliases[0].clone(),
        FormField::OpusModel => mp.aliases[1].clone(),
        FormField::SonnetModel => mp.aliases[2].clone(),
        FormField::HaikuModel => mp.aliases[3].clone(),
        _ => String::new(),
    }
}

// ── 事件处理 ──────────────────────────────────────────────────────────────────

/// 鼠标左键点击 → 执行该行对应的 Enter 动作（click as enter）。
///
/// 命中后先把光标移到对应项，再复用各 step 的 Enter 分支（构造 Enter KeyEvent），
/// 保证与键盘行为完全一致。行号反推与各 step 渲染布局一一对应（无滚动）。
pub(super) fn wizard_click(
    mouse: &ratatui_kit::crossterm::event::MouseEvent,
    area: ratatui_kit::ratatui::layout::Rect,
    state: &mut SetupWizardState,
) -> bool {
    let enter = ratatui_kit::crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let visual = mouse.row.saturating_sub(area.y).saturating_sub(1);
    match state.step {
        SetupStep::Language => {
            // 布局：空行 + prompt + 空行（header 3），每语言项 2 行（label + 空行）
            if let Some(idx) = hit_item(
                mouse,
                area,
                ListLayout {
                    header_rows: 3,
                    item_rows: 2,
                    footer_rows: 0,
                    visible_items: LANGUAGE_OPTIONS.len() as u16,
                    scroll_start: 0,
                    item_count: LANGUAGE_OPTIONS.len(),
                },
            ) {
                state.language_cursor = idx;
                handle_language_keys(state, enter);
                return true;
            }
        }
        SetupStep::Choose => {
            // 布局：空行 + prompt + 空行（header 3），每项 3 行（label + desc + 空行）
            if let Some(idx) = hit_item(
                mouse,
                area,
                ListLayout {
                    header_rows: 3,
                    item_rows: 3,
                    footer_rows: 0,
                    visible_items: SetupSource::ALL.len() as u16,
                    scroll_start: 0,
                    item_count: SetupSource::ALL.len(),
                },
            ) {
                state.choose_cursor = idx;
                state.source = SetupSource::ALL[idx];
                handle_choose_keys(state, enter);
                return true;
            }
        }
        SetupStep::Form => match state.form_mode {
            FormMode::Browse => {
                // 布局：空行 + 每 provider 7 行（base_url 非空时 8 行）+ submit 行（无滚动）
                // 与 render_browse 对齐：provider 行 + (url 行) + 空行 + 4 别名行 + 空行
                let mut cur = 1u16;
                if state.providers.is_empty() {
                    cur += 2; // "no providers" + 空行
                }
                for (i, mp) in state.providers.iter().enumerate() {
                    let item_h = if mp.base_url.is_empty() { 7u16 } else { 8u16 };
                    if visual >= cur && visual < cur + item_h {
                        state.browse_cursor = i;
                        handle_browse_keys(state, enter);
                        return true;
                    }
                    cur += item_h;
                }
                // submit 行（submit_error 存在时多 2 行）
                if state.submit_error.is_some() {
                    cur += 2;
                }
                if visual == cur {
                    state.browse_cursor = state.providers.len();
                    handle_browse_keys(state, enter);
                    return true;
                }
            }
            FormMode::Edit => {
                // 布局：空行（header 1）+ ProviderType..ApiKey 各 1 行；
                // 空行 + model 标题（header 8）+ FableModel..Confirm 各 1 行
                const FIELDS1: [FormField; 5] = [
                    FormField::ProviderType,
                    FormField::ProviderId,
                    FormField::BaseUrl,
                    FormField::TestConnectivity,
                    FormField::ApiKey,
                ];
                if let Some(idx) = hit_row(
                    mouse.row,
                    area,
                    ListLayout {
                        header_rows: 1,
                        item_rows: 1,
                        footer_rows: 0,
                        visible_items: FIELDS1.len() as u16,
                        scroll_start: 0,
                        item_count: FIELDS1.len(),
                    },
                ) {
                    state.form_focus = FIELDS1[idx];
                    state.edit_cursor_pos = get_raw_field_value(state).chars().count();
                    handle_edit_keys(state, enter);
                    return true;
                }
                const FIELDS2: [FormField; 5] = [
                    FormField::FableModel,
                    FormField::OpusModel,
                    FormField::SonnetModel,
                    FormField::HaikuModel,
                    FormField::Confirm,
                ];
                if let Some(idx) = hit_row(
                    mouse.row,
                    area,
                    ListLayout {
                        header_rows: 8,
                        item_rows: 1,
                        footer_rows: 0,
                        visible_items: FIELDS2.len() as u16,
                        scroll_start: 0,
                        item_count: FIELDS2.len(),
                    },
                ) {
                    state.form_focus = FIELDS2[idx];
                    state.edit_cursor_pos = get_raw_field_value(state).chars().count();
                    handle_edit_keys(state, enter);
                    return true;
                }
            }
        },
        SetupStep::Done => {
            // 布局：空行 + 标题 + 空行（header 3）+ 每 provider 8 行 + 空行 + Enter 提示行
            let selected_count = state.providers.iter().filter(|p| p.selected).count();
            let enter_row = (4 + 8 * selected_count) as u16;
            if visual == enter_row {
                handle_done_keys(state, enter);
                return true;
            }
        }
    }
    false
}

pub(super) fn handle_wizard_event(event: Event, mut state: SetupWizardState) -> EventResult {
    // 处理粘贴事件（仅 Form 编辑模式下且当前字段为文本输入时）
    if let Event::Paste(paste_text) = &event {
        if state.step == SetupStep::Form
            && state.form_mode == FormMode::Edit
            && state.form_focus.is_text_input()
        {
            handle_paste_to_text_input(&mut state, paste_text);
            *SETUP_WIZARD.state().write() = state;
            return EventResult::Consumed;
        }
        return EventResult::Ignored;
    }

    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }

    match state.step {
        SetupStep::Language => handle_language_keys(&mut state, key),
        SetupStep::Choose => handle_choose_keys(&mut state, key),
        SetupStep::Form => handle_form_keys(&mut state, key),
        SetupStep::Done => handle_done_keys(&mut state, key),
    }

    // 写回 state atom
    *SETUP_WIZARD.state().write() = state;
    EventResult::Consumed
}

fn handle_language_keys(
    state: &mut SetupWizardState,
    key: ratatui_kit::crossterm::event::KeyEvent,
) {
    use KeyCode::*;
    match key.code {
        Up => {
            state.language_cursor =
                (state.language_cursor + LANGUAGE_OPTIONS.len() - 1) % LANGUAGE_OPTIONS.len();
        }
        Down => {
            state.language_cursor = (state.language_cursor + 1) % LANGUAGE_OPTIONS.len();
        }
        Enter | Char(' ') => {
            let lang = LANGUAGE_OPTIONS[state.language_cursor].0.to_string();
            state.language = lang.clone();
            state.step = SetupStep::Choose;
            state.choose_cursor = 0;
            // 切换 i18n 语言
            i18n::switch(&lang);
        }
        Esc => {
            *atoms::WIZARD_ACTIVE.state().write() = false;
        }
        _ => {}
    }
}

fn handle_choose_keys(state: &mut SetupWizardState, key: ratatui_kit::crossterm::event::KeyEvent) {
    use KeyCode::*;
    match key.code {
        Up => {
            state.submit_error = None;
            state.choose_cursor =
                (state.choose_cursor + SetupSource::ALL.len() - 1) % SetupSource::ALL.len();
            state.source = SetupSource::ALL[state.choose_cursor];
        }
        Down => {
            state.submit_error = None;
            state.choose_cursor = (state.choose_cursor + 1) % SetupSource::ALL.len();
            state.source = SetupSource::ALL[state.choose_cursor];
        }
        Enter | Char(' ') => {
            state.submit_error = None;
            match state.source {
                SetupSource::MigrateClaudeCode => {
                    if !migrate_from_claude_code(state, None) {
                        state.source = SetupSource::CustomApi;
                        state.choose_cursor = 0;
                        state.submit_error = Some(
                            "迁移失败：未在 ~/.claude/settings.json 中找到有效的 Provider 配置。请确保文件中有 env.ANTHROPIC_API_KEY 或 env.OPENAI_API_KEY。"
                                .into(),
                        );
                        return;
                    }
                }
                SetupSource::PeriFreeService => {
                    state.providers = vec![peri_free_provider()];
                    state.active_provider = 0;
                }
                SetupSource::CustomApi => {
                    state.providers = vec![MigratedProvider::new(ProviderType::Anthropic)];
                    state.active_provider = 0;
                }
            }
            state.step = SetupStep::Form;
            state.form_mode = FormMode::Browse;
            state.browse_cursor = 0;
            state.form_focus = FormField::ProviderType;
        }
        Esc => {
            state.submit_error = None;
            state.step = SetupStep::Language;
        }
        _ => {}
    }
}

fn handle_form_keys(state: &mut SetupWizardState, key: ratatui_kit::crossterm::event::KeyEvent) {
    match state.form_mode {
        FormMode::Browse => handle_browse_keys(state, key),
        FormMode::Edit => handle_edit_keys(state, key),
    }
}

fn handle_done_keys(state: &mut SetupWizardState, key: ratatui_kit::crossterm::event::KeyEvent) {
    use KeyCode::*;
    match key.code {
        Enter => {
            // 保存配置并关闭 wizard
            if let Err(e) = save_setup(state) {
                tracing::error!("setup wizard: save failed: {e}");
            }
            *atoms::WIZARD_ACTIVE.state().write() = false;
        }
        Esc => {
            state.submit_error = None;
            state.step = SetupStep::Form;
            state.form_mode = FormMode::Browse;
        }
        _ => {}
    }
}

fn handle_browse_keys(state: &mut SetupWizardState, key: ratatui_kit::crossterm::event::KeyEvent) {
    use KeyCode::*;
    let max_pos = state.providers.len(); // submit button position
    match key.code {
        Up => {
            state.submit_error = None;
            if state.browse_cursor > 0 {
                state.browse_cursor -= 1;
            }
        }
        Down => {
            state.submit_error = None;
            if state.browse_cursor < max_pos {
                state.browse_cursor += 1;
            }
        }
        Char(' ') => {
            state.submit_error = None;
            if state.browse_cursor < state.providers.len() {
                let mp = &mut state.providers[state.browse_cursor];
                mp.selected = !mp.selected;
            }
        }
        Enter => {
            if state.browse_cursor < state.providers.len() {
                state.submit_error = None;
                state.active_provider = state.browse_cursor;
                state.form_mode = FormMode::Edit;
                state.form_focus = FormField::ProviderType;
                state.edit_cursor_pos = 0;
            } else {
                let has_valid = state
                    .providers
                    .iter()
                    .any(|p| p.selected && p.is_complete());
                if has_valid {
                    state.submit_error = None;
                    state.step = SetupStep::Done;
                } else {
                    state.submit_error = Some(
                        "No provider selected or incomplete. Select at least one provider with all fields filled."
                            .into(),
                    );
                }
            }
        }
        Esc => {
            state.submit_error = None;
            state.step = SetupStep::Choose;
        }
        _ => {}
    }
}

fn handle_edit_keys(state: &mut SetupWizardState, key: ratatui_kit::crossterm::event::KeyEvent) {
    use KeyCode::*;
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // 文本编辑按键：先处理
    if state.form_focus.is_text_input() {
        let handled = handle_text_input(state, &key);
        if handled {
            return;
        }
    }

    match key.code {
        Up => {
            state.form_focus = state.form_focus.prev();
            state.edit_cursor_pos = get_raw_field_value(state).chars().count();
        }
        Down => {
            state.form_focus = state.form_focus.next();
            state.edit_cursor_pos = get_raw_field_value(state).chars().count();
        }
        Left | Right if !is_ctrl && state.form_focus == FormField::ProviderType => {
            if let Some(mp) = state.active_provider_mut() {
                mp.provider_type.cycle();
            }
        }
        Char(' ') if state.form_focus == FormField::ProviderType => {
            if let Some(mp) = state.active_provider_mut() {
                mp.provider_type.cycle();
            }
        }
        Enter => {
            if state.form_focus == FormField::TestConnectivity {
                if let Some(mp) = state.active_provider_ref() {
                    state.connectivity_result = Some(test_connectivity(&mp.base_url));
                }
            } else if state.form_focus == FormField::Confirm {
                let mp = match state.active_provider_ref() {
                    Some(mp) => mp,
                    None => return,
                };
                if !mp.provider_id.trim().is_empty()
                    && !mp.api_key.trim().is_empty()
                    && mp.aliases.iter().all(|a| !a.trim().is_empty())
                {
                    state.form_mode = FormMode::Browse;
                }
            }
        }
        Esc => {
            state.form_mode = FormMode::Browse;
        }
        _ => {}
    }
}

fn handle_text_input(
    state: &mut SetupWizardState,
    key: &ratatui_kit::crossterm::event::KeyEvent,
) -> bool {
    use KeyCode::*;
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        Char(ch) if !is_ctrl => {
            let mut val = get_raw_field_value(state);
            let chars: Vec<char> = val.chars().collect();
            let pos = state.edit_cursor_pos.min(chars.len());
            let prefix: String = chars[..pos].iter().collect();
            let suffix: String = chars[pos..].iter().collect();
            val = format!("{}{}{}", prefix, ch, suffix);
            state.edit_cursor_pos = pos + 1;
            state.set_active_field_value(val);
            true
        }
        Backspace if !is_ctrl => {
            let mut val = get_raw_field_value(state);
            let chars: Vec<char> = val.chars().collect();
            if state.edit_cursor_pos > 0 && state.edit_cursor_pos <= chars.len() {
                let prefix: String = chars[..state.edit_cursor_pos - 1].iter().collect();
                let suffix: String = chars[state.edit_cursor_pos..].iter().collect();
                val = format!("{}{}", prefix, suffix);
                state.edit_cursor_pos -= 1;
            } else if state.edit_cursor_pos > chars.len() && !chars.is_empty() {
                let prefix: String = chars[..chars.len() - 1].iter().collect();
                val = prefix;
                state.edit_cursor_pos = chars.len() - 1;
            }
            state.set_active_field_value(val);
            true
        }
        Delete => {
            let val = get_raw_field_value(state);
            let chars: Vec<char> = val.chars().collect();
            if state.edit_cursor_pos < chars.len() {
                let prefix: String = chars[..state.edit_cursor_pos].iter().collect();
                let suffix: String = chars[state.edit_cursor_pos + 1..].iter().collect();
                state.set_active_field_value(format!("{}{}", prefix, suffix));
            }
            true
        }
        Left if !is_ctrl => {
            if state.edit_cursor_pos > 0 {
                state.edit_cursor_pos -= 1;
            }
            true
        }
        Right if !is_ctrl => {
            let val = get_raw_field_value(state);
            let max_pos = val.chars().count();
            if state.edit_cursor_pos < max_pos {
                state.edit_cursor_pos += 1;
            }
            true
        }
        Home if !is_ctrl => {
            state.edit_cursor_pos = 0;
            true
        }
        End if !is_ctrl => {
            let val = get_raw_field_value(state);
            state.edit_cursor_pos = val.chars().count();
            true
        }
        Char('w') if is_ctrl => {
            // Ctrl+W: 删除前一个词
            let val = get_raw_field_value(state);
            let chars: Vec<char> = val.chars().collect();
            let pos = state.edit_cursor_pos.min(chars.len());
            if pos == 0 {
                return true;
            }
            // 跳过前导空白
            let mut end = pos;
            while end > 0 && chars[end - 1].is_whitespace() {
                end -= 1;
            }
            // 跳过单词字符
            while end > 0 && !chars[end - 1].is_whitespace() {
                end -= 1;
            }
            let prefix: String = chars[..end].iter().collect();
            let suffix: String = chars[pos..].iter().collect();
            state.edit_cursor_pos = end;
            state.set_active_field_value(format!("{}{}", prefix, suffix));
            true
        }
        _ => false,
    }
}

/// 将剪贴板内容插入当前文本输入字段的光标位置。
/// 归一化换行符（\r\n → \n），截断至 10k 字符（CJK 安全），超出时记录警告。
fn handle_paste_to_text_input(state: &mut SetupWizardState, paste_text: &str) {
    const MAX_PASTE_CHARS: usize = 10_000;
    let normalized = paste_text.replace("\r\n", "\n").replace('\r', "\n");
    let truncated: String = normalized.chars().take(MAX_PASTE_CHARS).collect();
    if normalized.chars().count() != truncated.chars().count() {
        tracing::warn!(
            "setup wizard: paste truncated from {} to {MAX_PASTE_CHARS} chars (CJK-safe)",
            normalized.chars().count()
        );
    }

    let mut val = get_raw_field_value(state);
    let chars: Vec<char> = val.chars().collect();
    let pos = state.edit_cursor_pos.min(chars.len());
    let prefix: String = chars[..pos].iter().collect();
    let suffix: String = chars[pos..].iter().collect();
    let paste_len = truncated.chars().count();
    val = format!("{}{}{}", prefix, truncated, suffix);
    state.edit_cursor_pos = pos + paste_len;
    state.set_active_field_value(val);
}
