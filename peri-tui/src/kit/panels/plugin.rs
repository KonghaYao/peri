//! ratatui-kit PluginPanel v2 component.
//!
//! v2 Phase 2: 4-tab multi-view with dual-mode (list/detail) state machine.
//! ←/→ 切换视图，↑/↓ 导航，Enter 进详情/执行操作，Esc 返回列表。
//! 无 ScrollView——避免其内置 handler 与自定义 ↑/↓ 冲突。

use std::sync::OnceLock;

use crate::app::panel_types::PanelKind;
use crate::components::textarea::TextAreaState;
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, LANG_VERSION, PLUGIN_LIST, PLUGIN_SEARCH_RESULTS, PluginSummary,
    PluginViewTab, RENDER_HEARTBEAT,
};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{AreaTracker, ListLayout, hit_item, hit_row, is_scrollbar_column};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        style::{Color, Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

// ── Discover cache (non-reactive, safe in render body) ────────────────

/// Discover 插件列表缓存——避免 render body 中同步读盘。
/// 使用 Option<Vec<T>>：None = 未初始化，Some(vec) = 已填充（可能为空）。
/// 避免用 is_empty() 判断初始化状态——空数据集与未初始化无法区分。
static DISCOVER_CACHE: OnceLock<parking_lot::Mutex<Option<Vec<PluginSearchResultItem>>>> =
    OnceLock::new();

fn get_discover_cache() -> Vec<PluginSearchResultItem> {
    let cache = DISCOVER_CACHE.get_or_init(|| parking_lot::Mutex::new(None));
    {
        let guard = cache.lock();
        if let Some(ref data) = *guard {
            return data.clone();
        }
    }
    // 锁外执行磁盘 I/O，避免 render body 阻塞在锁上
    let data = load_discover_plugins_from_disk();
    let mut guard = cache.lock();
    if guard.is_none() {
        *guard = Some(data);
    }
    guard.as_ref().unwrap().clone()
}

fn refresh_discover_cache() {
    // 先在锁外执行磁盘 I/O，再拿锁替换数据
    let data = load_discover_plugins_from_disk();
    if let Some(cache) = DISCOVER_CACHE.get() {
        let mut guard = cache.lock();
        *guard = Some(data);
    }
}

// ── Marketplace cache (non-reactive, safe in render body) ─────────────

/// Marketplace 数据缓存——避免 render_marketplaces 每帧同步读盘。
/// 使用 Option<Vec<T>>：None = 未初始化，Some(vec) = 已填充（可能为空）。
static MARKETPLACE_CACHE: OnceLock<parking_lot::Mutex<Option<Vec<MsEntry>>>> = OnceLock::new();

fn get_marketplace_cache() -> Vec<MsEntry> {
    let cache = MARKETPLACE_CACHE.get_or_init(|| parking_lot::Mutex::new(None));
    {
        let guard = cache.lock();
        if let Some(ref data) = *guard {
            return data.clone();
        }
    }
    // 锁外执行磁盘 I/O，避免 render body 阻塞在锁上
    let data = load_marketplace_data();
    let mut guard = cache.lock();
    if guard.is_none() {
        *guard = Some(data);
    }
    guard.as_ref().unwrap().clone()
}

fn refresh_marketplace_cache() {
    // 先在锁外执行磁盘 I/O，再拿锁替换数据
    let data = load_marketplace_data();
    if let Some(cache) = MARKETPLACE_CACHE.get() {
        let mut guard = cache.lock();
        *guard = Some(data);
    }
}

// ── Search state machine ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum SearchState {
    Idle,
    Loading,
    Error(String),
}

// ── Discover detail actions ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscoverDetailAction {
    InstallUser,
    InstallProject,
    BackToList,
}

impl DiscoverDetailAction {
    const ALL: [DiscoverDetailAction; 3] = [
        DiscoverDetailAction::InstallUser,
        DiscoverDetailAction::InstallProject,
        DiscoverDetailAction::BackToList,
    ];

    fn label(&self) -> String {
        match self {
            Self::InstallUser => i18n::tr("panel-plugin-discover-install-user"),
            Self::InstallProject => i18n::tr("panel-plugin-discover-install-project"),
            Self::BackToList => i18n::tr("panel-plugin-action-back"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PluginSearchResultItem {
    name: String,
    version: String,
    marketplace: String,
    description: String,
    author: Option<String>,
}

// ── Marketplace view types ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
enum MsStatus {
    Fresh,
    Cached,
    Fetching,
    Stale,
    Failed,
    NotFound,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct MsEntry {
    name: String,
    source_label: String,
    plugin_count: usize,
    installed_count: usize,
    status: MsStatus,
    last_updated: Option<String>,
    auto_update: bool,
}

// ── Tab switching ──────────────────────────────────────────────────────

fn cycle_forward(t: PluginViewTab) -> PluginViewTab {
    match t {
        PluginViewTab::Installed => PluginViewTab::Discover,
        PluginViewTab::Discover => PluginViewTab::Marketplaces,
        PluginViewTab::Marketplaces => PluginViewTab::Errors,
        PluginViewTab::Errors => PluginViewTab::Installed,
    }
}

fn cycle_backward(t: PluginViewTab) -> PluginViewTab {
    match t {
        PluginViewTab::Installed => PluginViewTab::Errors,
        PluginViewTab::Errors => PluginViewTab::Marketplaces,
        PluginViewTab::Marketplaces => PluginViewTab::Discover,
        PluginViewTab::Discover => PluginViewTab::Installed,
    }
}

// ── Component ──────────────────────────────────────────────────────

#[component]
pub fn PluginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    let active_tab = hooks.use_state(|| PluginViewTab::Installed);
    let detail_plugin_idx = hooks.use_state(|| Option::<usize>::None);
    let action_index = hooks.use_state(|| 0usize);
    let confirm_action = hooks.use_state(|| Option::<String>::None);
    let cursor_visible = hooks.use_state(|| true);
    let cursor_last_toggle = hooks.use_state(std::time::Instant::now);
    let search_text = hooks.use_state(TextAreaState::default);
    let search_focus = hooks.use_state(|| false);
    let search_state = hooks.use_state(|| SearchState::Idle);
    let operation_loading = hooks.use_state(|| Option::<String>::None);
    let add_marketplace_input = hooks.use_state(TextAreaState::default);
    let add_marketplace_active = hooks.use_state(|| false);
    let marketplace_refreshing = hooks.use_state(|| false);
    // Marketplace detail state
    let marketplace_detail = hooks.use_state(|| Option::<usize>::None);
    let marketplace_detail_action = hooks.use_state(|| 0usize);
    // Discover list state
    let discover_cursor = hooks.use_state(|| 0usize);
    let discover_filtered = hooks.use_state(Vec::<usize>::new);
    let discover_detail_idx = hooks.use_state(|| Option::<usize>::None);
    let discover_detail_action = hooks.use_state(|| 0usize);
    let store = hooks.use_atom(&PLUGIN_LIST);
    let plugins: Vec<PluginSummary> = store.read().clone();
    hooks.use_atom(&LANG_VERSION);
    hooks.use_atom(&PLUGIN_SEARCH_RESULTS);
    let count = plugins.len();

    // RENDER_HEARTBEAT: 驱动 cursor blink（Discover 搜索框）
    {
        let _hb = hooks.use_atom(&RENDER_HEARTBEAT);
    }

    // 面板绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // High priority — 搜索框文本输入（仅 Discover tab 生效，先于 Normal handler）
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        {
        move |event| {
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
                                            if let Err(e) = client.send_raw_request("plugin/search", params).await {
                                                tracing::warn!(error = %e, "plugin search RPC failed");
                                            }
                                        });
                                    } else {
                                        tracing::warn!(target: "plugin-panel", "ACP_CLIENT_HANDLE not set, search skipped");
                                        *search_state.write() = SearchState::Error("ACP client not available".into());
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
                                    let name = peri_middlewares::plugin::MarketplaceManager::extract_name(&source);
                                    // 将同步磁盘 I/O 移到 dedicated blocking thread，
                                    // 避免阻塞 TUI 主事件循环。
                                    let source_for_blocking = source.clone();
                                    let name_for_refresh = name.clone();
                                    tokio::spawn(async move {
                                        let added = tokio::task::spawn_blocking(move || {
                                            let mut marketplaces = peri_middlewares::plugin::load_known_marketplaces(None).unwrap_or_default();
                                            let already_exists = marketplaces.iter().any(|km| km.source == source_for_blocking);
                                            if already_exists {
                                                return false;
                                            }
                                            marketplaces.push(peri_middlewares::plugin::KnownMarketplace {
                                                source: source_for_blocking,
                                                install_location: String::new(),
                                                auto_update: false,
                                                last_updated: String::new(),
                                            });
                                            let _ = peri_middlewares::plugin::save_known_marketplaces(&marketplaces, None);
                                            refresh_discover_cache();
                                            refresh_marketplace_cache();
                                            true
                                        }).await.unwrap();
                                        if added
                                            && let Some(cl) = ACP_CLIENT_HANDLE.get()
                                        {
                                            let client = cl.clone();
                                            let sid = client.current_session_id().unwrap_or_default();
                                            let _ = client.send_raw_request("marketplace/refresh", serde_json::json!({
                                                "name": name_for_refresh,
                                                "sessionId": sid,
                                            })).await;
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
            match key.code {
                // 搜索框已激活 → 进入搜索输入模式
                _ if *search_focus.read() => match key.code {
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
                                *search_state.write() = SearchState::Error("ACP client not available".into());
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
                    let filtered: Vec<usize> = items.iter().enumerate()
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
                        items.iter().enumerate()
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
                        && orig_idx < items.len() {
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
    });

    // ── 键盘 ──
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        {
        move |event| {
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
                                            let _ = client.send_raw_request("plugin/uninstall", serde_json::json!({
                                                "pluginId": plugin_id,
                                                "sessionId": sid,
                                            })).await;
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
                                    let marketplaces = peri_middlewares::plugin::load_known_marketplaces(None).unwrap_or_default();
                                    let filtered: Vec<_> = marketplaces.into_iter()
                                        .filter(|km| peri_middlewares::plugin::MarketplaceManager::extract_name(&km.source) != name)
                                        .collect();
                                    let _ = peri_middlewares::plugin::save_known_marketplaces(&filtered, None);
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
                    if discover_detail_idx.read().is_some()
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
                                            let _ = client.send_raw_request("plugin/install", serde_json::json!({
                                                "name": name,
                                                "marketplace": marketplace,
                                                "scope": "user",
                                                "sessionId": sid,
                                            })).await;
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
                                            let _ = client.send_raw_request("plugin/install", serde_json::json!({
                                                "name": name,
                                                "marketplace": marketplace,
                                                "scope": "project",
                                                "sessionId": sid,
                                            })).await;
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
                    if marketplace_detail.read().is_some()
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
                                            let _ = client.send_raw_request("marketplace/refresh", serde_json::json!({
                                                "name": name_for_refresh,
                                                "sessionId": sid,
                                            })).await;
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
                    if let Some(detail) = *detail_plugin_idx.read()
                        && let Some(detail_p) = PLUGIN_LIST.state().read().get(detail).cloned()
                        && let Some(idx) = hit_item(
                            &mouse,
                            area,
                            ListLayout {
                                // 标题 + 空行 + 状态 + 空行 + 4 字段 + 空行 + 4 capabilities
                                // + [可选 error 2 行] + 空行 + actions 标题 + 空行
                                header_rows: if detail_p.load_error.is_some() { 18 } else { 16 },
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
                                                let _ = client.send_raw_request("plugin/toggle", serde_json::json!({
                                                    "pluginId": plugin_id,
                                                    "enable": enable,
                                                    "scope": scope,
                                                    "sessionId": sid,
                                                })).await;
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
                                                let _ = client.send_raw_request("plugin/update", serde_json::json!({
                                                    "pluginId": plugin_id,
                                                    "sessionId": sid,
                                                })).await;
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
                                            let _ = client.send_raw_request("plugin/install", serde_json::json!({
                                                "name": name,
                                                "marketplace": marketplace,
                                                "scope": "user",
                                                "sessionId": sid,
                                            })).await;
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
                                            let _ = client.send_raw_request("plugin/install", serde_json::json!({
                                                "name": name,
                                                "marketplace": marketplace,
                                                "scope": "project",
                                                "sessionId": sid,
                                            })).await;
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
                                            let _ = client.send_raw_request("marketplace/refresh", serde_json::json!({
                                                "name": name_for_refresh,
                                                "sessionId": sid,
                                            })).await;
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

            match (in_detail, confirm_action.read().is_some(), key.code) {
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
                                        let _ = client.send_raw_request("plugin/uninstall", serde_json::json!({
                                            "pluginId": plugin_id,
                                            "sessionId": sid,
                                        })).await;
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
                                let marketplaces = peri_middlewares::plugin::load_known_marketplaces(None).unwrap_or_default();
                                let filtered: Vec<_> = marketplaces.into_iter()
                                    .filter(|km| peri_middlewares::plugin::MarketplaceManager::extract_name(&km.source) != name)
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
                                                let _ = client.send_raw_request("plugin/toggle", serde_json::json!({
                                                    "pluginId": plugin_id,
                                                    "enable": enable,
                                                    "scope": scope,
                                                    "sessionId": sid,
                                                })).await;
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
                                                let _ = client.send_raw_request("plugin/update", serde_json::json!({
                                                    "pluginId": plugin_id,
                                                    "sessionId": sid,
                                                })).await;
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
                        *action_index.write() = if ai + 1 >= actions.len() {
                            0
                        } else {
                            ai + 1
                        };
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
    });

    // ── 构建行 ──
    let sel = *selected.read();
    let current_tab = *active_tab.read();
    let detail_idx = *detail_plugin_idx.read();
    let ai = *action_index.read();
    const VISIBLE_ITEMS: usize = 5;
    let scroll_start = scroll_start_for_selected(sel, count, VISIBLE_ITEMS);

    // cursor blink toggle for Discover search box
    let show_cursor = {
        let now = std::time::Instant::now();
        let mut last = cursor_last_toggle.write_no_update();
        if now.duration_since(*last).as_millis() >= 500 {
            let mut v = cursor_visible.write_no_update();
            *v = !*v;
            *last = now;
        }
        *cursor_visible.read()
    };

    let guard = theme_def.read();
    let title_style = Style::new().fg(guard.component.panel.title).bold();
    let primary_style = Style::new().fg(guard.semantic.text.primary);
    let bold_style = Style::new().fg(guard.semantic.text.primary).bold();
    let muted_style = Style::new().fg(guard.semantic.text.muted);
    let dim_style = Style::new().fg(guard.semantic.text.dim);
    let error_style = Style::new().fg(guard.semantic.status.error);
    let warning_color = guard.semantic.status.warning;
    let success_color = guard.semantic.status.success;
    let title_color = guard.component.panel.title;
    let active_tab_style = Style::new()
        .fg(guard.semantic.text.primary)
        .bg(title_color)
        .bold();
    let inactive_tab_style = muted_style;

    let mut lines: Vec<Line<'_>> = Vec::new();

    // ── tab bar ──
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {}  ", i18n::tr("panel-plugin-tab-installed")),
            if current_tab == PluginViewTab::Installed {
                active_tab_style
            } else {
                inactive_tab_style
            },
        ),
        Span::styled(
            format!("  {}  ", i18n::tr("panel-plugin-tab-discover")),
            if current_tab == PluginViewTab::Discover {
                active_tab_style
            } else {
                inactive_tab_style
            },
        ),
        Span::styled(
            format!("  {}  ", i18n::tr("panel-plugin-tab-marketplaces")),
            if current_tab == PluginViewTab::Marketplaces {
                active_tab_style
            } else {
                inactive_tab_style
            },
        ),
        Span::styled(
            format!("  {}  ", i18n::tr("panel-plugin-tab-errors")),
            if current_tab == PluginViewTab::Errors {
                active_tab_style
            } else {
                inactive_tab_style
            },
        ),
    ]));

    // ── Content ──
    // 检查 discover detail 模式
    let dd_idx = *discover_detail_idx.read();
    if let Some(disc_idx) = dd_idx {
        let items = get_discover_cache();
        if let Some(dp) = items.get(disc_idx) {
            render_discover_detail(
                &mut lines,
                dp,
                *discover_detail_action.read(),
                bold_style,
                muted_style,
                dim_style,
                primary_style,
                success_color,
                title_color,
                title_style,
            );
        }
    } else if let Some(mp_sel) = *marketplace_detail.read() {
        let entries = get_marketplace_cache();
        if let Some(entry) = entries.get(mp_sel.saturating_sub(1)) {
            let ma = *marketplace_detail_action.read();
            // Title
            lines.push(Line::from(vec![Span::styled(
                i18n::tr_args(
                    "panel-plugin-detail-title",
                    &[("name".into(), FluentValue::from(entry.name.clone()))],
                ),
                bold_style,
            )]));
            lines.push(Line::from(""));
            // Source
            let source_display: String = entry.source_label.chars().take(60).collect();
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "    {}: ",
                        i18n::tr("panel-plugin-discover-field-marketplace")
                    ),
                    muted_style,
                ),
                Span::styled(source_display, dim_style),
            ]));
            lines.push(Line::from(""));
            // Actions
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", i18n::tr("panel-plugin-detail-actions")),
                bold_style,
            )]));
            lines.push(Line::from(""));
            let actions: [String; 2] = [
                i18n::tr("panel-plugin-marketplace-action-refresh"),
                i18n::tr("panel-plugin-marketplace-action-delete"),
            ];
            for (i, action_label) in actions.iter().enumerate() {
                let is_selected = i == ma;
                let cursor = if is_selected { ">" } else { " " };
                let style = if is_selected {
                    title_style
                } else {
                    primary_style
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", cursor), Style::new().fg(title_color)),
                    Span::styled(format!("    {}", action_label), style),
                ]));
            }
        }
    } else if let Some(di) = detail_idx {
        if let Some(p) = plugins.get(di) {
            let confirm_text = confirm_action.read().clone();
            render_detail(
                &mut lines,
                p,
                ai,
                bold_style,
                muted_style,
                dim_style,
                primary_style,
                error_style,
                success_color,
                title_color,
                title_style,
                confirm_text.as_deref(),
                warning_color,
            );
        }
    } else {
        match current_tab {
            PluginViewTab::Installed => render_installed(
                &mut lines,
                &plugins,
                sel,
                scroll_start,
                VISIBLE_ITEMS,
                count,
                bold_style,
                muted_style,
                dim_style,
                primary_style,
                title_style,
                error_style,
                success_color,
                title_color,
            ),
            PluginViewTab::Discover => {
                // Discover list: show cached marketplace plugins with real-time filtering
                // 当有搜索文本且远程搜索结果不为空时，使用远程结果；否则使用本地 cache
                let query = search_text.read().text.to_lowercase();
                let search_state_guard = PLUGIN_SEARCH_RESULTS.state();
                let search_results = search_state_guard.read();
                let remote_items: Vec<PluginSearchResultItem> =
                    if !query.is_empty() && !search_results.is_empty() {
                        search_results
                            .iter()
                            .map(|p| PluginSearchResultItem {
                                name: p.name.clone(),
                                version: p.version.clone(),
                                marketplace: p.marketplace.clone(),
                                description: p.description.clone(),
                                author: p.author.clone(),
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                drop(search_results);

                let use_remote = !remote_items.is_empty();
                let local_items = get_discover_cache();
                let display_items: Vec<PluginSearchResultItem> = if use_remote {
                    remote_items
                } else {
                    local_items
                };

                let filtered_items: Vec<&PluginSearchResultItem> = if use_remote || query.is_empty()
                {
                    display_items.iter().collect()
                } else {
                    let filtered_indices = discover_filtered.read().clone();
                    if filtered_indices.is_empty() {
                        // 刚切换过来，初始化过滤
                        display_items
                            .iter()
                            .enumerate()
                            .filter(|(_, item)| {
                                item.name.to_lowercase().contains(&query)
                                    || item.description.to_lowercase().contains(&query)
                                    || item.marketplace.to_lowercase().contains(&query)
                            })
                            .map(|(_, item)| item)
                            .collect()
                    } else {
                        filtered_indices
                            .iter()
                            .filter_map(|&i| display_items.get(i))
                            .collect()
                    }
                };
                let disc_sel = *discover_cursor.read();
                let disc_scroll =
                    scroll_start_for_selected(disc_sel, filtered_items.len(), VISIBLE_ITEMS);
                render_discover_list(
                    &mut lines,
                    search_text.read().text.as_str(),
                    show_cursor,
                    &search_state.read(),
                    &filtered_items,
                    disc_sel,
                    disc_scroll,
                    VISIBLE_ITEMS,
                    bold_style,
                    muted_style,
                    dim_style,
                    primary_style,
                    error_style,
                    success_color,
                    title_color,
                    title_style,
                )
            }
            PluginViewTab::Marketplaces => render_marketplaces(
                &mut lines,
                sel,
                bold_style,
                muted_style,
                success_color,
                warning_color,
                error_style,
                *marketplace_refreshing.read(),
                *add_marketplace_active.read(),
                add_marketplace_input.read().text.as_str(),
                confirm_action.read().clone().as_deref(),
                operation_loading.read().clone().as_deref(),
            ),
            PluginViewTab::Errors => render_errors(
                &mut lines,
                &plugins,
                bold_style,
                muted_style,
                dim_style,
                error_style,
            ),
        }
    }

    // ── footer ──
    let footer_text = if *add_marketplace_active.read() {
        i18n::tr("panel-plugin-marketplace-add-input-footer").to_string()
    } else if let Some(ref op) = *operation_loading.read() {
        match op.as_str() {
            "uninstall" => format!("{}...", i18n::tr("panel-plugin-action-uninstall")),
            "enable" => format!("{}...", i18n::tr("panel-plugin-action-enable")),
            "disable" => format!("{}...", i18n::tr("panel-plugin-action-disable")),
            "install" => format!("{}...", i18n::tr("panel-plugin-action-install")),
            "update" => format!("{}...", i18n::tr("panel-plugin-action-update")),
            _ => format!("{}...", op),
        }
    } else if confirm_action.read().is_some() {
        i18n::tr("panel-plugin-confirm-hint")
    } else if discover_detail_idx.read().is_some() {
        i18n::tr("common-nav-enter-close")
    } else if detail_idx.is_some() {
        i18n::tr("common-nav-enter-close")
    } else if marketplace_detail.read().is_some() {
        i18n::tr("panel-plugin-marketplace-detail-hint")
    } else {
        i18n::tr("common-nav-tab-close")
    };
    lines.push(Line::from(vec![Span::styled(footer_text, muted_style)]));

    drop(guard);

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Plugin, {
        Text(text: content)
    })
}

// ── Action list ──────────────────────────────────────────────────────

fn action_list(enabled: bool) -> Vec<&'static str> {
    let mut actions = Vec::new();
    if enabled {
        actions.push("disable");
    } else {
        actions.push("enable");
    }
    actions.push("uninstall");
    actions.push("update");
    actions.push("back");
    actions
}

fn action_label(action: &str) -> String {
    match action {
        "disable" => i18n::tr("panel-plugin-action-disable"),
        "enable" => i18n::tr("panel-plugin-action-enable"),
        "uninstall" => i18n::tr("panel-plugin-action-uninstall"),
        "update" => i18n::tr("panel-plugin-action-update"),
        "back" => i18n::tr("panel-plugin-action-back"),
        _ => action.to_string(),
    }
}

// ── Render: Installed list ────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_installed(
    lines: &mut Vec<Line<'_>>,
    plugins: &[PluginSummary],
    sel: usize,
    scroll_start: usize,
    visible: usize,
    count: usize,
    bold_style: Style,
    muted_style: Style,
    dim_style: Style,
    primary_style: Style,
    title_style: Style,
    error_style: Style,
    success_color: Color,
    title_color: Color,
) {
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "panel-plugin-stats",
            &[("count".into(), FluentValue::from(count as i64))],
        ),
        bold_style,
    )]));
    lines.push(Line::from(""));

    if plugins.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-empty"),
            muted_style,
        )]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-empty-hint"),
            muted_style,
        )]));
    } else {
        for (i, p) in plugins.iter().enumerate().skip(scroll_start).take(visible) {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                title_style
            } else {
                primary_style
            };

            // Status icon
            let (icon, icon_color) = if p.load_error.is_some() {
                ("✗", error_style.fg.unwrap_or_default())
            } else if !p.enabled {
                ("◯", muted_style.fg.unwrap_or_default())
            } else {
                ("✓", success_color)
            };

            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", cursor), Style::new().fg(title_color)),
                Span::styled(format!("{} ", icon), Style::new().fg(icon_color)),
                Span::styled(p.name.clone(), name_style),
                Span::styled(
                    format!(
                        " v{}",
                        if p.version.is_empty() {
                            i18n::tr("panel-plugin-version-unknown")
                        } else {
                            p.version.clone()
                        }
                    ),
                    muted_style,
                ),
            ]));
            if !p.description.is_empty() {
                lines.push(Line::from(vec![Span::styled(
                    format!("     {}", p.description),
                    dim_style,
                )]));
            } else {
                lines.push(Line::from(""));
            }
            let root: String = p.root.chars().take(72).collect();
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", root),
                dim_style,
            )]));
            // extras with i18n labels
            let mut extras: Vec<String> = Vec::new();
            if p.skills_count > 0 {
                extras.push(format!(
                    "{}:{}",
                    i18n::tr("panel-plugin-field-skills"),
                    p.skills_count
                ));
            }
            if p.commands_count > 0 {
                extras.push(format!(
                    "{}:{}",
                    i18n::tr("panel-plugin-field-commands"),
                    p.commands_count
                ));
            }
            if p.agents_count > 0 {
                extras.push(format!(
                    "{}:{}",
                    i18n::tr("panel-plugin-field-agents"),
                    p.agents_count
                ));
            }
            if p.mcp_count > 0 {
                extras.push(format!(
                    "{}:{}",
                    i18n::tr("panel-plugin-field-mcp"),
                    p.mcp_count
                ));
            }
            let extra = if extras.is_empty() {
                String::from("—")
            } else {
                extras.join(" · ")
            };
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", extra),
                dim_style,
            )]));
            lines.push(Line::from(""));
        }
    }
}

// ── Render: Detail + Actions ─────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_detail(
    lines: &mut Vec<Line<'_>>,
    p: &PluginSummary,
    action_index: usize,
    bold_style: Style,
    muted_style: Style,
    dim_style: Style,
    primary_style: Style,
    error_style: Style,
    success_color: Color,
    title_color: Color,
    title_style: Style,
    confirm_action_text: Option<&str>,
    warning_color: Color,
) {
    // Title
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "panel-plugin-detail-title",
            &[("name".into(), FluentValue::from(p.name.clone()))],
        ),
        bold_style,
    )]));
    lines.push(Line::from(""));

    // Status
    let (status_text, status_color) = if p.load_error.is_some() {
        (
            format!("  ✗ {}", i18n::tr("panel-plugin-detail-error")),
            error_style.fg.unwrap_or_default(),
        )
    } else if !p.enabled {
        (
            format!("  ◯ {}", i18n::tr("panel-plugin-status-disabled")),
            muted_style.fg.unwrap_or_default(),
        )
    } else {
        (
            format!("  ✓ {}", i18n::tr("panel-plugin-status-enabled")),
            success_color,
        )
    };
    lines.push(Line::from(vec![Span::styled(
        status_text,
        Style::new().fg(status_color),
    )]));
    lines.push(Line::from(""));

    // Fields
    let fields: [(&str, &dyn Fn() -> String); 4] = [
        ("panel-plugin-detail-marketplace", &|| p.marketplace.clone()),
        ("panel-plugin-detail-author", &|| {
            p.author.clone().unwrap_or_else(|| "—".to_string())
        }),
        ("panel-plugin-detail-path", &|| p.root.clone()),
        ("panel-plugin-detail-scope", &|| p.install_scope.clone()),
    ];
    for (label_key, get_value) in &fields {
        lines.push(Line::from(vec![
            Span::styled(format!("    {}: ", i18n::tr(label_key)), muted_style),
            Span::styled(get_value(), dim_style),
        ]));
    }
    lines.push(Line::from(""));

    // Capabilities
    let caps: [(&str, usize); 4] = [
        ("panel-plugin-field-skills", p.skills_count),
        ("panel-plugin-field-commands", p.commands_count),
        ("panel-plugin-field-agents", p.agents_count),
        ("panel-plugin-field-mcp", p.mcp_count),
    ];
    for (label_key, count) in &caps {
        let value = if *count > 0 {
            count.to_string()
        } else {
            "—".to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("    {}: ", i18n::tr(label_key)), muted_style),
            Span::styled(value, dim_style),
        ]));
    }

    // Load error
    if let Some(ref err) = p.load_error {
        lines.push(Line::from(""));
        let err_text: String = err.chars().take(72).collect();
        lines.push(Line::from(vec![Span::styled(
            format!("    {}", err_text),
            error_style,
        )]));
    }

    // Action menu
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("panel-plugin-detail-actions")),
        bold_style,
    )]));
    lines.push(Line::from(""));

    // Confirm hint (if in confirm mode)
    if let Some(action) = confirm_action_text {
        let confirm_key = match action {
            "uninstall" => "panel-plugin-confirm-uninstall",
            "delete_marketplace" => "panel-plugin-confirm-delete-mp",
            _ => "panel-plugin-confirm-uninstall",
        };
        lines.push(Line::from(vec![Span::styled(
            format!("  {}", i18n::tr(confirm_key)),
            Style::new().fg(warning_color),
        )]));
        lines.push(Line::from(""));
    }

    let actions = action_list(p.enabled);
    for (i, action) in actions.iter().enumerate() {
        let is_selected = i == action_index;
        let cursor = if is_selected { ">" } else { " " };
        let style = if is_selected {
            title_style
        } else {
            primary_style
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", cursor), Style::new().fg(title_color)),
            Span::styled(format!("    {}", action_label(action)), style),
        ]));
    }
}

// ── Render: Discover List ─────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_discover_list(
    lines: &mut Vec<Line<'_>>,
    search_text: &str,
    show_cursor: bool,
    search_state: &SearchState,
    items: &[&PluginSearchResultItem],
    sel: usize,
    scroll_start: usize,
    visible: usize,
    bold_style: Style,
    muted_style: Style,
    dim_style: Style,
    primary_style: Style,
    _error_style: Style,
    _success_color: Color,
    title_color: Color,
    title_style: Style,
) {
    // Early return for search state
    match search_state {
        SearchState::Loading => {
            lines.push(Line::from(vec![Span::styled("  Discover", bold_style)]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("panel-plugin-search-loading"),
                muted_style,
            )]));
            return;
        }
        SearchState::Error(msg) => {
            lines.push(Line::from(vec![Span::styled("  Discover", bold_style)]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                i18n::tr_args(
                    "panel-plugin-search-error",
                    &[("error".into(), FluentValue::from(msg.clone()))],
                ),
                _error_style,
            )]));
            return;
        }
        SearchState::Idle => {}
    }

    lines.push(Line::from(vec![Span::styled("  Discover", bold_style)]));
    lines.push(Line::from(""));

    // Search/filter box — simplified single-line input style
    {
        let display: String = search_text.chars().collect();
        let cursor = if show_cursor { "▌" } else { " " };
        let placeholder = if display.is_empty() {
            i18n::tr("panel-plugin-discover-input")
        } else {
            "".to_string()
        };
        lines.push(Line::from(vec![Span::styled(
            format!("  > {}{}{}", display, cursor, placeholder),
            muted_style,
        )]));
    }
    lines.push(Line::from(""));

    if items.is_empty() {
        if search_text.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("panel-plugin-discover-empty"),
                muted_style,
            )]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("panel-plugin-search-no-results"),
                muted_style,
            )]));
        }
    } else {
        for (i, item) in items.iter().enumerate().skip(scroll_start).take(visible) {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                title_style
            } else {
                primary_style
            };

            // 已安装标记
            let installed_mark = {
                let store = PLUGIN_LIST.state();
                let installed_guard = store.read();
                let installed_ids: std::collections::HashSet<&str> =
                    installed_guard.iter().map(|p| p.name.as_str()).collect();
                if installed_ids.contains(item.name.as_str()) {
                    " \u{2713}"
                } else {
                    ""
                }
            };

            // Name + version + marketplace
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", cursor), Style::new().fg(title_color)),
                Span::styled(format!("{} v{}  ", item.name, item.version), name_style),
                Span::styled(
                    format!("({}){}", item.marketplace, installed_mark),
                    dim_style,
                ),
            ]));

            // Description (truncated)
            let desc: String = if item.description.is_empty() {
                "—".into()
            } else {
                item.description.chars().take(60).collect()
            };
            lines.push(Line::from(vec![Span::styled(
                format!("    {}", desc),
                dim_style,
            )]));
            lines.push(Line::from(""));
        }
    }
}

// ── Render: Discover Detail ───────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_discover_detail(
    lines: &mut Vec<Line<'_>>,
    dp: &PluginSearchResultItem,
    action_cursor: usize,
    bold_style: Style,
    muted_style: Style,
    dim_style: Style,
    primary_style: Style,
    _success_color: Color,
    title_color: Color,
    title_style: Style,
) {
    // Title
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "panel-plugin-detail-title",
            &[("name".into(), FluentValue::from(dp.name.clone()))],
        ),
        bold_style,
    )]));
    lines.push(Line::from(""));

    // Fields
    {
        let fields: [(&str, &str); 4] = [
            ("panel-plugin-discover-field-version", &dp.version),
            ("panel-plugin-discover-field-marketplace", &dp.marketplace),
            (
                "panel-plugin-discover-field-author",
                dp.author.as_deref().unwrap_or("—"),
            ),
            (
                "panel-plugin-discover-field-description",
                if dp.description.is_empty() {
                    "—"
                } else {
                    &dp.description
                },
            ),
        ];
        for (label_key, value) in &fields {
            let truncated: String = value.chars().take(60).collect();
            lines.push(Line::from(vec![
                Span::styled(format!("    {}: ", i18n::tr(label_key)), muted_style),
                Span::styled(truncated, dim_style),
            ]));
        }
    }
    lines.push(Line::from(""));

    // Action menu
    lines.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("panel-plugin-detail-actions")),
        bold_style,
    )]));
    lines.push(Line::from(""));

    for (i, action) in DiscoverDetailAction::ALL.iter().enumerate() {
        let is_selected = i == action_cursor;
        let cursor = if is_selected { ">" } else { " " };
        let style = if is_selected {
            title_style
        } else {
            primary_style
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", cursor), Style::new().fg(title_color)),
            Span::styled(format!("    {}", action.label()), style),
        ]));
    }
}

// ── Obsolete: Render Discover (search box only) ────────────────────────

// ── Render: Marketplaces ─────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_marketplaces(
    lines: &mut Vec<Line<'_>>,
    sel: usize,
    bold_style: Style,
    muted_style: Style,
    success_color: Color,
    warning_color: Color,
    error_style: Style,
    refreshing: bool,
    add_active: bool,
    add_input: &str,
    confirm_action_text: Option<&str>,
    operation_name: Option<&str>,
) {
    // Show confirmation dialog for marketplace delete
    if let Some(action) = confirm_action_text
        && action == "delete_marketplace"
    {
        let name = operation_name.unwrap_or_default();
        lines.push(Line::from(vec![Span::styled(
            format!("  {}: {}", i18n::tr("panel-plugin-confirm-delete-mp"), name),
            Style::new().fg(warning_color),
        )]));
        lines.push(Line::from(""));
        return;
    }

    lines.push(Line::from(vec![Span::styled("  Marketplaces", bold_style)]));
    lines.push(Line::from(""));

    let entries = get_marketplace_cache();
    let dim_color = muted_style.fg.unwrap_or_default();

    // Add Marketplace 行 (item 0)
    if add_active {
        let display: String = add_input.chars().take(40).collect();
        lines.push(Line::from(vec![
            Span::styled("  > ", bold_style),
            Span::styled(
                format!(
                    "{} {}",
                    i18n::tr("panel-plugin-marketplace-add-label"),
                    display
                ),
                bold_style,
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-marketplace-add-url-hint"),
            muted_style,
        )]));
    } else {
        let is_sel = sel == 0;
        let cursor = if is_sel { ">" } else { " " };
        let style = if is_sel { bold_style } else { muted_style };
        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", cursor), style),
            Span::styled(
                format!("+ {}", i18n::tr("panel-plugin-marketplaces-add")),
                style,
            ),
        ]));
    }
    lines.push(Line::from(""));

    // Marketplace 条目
    for (i, entry) in entries.iter().enumerate() {
        let item_idx = i + 1;
        let is_selected = sel == item_idx;
        let cursor = if is_selected { ">" } else { " " };
        let name_style = if is_selected { bold_style } else { muted_style };

        // 状态图标
        let (icon, icon_color) = match entry.status {
            MsStatus::Fresh => ("●", success_color),
            MsStatus::Cached => ("●", success_color),
            MsStatus::Fetching => ("◌", warning_color),
            MsStatus::Stale => ("○", dim_color),
            MsStatus::Failed => ("✗", error_style.fg.unwrap_or_default()),
            MsStatus::NotFound => ("○", dim_color),
        };
        let status_text = match entry.status {
            MsStatus::Fresh => "fresh",
            MsStatus::Cached => "cached",
            MsStatus::Fetching => "fetching",
            MsStatus::Stale => "stale",
            MsStatus::Failed => "failed",
            MsStatus::NotFound => "not fetched",
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", cursor), name_style),
            Span::styled(format!("{} ", icon), Style::new().fg(icon_color)),
            Span::styled(format!("{} ", entry.name), name_style),
            Span::styled(format!("({})", status_text), muted_style),
        ]));

        // 第二行: source + stats
        let stats = format!(
            "{}: {}  |  plugins: {}  |  installed: {}",
            if entry.auto_update { "auto" } else { "manual" },
            entry.source_label.chars().take(30).collect::<String>(),
            entry.plugin_count,
            entry.installed_count,
        );
        lines.push(Line::from(vec![Span::styled(
            format!("     {}", stats),
            Style::new().fg(dim_color),
        )]));
        lines.push(Line::from(""));
    }

    // Footer hints
    if refreshing {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-marketplace-refreshing"),
            Style::new().fg(warning_color),
        )]));
    } else {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-marketplace-hint-keys"),
            muted_style,
        )]));
    }
}

// ── Render: Errors ────────────────────────────────────────────────────

fn render_errors(
    lines: &mut Vec<Line<'_>>,
    plugins: &[PluginSummary],
    bold_style: Style,
    muted_style: Style,
    dim_style: Style,
    error_style: Style,
) {
    let errors: Vec<&PluginSummary> = plugins.iter().filter(|p| p.load_error.is_some()).collect();
    lines.push(Line::from(vec![Span::styled(
        format!(
            "  {} ({})",
            i18n::tr("panel-plugin-errors-title"),
            errors.len()
        ),
        bold_style,
    )]));
    lines.push(Line::from(""));

    if errors.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-errors-empty"),
            muted_style,
        )]));
    } else {
        for p in &errors {
            lines.push(Line::from(vec![Span::styled(
                format!("  ✗ {} v{}", p.name, p.version),
                error_style,
            )]));
            if let Some(ref err) = p.load_error {
                let err_text: String = err.chars().take(72).collect();
                lines.push(Line::from(vec![Span::styled(
                    format!("      {}", err_text),
                    dim_style,
                )]));
            }
            lines.push(Line::from(""));
        }
    }
}

fn load_marketplace_data() -> Vec<MsEntry> {
    use peri_middlewares::plugin::{
        MarketplaceManager, MarketplaceSource, load_known_marketplaces, marketplace,
    };

    let known = load_known_marketplaces(None).unwrap_or_default();
    let cache_dir = peri_middlewares::plugin::marketplaces_cache_dir();
    let _ = std::fs::create_dir_all(&cache_dir);

    let installed = peri_middlewares::plugin::load_installed_plugins(None).unwrap_or_default();

    known
        .iter()
        .map(|km| {
            let name = MarketplaceManager::extract_name(&km.source);

            // 确定状态
            let cache_path = cache_dir.join(&name);
            let manifest_path = marketplace::find_marketplace_json(&cache_path);
            let mut status = if km.install_location.is_empty() {
                MsStatus::NotFound
            } else if manifest_path.is_none() {
                MsStatus::NotFound
            } else {
                MsStatus::Cached
            };

            // B3: 检查 manifest mtime，超过 24h 标记为 Stale
            if status == MsStatus::Cached
                && let Some(ref path) = manifest_path
                && let Ok(meta) = std::fs::metadata(path)
                && let Ok(mtime) = meta.modified()
                && let Ok(elapsed) = mtime.elapsed()
                && elapsed.as_secs() > 24 * 3600
            {
                status = MsStatus::Stale;
            }

            // 从 cached manifest 统计插件数
            let mut manifest_parse_failed = false;
            let plugin_count = match manifest_path.as_ref() {
                Some(path) => {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) {
                            manifest
                                .get("plugins")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0)
                        } else {
                            manifest_parse_failed = true;
                            0
                        }
                    } else {
                        manifest_parse_failed = true;
                        0
                    }
                }
                None => 0,
            };

            if manifest_parse_failed {
                status = MsStatus::Failed;
            }

            // 统计已安装的插件数（来自此 marketplace）
            let installed_count = installed
                .plugins
                .iter()
                .filter(|p| {
                    let mp = if let Some((_, mkt)) = p.id.split_once('@') {
                        mkt
                    } else {
                        ""
                    };
                    mp == name
                })
                .count();

            MsEntry {
                name,
                source_label: match &km.source {
                    MarketplaceSource::GitHub { repo } => format!("github:{}", repo),
                    MarketplaceSource::Git { url } => format!("git:{}", url),
                    MarketplaceSource::Url { url } => url.clone(),
                    MarketplaceSource::Directory { path } => path.clone(),
                    MarketplaceSource::File { path } => path.clone(),
                    MarketplaceSource::Npm { package } => format!("npm:{}", package),
                },
                plugin_count,
                installed_count,
                status,
                last_updated: if km.last_updated.is_empty() {
                    None
                } else {
                    Some(km.last_updated.clone())
                },
                auto_update: km.auto_update,
            }
        })
        .collect()
}

fn load_discover_plugins_from_disk() -> Vec<PluginSearchResultItem> {
    use peri_middlewares::plugin::{
        MarketplaceManager, MarketplaceSource, load_known_marketplaces, marketplace,
    };

    let mut known = load_known_marketplaces(None).unwrap_or_default();
    let cache_dir = peri_middlewares::plugin::marketplaces_cache_dir();
    let _ = std::fs::create_dir_all(&cache_dir);

    // 确保 official marketplace 已注册（参考项目行为：自动注入）
    let has_official = known.iter().any(|km| match &km.source {
        MarketplaceSource::GitHub { repo } => repo == "anthropics/claude-plugins-official",
        _ => false,
    });
    if !has_official {
        known.push(peri_middlewares::plugin::KnownMarketplace {
            source: MarketplaceSource::GitHub {
                repo: "anthropics/claude-plugins-official".into(),
            },
            install_location: cache_dir
                .join("claude-plugins-official")
                .to_string_lossy()
                .to_string(),
            auto_update: true,
            last_updated: String::new(),
        });
    }

    let mut plugins: Vec<PluginSearchResultItem> = Vec::new();
    let installed = peri_middlewares::plugin::load_installed_plugins(None).unwrap_or_default();
    let installed_ids: std::collections::HashSet<String> =
        installed.plugins.iter().map(|p| p.id.clone()).collect();

    for km in &known {
        let mp_name = MarketplaceManager::extract_name(&km.source);
        let mp_dir = cache_dir.join(&mp_name);
        let manifest_path = match marketplace::find_marketplace_json(&mp_dir) {
            Some(path) => path,
            None => continue,
        };
        if let Ok(content) = std::fs::read_to_string(&manifest_path)
            && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(plugin_list) = manifest.get("plugins").and_then(|v| v.as_array())
        {
            for p in plugin_list {
                let name = p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let plugin_id = format!("{}@{}", name, mp_name);
                if installed_ids.contains(&plugin_id) {
                    continue;
                }
                // author 可能是字符串或 {"name": "..."} 对象
                let author = p.get("author").and_then(|v| {
                    v.as_str().map(|s| s.to_string()).or_else(|| {
                        v.get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                    })
                });
                let version = p
                    .get("version")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("—")
                    .to_string();
                plugins.push(PluginSearchResultItem {
                    name,
                    version,
                    marketplace: mp_name.clone(),
                    description: p
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    author,
                });
            }
        }
    }
    plugins
}

fn close_panel() {
    crate::kit::panel_registry::close_active_panel();
}
