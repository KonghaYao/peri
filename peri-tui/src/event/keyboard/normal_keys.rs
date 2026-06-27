use tui_textarea::{Input, Key};

use super::super::Action;
use crate::app::{App, PendingAttachment};

/// Normal mode key handling: main match block arm bodies
pub(super) fn handle_normal_keys(app: &mut App, input: Input) -> anyhow::Result<Option<Action>> {
    use super::update_slash_hint_detection;
    use super::{inject_at_mention_path, update_at_mention_detection};

    match input {
        // Ctrl+C: interrupt agent / double-tap to quit
        Input {
            key: Key::Char('c'),
            ctrl: true,
            ..
        } => {
            if let Some(action) = handle_ctrl_c(app) {
                return Ok(Some(action));
            }
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
        Input { key: Key::Up, .. } => handle_up(app),

        // Down: @ 提及导航 > hint navigation > history restore (only last row) > textarea cursor
        Input { key: Key::Down, .. } => handle_down(app),

        // Ctrl+V: try pasting clipboard image first, fallback to text paste
        // Loading 时同样允许——粘贴的文本/图片会进入 textarea / pending_attachments，
        // 后续 Enter 把消息 push 到 MessageQueue。
        Input {
            key: Key::Char('v'),
            ctrl: true,
            ..
        } => handle_ctrl_v(app),

        // Tab: @ 提及补全 > hint overlay candidate navigation and completion
        Input {
            key: Key::Tab,
            shift: false,
            ..
        } => handle_tab(app),

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
            inject_at_mention_path(app);
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
            app.session_mgr.current_mut().ui.textarea.input(Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }

        // Enter: submit (non-loading) or buffer (loading)
        Input {
            key: Key::Enter, ..
        } => {
            // 关闭可能残留的 @ mention 弹窗
            if app.session_mgr.current_mut().ui.at_mention.active {
                app.session_mgr.current_mut().ui.at_mention.close();
            }
            let text = app.session_mgr.current_mut().ui.textarea.lines().join("\n");
            let text = text.trim().to_string();
            if !text.is_empty() {
                if app.session_mgr.current_mut().ui.loading {
                    // Loading state: buffer to pending_messages（Agent 完成后自动提交）
                    app.session_mgr
                        .current_mut()
                        .messages
                        .pending_messages
                        .push(text);
                    app.session_mgr.current_mut().ui.textarea = crate::app::build_textarea(false);
                } else if text.starts_with('/') {
                    app.session_mgr.current_mut().ui.textarea = crate::app::build_textarea(false);
                    // SAFETY: command_registry is nested inside App; dispatch needs &mut App
                    let registry = std::mem::take(
                        &mut app.session_mgr.current_mut().commands.command_registry,
                    );
                    let known = registry.dispatch(app, &text);
                    app.session_mgr.current_mut().commands.command_registry = registry;
                    if known {
                        // Command matched, done
                    } else {
                        // Command not matched, try Skill matching
                        let skill_name: String = text
                            .trim_start_matches('/')
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                            .collect();
                        if let Some(_skill) = app
                            .session_mgr
                            .current_mut()
                            .commands
                            .skills
                            .iter()
                            .find(|s| s.name == skill_name)
                        {
                            // Skill matched: submit full message to agent
                            return Ok(Some(Action::Submit(text)));
                        } else if app
                            .session_mgr
                            .current_mut()
                            .commands
                            .agent_commands
                            .contains(&skill_name)
                        {
                            // Agent command matched (from ACP AvailableCommandsUpdate): submit to agent
                            tracing::debug!(skill_name, "Matched agent command, submitting to ACP");
                            return Ok(Some(Action::Submit(text)));
                        } else {
                            // 未知命令/Skill：作为普通输入提交给 Agent
                            tracing::debug!(
                                skill_name,
                                "Unknown slash command, submitting as normal input"
                            );
                            return Ok(Some(Action::Submit(text)));
                        }
                    }
                } else {
                    app.session_mgr.current_mut().ui.textarea = crate::app::build_textarea(false);
                    return Ok(Some(Action::Submit(text)));
                }
            }
        }

        // VS Code terminal maps Option+Backspace to PageUp; perform word-delete when textarea has content
        Input {
            key: Key::PageUp, ..
        } if std::env::var("TERM_PROGRAM").as_deref() == Ok("vscode") => {
            let session = &mut app.session_mgr.current_mut();
            let has_content = session
                .ui
                .textarea
                .lines()
                .iter()
                .any(|line| !line.is_empty());
            if has_content {
                session.ui.textarea.delete_word();
            }
        }

        // Ctrl+U / Ctrl+D: half-page scroll
        Input {
            key: Key::Char('u'),
            ctrl: true,
            ..
        } => {
            let session = &app.session_mgr.current_mut();
            let has_content = session
                .ui
                .textarea
                .lines()
                .iter()
                .any(|line| !line.is_empty());
            if has_content {
                app.session_mgr
                    .current_mut()
                    .ui
                    .textarea
                    .delete_line_by_head();
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
            app.session_mgr.current_mut().ui.textarea.input(input);
            // 任意输入清除 prediction
            app.session_mgr.current_mut().ui.prediction = None;
            // When input changes: reset cursor (don't pre-select; wait for user to press Tab/Up/Down)
            // Loading 时也需更新——用户在 queue 下一条消息时同样期望 slash hint / @mention 弹窗。
            app.session_mgr.current_mut().ui.hint_cursor = None;
            update_at_mention_detection(app);
            update_slash_hint_detection(app);
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

    Ok(Some(Action::Redraw))
}

// ── Per-arm helper functions ──────────────────────────────────────────────

fn handle_ctrl_c(app: &mut App) -> Option<Action> {
    let session = &mut app.session_mgr.current_mut();

    // 优先级 1: 输入框有内容 → 清空输入框
    if session.ui.textarea.lines().iter().any(|l| !l.is_empty()) {
        session
            .ui
            .textarea
            .move_cursor(tui_textarea::CursorMove::Head);
        session.ui.textarea.select_all();
        session.ui.textarea.cut();
        app.global_ui.quit_pending_since = None;
        return None;
    }

    // 优先级 2: Agent 运行中 → 中断 agent
    if session.ui.loading {
        app.interrupt();
        app.global_ui.quit_pending_since = None;
        return None;
    }

    // 优先级 3: Agent 未运行 → quit-pending 逻辑
    if let Some(since) = app.global_ui.quit_pending_since {
        if since.elapsed() < std::time::Duration::from_secs(2) {
            return Some(Action::Quit);
        } else {
            app.global_ui.quit_pending_since = Some(std::time::Instant::now());
        }
    } else {
        app.global_ui.quit_pending_since = Some(std::time::Instant::now());
    }
    None
}

fn handle_up(app: &mut App) {
    let hint_count = app.hint_candidates_count();
    if app.session_mgr.current_mut().ui.at_mention.active {
        app.session_mgr.current_mut().ui.at_mention.move_up();
    } else if hint_count > 0 {
        let cur = app.session_mgr.current_mut().ui.hint_cursor.unwrap_or(0);
        app.session_mgr.current_mut().ui.hint_cursor = if cur == 0 {
            Some(hint_count - 1)
        } else {
            Some(cur - 1)
        };
    } else {
        let (row, _col) = app.session_mgr.current_mut().ui.textarea.cursor();
        if row == 0 {
            app.history_up();
        } else {
            app.session_mgr.current_mut().ui.textarea.input(Input {
                key: Key::Up,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
    }
}

fn handle_down(app: &mut App) {
    let hint_count = app.hint_candidates_count();
    if app.session_mgr.current_mut().ui.at_mention.active {
        app.session_mgr.current_mut().ui.at_mention.move_down();
    } else if hint_count > 0 {
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
    } else if app.session_mgr.current_mut().ui.history_index.is_some() {
        app.history_down();
    } else {
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
        } else {
            app.session_mgr.current_mut().ui.textarea.input(Input {
                key: Key::Down,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
    }
}

fn handle_ctrl_v(app: &mut App) {
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
        } else if let Ok(text) = clipboard.get_text() {
            let text = text.replace('\r', "\n");
            app.session_mgr.current_mut().ui.textarea.insert_str(&text);
        }
    }
}

fn handle_tab(app: &mut App) {
    use super::inject_at_mention_path;

    // Prediction 接受优先级最高
    if let Some(pred) = app.session_mgr.current_mut().ui.prediction.take() {
        app.session_mgr
            .current_mut()
            .ui
            .textarea
            .insert_str(&pred.text);
        return;
    }

    if app.session_mgr.current_mut().ui.at_mention.active {
        inject_at_mention_path(app);
    } else {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::build_textarea;
    use crate::event::Action;

    async fn make_app() -> App {
        let (app, _) = App::new_headless(80, 24).await;
        app
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

        let result = handle_ctrl_c(&mut app);

        assert!(result.is_none(), "有内容时 Ctrl+C 不应返回 Quit");
        let lines = app.session_mgr.current_mut().ui.textarea.lines().to_vec();
        assert!(
            lines.iter().all(|l| l.is_empty()),
            "清空后 textarea 应为空，实际: {:?}",
            lines
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

        let result = handle_ctrl_c(&mut app);

        assert!(result.is_none(), "中断 agent 不应返回 Quit");
        assert!(
            app.global_ui.quit_pending_since.is_none(),
            "中断 agent 不应进入 quit-pending"
        );
    }

    #[tokio::test]
    async fn test_ctrl_c_enters_quit_pending_when_idle_and_empty() {
        let mut app = make_app().await;

        let result = handle_ctrl_c(&mut app);

        assert!(result.is_none(), "第一次 Ctrl+C 不应返回 Quit");
        assert!(
            app.global_ui.quit_pending_since.is_some(),
            "空闲时应进入 quit-pending"
        );

        let result = handle_ctrl_c(&mut app);
        assert!(
            matches!(result, Some(Action::Quit)),
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
        let result = handle_ctrl_c(&mut app);

        assert!(result.is_none(), "有内容时不应退出");
        assert!(
            app.global_ui.quit_pending_since.is_none(),
            "清空输入框应重置 quit-pending"
        );
    }
}
