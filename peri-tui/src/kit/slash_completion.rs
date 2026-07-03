//! SlashCompletion：斜杠命令补全弹窗。
//!
//! SlashCompletion：输入区本地 owner，自己处理方向键/确认/取消，
//! 通过回调把选择结果反馈给 InputArea。

use std::sync::{Arc, Mutex};

use ratatui_kit::{
    crossterm::event::{Event, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Modifier, Style, Stylize},
        text::{Line, Span},
        widgets::{Block, Borders, Paragraph},
    },
};

use crate::kit::atoms::SLASH_SELECTED_INDEX;
use crate::kit::inline_nav::{
    InlineNavAction, clamp_selection, classify_inline_nav, next_selection, previous_selection,
};
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

    // 不在此处写 SLASH_SELECTED_INDEX——事件处理器 (next_selection/previous_selection)
    // 已通过 saturating_sub/min 保持边界安全。render body 写 atom 会触发级联重渲染，
    // 在 slash_active 从 true→false 过渡时可能导致无限渲染循环和 CPU 100%。

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        match classify_inline_nav(&key) {
            Some(InlineNavAction::MoveUp) => {
                let mut s = selection.write();
                *s = previous_selection(*s);
                EventResult::Consumed
            }
            Some(InlineNavAction::MoveDown) => {
                let mut s = selection.write();
                *s = next_selection(*s, item_count);
                EventResult::Consumed
            }
            Some(InlineNavAction::Confirm) => {
                let selected = {
                    let sel_idx = clamp_selection(*selection.read(), item_count);
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
            Some(InlineNavAction::Cancel) => {
                let mut on_cancel = on_cancel
                    .lock()
                    .expect("SlashCompletion on_cancel poisoned");
                (*on_cancel)(());
                EventResult::Consumed
            }
            None => EventResult::Ignored,
        }
    });

    let popup_tokens = &theme::component().popup;
    let semantic = theme::semantic();
    let sel_idx = clamp_selection(*selection.read(), item_count);
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
                    .fg(popup_tokens.selected_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(semantic.text.primary)
            };
            let detail_style = if selected {
                Style::default().fg(popup_tokens.selected_fg)
            } else {
                Style::default().fg(semantic.text.muted)
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
    let popup_block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::new().fg(popup_tokens.border))
        .title_top(
            Line::from(format!(" /{} ", props.prefix))
                .fg(popup_tokens.action_primary)
                .bold(),
        );

    let text_render = if empty {
        Paragraph::new(Line::from("  (no matches)").fg(semantic.text.muted))
    } else {
        Paragraph::new(ratatui::text::Text::from(display_lines))
    }
    .block(popup_block);

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(theme::component().popup.inline_height),
        ) {
            Text(text: text_render)
        }
    )
}
