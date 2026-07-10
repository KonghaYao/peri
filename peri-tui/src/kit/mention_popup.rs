//! MentionPopup：@ 提及补全弹窗。
//!
//! MentionPopup：输入区本地 owner，自己处理方向键/确认/取消，
//! 通过回调把选择结果反馈给 InputArea。

use std::sync::{Arc, Mutex};

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use ratatui_kit::{
    crossterm::event::{Event, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
        widgets::{Block, Borders, Paragraph},
    },
};

use crate::kit::atoms::MENTION_SELECTED_INDEX;
use crate::kit::inline_nav::{
    InlineNavAction, clamp_selection, classify_inline_nav, next_selection, previous_selection,
};
use peri_theme::atoms::THEME_ATOM;

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

    let filtered: Vec<String> = if props.prefix.is_empty() {
        props.items.iter().take(20).cloned().collect()
    } else {
        let matcher = SkimMatcherV2::default();
        let query = props.prefix.to_lowercase();
        let mut scored: Vec<(i64, &String)> = props
            .items
            .iter()
            .filter_map(|item| {
                let score = matcher.fuzzy_match(item, &query)?;
                Some((score, item))
            })
            .collect();
        // 按分数降序排列
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
        scored
            .into_iter()
            .take(20)
            .map(|(_, item)| item.clone())
            .collect()
    };

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
                    let mut on_select = on_select.lock().expect("MentionPopup on_select poisoned");
                    (*on_select)(item);
                } else {
                    let mut on_cancel = on_cancel.lock().expect("MentionPopup on_cancel poisoned");
                    (*on_cancel)(());
                }
                EventResult::Consumed
            }
            Some(InlineNavAction::Cancel) => {
                let mut on_cancel = on_cancel.lock().expect("MentionPopup on_cancel poisoned");
                (*on_cancel)(());
                EventResult::Consumed
            }
            None => EventResult::Ignored,
        }
    });

    let state = THEME_ATOM.state();
    let guard = state.read();
    let popup_tokens = &guard.component.popup;
    let semantic = guard.semantic;
    let sel_idx = clamp_selection(*selection.read(), item_count);
    let display_lines: Vec<Line<'_>> = filtered
        .iter()
        .enumerate()
        .map(|(i, item)| {
            if i == sel_idx {
                Line::from(format!("> {}", item))
                    .fg(popup_tokens.selected_fg)
                    .bold()
            } else {
                Line::from(format!("  {}", item)).fg(semantic.text.primary)
            }
        })
        .collect();

    let empty = display_lines.is_empty();
    let popup_block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::new().fg(popup_tokens.border))
        .title_top(
            Line::from(format!(" @{} ", props.prefix))
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
            height: Constraint::Length(THEME_ATOM.state().read().component.popup.inline_height),
        ) {
            Text(text: text_render)
        }
    )
}
