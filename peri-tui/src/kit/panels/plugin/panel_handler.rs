use crate::components::textarea::TextAreaState;
use crate::kit::atoms::{PLUGIN_LIST, PluginViewTab};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{ListLayout, hit_item, hit_row, is_scrollbar_column};
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State};
use ratatui_kit::ratatui::layout::Rect;

use super::data::get_marketplace_cache;
use super::{VISIBLE_ITEMS, action_list, cycle_backward, cycle_forward};

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_panel_event(
    event: Event,
    event_scope: &crate::application::operations::OperationScope,
    area: Option<Rect>,
    selected: State<usize>,
    active_tab: State<PluginViewTab>,
    discover: State<super::discover::DiscoverState>,
    action_index: State<usize>,
    confirm_action: State<Option<String>>,
    marketplace_confirmation: State<Option<super::MarketplaceIdentity>>,
    operation: State<super::operation::OperationState>,
    detail_plugin: State<Option<super::InstalledIdentity>>,
    marketplace_detail: State<Option<super::MarketplaceIdentity>>,
    marketplace_detail_action: State<usize>,
    add_marketplace_input: State<TextAreaState>,
    add_marketplace_active: State<bool>,
) -> EventResult {
    if event_scope != &super::current_scope() {
        return EventResult::Consumed;
    }
    if super::identity::reconcile_details(
        detail_plugin,
        marketplace_detail,
        marketplace_confirmation,
        confirm_action,
        operation,
    ) {
        return EventResult::Consumed;
    }
    if super::operation::handle_pending_event(&event, operation) {
        return EventResult::Consumed;
    }
    // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
    if let Event::Mouse(mouse) = event {
        if let Some(area) = area
            && !is_scrollbar_column(&mouse, area)
        {
            // ── 确认模式（uninstall / delete_marketplace）：点击 = 确认（Enter @L625）──
            let confirmation = confirm_action.read().clone();
            if let Some(action) = confirmation
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            {
                let saved_marketplace = marketplace_confirmation.read().clone();
                match action.as_str() {
                    "uninstall" => {
                        super::operation::confirm_uninstall(
                            operation,
                            confirm_action,
                            detail_plugin,
                        );
                    }
                    "delete_marketplace" => {
                        super::operation::confirm_marketplace_remove(
                            saved_marketplace,
                            operation,
                            confirm_action,
                            marketplace_confirmation,
                        );
                    }
                    _ => {
                        *confirm_action.write() = None;
                        *marketplace_confirmation.write() = None;
                        operation.write().confirmation = None;
                    }
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
                let identity = marketplace_detail.read().clone();
                let entries = get_marketplace_cache();
                if let Some(entry) = identity
                    .as_ref()
                    .and_then(|key| key.resolve(&entries, event_scope))
                {
                    match idx {
                        0 => {
                            super::operation::dispatch(
                                super::operation::PluginOperation::marketplace_refresh(
                                    entry.name.clone(),
                                ),
                                operation,
                            );
                        }
                        _ => {
                            // Delete
                            *confirm_action.write() = Some("delete_marketplace".into());
                            *marketplace_confirmation.write() = identity.clone();
                        }
                    }
                }
                *marketplace_detail.write() = None;
                *marketplace_detail_action.write() = 0;
                return EventResult::Consumed;
            }
            // ── Installed 详情：点击 action 行 = 执行（Enter @L738）──
            // 提取 detail_plugin 的 guard：if-let scrutinee 临时 guard
            // 存活到块结束，块内多次 write detail_plugin 会死锁。
            let detail_plugin_opt = detail_plugin.read().clone();
            let plugins = PLUGIN_LIST.state().read().clone();
            if let Some(detail) = detail_plugin_opt
                && let Some(detail_p) = detail.resolve(&plugins, event_scope)
                && let Some(idx) = super::operation::detail_action_index(
                    &event,
                    Some(area),
                    detail_p,
                    *action_index.read(),
                )
            {
                *action_index.write() = idx;
                let actions = action_list(detail_p.enabled);
                if let Some(action) = actions.get(idx) {
                    super::operation::installed_action(
                        action,
                        detail_p,
                        operation,
                        confirm_action,
                        detail_plugin,
                    );
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
                        *detail_plugin.write() =
                            PLUGIN_LIST.state().read().get(idx).map(|plugin| {
                                super::InstalledIdentity::new(plugin, event_scope.clone())
                            });
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
                        *marketplace_detail.write() = get_marketplace_cache()
                            .get(s.saturating_sub(1))
                            .map(|entry| {
                                super::MarketplaceIdentity::new(entry, event_scope.clone())
                            });
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
    let in_detail = detail_plugin.read().is_some();
    let in_marketplace_detail = marketplace_detail.read().is_some();

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
                let identity = marketplace_detail.read().clone();
                let entries = get_marketplace_cache();
                if let Some(entry) = identity
                    .as_ref()
                    .and_then(|key| key.resolve(&entries, event_scope))
                {
                    match *marketplace_detail_action.read() {
                        0 => {
                            super::operation::dispatch(
                                super::operation::PluginOperation::marketplace_refresh(
                                    entry.name.clone(),
                                ),
                                operation,
                            );
                        }
                        _ => {
                            // Delete
                            *confirm_action.write() = Some("delete_marketplace".into());
                            *marketplace_confirmation.write() = identity.clone();
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
            let saved_marketplace = marketplace_confirmation.read().clone();

            match action.as_str() {
                "uninstall" => {
                    super::operation::confirm_uninstall(operation, confirm_action, detail_plugin);
                }
                "delete_marketplace" => {
                    super::operation::confirm_marketplace_remove(
                        saved_marketplace,
                        operation,
                        confirm_action,
                        marketplace_confirmation,
                    );
                }
                _ => {
                    *confirm_action.write() = None;
                    *marketplace_confirmation.write() = None;
                    operation.write().confirmation = None;
                }
            }
        }
        (_, true, _) => {
            // 读守卫已释放，确认取消在当前事件内同步完成。
            *confirm_action.write() = None;
            *marketplace_confirmation.write() = None;
            operation.write().confirmation = None;
        }
        // ── Tab/Shift+Tab 切换视图 ──
        (false, false, KeyCode::Tab) => {
            let mut tab = active_tab.write();
            *tab = cycle_forward(*tab);
            *selected.write() = 0;
            discover.write().selected = 0;
        }
        (false, false, KeyCode::BackTab) => {
            let mut tab = active_tab.write();
            *tab = cycle_backward(*tab);
            *selected.write() = 0;
            discover.write().selected = 0;
        }
        // ── List: Enter → 进入详情 / 删除 marketplace / discover detail ──
        (false, false, KeyCode::Enter) => {
            if *active_tab.read() == PluginViewTab::Installed {
                let s = *selected.read();
                let c = PLUGIN_LIST.state().read().len();
                if c > 0 {
                    *detail_plugin.write() =
                        PLUGIN_LIST.state().read().get(s).map(|plugin| {
                            super::InstalledIdentity::new(plugin, event_scope.clone())
                        });
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
                    *marketplace_detail.write() = get_marketplace_cache()
                        .get(s.saturating_sub(1))
                        .map(|entry| super::MarketplaceIdentity::new(entry, event_scope.clone()));
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
            let identity = detail_plugin.read().clone();
            let plugins = PLUGIN_LIST.state().read().clone();
            if let Some(p) = identity
                .as_ref()
                .and_then(|key| key.resolve(&plugins, event_scope))
            {
                let actions = action_list(p.enabled);
                let ai =
                    super::operation::detail_action_index(&event, area, p, *action_index.read());
                if let Some(action) = ai.and_then(|index| actions.get(index)) {
                    super::operation::installed_action(
                        action,
                        p,
                        operation,
                        confirm_action,
                        detail_plugin,
                    );
                }
            }
        }
        // ── Detail: ↑/↓ 导航操作菜单 ──
        (true, false, KeyCode::Up) => {
            let identity = detail_plugin.read().clone();
            let plugins = PLUGIN_LIST.state().read().clone();
            if let Some(p) = identity
                .as_ref()
                .and_then(|key| key.resolve(&plugins, event_scope))
            {
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
            let identity = detail_plugin.read().clone();
            let plugins = PLUGIN_LIST.state().read().clone();
            if let Some(p) = identity
                .as_ref()
                .and_then(|key| key.resolve(&plugins, event_scope))
            {
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
            discover.write().selected = 0;
        }
        (false, false, KeyCode::Right) => {
            let mut tab = active_tab.write();
            *tab = cycle_forward(*tab);
            *selected.write() = 0;
            discover.write().selected = 0;
        }
        (false, false, KeyCode::Up) => {
            if *active_tab.read() == PluginViewTab::Installed {
                let mut s = selected.write();
                *s = previous_selection(*s);
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
