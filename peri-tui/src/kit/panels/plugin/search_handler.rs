use crate::components::textarea::TextAreaState;
use crate::kit::atoms::{ACP_CLIENT_HANDLE, PluginViewTab};
use crate::kit::panel_mouse::is_scrollbar_column;
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State};
use ratatui_kit::ratatui::layout::Rect;

use super::data::{refresh_discover_cache, refresh_marketplace_cache};
use super::{close_panel, discover::DiscoverState};

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_search_event(
    event: Event,
    area: Option<Rect>,
    active_tab: State<PluginViewTab>,
    discover: State<DiscoverState>,
    detail_plugin_idx: State<Option<usize>>,
    marketplace_detail: State<Option<usize>>,
    marketplace_detail_action: State<usize>,
    confirm_action: State<Option<String>>,
    operation_loading: State<Option<String>>,
    add_marketplace_input: State<TextAreaState>,
    add_marketplace_active: State<bool>,
) -> EventResult {
    // 鼠标：add_marketplace 输入与 Discover tab（click as enter）
    if let Event::Mouse(mouse) = event {
        // 详情/confirm 模式由 Normal handler 负责命中
        if detail_plugin_idx.read().is_some()
            || marketplace_detail.read().is_some()
            || confirm_action.read().is_some()
        {
            return EventResult::Ignored;
        }
        if let Some(area) = area
            && !is_scrollbar_column(&mouse, area)
        {
            // add_marketplace 输入模式：点击仅消费（文本输入，无动作）
            if *add_marketplace_active.read() {
                return match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
                    _ => EventResult::Ignored,
                };
            }
            if *active_tab.read() == PluginViewTab::Discover {
                return super::discover_handler::handle_event(
                    event,
                    Some(area),
                    discover,
                    operation_loading,
                );
            }
        }
        return EventResult::Ignored;
    }
    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }

    if *active_tab.read() == PluginViewTab::Discover
        && detail_plugin_idx.read().is_none()
        && marketplace_detail.read().is_none()
        && confirm_action.read().is_none()
        && !*add_marketplace_active.read()
    {
        // Existing operation-loading cleanup remains owned by Normal handler.
        if operation_loading.read().is_some() {
            return EventResult::Ignored;
        }
        return super::discover_handler::handle_event(event, area, discover, operation_loading);
    }

    // ── ESC handling: detail exit / confirm cancel / close panel ──
    if key.code == KeyCode::Esc {
        if detail_plugin_idx.read().is_some() {
            *detail_plugin_idx.write() = None;
            return EventResult::Consumed;
        }
        if marketplace_detail.read().is_some() {
            *marketplace_detail.write() = None;
            *marketplace_detail_action.write() = 0;
            return EventResult::Consumed;
        }
        if confirm_action.read().is_some() {
            *confirm_action.write() = None;
            *operation_loading.write() = None;
            return EventResult::Consumed;
        }
        close_panel();
        return EventResult::Consumed;
    }

    let in_detail = detail_plugin_idx.read().is_some();
    let in_marketplace_detail = marketplace_detail.read().is_some();
    if in_detail || in_marketplace_detail || confirm_action.read().is_some() {
        return EventResult::Ignored;
    }
    // marketplace add 输入模式
    if *add_marketplace_active.read() {
        return match key.code {
            KeyCode::Enter => {
                let url = add_marketplace_input.read().text.clone();
                if !url.is_empty() {
                    let result = peri_middlewares::plugin::parse_marketplace_input(&url);
                    match result {
                        Ok(source) => {
                            let name =
                                peri_middlewares::plugin::MarketplaceManager::extract_name(&source);
                            // 将同步磁盘 I/O 移到 dedicated blocking thread，
                            // 避免阻塞 TUI 主事件循环。
                            let source_for_blocking = source.clone();
                            let name_for_refresh = name.clone();
                            tokio::spawn(async move {
                                let added = tokio::task::spawn_blocking(move || {
                                    let mut marketplaces =
                                        peri_middlewares::plugin::load_known_marketplaces(None)
                                            .unwrap_or_default();
                                    let already_exists = marketplaces
                                        .iter()
                                        .any(|km| km.source == source_for_blocking);
                                    if already_exists {
                                        return false;
                                    }
                                    marketplaces.push(peri_middlewares::plugin::KnownMarketplace {
                                        source: source_for_blocking,
                                        install_location: String::new(),
                                        auto_update: false,
                                        last_updated: String::new(),
                                    });
                                    let _ = peri_middlewares::plugin::save_known_marketplaces(
                                        &marketplaces,
                                        None,
                                    );
                                    refresh_discover_cache();
                                    refresh_marketplace_cache();
                                    true
                                })
                                .await
                                .unwrap();
                                if added && let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                    let client = cl.clone();
                                    let sid = client.current_session_id().unwrap_or_default();
                                    let _ = client
                                        .send_raw_request(
                                            "marketplace/refresh",
                                            serde_json::json!({
                                                "name": name_for_refresh,
                                                "sessionId": sid,
                                            }),
                                        )
                                        .await;
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!(target: "plugin-panel", error = %e, "invalid marketplace input");
                        }
                    }
                }
                *add_marketplace_input.write() = TextAreaState::default();
                *add_marketplace_active.write() = false;
                EventResult::Consumed
            }
            KeyCode::Esc => {
                *add_marketplace_input.write() = TextAreaState::default();
                *add_marketplace_active.write() = false;
                EventResult::Consumed
            }
            KeyCode::Char(c) => {
                add_marketplace_input.write().insert_char(c);
                EventResult::Consumed
            }
            KeyCode::Backspace => {
                add_marketplace_input.write().backspace();
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        };
    }

    EventResult::Ignored
}
