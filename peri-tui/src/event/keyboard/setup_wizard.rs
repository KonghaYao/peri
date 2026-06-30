use tui_textarea::{Input, Key};

use super::super::Action;
use crate::app::App;

/// Setup wizard 模式下拦截所有按键
pub(super) fn handle_setup_wizard(app: &mut App, input: &Input) -> Option<Action> {
    if app.global_ui.setup_wizard.is_none() {
        return None;
    }

    // Ctrl+C: exit flow (matching normal-mode behaviour)
    if matches!(
        input,
        Input {
            key: Key::Char('c'),
            ctrl: true,
            ..
        }
    ) {
        if let Some(since) = app.global_ui.quit_pending_since {
            if since.elapsed() < std::time::Duration::from_secs(2) {
                return Some(Action::Quit);
            } else {
                app.global_ui.quit_pending_since = Some(std::time::Instant::now());
            }
        } else {
            app.global_ui.quit_pending_since = Some(std::time::Instant::now());
        }
        return Some(Action::Redraw);
    }

    let input_clone = input.clone();
    if let Some(ref mut wizard) = app.global_ui.setup_wizard {
        if let Some(action) = crate::app::setup_wizard::handle_setup_wizard_key(wizard, input_clone)
        {
            match action {
                crate::app::setup_wizard::SetupWizardAction::SaveAndClose => {
                    let wizard = app
                        .global_ui
                        .setup_wizard
                        .take()
                        .expect("setup_wizard must be Some (checked above)");
                    match crate::app::setup_wizard::save_setup(&wizard) {
                        Ok(cfg) => app.refresh_after_setup(cfg),
                        Err(e) => {
                            let msg_text = app.services.lc.tr_args(
                                "config-save-failed",
                                &[("error".into(), e.to_string().into())],
                            );
                            app.session_mgr
                                .current_mut()
                                .messages
                                .push_system_note(msg_text);
                            app.render_rebuild();
                        }
                    }
                }
                crate::app::setup_wizard::SetupWizardAction::Skip => {
                    let from_command = app
                        .global_ui
                        .setup_wizard
                        .as_ref()
                        .map(|w| w.from_command)
                        .unwrap_or(false);
                    app.global_ui.setup_wizard = None;
                    if !from_command {
                        return Some(Action::Quit);
                    }
                }
                crate::app::setup_wizard::SetupWizardAction::SetLanguage(lang) => {
                    let _ = app.services.lc.switch(&lang);
                }
                crate::app::setup_wizard::SetupWizardAction::Redraw => {}
            }
        }
    }
    Some(Action::Redraw)
}
