use tui_textarea::Input;

use super::super::Action;
use crate::{
    app::{
        betas_panel::BetasPanel,
        panel_manager::{EventResult, PanelKind},
        App,
    },
    with_global_panels, with_session_panels,
};

/// PanelManager 分发：先处理 session panels，再处理 global panels
pub(super) fn handle_panels(app: &mut App, input: &Input) -> Option<Action> {
    // Session panels: Model, Agent, Hooks, Login, Config, ThreadBrowser
    let session_kind = app.session_mgr.current_mut().session_panels.active_kind();
    if matches!(
        session_kind,
        Some(PanelKind::Model)
            | Some(PanelKind::Agent)
            | Some(PanelKind::Hooks)
            | Some(PanelKind::Login)
            | Some(PanelKind::Config)
            | Some(PanelKind::ThreadBrowser)
    ) {
        with_session_panels!(app, |sp, ctx| {
            let result = sp.dispatch_key(input.clone(), &mut ctx);
            match result {
                EventResult::ClosePanel => {
                    sp.close();
                    app.session_mgr.current_mut().ui.panel_selection.clear();
                    app.session_mgr.current_mut().ui.panel_area = None;
                }
                EventResult::OpenThread(thread_id) => {
                    sp.close();
                    app.session_mgr.current_mut().ui.panel_selection.clear();
                    app.session_mgr.current_mut().ui.panel_area = None;
                    // with_session_panels! macro puts sp back at closure end,
                    // but OpenThread needs to put back first then call open_thread_with_feedback
                    app.session_mgr.current_mut().session_panels = sp;
                    // Early return prevents macro from putting back again
                    app.open_thread_with_feedback(thread_id);
                    return Some(Action::Redraw);
                }
                _ => {}
            }
            result
        });
        return Some(Action::Redraw);
    }

    // Global panels: Status, Memory, Mcp, Cron, Plugin
    let global_kind = app.global_panels.active_kind();
    if matches!(
        global_kind,
        Some(PanelKind::Status)
            | Some(PanelKind::Memory)
            | Some(PanelKind::Mcp)
            | Some(PanelKind::Cron)
            | Some(PanelKind::Plugin)
            | Some(PanelKind::Betas)
    ) {
        with_global_panels!(app, |pm, ctx| {
            let result = pm.dispatch_key(input.clone(), &mut ctx);
            match result {
                EventResult::ClosePanel => {
                    pm.close();
                    app.session_mgr.current_mut().ui.panel_selection.clear();
                    app.session_mgr.current_mut().ui.panel_area = None;
                }
                EventResult::OpenPanel(PanelKind::Memory) => {
                    app.global_panels = pm;
                    if let Err(e) = app.memory_panel_open_editor() {
                        tracing::error!("Failed to open editor: {}", e);
                    }
                    return Some(Action::Redraw);
                }
                EventResult::Consumed
                    if global_kind == Some(PanelKind::Betas) && is_toggle_key(input) =>
                {
                    // Beta 切换后即时保存
                    if let Some(panel) = pm.get::<BetasPanel>() {
                        let cfg = ctx.services.peri_config.clone();
                        let mut cfg_guard = cfg.write();
                        panel.apply_to_config(&mut cfg_guard);
                        let _ = App::save_config(
                            &cfg_guard,
                            ctx.services.config_path_override.as_deref(),
                        );
                    }
                }
                _ => {}
            }
            result
        });
        return Some(Action::Redraw);
    }

    None
}

/// 判断是否为切换键（Left/Right/Space）
fn is_toggle_key(input: &Input) -> bool {
    use tui_textarea::Key;
    matches!(
        input,
        Input {
            key: Key::Left,
            ctrl: false,
            ..
        } | Input {
            key: Key::Right,
            ctrl: false,
            ..
        } | Input {
            key: Key::Char(' '),
            ctrl: false,
            ..
        }
    )
}
