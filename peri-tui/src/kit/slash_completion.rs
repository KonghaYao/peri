//! SlashCompletion：斜杠命令补全弹窗。
//!
//! SlashCompletion：输入区本地 owner，自己处理方向键/确认/取消，
//! 通过回调把选择结果反馈给 InputArea。

use std::sync::{Arc, Mutex};

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Modifier, Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::kit::atoms::SLASH_SELECTED_INDEX;
use crate::kit::theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashActionKind {
    Panel,
    Command,
}

#[derive(Debug, Clone)]
pub struct SlashCompletionItem {
    pub label: String,
    pub insert_text: String,
    pub description: String,
    pub kind: SlashActionKind,
}

#[derive(Default, Props)]
pub struct SlashCompletionProps {
    pub prefix: String,
    pub items: Vec<SlashCompletionItem>,
    pub on_select: Arc<Mutex<Handler<'static, SlashCompletionItem>>>,
    pub on_cancel: Arc<Mutex<Handler<'static, ()>>>,
}

#[component]
pub fn SlashCompletion(
    props: &SlashCompletionProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let selection = hooks.use_atom(&SLASH_SELECTED_INDEX);

    let filtered: Vec<SlashCompletionItem> = props
        .items
        .iter()
        .filter(|item| {
            props.prefix.is_empty()
                || item
                    .label
                    .to_lowercase()
                    .starts_with(&props.prefix.to_lowercase())
        })
        .cloned()
        .collect();

    let item_count = filtered.len();
    let filtered_for_handler = filtered.clone();
    let on_select = Arc::clone(&props.on_select);
    let on_cancel = Arc::clone(&props.on_cancel);

    if item_count == 0 {
        let mut sel = selection.write();
        if *sel != 0 {
            *sel = 0;
        }
    } else {
        let mut sel = selection.write();
        if *sel >= item_count {
            *sel = item_count - 1;
        }
    }

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
                    let mut on_select = on_select
                        .lock()
                        .expect("SlashCompletion on_select poisoned");
                    (*on_select)(item);
                } else {
                    let mut on_cancel = on_cancel
                        .lock()
                        .expect("SlashCompletion on_cancel poisoned");
                    (*on_cancel)(());
                }
                EventResult::Consumed
            }
            KeyCode::Esc => {
                let mut on_cancel = on_cancel
                    .lock()
                    .expect("SlashCompletion on_cancel poisoned");
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
            let selected = i == sel_idx;
            let marker = if selected { "> " } else { "  " };
            let kind_label = match item.kind {
                SlashActionKind::Panel => "[panel]",
                SlashActionKind::Command => "[cmd]",
            };
            let line_style = if selected {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };
            let detail_style = if selected {
                Style::default().fg(theme::THINKING)
            } else {
                Style::default().fg(theme::MUTED)
            };

            Line::from(vec![
                Span::styled(marker, line_style),
                Span::styled(format!("/{}", item.label), line_style),
                Span::styled(format!(" {}", kind_label), detail_style),
                Span::styled(format!(" — {}", item.description), detail_style),
            ])
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
            top_title: Line::from(format!(" /{} ", props.prefix)).fg(theme::THINKING).bold(),
            width: Constraint::Fill(1),
            height: Constraint::Length(10),
        ) {
            Text(text: text_render)
        }
    )
}
