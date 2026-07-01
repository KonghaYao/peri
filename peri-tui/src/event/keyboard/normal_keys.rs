use tui_textarea::{Input, Key};

use crate::app::{App, PendingAttachment};
use crate::runtime::effect::Effect;
use crate::state_machine::input::{CursorDirection, InputOp};
use crate::state_machine::State;

/// Normal mode key handling: main match block arm bodies.
///
/// Returns a `Vec<Effect>` that the main_loop will route to the state machine's
/// InputState for textarea mutations, and to the legacy App for non-textarea
/// side effects.
pub(super) fn handle_normal_keys(app: &mut App, input: Input, state: &State) -> Vec<Effect> {
    use super::update_slash_hint_detection;
    use super::{inject_at_mention_path, update_at_mention_detection};

    // 从 v2 state machine 获取当前 InputState 引用，供 @mention/slash 检测使用
    let input_state = match state {
        State::Idle(idle) => &idle.input,
        State::Streaming(s) => &s.input,
        _ => {
            // Modal/Switching 状态无输入，返回空 Vec
            return vec![Effect::Render];
        }
    };

    match input {
        // Ctrl+C: interrupt agent / double-tap to quit
        Input {
            key: Key::Char('c'),
            ctrl: true,
            ..
        } => {
            return handle_ctrl_c(app);
        }

        // ESC: no longer quits main window; only clears buffer while loading
        Input { key: Key::Esc, .. } if app.session_mgr.current_mut().ui.loading => {
            // 清除 loading 期间缓存的 pending_messages
            if !app
                .session_mgr
                .current_mut()
                .messages
                .pending_messages
                .is_empty()
            {
                app.session_mgr
                    .current_mut()
                    .messages
                    .pending_messages
                    .clear();
            }
            // 设置 rewind 不可用提示（Status Bar，3秒后消失）
            app.global_ui.rewind_busy_hint_until =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
        }

        // Esc: 关闭 @ 提及弹窗
        Input { key: Key::Esc, .. } if app.session_mgr.current_mut().ui.at_mention.active => {
            app.session_mgr.current_mut().ui.at_mention.close();
            app.session_mgr.current_mut().ui.at_mention.close();
        }
        // Esc: 关闭 slash hint 弹窗
        Input { key: Key::Esc, .. } if app.session_mgr.current_mut().ui.slash_hint.active => {
            app.session_mgr.current_mut().ui.slash_hint.deactivate();
            app.session_mgr.current_mut().ui.hint_cursor = None;
        }

        // Esc: 双击触发 rewind 选择器（仅空闲时）
        Input { key: Key::Esc, .. } if !app.session_mgr.current().ui.loading => {
            if let Some(since) = app.global_ui.rewind_pending_since {
                if since.elapsed() < std::time::Duration::from_secs(2) {
                    // 双击 ESC → 打开 rewind 选择器
                    app.global_ui.rewind_pending_since = None;
                    app.open_rewind_prompt();
                } else {
                    app.global_ui.rewind_pending_since = Some(std::time::Instant::now());
                }
            } else {
                app.global_ui.rewind_pending_since = Some(std::time::Instant::now());
            }
        }

        // Up: @ 提及导航 > hint navigation > history browse (only first row) > textarea cursor
        Input { key: Key::Up, .. } => return handle_up(app),

        // Down: @ 提及导航 > hint navigation > history restore (only last row) > textarea cursor
        Input { key: Key::Down, .. } => return handle_down(app),

        // Ctrl+V: try pasting clipboard image first, fallback to text paste
        // Loading 时同样允许——粘贴的文本/图片会进入 textarea / pending_attachments，
        // 后续 Enter 把消息 push 到 MessageQueue。
        Input {
            key: Key::Char('v'),
            ctrl: true,
            ..
        } => return handle_ctrl_v(app),

        // Tab: @ 提及补全 > hint overlay candidate navigation and completion
        Input {
            key: Key::Tab,
            shift: false,
            ..
        } => return handle_tab(app),

        // Enter with @ mention active and candidates: inject selected path
        Input {
            key: Key::Enter, ..
        } if app.session_mgr.current_mut().ui.at_mention.active
            && !app
                .session_mgr
                .current_mut()
                .ui
                .at_mention
                .candidates
                .is_empty() =>
        {
            return inject_at_mention_path(app);
        }

        // Enter with hints available: confirm selection (defaults to first if none selected)
        Input {
            key: Key::Enter, ..
        } if app.hint_candidates_count() > 0 => {
            if app.session_mgr.current_mut().ui.hint_cursor.is_none() {
                app.session_mgr.current_mut().ui.hint_cursor = Some(0);
            }
            app.hint_complete();
        }

        // Shift+Enter / Alt+Enter: insert newline (Shift works everywhere; Alt (Option) for macOS)
        Input {
            key: Key::Enter, ..
        } if input.shift || input.alt => {
            return vec![Effect::ApplyInputOp(InputOp::InsertNewline), Effect::Render];
        }

        // Enter: submit (slash command routing falls through to keyboard, plain text handled by SM)
        Input {
            key: Key::Enter, ..
        } => {
            // 关闭可能残留的 @ mention 弹窗
            if app.session_mgr.current_mut().ui.at_mention.active {
                app.session_mgr.current_mut().ui.at_mention.close();
            }
            let text = app.session_mgr.current_mut().ui.textarea.lines().join("\n");
            let text = text.trim().to_string();
            if !text.is_empty() && text.starts_with('/') {
                // Slash command dispatch: SM returns empty for slash commands,
                // so keyboard fallback must handle CommandRegistry dispatch.
                let registry =
                    std::mem::take(&mut app.session_mgr.current_mut().commands.command_registry);
                let result = registry.dispatch(app, &text);
                app.session_mgr.current_mut().commands.command_registry = registry;
                if let Some(effects) = result {
                    if !effects.is_empty() {
                        return effects;
                    }
                    // Command matched, no effects needed — fall through
                } else {
                    // Command not matched, try Skill matching
                    let skill_name: String = text
                        .trim_start_matches('/')
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                        .collect();
                    if app
                        .session_mgr
                        .current_mut()
                        .commands
                        .skills
                        .iter()
                        .any(|s| s.name == skill_name)
                        || app
                            .session_mgr
                            .current_mut()
                            .commands
                            .agent_commands
                            .contains(&skill_name)
                    {
                        // Skill / Agent command matched: submit to agent.
                        // UserBubble is pushed by push_user_bubble so it lands
                        // in v2 state.view.
                        app.session_mgr
                            .current_mut()
                            .messages
                            .push_user_bubble(text.clone());
                        return vec![Effect::SubmitMessage { text }, Effect::Render];
                    }
                    // Unknown slash: submit as normal input
                    app.session_mgr
                        .current_mut()
                        .messages
                        .push_user_bubble(text.clone());
                    return vec![Effect::SubmitMessage { text }, Effect::Render];
                }
            }
            // Plain text Enter: SM handles via SubmitMessage. Keyboard returns empty.
            return vec![];
        }

        // VS Code terminal maps Option+Backspace to PageUp; perform word-delete when textarea has content
        Input {
            key: Key::PageUp, ..
        } if std::env::var("TERM_PROGRAM").as_deref() == Ok("vscode") => {
            let session = &app.session_mgr.current_mut();
            let has_content = session
                .ui
                .textarea
                .lines()
                .iter()
                .any(|line| !line.is_empty());
            if has_content {
                return vec![
                    Effect::ApplyInputOp(InputOp::DeletePrevWord),
                    Effect::Render,
                ];
            }
        }

        // Ctrl+U / Ctrl+D: half-page scroll
        Input {
            key: Key::Char('u'),
            ctrl: true,
            ..
        } => {
            let has_content = app
                .session_mgr
                .current_mut()
                .ui
                .textarea
                .lines()
                .iter()
                .any(|line| !line.is_empty());
            if has_content {
                return vec![
                    Effect::ApplyInputOp(InputOp::DeleteToLineStart),
                    Effect::Render,
                ];
            } else {
                for _ in 0..20 {
                    app.scroll_up();
                }
            }
        }
        Input {
            key: Key::Char('d'),
            ctrl: true,
            ..
        } => {
            for _ in 0..20 {
                app.scroll_down();
            }
        }

        // Del: remove last pending attachment
        Input {
            key: Key::Delete, ..
        } if !app.session_mgr.current_mut().ui.loading
            && !app
                .session_mgr
                .current_mut()
                .metadata
                .pending_attachments
                .is_empty() =>
        {
            app.pop_pending_attachment();
        }

        // Intercept plain Enter to avoid textarea default newline; allow input during loading
        input if input.key != Key::Enter => {
            // Exit history browsing
            if app.session_mgr.current_mut().ui.history_index.is_some() {
                app.exit_history();
            }
            // 任意输入清除 prediction
            app.session_mgr.current_mut().ui.prediction = None;
            // When input changes: reset cursor (don't pre-select; wait for user to press Tab/Up/Down)
            // Loading 时也需更新——用户在 queue 下一条消息时同样期望 slash hint / @mention 弹窗。
            app.session_mgr.current_mut().ui.hint_cursor = None;
            update_at_mention_detection(app, input_state);
            update_slash_hint_detection(app, input_state);

            // Route to SM InputState via Effect
            // Backspace
            if input.key == Key::Backspace {
                return vec![
                    Effect::ApplyInputOp(InputOp::DeletePrevChar),
                    Effect::Render,
                ];
            }
            // Delete
            if input.key == Key::Delete {
                return vec![
                    Effect::ApplyInputOp(InputOp::DeleteNextChar),
                    Effect::Render,
                ];
            }
            // Ctrl+W / Option+Backspace → delete word
            if input.key == Key::Char('w') && input.ctrl {
                return vec![
                    Effect::ApplyInputOp(InputOp::DeletePrevWord),
                    Effect::Render,
                ];
            }
            // Ctrl+A → select all
            if input.key == Key::Char('a') && input.ctrl {
                return vec![Effect::ApplyInputOp(InputOp::SelectAll), Effect::Render];
            }
            // Left / Right arrows
            if input.key == Key::Left {
                return vec![
                    Effect::ApplyInputOp(InputOp::MoveCursor(CursorDirection::Left)),
                    Effect::Render,
                ];
            }
            if input.key == Key::Right {
                return vec![
                    Effect::ApplyInputOp(InputOp::MoveCursor(CursorDirection::Right)),
                    Effect::Render,
                ];
            }
            // Home / End
            if input.key == Key::Home {
                return vec![
                    Effect::ApplyInputOp(InputOp::MoveCursor(CursorDirection::LineStart)),
                    Effect::Render,
                ];
            }
            if input.key == Key::End {
                return vec![
                    Effect::ApplyInputOp(InputOp::MoveCursor(CursorDirection::LineEnd)),
                    Effect::Render,
                ];
            }
            // Ctrl+N → keep existing logic (new session, not textarea)
            if input.key == Key::Char('n') && input.ctrl {
                // Handled by SM/app-level — fall through to Render
            }
            // Plain character input
            if let Key::Char(c) = input.key {
                if !input.ctrl && !input.alt {
                    return vec![Effect::ApplyInputOp(InputOp::InsertChar(c)), Effect::Render];
                }
            }
            // Fallback: any other input not matched above — still need a render
            // if textarea was modified by older code paths or SM transitions.
        }
        _ => {
            // Any other key cancels quit-pending state (Ctrl+C double-tap)
            app.global_ui.quit_pending_since = None;
            // Note: do NOT reset rewind_pending_since here. The fallback arm
            // captures keys like Key::Enter (with unmatched modifiers) and
            // terminal-generated sequences (e.g. focus events, unknown keys).
            // Resetting here would break the ESC double-tap detection because
            // spurious key events between two ESC presses would clear the state.
            // rewind_pending_since is naturally reset when the user types actual
            // content (the `input if input.key != Key::Enter` arm above).
        }
    }

    vec![Effect::Render]
}

// ── Per-arm helper functions ──────────────────────────────────────────────

fn handle_ctrl_c(app: &mut App) -> Vec<Effect> {
    let session = &mut app.session_mgr.current_mut();

    // 优先级 1: 输入框有内容 → 清空输入框
    if session.ui.textarea.lines().iter().any(|l| !l.is_empty()) {
        app.global_ui.quit_pending_since = None;
        return vec![Effect::ApplyInputOp(InputOp::Clear), Effect::Render];
    }

    // 优先级 2: Agent 运行中 → 中断 agent
    if session.ui.loading {
        app.interrupt();
        app.global_ui.quit_pending_since = None;
        return vec![Effect::Render];
    }

    // 优先级 3: Agent 未运行 → quit-pending 逻辑
    if let Some(since) = app.global_ui.quit_pending_since {
        if since.elapsed() < std::time::Duration::from_secs(2) {
            return vec![Effect::Quit];
        } else {
            app.global_ui.quit_pending_since = Some(std::time::Instant::now());
        }
    } else {
        app.global_ui.quit_pending_since = Some(std::time::Instant::now());
    }
    vec![Effect::Render]
}

fn handle_up(app: &mut App) -> Vec<Effect> {
    let hint_count = app.hint_candidates_count();
    if app.session_mgr.current_mut().ui.at_mention.active {
        app.session_mgr.current_mut().ui.at_mention.move_up();
        return vec![Effect::Render];
    }
    if hint_count > 0 {
        let cur = app.session_mgr.current_mut().ui.hint_cursor.unwrap_or(0);
        app.session_mgr.current_mut().ui.hint_cursor = if cur == 0 {
            Some(hint_count - 1)
        } else {
            Some(cur - 1)
        };
        return vec![Effect::Render];
    }
    // Check cursor row from textarea (synced from InputState before keyboard runs)
    let (row, _col) = app.session_mgr.current_mut().ui.textarea.cursor();
    if row == 0 {
        app.history_up();
        vec![Effect::Render]
    } else {
        vec![
            Effect::ApplyInputOp(InputOp::MoveCursor(CursorDirection::Up)),
            Effect::Render,
        ]
    }
}

fn handle_down(app: &mut App) -> Vec<Effect> {
    let hint_count = app.hint_candidates_count();
    if app.session_mgr.current_mut().ui.at_mention.active {
        app.session_mgr.current_mut().ui.at_mention.move_down();
        return vec![Effect::Render];
    }
    if hint_count > 0 {
        let cur = app
            .session_mgr
            .current_mut()
            .ui
            .hint_cursor
            .unwrap_or(hint_count - 1);
        app.session_mgr.current_mut().ui.hint_cursor = if cur + 1 >= hint_count {
            Some(0)
        } else {
            Some(cur + 1)
        };
        return vec![Effect::Render];
    }
    if app.session_mgr.current_mut().ui.history_index.is_some() {
        app.history_down();
        return vec![Effect::Render];
    }
    // Check cursor row from textarea (synced from InputState before keyboard runs)
    let (row, _col) = app.session_mgr.current_mut().ui.textarea.cursor();
    let last_row = app
        .session_mgr
        .current_mut()
        .ui
        .textarea
        .lines()
        .len()
        .saturating_sub(1);
    if row >= last_row {
        app.history_down();
        vec![Effect::Render]
    } else {
        vec![
            Effect::ApplyInputOp(InputOp::MoveCursor(CursorDirection::Down)),
            Effect::Render,
        ]
    }
}

fn handle_ctrl_v(app: &mut App) -> Vec<Effect> {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        if let Ok(img) = clipboard.get_image() {
            let (w, h) = (img.width as u32, img.height as u32);
            if let Ok((b64, sz)) = super::super::mouse::rgba_to_png_base64(w, h, &img.bytes) {
                let n = app
                    .session_mgr
                    .current_mut()
                    .metadata
                    .pending_attachments
                    .len()
                    + 1;
                app.add_pending_attachment(PendingAttachment {
                    label: format!("clipboard_{}.png", n),
                    media_type: "image/png".to_string(),
                    base64_data: b64,
                    size_bytes: sz,
                });
            }
            return vec![Effect::Render];
        } else if let Ok(text) = clipboard.get_text() {
            let text = text.replace('\r', "\n");
            return vec![
                Effect::ApplyInputOp(InputOp::InsertStr(text)),
                Effect::Render,
            ];
        }
    }
    vec![Effect::Render]
}

fn handle_tab(app: &mut App) -> Vec<Effect> {
    use super::inject_at_mention_path;

    // Prediction 接受优先级最高
    if let Some(pred) = app.session_mgr.current_mut().ui.prediction.take() {
        return vec![
            Effect::ApplyInputOp(InputOp::InsertStr(pred.text)),
            Effect::Render,
        ];
    }

    if app.session_mgr.current_mut().ui.at_mention.active {
        return inject_at_mention_path(app);
    }
    let count = app.hint_candidates_count();
    if count > 0 {
        match app.session_mgr.current_mut().ui.hint_cursor {
            Some(cur) if cur + 1 < count => {
                app.session_mgr.current_mut().ui.hint_cursor = Some(cur + 1);
            }
            Some(_) => {
                app.session_mgr.current_mut().ui.hint_cursor = Some(0);
            }
            None => {
                app.session_mgr.current_mut().ui.hint_cursor = Some(0);
            }
        }
    }
    vec![Effect::Render]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::build_textarea;
    use crate::state_machine::{IdleState, State};

    async fn make_app() -> App {
        let (app, _) = App::new_headless(80, 24).await;
        app
    }

    fn idle_state() -> State {
        State::Idle(IdleState::default())
    }

    #[tokio::test]
    async fn test_ctrl_c_clears_textarea_when_has_content() {
        let mut app = make_app().await;
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr
            .current_mut()
            .ui
            .textarea
            .insert_str("hello world");

        let effects = handle_ctrl_c(&mut app);

        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::ApplyInputOp(InputOp::Clear))),
            "有内容时 Ctrl+C 应返回 ClearInputBuffer"
        );
        assert!(
            app.global_ui.quit_pending_since.is_none(),
            "清空输入框不应进入 quit-pending"
        );
    }

    #[tokio::test]
    async fn test_ctrl_c_interrupts_agent_when_textarea_empty() {
        let mut app = make_app().await;
        app.set_loading(true);

        let effects = handle_ctrl_c(&mut app);

        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Quit)),
            "中断 agent 不应返回 Quit"
        );
        assert!(
            app.global_ui.quit_pending_since.is_none(),
            "中断 agent 不应进入 quit-pending"
        );
    }

    #[tokio::test]
    async fn test_ctrl_c_enters_quit_pending_when_idle_and_empty() {
        let mut app = make_app().await;

        let effects = handle_ctrl_c(&mut app);

        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Quit)),
            "第一次 Ctrl+C 不应返回 Quit"
        );
        assert!(
            app.global_ui.quit_pending_since.is_some(),
            "空闲时应进入 quit-pending"
        );

        let effects = handle_ctrl_c(&mut app);
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Quit)),
            "2 秒内第二次 Ctrl+C 应返回 Quit"
        );
    }

    #[tokio::test]
    async fn test_ctrl_c_does_not_quit_when_textarea_has_content() {
        let mut app = make_app().await;
        let _ = handle_ctrl_c(&mut app);
        assert!(app.global_ui.quit_pending_since.is_some());

        app.session_mgr
            .current_mut()
            .ui
            .textarea
            .insert_str("some text");
        let effects = handle_ctrl_c(&mut app);

        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Quit)),
            "有内容时不应退出"
        );
        assert!(
            app.global_ui.quit_pending_since.is_none(),
            "清空输入框应重置 quit-pending"
        );
    }

    // ── Cron #26 step 7e.7: slash command submit routes UserBubble to v2 ──

    /// 未知 slash command 提交时应入队 UserBubble 到 pending_v2_user_bubbles
    /// 并返回 SubmitMessage Effect。
    #[tokio::test]
    async fn test_unknown_slash_command_submit_enqueues_user_bubble() {
        let mut app = make_app().await;
        let state = idle_state();
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr
            .current_mut()
            .ui
            .textarea
            .insert_str("/unknown-cmd arg1");

        let effects = handle_normal_keys(
            &mut app,
            Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &state,
        );

        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::SubmitMessage { .. })),
            "未知 slash command 应返回 SubmitMessage"
        );
        let pending = &app.session_mgr.current().messages.pending_v2_user_bubbles;
        assert_eq!(
            pending.len(),
            1,
            "slash command submit 应入队 1 个 UserBubble 到 pending_v2_user_bubbles"
        );
        assert_eq!(pending[0], "/unknown-cmd arg1");
    }

    /// Agent command 提交时也应入队并返回 SubmitMessage。
    #[tokio::test]
    async fn test_agent_command_submit_enqueues_user_bubble() {
        let mut app = make_app().await;
        let state = idle_state();
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr
            .current_mut()
            .commands
            .agent_commands
            .insert("my-agent-cmd".to_string());
        app.session_mgr
            .current_mut()
            .ui
            .textarea
            .insert_str("/my-agent-cmd");

        let effects = handle_normal_keys(
            &mut app,
            Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &state,
        );

        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::SubmitMessage { .. })));
        let pending = &app.session_mgr.current().messages.pending_v2_user_bubbles;
        assert_eq!(pending.len(), 1, "agent command submit 应入队 UserBubble");
        assert_eq!(pending[0], "/my-agent-cmd");
    }
}
