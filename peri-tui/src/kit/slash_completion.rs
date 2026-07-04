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
    Skill,
}

#[derive(Debug, Clone)]
pub struct SlashCompletionItem {
    pub label: String,
    pub insert_text: String,
    pub description: String,
    pub kind: SlashActionKind,
    /// label 的小写版本，预计算避免每帧 to_lowercase() 分配。
    pub label_lowercase: String,
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

    // 预计算一次 prefix_lower，避免过滤循环中反复分配。
    let prefix_lower = props.prefix.to_lowercase();
    let filtered: Vec<SlashCompletionItem> = props
        .items
        .iter()
        .filter(|item| props.prefix.is_empty() || item.label_lowercase.starts_with(&prefix_lower))
        .cloned()
        .collect();

    // items 已在 build_slash_items() 端字母序排序，此处不再重排

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

    // 双列布局：计算 label 列最大宽度（含 / 前缀），描述列自然对齐
    let max_label_width = filtered
        .iter()
        .map(|item| item.label.chars().count() + 1) // +1 for '/'
        .max()
        .unwrap_or(0);
    let display_lines: Vec<Line<'_>> = filtered
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let selected = i == sel_idx;
            let marker = if selected { "> " } else { "  " };

            // S16：三层 slash 用颜色区分，不用方括号标签
            let tier_color = match item.kind {
                SlashActionKind::Panel => semantic.border.active,
                SlashActionKind::Command => semantic.text.muted,
                SlashActionKind::Skill => semantic.status.warning,
            };

            let line_style = if selected {
                Style::default()
                    .fg(popup_tokens.selected_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(tier_color)
            };

            let detail_style = if selected {
                Style::default().fg(popup_tokens.selected_fg)
            } else {
                Style::default().fg(semantic.text.dim)
            };

            // 双列：label 左对齐补足到 max_label_width，描述从固定列开始
            let padded_label = format!("/{:<width$}", item.label, width = max_label_width);

            Line::from(vec![
                Span::styled(marker, line_style),
                Span::styled(padded_label, line_style),
                Span::styled(format!("  {}", item.description), detail_style),
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

    // 计算可见窗口：只渲染可见区域内的项，避免选中项滚出视野
    let popup_h = theme::component().popup.inline_height;
    let visible_rows = popup_h.saturating_sub(2) as usize; // 减去上下边框
    let scroll_start = if item_count <= visible_rows {
        0
    } else {
        let max_scroll = item_count.saturating_sub(visible_rows);
        // 选中项保持在可视区域上 1/3 处，避免靠近边缘
        sel_idx.saturating_sub(visible_rows / 3).min(max_scroll)
    };
    let visible_lines: Vec<Line<'_>> = display_lines
        .into_iter()
        .skip(scroll_start)
        .take(visible_rows)
        .collect();

    let text_render = if empty {
        Paragraph::new(Line::from("  (no matches)").fg(semantic.text.muted))
    } else {
        Paragraph::new(ratatui::text::Text::from(visible_lines))
    }
    .block(popup_block);

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(popup_h),
        ) {
            Text(text: text_render)
        }
    )
}
