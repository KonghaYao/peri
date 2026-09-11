//! Discover input decisions and their UI/RPC effects share one selected identity.

use super::PluginSearchResultItem;
use super::discover::{DiscoverState, SearchSession};
use super::{DiscoverDetailAction, VISIBLE_ITEMS, close_panel, data::get_discover_cache};
use crate::kit::atoms::ACP_CLIENT_HANDLE;
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{ListLayout, hit_item, is_scrollbar_column};
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State};
use ratatui_kit::ratatui::layout::Rect;

#[derive(Debug, PartialEq)]
pub(super) enum SearchAction {
    Pass,
    Consume,
    FocusInput,
    Insert(char),
    Backspace,
    Submit,
    Open(usize),
    Up,
    Down,
    Escape,
    DetailAction(usize),
    LeaveDetail,
}

pub(super) fn decide(
    event: &Event,
    area: Option<Rect>,
    state: &DiscoverState,
    local: &[PluginSearchResultItem],
) -> SearchAction {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if state.detail.is_some() {
                return match key.code {
                    KeyCode::Up => SearchAction::Up,
                    KeyCode::Down => SearchAction::Down,
                    KeyCode::Enter => SearchAction::DetailAction(state.detail_action),
                    KeyCode::Esc => SearchAction::Escape,
                    KeyCode::Tab => SearchAction::LeaveDetail,
                    _ => SearchAction::Consume,
                };
            }
            match key.code {
                KeyCode::Char(c) => SearchAction::Insert(c),
                KeyCode::Backspace => SearchAction::Backspace,
                KeyCode::Enter if state.focused => SearchAction::Submit,
                KeyCode::Enter => SearchAction::Open(state.selected),
                KeyCode::Up => SearchAction::Up,
                KeyCode::Down => SearchAction::Down,
                KeyCode::Esc => SearchAction::Escape,
                _ => SearchAction::Pass,
            }
        }
        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
            let Some(area) = area else {
                return SearchAction::Pass;
            };
            if is_scrollbar_column(mouse, area) {
                return SearchAction::Pass;
            }
            if state.detail.is_some() {
                return hit_item(
                    mouse,
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
                .map(SearchAction::DetailAction)
                .unwrap_or(SearchAction::Consume);
            }
            if hit_item(
                mouse,
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
                return if state.focused {
                    SearchAction::Submit
                } else {
                    SearchAction::FocusInput
                };
            }
            if !matches!(state.status, super::SearchState::Idle) {
                return SearchAction::Consume;
            }
            let count = state.visible_items(local).len();
            hit_item(
                mouse,
                area,
                ListLayout {
                    header_rows: 5,
                    item_rows: 3,
                    footer_rows: 0,
                    visible_items: VISIBLE_ITEMS as u16,
                    scroll_start: scroll_start_for_selected(state.selected, count, VISIBLE_ITEMS),
                    item_count: count,
                },
            )
            .map(SearchAction::Open)
            .unwrap_or(SearchAction::Consume)
        }
        _ => SearchAction::Pass,
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum SearchEffect {
    Pass,
    Consumed,
    Submit,
    ClosePanel,
    Install {
        item: PluginSearchResultItem,
        scope: &'static str,
    },
}

pub(super) fn apply(
    state: &mut DiscoverState,
    action: SearchAction,
    local: &[PluginSearchResultItem],
) -> SearchEffect {
    match action {
        SearchAction::Pass => return SearchEffect::Pass,
        SearchAction::Consume => {}
        SearchAction::FocusInput => state.focused = true,
        SearchAction::Insert(c) => state.edit(|editor| editor.insert_char(c)),
        SearchAction::Backspace => state.edit(|editor| {
            editor.backspace();
        }),
        SearchAction::Submit => return SearchEffect::Submit,
        SearchAction::Open(index) => state.open_selected(index, local),
        SearchAction::Up if state.detail.is_some() => {
            state.detail_action = previous_selection(state.detail_action)
        }
        SearchAction::Down if state.detail.is_some() => {
            state.detail_action =
                next_selection(state.detail_action, DiscoverDetailAction::ALL.len())
        }
        SearchAction::Up => {
            if state.selected == 0 {
                state.focused = true;
            } else {
                state.selected = previous_selection(state.selected);
            }
        }
        SearchAction::Down => {
            if state.focused {
                state.focused = false;
            } else {
                state.selected = next_selection(state.selected, state.visible_items(local).len());
            }
        }
        SearchAction::Escape => {
            if state.detail.is_some() {
                state.close_detail();
            } else if !state.editor.text.is_empty() {
                state.edit(|editor| editor.clear());
            } else if state.focused {
                state.focused = false;
            } else {
                state.close();
                return SearchEffect::ClosePanel;
            }
        }
        SearchAction::LeaveDetail => {
            state.close_detail();
            return SearchEffect::Pass;
        }
        SearchAction::DetailAction(index) => {
            state.detail_action = index;
            let selection = state.detail_selection();
            state.close_detail();
            if let Some((item, action)) = selection {
                let scope = match action {
                    DiscoverDetailAction::InstallUser => "user",
                    DiscoverDetailAction::InstallProject => "project",
                    DiscoverDetailAction::BackToList => return SearchEffect::Consumed,
                };
                return SearchEffect::Install { item, scope };
            }
        }
    }
    SearchEffect::Consumed
}

pub(super) fn handle_event(
    event: Event,
    area: Option<Rect>,
    discover: State<DiscoverState>,
    operation_loading: State<Option<String>>,
) -> EventResult {
    let local = get_discover_cache();
    let action = decide(&event, area, &discover.read(), &local);
    if action == SearchAction::Pass {
        return EventResult::Ignored;
    }
    let effect = apply(&mut discover.write(), action, &local);
    match effect {
        SearchEffect::Pass => return EventResult::Ignored,
        SearchEffect::Consumed => {}
        SearchEffect::ClosePanel => close_panel(),
        SearchEffect::Submit => {
            let session = SearchSession::current();
            let ticket = discover.write().begin_search(session);
            if let Some(ticket) = ticket {
                drop(super::search_request::launch_search(
                    ticket,
                    ACP_CLIENT_HANDLE.get().cloned(),
                    move |ticket, result| {
                        if let Some(mut state) = discover.try_write() {
                            state.complete(ticket, result);
                            None
                        } else {
                            // A render may still borrow the hook. Return ownership
                            // for cancellable delivery retry; hook Drop cancels it.
                            Some(result)
                        }
                    },
                ));
            }
        }
        SearchEffect::Install { item, scope } => {
            *operation_loading.write() = Some("install".into());
            if let Some(client) = ACP_CLIENT_HANDLE.get().cloned() {
                let sid = client.current_session_id().unwrap_or_default();
                tokio::spawn(async move {
                    let _ = client.send_raw_request("plugin/install", serde_json::json!({
                        "name": item.name, "marketplace": item.marketplace, "scope": scope, "sessionId": sid,
                    })).await;
                });
            }
        }
    }
    EventResult::Consumed
}
