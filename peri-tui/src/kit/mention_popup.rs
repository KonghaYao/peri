//! MentionPopup：@ 提及补全弹窗。
//!
//! MentionPopup：输入区本地 owner，自己处理方向键/确认/取消，
//! 通过回调把选择结果反馈给 InputArea。

use std::sync::{Arc, Mutex};

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
        widgets::Paragraph,
    },
};

use crate::kit::atoms::MENTION_SELECTED_INDEX;
use crate::kit::theme;

#[derive(Default, Props)]
pub struct MentionPopupProps {
    pub prefix: String,
    pub items: Vec<String>,
    pub on_select: Arc<Mutex<Handler<'static, String>>>,
    pub on_cancel: Arc<Mutex<Handler<'static, ()>>>,
}

#[component]
pub fn MentionPopup(props: &MentionPopupProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selection = hooks.use_atom(&MENTION_SELECTED_INDEX);

    let filtered: Vec<String> = props
        .items
        .iter()
        .filter(|item| {
            props.prefix.is_empty() || item.to_lowercase().contains(&props.prefix.to_lowercase())
        })
        .cloned()
        .collect();

    let item_count = filtered.len();
    let filtered_for_handler = filtered.clone();
    let on_select = Arc::clone(&props.on_select);
    let on_cancel = Arc::clone(&props.on_cancel);

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        match key.code {
            KeyCode::Up => {
                let mut s = selection.write();
                *s = s.saturating_sub(1);
                EventResult::Consumed
            }
            KeyCode::Down => {
                let mut s = selection.write();
                if item_count > 0 {
                    *s = (s.saturating_add(1)).min(item_count - 1);
                }
                EventResult::Consumed
            }
            KeyCode::Enter => {
                let selected = {
                    let sel_idx = *selection.read();
                    filtered_for_handler.get(sel_idx).cloned()
                };
                if let Some(item) = selected {
                    let mut on_select = on_select.lock().expect("MentionPopup on_select poisoned");
                    (*on_select)(item);
                } else {
                    let mut on_cancel = on_cancel.lock().expect("MentionPopup on_cancel poisoned");
                    (*on_cancel)(());
                }
                EventResult::Consumed
            }
            KeyCode::Esc => {
                let mut on_cancel = on_cancel.lock().expect("MentionPopup on_cancel poisoned");
                (*on_cancel)(());
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    let sel_idx = *selection.read();
    let display_lines: Vec<Line<'_>> = filtered
        .iter()
        .enumerate()
        .map(|(i, item)| {
            if i == sel_idx {
                Line::from(format!("> {}", item)).fg(theme::THINKING).bold()
            } else {
                Line::from(format!("  {}", item)).fg(theme::TEXT)
            }
        })
        .collect();

    let empty = display_lines.is_empty();
    let text_render = if empty {
        Paragraph::new(Line::from("  (no matches)").fg(theme::MUTED))
    } else {
        Paragraph::new(ratatui::text::Text::from(display_lines))
    };

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::THINKING),
            top_title: Line::from(format!(" @{} ", props.prefix)).fg(theme::THINKING).bold(),
            width: Constraint::Length(50),
            height: Constraint::Length(10),
        ) {
            Text(text: text_render)
        }
    )
}
