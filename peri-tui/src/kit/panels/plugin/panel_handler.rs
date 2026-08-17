use crate::components::textarea::TextAreaState;
use crate::kit::atoms::{ACP_CLIENT_HANDLE, PLUGIN_LIST, PluginViewTab};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{ListLayout, hit_item, hit_row, is_scrollbar_column};
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State};
use ratatui_kit::ratatui::layout::Rect;

use super::data::{
    get_discover_cache, get_marketplace_cache, refresh_discover_cache, refresh_marketplace_cache,
};
use super::{DiscoverDetailAction, VISIBLE_ITEMS, action_list, cycle_backward, cycle_forward};

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_panel_event(
    event: Event,
    area: Option<Rect>,
    selected: State<usize>,
    active_tab: State<PluginViewTab>,
    action_index: State<usize>,
    confirm_action: State<Option<String>>,
    operation_loading: State<Option<String>>,
    detail_plugin_idx: State<Option<usize>>,
    discover_cursor: State<usize>,
    discover_detail_idx: State<Option<usize>>,
    discover_detail_action: State<usize>,
    discover_filtered: State<Vec<usize>>,
    marketplace_detail: State<Option<usize>>,
    marketplace_detail_action: State<usize>,
    marketplace_refreshing: State<bool>,
    search_text: State<TextAreaState>,
    add_marketplace_input: State<TextAreaState>,
    add_marketplace_active: State<bool>,
) -> EventResult {
    // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
    if let Event::Mouse(mouse) = event {
        if let Some(area) = area
            && !is_scrollbar_column(&mouse, area)
        {
            // ── 确认模式（uninstall / delete_marketplace）：点击 = 确认（Enter @L625）──
            if let Some(action) = confirm_action.read().clone()
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            {
                let saved_loading = operation_loading.read().clone();
                match action.as_str() {
                    "uninstall" => {
                        let idx = detail_plugin_idx.read().unwrap_or(0);
                        if let Some(p) = PLUGIN_LIST.state().read().get(idx) {
                            let plugin_id = if p.marketplace.is_empty() {
                                p.name.clone()
                            } else {
                                format!("{}@{}", p.name, p.marketplace)
                            };
                            if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                let client = cl.clone();
                                let sid = client.current_session_id().unwrap_or_default();
                                tokio::spawn(async move {
                                    let _ = client
                                        .send_raw_request(
                                            "plugin/uninstall",
                                            serde_json::json!({
                                                "pluginId": plugin_id,
                                                "sessionId": sid,
                                            }),
                                        )
                                        .await;
                                });
                            }
                        }
                        *detail_plugin_idx.write() = None;
                        // 关闭确认弹窗（独立线程避免 RwLock 重入）
                        std::thread::spawn(move || {
                            *confirm_action.write() = None;
                            *operation_loading.write() = None;
                        });
                    }
                    "delete_marketplace" => {
                        let name = saved_loading.unwrap_or_default();
                        std::thread::spawn(move || {
                            let marketplaces =
                                peri_middlewares::plugin::load_known_marketplaces(None)
                                    .unwrap_or_default();
                            let filtered: Vec<_> = marketplaces
                                .into_iter()
                                .filter(|km| {
                                    peri_middlewares::plugin::MarketplaceManager::extract_name(
                                        &km.source,
                                    ) != name
                                })
                                .collect();
                            let _ =
                                peri_middlewares::plugin::save_known_marketplaces(&filtered, None);
                            refresh_discover_cache();
                            refresh_marketplace_cache();
                            *confirm_action.write() = None;
                            *operation_loading.write() = None;
                        });
                    }
                    _ => {
                        // 未知确认动作：安全起见也从独立线程写 state
                        std::thread::spawn(move || {
                            *confirm_action.write() = None;
                            *operation_loading.write() = None;
                        });
                    }
                }
                return EventResult::Consumed;
            }
            // ── Discover 详情：点击 action 行 = 执行（Enter @L487）──
            // if-let scrutinee 中的临时 guard 会存活到整个块结束，
            // 块内对同一 atom 的 write() 会死锁——先提取为 bool。
            let in_discover_detail = discover_detail_idx.read().is_some();
            if in_discover_detail
                && let Some(idx) = hit_item(
                    &mouse,
                    area,
                    ListLayout {
                        header_rows: 10,
                        item_rows: 1,
                        footer_rows: 0,
                        visible_items: 3,
                        scroll_start: 0,
                        item_count: 3,
                    },
                )
            {
                *discover_detail_action.write() = idx;
                let disc_idx = discover_detail_idx.read().unwrap_or(0);
                match DiscoverDetailAction::ALL.get(idx).copied() {
                    Some(DiscoverDetailAction::InstallUser) => {
                        let items = get_discover_cache();
                        if let Some(dp) = items.get(disc_idx) {
                            let name = dp.name.clone();
                            let marketplace = dp.marketplace.clone();
                            *operation_loading.write() = Some("install".into());
                            if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                let client = cl.clone();
                                let sid = client.current_session_id().unwrap_or_default();
                                tokio::spawn(async move {
                                    let _ = client
                                        .send_raw_request(
                                            "plugin/install",
                                            serde_json::json!({
                                                "name": name,
                                                "marketplace": marketplace,
                                                "scope": "user",
                                                "sessionId": sid,
                                            }),
                                        )
                                        .await;
                                });
                            }
                        }
                        *discover_detail_idx.write() = None;
                        *discover_detail_action.write() = 0;
                    }
                    Some(DiscoverDetailAction::InstallProject) => {
                        let items = get_discover_cache();
                        if let Some(dp) = items.get(disc_idx) {
                            let name = dp.name.clone();
                            let marketplace = dp.marketplace.clone();
                            *operation_loading.write() = Some("install".into());
                            if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                let client = cl.clone();
                                let sid = client.current_session_id().unwrap_or_default();
                                tokio::spawn(async move {
                                    let _ = client
                                        .send_raw_request(
                                            "plugin/install",
                                            serde_json::json!({
                                                "name": name,
                                                "marketplace": marketplace,
                                                "scope": "project",
                                                "sessionId": sid,
                                            }),
                                        )
                                        .await;
                                });
                            }
                        }
                        *discover_detail_idx.write() = None;
                        *discover_detail_action.write() = 0;
                    }
                    Some(DiscoverDetailAction::BackToList) => {
                        *discover_detail_idx.write() = None;
                        *discover_detail_action.write() = 0;
                    }
                    None => {}
                }
                return EventResult::Consumed;
            }
            // ── Marketplace 详情：点击 action 行 = 执行（Enter @L577）──
            // 同上：提取为 bool，避免 if-let scrutinee guard 存活到块内
            let in_marketplace_detail = marketplace_detail.read().is_some();
            if in_marketplace_detail
                && let Some(idx) = hit_item(
                    &mouse,
                    area,
                    ListLayout {
                        header_rows: 7,
                        item_rows: 1,
                        footer_rows: 0,
                        visible_items: 2,
                        scroll_start: 0,
                        item_count: 2,
                    },
                )
            {
                *marketplace_detail_action.write() = idx;
                let s = marketplace_detail.read().unwrap_or(0);
                let entries = get_marketplace_cache(); // 非阻塞缓存读取
                if let Some(entry) = entries.get(s.saturating_sub(1)) {
                    match idx {
                        0 => {
                            // Refresh
                            let name = entry.source_label.clone();
                            *marketplace_refreshing.write() = true;
                            if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                let client = cl.clone();
                                let sid = client.current_session_id().unwrap_or_default();
                                let name_for_refresh = name.clone();
                                tokio::spawn(async move {
                                    let _ = client
                                        .send_raw_request(
                                            "marketplace/refresh",
                                            serde_json::json!({
                                                "name": name_for_refresh,
                                                "sessionId": sid,
                                            }),
                                        )
                                        .await;
                                });
                            }
                            // 将缓存刷新（同步 I/O）移到 blocking thread
                            tokio::task::spawn_blocking(|| {
                                refresh_discover_cache();
                                refresh_marketplace_cache();
                            });
                        }
                        _ => {
                            // Delete
                            *confirm_action.write() = Some("delete_marketplace".into());
                            *operation_loading.write() = Some(entry.name.clone());
                        }
                    }
                }
                *marketplace_detail.write() = None;
                *marketplace_detail_action.write() = 0;
                return EventResult::Consumed;
            }
            // ── Installed 详情：点击 action 行 = 执行（Enter @L738）──
            // 提取 detail_plugin_idx 的 guard：if-let scrutinee 临时 guard
            // 存活到块结束，块内多次 write detail_plugin_idx 会死锁。
            let detail_plugin_opt = *detail_plugin_idx.read();
            if detail_plugin_opt.is_some()
                && let Some(detail) = detail_plugin_opt
                && let Some(detail_p) = PLUGIN_LIST.state().read().get(detail).cloned()
                && let Some(idx) = hit_item(
                    &mouse,
                    area,
                    ListLayout {
                        // 标题 + 空行 + 状态 + 空行 + 4 字段 + 空行 + 4 capabilities
                        // + [可选 error 2 行] + 空行 + actions 标题 + 空行
                        header_rows: if detail_p.load_error.is_some() {
                            18
                        } else {
                            16
                        },
                        item_rows: 1,
                        footer_rows: 0,
                        visible_items: 4,
                        scroll_start: 0,
                        item_count: 4,
                    },
                )
            {
                *action_index.write() = idx;
                let actions = action_list(detail_p.enabled);
                if let Some(action) = actions.get(idx) {
                    match *action {
                        "uninstall" => {
                            *confirm_action.write() = Some("uninstall".into());
                        }
                        "back" => {
                            *detail_plugin_idx.write() = None;
                        }
                        "enable" | "disable" => {
                            *operation_loading.write() = Some(action.to_string());
                            let detail = detail_plugin_idx.read().unwrap_or(0);
                            let plugin_info = PLUGIN_LIST.state().read().get(detail).cloned();
                            if let Some(p) = plugin_info {
                                let plugin_id = if p.marketplace.is_empty() {
                                    p.name.clone()
                                } else {
                                    format!("{}@{}", p.name, p.marketplace)
                                };
                                let plugin_id_for_persist = plugin_id.clone();
                                let enable = *action == "enable";
                                let scope = p.install_scope.clone();
                                if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                    let client = cl.clone();
                                    let sid = client.current_session_id().unwrap_or_default();
                                    tokio::spawn(async move {
                                        let _ = client
                                            .send_raw_request(
                                                "plugin/toggle",
                                                serde_json::json!({
                                                    "pluginId": plugin_id,
                                                    "enable": enable,
                                                    "scope": scope,
                                                    "sessionId": sid,
                                                }),
                                            )
                                            .await;
                                    });
                                }
                                // 将同步写盘移到 blocking thread，避免阻塞 TUI 主事件循环
                                tokio::task::spawn_blocking(move || {
                                    let _ = peri_middlewares::plugin::save_claude_settings_enabled_plugins(
                                        &[(plugin_id_for_persist, enable)],
                                        None,
                                    );
                                });
                            }
                            *detail_plugin_idx.write() = None;
                        }
                        "update" => {
                            *operation_loading.write() = Some("update".into());
                            let detail = detail_plugin_idx.read().unwrap_or(0);
                            let p = PLUGIN_LIST.state().read().get(detail).cloned();
                            if let Some(p) = p {
                                let plugin_id = if p.marketplace.is_empty() {
                                    p.name.clone()
                                } else {
                                    format!("{}@{}", p.name, p.marketplace)
                                };
                                if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                    let client = cl.clone();
                                    let sid = client.current_session_id().unwrap_or_default();
                                    tokio::spawn(async move {
                                        let _ = client
                                            .send_raw_request(
                                                "plugin/update",
                                                serde_json::json!({
                                                    "pluginId": plugin_id,
                                                    "sessionId": sid,
                                                }),
                                            )
                                            .await;
                                    });
                                }
                            }
                            *detail_plugin_idx.write() = None;
                        }
                        other => {
                            tracing::info!(target: "plugin-panel", "unknown action {} on {}", other, detail_p.name);
                        }
                    }
                }
                return EventResult::Consumed;
            }
            // ── 列表模式 ──
            match *active_tab.read() {
                PluginViewTab::Installed => {
                    let count = PLUGIN_LIST.state().read().len();
                    let scroll_start =
                        scroll_start_for_selected(*selected.read(), count, VISIBLE_ITEMS);
                    if let Some(idx) = hit_item(
                        &mouse,
                        area,
                        ListLayout {
                            header_rows: 3,
                            item_rows: 4,
                            footer_rows: 0,
                            visible_items: VISIBLE_ITEMS as u16,
                            scroll_start,
                            item_count: count,
                        },
                    ) {
                        *selected.write() = idx;
                        *detail_plugin_idx.write() = Some(idx);
                        *action_index.write() = 0;
                        return EventResult::Consumed;
                    }
                }
                PluginViewTab::Marketplaces => {
                    let entries_len = get_marketplace_cache().len();
                    // Add 行（内容行 3）单独命中：激活 add 输入（Enter @L722）
                    if hit_row(
                        mouse.row,
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
                        *selected.write() = 0;
                        *add_marketplace_active.write() = true;
                        *add_marketplace_input.write() = TextAreaState::default();
                        return EventResult::Consumed;
                    }
                    // 条目（内容行 5 起，每项 3 行）：点击 = 进详情（Enter @L726）
                    if let Some(idx) = hit_item(
                        &mouse,
                        area,
                        ListLayout {
                            header_rows: 5,
                            item_rows: 3,
                            footer_rows: 0,
                            visible_items: entries_len as u16,
                            scroll_start: 0,
                            item_count: entries_len,
                        },
                    ) {
                        let s = idx + 1;
                        *selected.write() = s;
                        *marketplace_detail.write() = Some(s);
                        *marketplace_detail_action.write() = 0;
                        return EventResult::Consumed;
                    }
                }
                PluginViewTab::Discover | PluginViewTab::Errors => {}
            }
        }
        return match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
            _ => EventResult::Ignored,
        };
    }
    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }
    let in_detail = detail_plugin_idx.read().is_some();
    let in_discover_detail = discover_detail_idx.read().is_some();
    let in_marketplace_detail = marketplace_detail.read().is_some();

    // 任意键盘事件清除 operation_loading（操作完成后用户按任意键消除 loading 状态）
    // 注意：read() 可能持有读锁，write() 需要写锁——必须先将读值提取到局部变量
    // 让读锁释放后再写，否则在 std::sync::RwLock 上触发死锁。
    let has_loading = operation_loading.read().is_some();
    let no_confirm = confirm_action.read().is_none();
    if has_loading && no_confirm {
        *operation_loading.write() = None;
        tokio::task::spawn_blocking(refresh_discover_cache);
        return EventResult::Consumed;
    }

    // ── Discover detail 模式 ──
    if in_discover_detail {
        return match key.code {
            KeyCode::Up => {
                let mut da = discover_detail_action.write();
                *da = da.saturating_sub(1);
                EventResult::Consumed
            }
            KeyCode::Down => {
                let mut da = discover_detail_action.write();
                let max = DiscoverDetailAction::ALL.len().saturating_sub(1);
                if *da < max {
                    *da += 1;
                }
                EventResult::Consumed
            }
            KeyCode::Enter => {
                let action = DiscoverDetailAction::ALL
                    .get(*discover_detail_action.read())
                    .copied();
                let idx = discover_detail_idx.read().unwrap_or(0);
                match action {
                    Some(DiscoverDetailAction::InstallUser) => {
                        let items = get_discover_cache();
                        if let Some(dp) = items.get(idx) {
                            let name = dp.name.clone();
                            let marketplace = dp.marketplace.clone();
                            *operation_loading.write() = Some("install".into());
                            if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                let client = cl.clone();
                                let sid = client.current_session_id().unwrap_or_default();
                                tokio::spawn(async move {
                                    let _ = client
                                        .send_raw_request(
                                            "plugin/install",
                                            serde_json::json!({
                                                "name": name,
                                                "marketplace": marketplace,
                                                "scope": "user",
                                                "sessionId": sid,
                                            }),
                                        )
                                        .await;
                                });
                            }
                        }
                        *discover_detail_idx.write() = None;
                        *discover_detail_action.write() = 0;
                    }
                    Some(DiscoverDetailAction::InstallProject) => {
                        let items = get_discover_cache();
                        if let Some(dp) = items.get(idx) {
                            let name = dp.name.clone();
                            let marketplace = dp.marketplace.clone();
                            *operation_loading.write() = Some("install".into());
                            if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                let client = cl.clone();
                                let sid = client.current_session_id().unwrap_or_default();
                                tokio::spawn(async move {
                                    let _ = client
                                        .send_raw_request(
                                            "plugin/install",
                                            serde_json::json!({
                                                "name": name,
                                                "marketplace": marketplace,
                                                "scope": "project",
                                                "sessionId": sid,
                                            }),
                                        )
                                        .await;
                                });
                            }
                        }
                        *discover_detail_idx.write() = None;
                        *discover_detail_action.write() = 0;
                    }
                    Some(DiscoverDetailAction::BackToList) => {
                        *discover_detail_idx.write() = None;
                        *discover_detail_action.write() = 0;
                    }
                    None => {}
                }
                EventResult::Consumed
            }
            KeyCode::Esc => {
                *discover_detail_idx.write() = None;
                *discover_detail_action.write() = 0;
                EventResult::Consumed
            }
            KeyCode::Tab => {
                *discover_detail_idx.write() = None;
                *discover_detail_action.write() = 0;
                let mut tab = active_tab.write();
                *tab = cycle_forward(*tab);
                *selected.write() = 0;
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        };
    }

    // ── Marketplace detail 模式 ──
    if in_marketplace_detail {
        return match key.code {
            KeyCode::Up => {
                let mut ma = marketplace_detail_action.write();
                *ma = ma.saturating_sub(1);
                EventResult::Consumed
            }
            KeyCode::Down => {
                let mut ma = marketplace_detail_action.write();
                if *ma < 1 {
                    *ma += 1;
                }
                EventResult::Consumed
            }
            KeyCode::Enter => {
                let s = marketplace_detail.read().unwrap_or(0);
                let entries = get_marketplace_cache(); // 非阻塞缓存读取
                if let Some(entry) = entries.get(s.saturating_sub(1)) {
                    match *marketplace_detail_action.read() {
                        0 => {
                            // Refresh
                            let name = entry.source_label.clone();
                            *marketplace_refreshing.write() = true;
                            if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                let client = cl.clone();
                                let sid = client.current_session_id().unwrap_or_default();
                                let name_for_refresh = name.clone();
                                tokio::spawn(async move {
                                    let _ = client
                                        .send_raw_request(
                                            "marketplace/refresh",
                                            serde_json::json!({
                                                "name": name_for_refresh,
                                                "sessionId": sid,
                                            }),
                                        )
                                        .await;
                                });
                            }
                            // 将缓存刷新（同步 I/O）移到 blocking thread
                            tokio::task::spawn_blocking(|| {
                                refresh_discover_cache();
                                refresh_marketplace_cache();
                            });
                        }
                        _ => {
                            // Delete
                            *confirm_action.write() = Some("delete_marketplace".into());
                            *operation_loading.write() = Some(entry.name.clone());
                        }
                    }
                }
                *marketplace_detail.write() = None;
                *marketplace_detail_action.write() = 0;
                EventResult::Consumed
            }
            KeyCode::Esc => {
                *marketplace_detail.write() = None;
                *marketplace_detail_action.write() = 0;
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        };
    }

    // 必须先提取为局部变量再 match：match scrutinee 中的临时
    // RwLockReadGuard 会存活到整个 match 表达式结束，分支内对同一
    // atom 执行 write() 会触发同线程 read→write 重入死锁（[bug] 卸载卡死）。
    let confirm_active = confirm_action.read().is_some();
    match (in_detail, confirm_active, key.code) {
        // ── 确认模式优先 ──
        (_, true, KeyCode::Enter) => {
            let action = confirm_action.read().clone().unwrap_or_default();
            // 不在事件处理器线程内写 confirm_action：generational-box
            // SyncStorage 的 parking_lot::RwLock 不允许同一线程
            // read→write 重入，否则死锁（[回归] marketplace 删除卡死）。
            // 状态更新统一移到独立线程执行。
            // *confirm_action.write() = None;

            let saved_loading = operation_loading.read().clone();
            // *operation_loading.write() = Some(action.clone());

            match action.as_str() {
                "uninstall" => {
                    let idx = detail_plugin_idx.read().unwrap_or(0);
                    if let Some(p) = PLUGIN_LIST.state().read().get(idx) {
                        let plugin_id = if p.marketplace.is_empty() {
                            p.name.clone()
                        } else {
                            format!("{}@{}", p.name, p.marketplace)
                        };
                        if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                            let client = cl.clone();
                            let sid = client.current_session_id().unwrap_or_default();
                            tokio::spawn(async move {
                                let _ = client
                                    .send_raw_request(
                                        "plugin/uninstall",
                                        serde_json::json!({
                                            "pluginId": plugin_id,
                                            "sessionId": sid,
                                        }),
                                    )
                                    .await;
                            });
                        }
                    }
                    *detail_plugin_idx.write() = None;
                    // 关闭确认弹窗（独立线程避免 RwLock 重入）
                    std::thread::spawn(move || {
                        *confirm_action.write() = None;
                        *operation_loading.write() = None;
                    });
                }
                "delete_marketplace" => {
                    let name = saved_loading.unwrap_or_default();
                    std::thread::spawn(move || {
                        let marketplaces = peri_middlewares::plugin::load_known_marketplaces(None)
                            .unwrap_or_default();
                        let filtered: Vec<_> = marketplaces
                            .into_iter()
                            .filter(|km| {
                                peri_middlewares::plugin::MarketplaceManager::extract_name(
                                    &km.source,
                                ) != name
                            })
                            .collect();
                        let _ = peri_middlewares::plugin::save_known_marketplaces(&filtered, None);
                        refresh_discover_cache();
                        refresh_marketplace_cache();
                        // State 写移到独立线程，避免事件循环线程的 RwLock 重入死锁
                        *confirm_action.write() = None;
                        *operation_loading.write() = None;
                    });
                }
                _ => {
                    // 未知确认动作：安全起见也从独立线程写 state
                    std::thread::spawn(move || {
                        *confirm_action.write() = None;
                        *operation_loading.write() = None;
                    });
                }
            }
        }
        (_, true, _) => {
            // 任意非 Enter 键关闭确认弹窗（独立线程避免 RwLock 重入）
            std::thread::spawn(move || {
                *confirm_action.write() = None;
                *operation_loading.write() = None;
            });
        }
        // ── Tab/Shift+Tab 切换视图 ──
        (false, false, KeyCode::Tab) => {
            let mut tab = active_tab.write();
            *tab = cycle_forward(*tab);
            *selected.write() = 0;
            *discover_cursor.write() = 0;
        }
        (false, false, KeyCode::BackTab) => {
            let mut tab = active_tab.write();
            *tab = cycle_backward(*tab);
            *selected.write() = 0;
            *discover_cursor.write() = 0;
        }
        // ── List: Enter → 进入详情 / 删除 marketplace / discover detail ──
        (false, false, KeyCode::Enter) => {
            if *active_tab.read() == PluginViewTab::Installed {
                let s = *selected.read();
                let c = PLUGIN_LIST.state().read().len();
                if c > 0 {
                    *detail_plugin_idx.write() = Some(s);
                    *action_index.write() = 0;
                }
            } else if *active_tab.read() == PluginViewTab::Discover {
                // Enter on discover list → go to detail (handled in High priority)
                // Fall through for Discover tab
            } else if *active_tab.read() == PluginViewTab::Marketplaces {
                let s = *selected.read();
                let entries = get_marketplace_cache(); // 非阻塞缓存读取
                if s == 0 {
                    // "Add Marketplace" — activate text input
                    *add_marketplace_active.write() = true;
                    *add_marketplace_input.write() = TextAreaState::default();
                } else if entries.get(s.saturating_sub(1)).is_some() {
                    // Enter marketplace detail
                    *marketplace_detail.write() = Some(s);
                    *marketplace_detail_action.write() = 0;
                }
            } else if *active_tab.read() == PluginViewTab::Errors {
                let mut tab = active_tab.write();
                *tab = PluginViewTab::Installed;
                *selected.write() = 0;
            }
        }
        // ── Detail: Enter → 执行操作 ──
        (true, false, KeyCode::Enter) => {
            let idx = detail_plugin_idx.read().unwrap_or(0);
            if let Some(p) = PLUGIN_LIST.state().read().get(idx) {
                let actions = action_list(p.enabled);
                let ai = *action_index.read();
                if let Some(action) = actions.get(ai) {
                    match *action {
                        "uninstall" => {
                            *confirm_action.write() = Some("uninstall".into());
                        }
                        "back" => {
                            *detail_plugin_idx.write() = None;
                        }
                        "enable" | "disable" => {
                            *operation_loading.write() = Some(action.to_string());
                            let idx = detail_plugin_idx.read().unwrap_or(0);
                            let plugin_info = PLUGIN_LIST.state().read().get(idx).cloned();
                            if let Some(p) = plugin_info {
                                let plugin_id = if p.marketplace.is_empty() {
                                    p.name.clone()
                                } else {
                                    format!("{}@{}", p.name, p.marketplace)
                                };
                                let plugin_id_for_persist = plugin_id.clone();
                                let enable = *action == "enable";
                                let scope = p.install_scope.clone();
                                if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                    let client = cl.clone();
                                    let sid = client.current_session_id().unwrap_or_default();
                                    tokio::spawn(async move {
                                        let _ = client
                                            .send_raw_request(
                                                "plugin/toggle",
                                                serde_json::json!({
                                                    "pluginId": plugin_id,
                                                    "enable": enable,
                                                    "scope": scope,
                                                    "sessionId": sid,
                                                }),
                                            )
                                            .await;
                                    });
                                }
                                // 将同步写盘移到 blocking thread，避免阻塞 TUI 主事件循环
                                tokio::task::spawn_blocking(move || {
                                    let _ = peri_middlewares::plugin::save_claude_settings_enabled_plugins(
                                        &[(plugin_id_for_persist, enable)],
                                        None,
                                    );
                                });
                            }
                            *detail_plugin_idx.write() = None;
                        }
                        "update" => {
                            *operation_loading.write() = Some("update".into());
                            let idx = detail_plugin_idx.read().unwrap_or(0);
                            let p = PLUGIN_LIST.state().read().get(idx).cloned();
                            if let Some(p) = p {
                                let plugin_id = if p.marketplace.is_empty() {
                                    p.name.clone()
                                } else {
                                    format!("{}@{}", p.name, p.marketplace)
                                };
                                if let Some(cl) = ACP_CLIENT_HANDLE.get() {
                                    let client = cl.clone();
                                    let sid = client.current_session_id().unwrap_or_default();
                                    tokio::spawn(async move {
                                        let _ = client
                                            .send_raw_request(
                                                "plugin/update",
                                                serde_json::json!({
                                                    "pluginId": plugin_id,
                                                    "sessionId": sid,
                                                }),
                                            )
                                            .await;
                                    });
                                }
                            }
                            *detail_plugin_idx.write() = None;
                        }
                        other => {
                            tracing::info!(target: "plugin-panel", "unknown action {} on {}", other, p.name);
                        }
                    }
                }
            }
        }
        // ── Detail: ↑/↓ 导航操作菜单 ──
        (true, false, KeyCode::Up) => {
            let idx = detail_plugin_idx.read().unwrap_or(0);
            if let Some(p) = PLUGIN_LIST.state().read().get(idx) {
                let actions = action_list(p.enabled);
                let ai = *action_index.read();
                *action_index.write() = if ai == 0 {
                    actions.len().saturating_sub(1)
                } else {
                    ai - 1
                };
            }
        }
        (true, false, KeyCode::Down) => {
            let idx = detail_plugin_idx.read().unwrap_or(0);
            if let Some(p) = PLUGIN_LIST.state().read().get(idx) {
                let actions = action_list(p.enabled);
                let ai = *action_index.read();
                *action_index.write() = if ai + 1 >= actions.len() { 0 } else { ai + 1 };
            }
        }
        // ── List: ←/→/↑/↓ ──
        (false, false, KeyCode::Left) => {
            let mut tab = active_tab.write();
            *tab = cycle_backward(*tab);
            *selected.write() = 0;
            *discover_cursor.write() = 0;
        }
        (false, false, KeyCode::Right) => {
            let mut tab = active_tab.write();
            *tab = cycle_forward(*tab);
            *selected.write() = 0;
            *discover_cursor.write() = 0;
        }
        (false, false, KeyCode::Up) => {
            if *active_tab.read() == PluginViewTab::Installed {
                let mut s = selected.write();
                *s = previous_selection(*s);
            } else if *active_tab.read() == PluginViewTab::Discover {
                let mut c = discover_cursor.write();
                *c = previous_selection(*c);
            } else if *active_tab.read() == PluginViewTab::Marketplaces {
                let mut s = selected.write();
                *s = previous_selection(*s);
            }
        }
        (false, false, KeyCode::Down) => {
            if *active_tab.read() == PluginViewTab::Installed {
                let mut s = selected.write();
                let c = PLUGIN_LIST.state().read().len();
                if c > 0 {
                    *s = next_selection(*s, c);
                }
            } else if *active_tab.read() == PluginViewTab::Discover {
                let items = get_discover_cache();
                let filtered = discover_filtered.read().clone();
                let count = if search_text.read().text.is_empty() {
                    items.len()
                } else {
                    filtered.len()
                };
                if count > 0 {
                    let mut c = discover_cursor.write();
                    *c = next_selection(*c, count);
                }
            } else if *active_tab.read() == PluginViewTab::Marketplaces {
                let mut s = selected.write();
                let c = get_marketplace_cache().len() + 1; // +1 for Add，非阻塞缓存读取
                if c > 0 {
                    *s = next_selection(*s, c);
                }
            }
        }
        _ => {}
    }
    EventResult::Consumed
}
