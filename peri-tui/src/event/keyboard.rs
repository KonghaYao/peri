use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use tui_textarea::Input;

use super::Action;
use crate::app::App;

// ── Submodule declarations ─────────────────────────────────────────────────
mod bar_focus;
mod normal_keys;
mod popups;
mod setup_wizard;
mod shortcuts;

// ---------------------------------------------------------------------------
// macOS key-binding compatibility layer
// ---------------------------------------------------------------------------
// On macOS, the Option (Alt) key acts as a character compose modifier.
// Terminals emit a composed Unicode character *without* any modifier flags.
// We maintain a central mapping table so each shortcut only needs to be
// defined once, keeping the macOS workaround auditable.
// ---------------------------------------------------------------------------

/// A cross-platform key-binding definition that accounts for macOS Option-key
/// character composition.
pub(super) struct KeyBinding {
    /// Human-readable label (for status bar / docs).
    label: &'static str,
    /// Character produced on macOS when Option (+ optional Shift) is held.
    macos_char: Option<char>,
    /// Required modifiers on non-macOS terminals (Linux/Windows).
    modifiers: KeyModifiers,
    /// The primary key code (ignoring macOS compose).
    key: KeyCode,
}

impl KeyBinding {
    /// Returns `true` if `key_event` matches this binding on *any* platform.
    pub(super) fn matches(&self, key_event: &ratatui::crossterm::event::KeyEvent) -> bool {
        // macOS path: terminal emits a composed char with no modifiers.
        if let Some(ch) = self.macos_char {
            if matches!(key_event.code, KeyCode::Char(c) if c == ch) {
                return true;
            }
        }
        // Standard path: check modifiers + key code.
        let mods_ok = key_event.modifiers.contains(self.resolved_modifiers());
        let key_ok = match (&self.key, &key_event.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => a.eq_ignore_ascii_case(b),
            (a, b) => a == b,
        };
        mods_ok && key_ok
    }

    /// Resolve the actual modifiers needed. bitflags `|` is not const,
    /// so multi-flag bindings store `KeyModifiers::empty()` and reconstruct here.
    fn resolved_modifiers(&self) -> KeyModifiers {
        match self.label {
            "Alt+Shift+M" => KeyModifiers::ALT | KeyModifiers::SHIFT,
            "Ctrl+Shift+T" => KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            _ => self.modifiers,
        }
    }
}

/// Central shortcut registry.  Add new shortcuts here — the `matches()` call
/// in each handler block is the only site that needs updating.
pub(super) static SHORTCUT_CYCLE_MODE: KeyBinding = KeyBinding {
    label: "Alt+M",
    macos_char: Some('µ'),
    modifiers: KeyModifiers::ALT,
    key: KeyCode::Char('m'),
};

pub(super) static SHORTCUT_CYCLE_PROVIDER: KeyBinding = KeyBinding {
    label: "Alt+Shift+M",
    macos_char: Some('Â'),
    modifiers: KeyModifiers::empty(), // resolved_modifiers() returns ALT|SHIFT
    key: KeyCode::Char('m'),
};

// Ctrl+T / Ctrl+Shift+T: cross-platform model/provider cycling.
// Ctrl combos have no macOS composition issue, so macos_char is None.
pub(super) static SHORTCUT_CTRL_CYCLE_MODE: KeyBinding = KeyBinding {
    label: "Ctrl+T",
    macos_char: None,
    modifiers: KeyModifiers::CONTROL,
    key: KeyCode::Char('t'),
};

pub(super) static SHORTCUT_CTRL_CYCLE_PROVIDER: KeyBinding = KeyBinding {
    label: "Ctrl+Shift+T",
    macos_char: None,
    modifiers: KeyModifiers::empty(), // resolved_modifiers() returns CONTROL|SHIFT
    key: KeyCode::Char('t'),
};

pub(super) static SHORTCUT_BG_BAR: KeyBinding = KeyBinding {
    label: "Ctrl+B",
    macos_char: None,
    modifiers: KeyModifiers::CONTROL,
    key: KeyCode::Char('b'),
};

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
/// needed.
pub fn handle_key_event(
    app: &mut App,
    key_event: ratatui::crossterm::event::KeyEvent,
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

    // Stage 3-6: Shortcuts (BackTab, Ctrl+B, Ctrl+T, Ctrl+Shift+T)
    if let Some(action) = shortcuts::handle_shortcuts(app, &key_event) {
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
    normal_keys::handle_normal_keys(app, input)
}

/// 检测 textarea 中 @ 提及模式，更新状态并同步刷新候选
pub(super) fn update_at_mention_detection(app: &mut App) {
    let textarea = &app.session_mgr.current_mut().ui.textarea;
    let text = textarea.lines().join("\n");
    let (row, col) = textarea.cursor();
    let mut pos = 0usize;
    for (i, line) in textarea.lines().iter().enumerate() {
        if i == row {
            pos += line.chars().take(col).map(|c| c.len_utf8()).sum::<usize>();
            break;
        }
        pos += line.len() + 1;
    }

    let at = &mut app.session_mgr.current_mut().ui.at_mention;
    at.ensure_cwd(app.services.cwd.clone());

    if let Some((query, start)) = crate::app::AtMentionState::detect(&text, pos) {
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

/// 检测 textarea 中 / skill/command token，更新 slash_hint 状态。
/// 参考 update_at_mention_detection 模式：将 (row, col) 转为字节偏移后调用 detect。
/// 当 @mention 活跃时自动 deactivate 避免双弹窗。
pub(super) fn update_slash_hint_detection(app: &mut App) {
    let (text, cursor_pos) = {
        let textarea = &app.session_mgr.current_mut().ui.textarea;
        let text = textarea.lines().join("\n");
        let (row, col) = textarea.cursor();
        let mut pos = 0usize;
        for (i, line) in textarea.lines().iter().enumerate() {
            if i == row {
                pos += line.chars().take(col).map(|c| c.len_utf8()).sum::<usize>();
                break;
            }
            pos += line.len() + 1; // +1 for newline
        }
        (text, pos)
    }; // textarea mutable borrow 在此结束 ← 关键：Rust NLL 通过作用域释放

    // 先检查 at_mention 状态（不可变借用）
    let at_mention_active = app.session_mgr.current().ui.at_mention.active;

    let slash = &mut app.session_mgr.current_mut().ui.slash_hint;

    if at_mention_active {
        slash.deactivate();
        return;
    }

    if let Some((prefix, start)) = crate::app::SlashHintState::detect(&text, cursor_pos) {
        if slash.active && slash.prefix == prefix && slash.token_start == start {
            return; // 未变化
        }
        slash.activate(prefix, start);
    } else {
        slash.deactivate();
    }
}

/// 将选中的 @ 提及路径注入 textarea
pub(super) fn inject_at_mention_path(app: &mut App) {
    let at = &app.session_mgr.current_mut().ui.at_mention;
    let path = match at.selected_path() {
        Some(p) => p,
        None => return,
    };
    let is_dir = at.selected_candidate().is_some_and(|e| e.is_dir);
    let query_start = at.query_start;
    let query_len = at.query.len();

    let textarea = &app.session_mgr.current_mut().ui.textarea;
    let full_text: String = textarea.lines().join("\n");

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

    let mut new_ta = crate::app::build_textarea(false);
    new_ta.insert_str(&new_text);
    app.session_mgr.current_mut().ui.textarea = new_ta;

    if is_dir {
        app.session_mgr.current_mut().ui.textarea.insert_str("/");
        update_at_mention_detection(app);
    } else {
        app.session_mgr.current_mut().ui.textarea.insert_str(" ");
        app.session_mgr.current_mut().ui.at_mention.close();
    }
}
