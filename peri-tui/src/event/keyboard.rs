use anyhow::Result;
use ratatui::crossterm::event::KeyEventKind;
use tui_textarea::Input;

use super::Action;
use crate::app::App;
use crate::state_machine::input::InputOp;

// ── Submodule declarations ─────────────────────────────────────────────────
mod bar_focus;
mod normal_keys;
mod popups;
mod setup_wizard;
// NOTE (B3 Gap 1): shortcuts.rs deleted — all its shortcuts (BackTab,
// Ctrl+T/Shift+T, Ctrl+B, Ctrl+O, Alt+M, Alt+Shift+M) are now dispatched by
// the state machine (idle.rs). is_sm_handled_shortcut() prevents
// double-execution.

// NOTE (B3 Gap 1): KeyBinding struct and all SHORTCUT_* statics deleted —
// macOS Option-key composed characters are now handled directly in
// idle.rs (Char('\u{b5}') → CycleModel, Char('\u{c2}') → CycleProvider).

/// Returns the platform-appropriate label for the model-cycling shortcut.
pub fn cycle_model_label() -> &'static str {
    "Ctrl+T"
}

/// Returns the platform-appropriate label for the provider-cycling shortcut.
pub fn cycle_provider_label() -> &'static str {
    "Ctrl+Shift+T"
}

/// Handles a single key event, dispatching to panels, prompts, textarea, or
/// application-level shortcuts. Returns an `Action` when a redraw or quit is
/// needed. `state` is the v2 state machine state for InputState reads.
pub fn handle_key_event(
    app: &mut App,
    key_event: ratatui::crossterm::event::KeyEvent,
    state: &crate::state_machine::State,
) -> Result<Option<Action>> {
    // Only process Press events; ignore Release (prevents double-fires)
    if key_event.kind == KeyEventKind::Release {
        return Ok(Some(Action::Redraw));
    }

    // Stage 1-2: Bar focus / focused-only mode
    if let Some(action) = bar_focus::handle_bar_focus(app, &key_event) {
        return Ok(Some(action));
    }
    if let Some(action) = bar_focus::handle_focused_only(app, &key_event) {
        return Ok(Some(action));
    }

    let input = Input::from(key_event);

    // Stage 7: Setup wizard
    if let Some(action) = setup_wizard::handle_setup_wizard(app, &input) {
        return Ok(Some(action));
    }

    // Stage 9: Popups (OAuth > AskUser > HITL)
    if let Some(action) = popups::handle_popups(app, &input) {
        return Ok(Some(action));
    }

    // Stage 13: Normal key handling (main match block)
    let effects = normal_keys::handle_normal_keys(app, input, state);
    Ok(Some(Action::Effects(effects)))
}

/// 从 v2 InputState 检测 @ 提及模式，更新状态并同步刷新候选
pub(super) fn update_at_mention_detection(
    app: &mut App,
    input: &crate::state_machine::input::InputState,
) {
    let text = input.text();
    let cursor_byte = input.cursor.to_byte_offset(&input.lines);

    let at = &mut app.session_mgr.current_mut().ui.at_mention;
    at.ensure_cwd(app.services.cwd.clone());

    if let Some((query, start)) = crate::app::AtMentionState::detect(&text, cursor_byte) {
        if at.active && at.query == query {
            return; // 未变化
        }
        at.activate(query.clone(), start);
        // 同步刷新候选列表
        at.refresh_candidates();
    } else if at.active {
        at.close();
    }
}

/// 检测 v2 InputState 中 / skill/command token，更新 slash_hint 状态。
/// 当 @mention 活跃时自动 deactivate 避免双弹窗。
pub(super) fn update_slash_hint_detection(
    app: &mut App,
    input: &crate::state_machine::input::InputState,
) {
    let text = input.text();
    let cursor_byte = input.cursor.to_byte_offset(&input.lines);

    // 先检查 at_mention 状态（不可变借用）
    let at_mention_active = app.session_mgr.current().ui.at_mention.active;

    let slash = &mut app.session_mgr.current_mut().ui.slash_hint;

    if at_mention_active {
        slash.deactivate();
        return;
    }

    if let Some((prefix, start)) = crate::app::SlashHintState::detect(&text, cursor_byte) {
        if slash.active && slash.prefix == prefix && slash.token_start == start {
            return; // 未变化
        }
        slash.activate(prefix, start);
    } else {
        slash.deactivate();
    }
}

/// 将选中的 @ 提及路径构造为 Effect::ApplyInputOp(SetText)，由 main_loop 写入 InputState
pub(super) fn inject_at_mention_path(app: &mut App) -> Vec<crate::runtime::effect::Effect> {
    let (path, is_dir, query_start, query_len) = {
        let at = &app.session_mgr.current_mut().ui.at_mention;
        let path = match at.selected_path() {
            Some(p) => p,
            None => return vec![],
        };
        let is_dir = at.selected_candidate().is_some_and(|e| e.is_dir);
        let query_start = at.query_start;
        let query_len = at.query.len();
        (path, is_dir, query_start, query_len)
    };

    // 从 v2 InputState 读取当前 full_text
    let full_text: String = app.session_mgr.current().ui.textarea.lines().join("\n");

    let needs_quotes = path.contains(' ');
    let replacement = if needs_quotes {
        format!("@\"{}\"", path)
    } else {
        format!("@{}", path)
    };

    let mut new_text = String::with_capacity(full_text.len() + replacement.len());
    new_text.push_str(&full_text[..query_start]);
    new_text.push_str(&replacement);
    let after_end = query_start + 1 + query_len;
    if after_end < full_text.len() {
        new_text.push_str(&full_text[after_end..]);
    }

    let mut effects = vec![crate::runtime::effect::Effect::ApplyInputOp(
        InputOp::SetText(new_text),
    )];

    if is_dir {
        effects.push(crate::runtime::effect::Effect::ApplyInputOp(
            InputOp::InsertStr("/".to_string()),
        ));
    } else {
        effects.push(crate::runtime::effect::Effect::ApplyInputOp(
            InputOp::InsertStr(" ".to_string()),
        ));
        app.session_mgr.current_mut().ui.at_mention.close();
        effects.push(crate::runtime::effect::Effect::Render);
    }
    effects
}
