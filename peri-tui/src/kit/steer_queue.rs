use std::sync::{Arc, Mutex};

use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{layout::Constraint, style::Style},
};

use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, TERMINAL_CAPS};
use crate::kit::focus_router::{FocusLayer, active_layer};
use crate::kit::terminal_caps::symbols;

mod view;
use view::{QueueFrame, QueueView, Selection, ViewState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SteerQueueItem {
    pub id: String,
    pub text: String,
    pub state: SteerItemState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteerItemState {
    Queued,
    Submitting,
    Dispatching,
    Withdrawing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SteerQueueAction {
    Dispatch { ids: Vec<String> },
    TakeBack { id: String },
}

#[derive(Default, Props)]
pub struct SteerQueueProps {
    pub items: Vec<SteerQueueItem>,
    pub max_rows: usize,
    pub max_height: Option<u16>,
    pub on_height_change: Arc<Mutex<Handler<'static, u16>>>,
    pub on_action: Arc<Mutex<Handler<'static, SteerQueueAction>>>,
}

#[component]
pub fn SteerQueue(props: &SteerQueueProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_atom(&THEME_ATOM);
    let caps = hooks.use_atom(&TERMINAL_CAPS);
    let _language = hooks.use_atom(&LANG_VERSION);
    let (_, terminal_height) = hooks.use_terminal_size();
    let state = hooks.use_state(ViewState::default);
    let rendered = hooks.use_state(|| Arc::new(Mutex::new(QueueFrame::default())));
    let max_rows = if props.max_rows == 0 {
        5
    } else {
        props.max_rows
    };
    let items = props.items.clone();
    let on_action = Arc::clone(&props.on_action);
    let frame = Arc::clone(&rendered.read());

    hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
        if items.is_empty() || !matches!(active_layer(), FocusLayer::Input) {
            return EventResult::Ignored;
        }
        let frame = frame.lock().expect("SteerQueue frame poisoned").clone();
        match event {
            Event::Mouse(mouse) => {
                if !frame.contains(mouse.column, mouse.row) {
                    if matches!(mouse.kind, MouseEventKind::Down(_)) {
                        state.write().selection = None;
                    }
                    return EventResult::Ignored;
                }
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        crate::kit::atoms::FOCUSED_ENTRY.set(None);
                        if let Some(control) = frame.hit(mouse.column, mouse.row) {
                            if control.enabled {
                                activate(&control.selection, &frame, &items, state, &on_action);
                            }
                        } else if let Some(id) = frame.row_at(mouse.row) {
                            state.write().selection = Some(Selection::Dispatch(id.to_owned()));
                        }
                        EventResult::Consumed
                    }
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                        let mut current = state.write();
                        current.expanded = true;
                        current.selection = None;
                        current.offset = if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                            frame
                                .offset
                                .saturating_add(1)
                                .min(items.len().saturating_sub(frame.rows.len()))
                        } else {
                            frame.offset.saturating_sub(1)
                        };
                        EventResult::Consumed
                    }
                    _ => EventResult::Ignored,
                }
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let targets = view::selections(&items, max_rows);
                if key.code == KeyCode::F(6) && key.modifiers == KeyModifiers::NONE {
                    crate::kit::atoms::FOCUSED_ENTRY.set(None);
                    let mut current = state.write();
                    current.selection = if current.selection.is_some() {
                        None
                    } else {
                        targets
                            .iter()
                            .find(|target| matches!(target, Selection::Dispatch(_)))
                            .or(targets.first())
                            .cloned()
                    };
                    return EventResult::Consumed;
                }
                let Some(selected) = state.read().selection.clone() else {
                    return EventResult::Ignored;
                };
                if key.code == KeyCode::Esc {
                    state.write().selection = None;
                    return EventResult::Consumed;
                }
                if key.modifiers != KeyModifiers::NONE {
                    state.write().selection = None;
                    return EventResult::Ignored;
                }
                match key.code {
                    KeyCode::Enter => {
                        activate(&selected, &frame, &items, state, &on_action);
                        EventResult::Consumed
                    }
                    KeyCode::Tab | KeyCode::Right | KeyCode::Down | KeyCode::Left | KeyCode::Up => {
                        if !targets.is_empty() {
                            let index = targets
                                .iter()
                                .position(|target| *target == selected)
                                .unwrap_or(0);
                            let next = if matches!(key.code, KeyCode::Left | KeyCode::Up) {
                                (index + targets.len() - 1) % targets.len()
                            } else {
                                (index + 1) % targets.len()
                            };
                            let mut current = state.write();
                            current.selection = Some(targets[next].clone());
                            if let Some(id) = targets[next].item_id()
                                && items
                                    .iter()
                                    .position(|item| item.id == id)
                                    .is_some_and(|index| index >= max_rows)
                            {
                                current.expanded = true;
                            }
                        }
                        EventResult::Consumed
                    }
                    _ => {
                        // 普通输入立即交回 composer；队列只持有显式进入的导航焦点。
                        state.write().selection = None;
                        EventResult::Ignored
                    }
                }
            }
            Event::Paste(_) => {
                state.write().selection = None;
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        }
    });

    let current = state.read().clone();
    let visible = if current.expanded {
        props
            .items
            .len()
            .min(usize::from(terminal_height.saturating_sub(7).max(1)))
    } else {
        props.items.len().min(max_rows)
    };
    let height = if props.items.is_empty() {
        0
    } else {
        visible
            .saturating_add(usize::from(view::HEADER_ROWS))
            .min(usize::from(u16::MAX)) as u16
    }
    .min(props.max_height.unwrap_or(u16::MAX));
    let on_height_change = Arc::clone(&props.on_height_change);
    hooks.use_effect(
        move || {
            (*on_height_change
                .lock()
                .expect("SteerQueue on_height_change poisoned"))(height);
        },
        height,
    );
    let queue = QueueView {
        items: props.items.clone(),
        max_rows,
        state: current,
        symbols: symbols(&caps.read()),
        semantic: theme.read().semantic,
        separator_style: Style::default().fg(theme.read().component.input.border),
        title: i18n::tr_args(
            "steer-queue-title",
            &[("count".to_owned(), FluentValue::from(props.items.len()))],
        ),
        frame: Arc::clone(&rendered.read()),
    };
    element!(View(height: Constraint::Length(height), width: Constraint::Fill(1)) {
        widget(queue)
    })
}

fn activate(
    selection: &Selection,
    frame: &QueueFrame,
    items: &[SteerQueueItem],
    state: State<ViewState>,
    handler: &Arc<Mutex<Handler<'static, SteerQueueAction>>>,
) {
    let Some(control) = frame
        .controls
        .iter()
        .find(|control| control.selection == *selection && control.enabled)
    else {
        return;
    };
    if control.selection == Selection::Expand {
        let mut current = state.write();
        current.expanded = !current.expanded;
        current.offset = 0;
        return;
    }
    if let Some(action) = view::validated_action(selection, &frame.dispatch_ids, items) {
        state.write().selection = None;
        (*handler.lock().expect("SteerQueue on_action poisoned"))(action);
    }
}

#[cfg(test)]
#[path = "steer_queue/view_test.rs"]
mod tests;
