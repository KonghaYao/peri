use crate::components::textarea::TextAreaState;
use crate::kit::atoms::{ACP_CLIENT_HANDLE, PLUGIN_SEARCH_RESULTS, PluginViewTab};
use crate::kit::list_nav::scroll_start_for_selected;
use crate::kit::panel_mouse::{ListLayout, hit_item, is_scrollbar_column};
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State};
use ratatui_kit::ratatui::layout::Rect;

use super::data::{get_discover_cache, refresh_discover_cache, refresh_marketplace_cache};
use super::{SearchState, VISIBLE_ITEMS, close_panel};

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_search_event(
    event: Event,
    area: Option<Rect>,
    active_tab: State<PluginViewTab>,
    search_text: State<TextAreaState>,
    search_focus: State<bool>,
    search_state: State<SearchState>,
    discover_cursor: State<usize>,
    discover_filtered: State<Vec<usize>>,
    discover_detail_idx: State<Option<usize>>,
    discover_detail_action: State<usize>,
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
            || discover_detail_idx.read().is_some()
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
                // 搜索框聚焦：点击搜索框行 = 触发远程搜索（Enter @L351）
                if *search_focus.read() {
                    if matches!(*search_state.read(), SearchState::Idle)
                        && hit_item(
                            &mouse,
                            area,
                            ListLayout {
                                header_rows: 3,
                                item_rows: 1,
                                footer_rows: 0,
                                visible_items: 1,
                                scroll_start: 0,
                                item_count: 1,
                            },
                        )
                        .is_some()
                    {
                        let q = search_text.read().text.clone();
                        if !q.is_empty() {
                            *search_state.write() = SearchState::Loading;
                            PLUGIN_SEARCH_RESULTS.state().write().clear();
                            let query = q.clone();
                            if let Some(client_handle) = ACP_CLIENT_HANDLE.get() {
                                let client = client_handle.clone();
                                let sid = client.current_session_id().unwrap_or_default();
                                tokio::spawn(async move {
                                    let params = serde_json::json!({
                                        "query": query,
                                        "sessionId": sid,
                                    });
                                    if let Err(e) =
                                        client.send_raw_request("plugin/search", params).await
                                    {
                                        tracing::warn!(error = %e, "plugin search RPC failed");
                                    }
                                });
                            } else {
                                tracing::warn!(target: "plugin-panel", "ACP_CLIENT_HANDLE not set, search skipped");
                                *search_state.write() =
                                    SearchState::Error("ACP client not available".into());
                            }
                        }
                    }
                    return match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
                        _ => EventResult::Ignored,
                    };
                }
                // 列表（未聚焦）：Loading/Error 时列表未渲染，仅消费
                if !matches!(*search_state.read(), SearchState::Idle) {
                    return match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
                        _ => EventResult::Ignored,
                    };
                }
                // 复刻渲染的可见列表 → 原始 cache 索引映射（Enter @L428）
                let query = search_text.read().text.to_lowercase();
                let has_remote = !PLUGIN_SEARCH_RESULTS.state().read().is_empty();
                let items = get_discover_cache();
                let orig_indices: Vec<usize> = if !query.is_empty() && has_remote {
                    // 远程结果展示：无本地 cache 索引映射，仅消费
                    Vec::new()
                } else if query.is_empty() {
                    (0..items.len()).collect()
                } else {
                    let filtered_indices = discover_filtered.read().clone();
                    if filtered_indices.is_empty() {
                        items
                            .iter()
                            .enumerate()
                            .filter(|(_, item)| {
                                item.name.to_lowercase().contains(&query)
                                    || item.description.to_lowercase().contains(&query)
                                    || item.marketplace.to_lowercase().contains(&query)
                            })
                            .map(|(i, _)| i)
                            .collect()
                    } else {
                        filtered_indices
                            .iter()
                            .copied()
                            .filter(|&i| i < items.len())
                            .collect()
                    }
                };
                let disc_scroll = scroll_start_for_selected(
                    *discover_cursor.read(),
                    orig_indices.len(),
                    VISIBLE_ITEMS,
                );
                if let Some(idx) = hit_item(
                    &mouse,
                    area,
                    ListLayout {
                        header_rows: 5,
                        item_rows: 3,
                        footer_rows: 0,
                        visible_items: VISIBLE_ITEMS as u16,
                        scroll_start: disc_scroll,
                        item_count: orig_indices.len(),
                    },
                ) {
                    *discover_cursor.write() = idx;
                    if let Some(&orig) = orig_indices.get(idx) {
                        *discover_detail_idx.write() = Some(orig);
                        *discover_detail_action.write() = 0;
                    }
                    return EventResult::Consumed;
                }
                return match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
                    _ => EventResult::Ignored,
                };
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
        if discover_detail_idx.read().is_some() {
            // let discover detail handler manage Esc
            return EventResult::Ignored;
        }
        if *active_tab.read() == PluginViewTab::Discover {
            return EventResult::Ignored;
        }
        close_panel();
        return EventResult::Consumed;
    }

    let in_detail = detail_plugin_idx.read().is_some();
    let in_discover_detail = discover_detail_idx.read().is_some();
    let in_marketplace_detail = marketplace_detail.read().is_some();
    if in_detail || in_discover_detail || in_marketplace_detail || confirm_action.read().is_some() {
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

    if *active_tab.read() != PluginViewTab::Discover {
        return EventResult::Ignored;
    }

    // Discover tab: search focus mode 或 filter 模式
    // guard arm 中的临时 guard 会存活到整个 arm 体，arm 内对
    // search_focus 的 write() 会死锁——先提取为 bool。
    let search_active = *search_focus.read();
    match key.code {
        // 搜索框已激活 → 进入搜索输入模式
        _ if search_active => match key.code {
            KeyCode::Char(c) => {
                let mut t = search_text.write();
                t.insert_char(c);
                EventResult::Consumed
            }
            KeyCode::Backspace => {
                search_text.write().backspace();
                EventResult::Consumed
            }
            KeyCode::Enter => {
                let q = search_text.read().text.clone();
                if !q.is_empty() {
                    *search_state.write() = SearchState::Loading;
                    PLUGIN_SEARCH_RESULTS.state().write().clear();
                    let query = q.clone();
                    if let Some(client_handle) = ACP_CLIENT_HANDLE.get() {
                        let client = client_handle.clone();
                        let sid = client.current_session_id().unwrap_or_default();
                        tokio::spawn(async move {
                            let params = serde_json::json!({
                                "query": query,
                                "sessionId": sid,
                            });
                            if let Err(e) = client.send_raw_request("plugin/search", params).await {
                                tracing::warn!(error = %e, "plugin search RPC failed");
                            }
                        });
                    } else {
                        tracing::warn!(target: "plugin-panel", "ACP_CLIENT_HANDLE not set, search skipped");
                        *search_state.write() =
                            SearchState::Error("ACP client not available".into());
                    }
                }
                EventResult::Consumed
            }
            KeyCode::Esc => {
                let mut t = search_text.write();
                if t.text.is_empty() {
                    *search_focus.write() = false;
                } else {
                    t.text.clear();
                    t.cursor = 0;
                }
                EventResult::Consumed
            }
            // Left/Right 透明传给 Normal handler（保持 tab 切换）
            KeyCode::Left | KeyCode::Right => EventResult::Ignored,
            _ => EventResult::Ignored,
        },
        // ── 未聚焦搜索框：Char/Backspace 启动实时过滤，Enter 进入详情 ──
        KeyCode::Char(c) => {
            search_text.write().insert_char(c);
            // 实时过滤 discover 列表
            let items = get_discover_cache();
            let query = search_text.read().text.to_lowercase();
            let filtered: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    item.name.to_lowercase().contains(&query)
                        || item.description.to_lowercase().contains(&query)
                        || item.marketplace.to_lowercase().contains(&query)
                })
                .map(|(i, _)| i)
                .collect();
            *discover_filtered.write() = filtered;
            *discover_cursor.write() = 0;
            EventResult::Consumed
        }
        KeyCode::Backspace => {
            search_text.write().backspace();
            let items = get_discover_cache();
            let query = search_text.read().text.to_lowercase();
            let filtered: Vec<usize> = if query.is_empty() {
                (0..items.len()).collect()
            } else {
                items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| {
                        item.name.to_lowercase().contains(&query)
                            || item.description.to_lowercase().contains(&query)
                            || item.marketplace.to_lowercase().contains(&query)
                    })
                    .map(|(i, _)| i)
                    .collect()
            };
            *discover_filtered.write() = filtered;
            *discover_cursor.write() = 0;
            EventResult::Consumed
        }
        KeyCode::Enter => {
            // 进入 discover 详情页
            let items = get_discover_cache();
            let filtered = discover_filtered.read().clone();
            let cursor = *discover_cursor.read();
            if let Some(&orig_idx) = filtered.get(cursor)
                && orig_idx < items.len()
            {
                *discover_detail_idx.write() = Some(orig_idx);
                *discover_detail_action.write() = 0;
            }
            EventResult::Consumed
        }
        // Left/Right 透明传给 Normal handler（保持 tab 切换）
        KeyCode::Left | KeyCode::Right => EventResult::Ignored,
        _ => EventResult::Ignored,
    }
}
