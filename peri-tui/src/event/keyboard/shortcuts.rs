use ratatui::crossterm::event::KeyCode;

use super::super::Action;
use super::{
    SHORTCUT_BG_BAR, SHORTCUT_CTRL_CYCLE_MODE, SHORTCUT_CTRL_CYCLE_PROVIDER, SHORTCUT_CYCLE_MODE,
    SHORTCUT_CYCLE_PROVIDER,
};
use crate::app::{App, MessageViewModel};

/// 处理全局快捷键：BackTab（权限循环）、Ctrl+B（bg bar）、Ctrl+T（模型切换）、Ctrl+Shift+T（Provider 切换）、Ctrl+O（diff 切换）
pub(super) fn handle_shortcuts(
    app: &mut App,
    key_event: &ratatui::crossterm::event::KeyEvent,
) -> Option<Action> {
    // Shift+Tab (BackTab): cycle permission mode
    if matches!(key_event.code, KeyCode::BackTab) {
        let _new_mode = app.services.permission_mode.cycle();
        app.global_ui.mode_highlight_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
        return Some(Action::Redraw);
    }

    // Ctrl+O: toggle inline diff (only when OAuth popup is NOT active)
    if key_event
        .modifiers
        .contains(ratatui::crossterm::event::KeyModifiers::CONTROL)
        && matches!(key_event.code, KeyCode::Char('o'))
    {
        if app.global_ui.oauth_prompt.is_none() {
            app.toggle_diff();
        }
        return Some(Action::Redraw);
    }

    // Ctrl+B: 跳转到后台 agent bar
    if SHORTCUT_BG_BAR.matches(key_event) {
        if !app.session_mgr.current_mut().background_agents.is_empty() {
            app.session_mgr.current_mut().ui.bg_bar_cursor = Some(0);
        }
        return Some(Action::Redraw);
    }

    // Ctrl+T / Alt+M: cycle model aliases
    if SHORTCUT_CTRL_CYCLE_MODE.matches(key_event) || SHORTCUT_CYCLE_MODE.matches(key_event) {
        {
            let cfg_arc = app.services.peri_config.clone();
            let mut cfg = cfg_arc.write();
            let aliases = ["opus", "sonnet", "haiku"];
            let current = cfg.config.active_alias.as_str();
            let idx = aliases.iter().position(|&a| a == current).unwrap_or(0);
            let next = aliases[(idx + 1) % aliases.len()];
            cfg.config.active_alias = next.to_string();
            if let Err(e) = App::save_config(&cfg, app.services.config_path_override.as_deref()) {
                app.session_mgr
                    .current_mut()
                    .messages
                    .view_messages
                    .push(MessageViewModel::system(format!("配置保存失败: {}", e)));
            }
            if let Some(p) = crate::app::agent::LlmProvider::from_config(&cfg) {
                app.services.provider_name = p.display_name().to_string();
                app.services.model_name = p.model_name().to_string();
            }
            if let Some(ref acp_client) = app.acp_client {
                let acp = acp_client.clone();
                let alias = next.to_string();
                tokio::spawn(async move {
                    let _ = acp.set_config_option("model", &alias).await;
                });
            }
            app.global_ui.model_highlight_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
        }
        return Some(Action::Redraw);
    }

    // Ctrl+Shift+T / Alt+Shift+M: cycle providers
    if SHORTCUT_CTRL_CYCLE_PROVIDER.matches(key_event) || SHORTCUT_CYCLE_PROVIDER.matches(key_event)
    {
        {
            let cfg_arc = app.services.peri_config.clone();
            let mut cfg = cfg_arc.write();
            let providers_len = cfg.config.providers.len();
            if providers_len > 1 {
                let current_id = cfg.config.active_provider_id.as_str();
                let next_id = {
                    let providers = &cfg.config.providers;
                    let idx = providers
                        .iter()
                        .position(|p| p.id == current_id)
                        .unwrap_or(0);
                    let next_idx = (idx + 1) % providers.len();
                    providers[next_idx].id.clone()
                };
                cfg.config.active_provider_id = next_id;
                if let Some(p) = crate::app::agent::LlmProvider::from_config(&cfg) {
                    app.services.provider_name = p.display_name().to_string();
                    app.services.model_name = p.model_name().to_string();
                }
                if let Err(e) = App::save_config(&cfg, app.services.config_path_override.as_deref())
                {
                    app.session_mgr
                        .current_mut()
                        .messages
                        .view_messages
                        .push(MessageViewModel::system(format!("配置保存失败: {}", e)));
                }
                app.global_ui.provider_highlight_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(2000));
            }
        }
        return Some(Action::Redraw);
    }

    None
}
