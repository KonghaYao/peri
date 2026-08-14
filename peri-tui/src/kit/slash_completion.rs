//! SlashCompletion：斜杠命令补全弹窗。
//!
//! SlashCompletion：输入区本地 owner，自己处理方向键/确认/取消，
//! 通过回调把选择结果反馈给 InputArea。

use std::sync::{Arc, Mutex};

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use ratatui_kit::{
    crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Modifier, Style, Stylize},
        text::{Line, Span},
        widgets::{Block, Borders, Paragraph},
    },
};

use crate::i18n;
use crate::kit::atoms::SLASH_SELECTED_INDEX;
use crate::kit::inline_nav::{
    InlineNavAction, clamp_selection, classify_inline_nav, next_selection, previous_selection,
};
use crate::kit::panel_mouse::{AreaTracker, ListLayout, hit_item};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashActionKind {
    Panel,
    Command,
    Skill,
    McpSkill,
}

/// Slash 条目按 kind 的静态语义色（S16：三层 slash 用颜色区分，不用方括号标签）。
/// McpSkill 用 `semantic.model_info`（spec 允许「主题中专门 token」，
/// `semantic.status.info` 不存在——不加 peri-theme 新字段，零范围蔓延）。
pub(crate) fn slash_kind_color(
    kind: &SlashActionKind,
    semantic: &peri_theme::semantic::SemanticTokens,
) -> ratatui::style::Color {
    match kind {
        SlashActionKind::Panel => semantic.border.active,
        SlashActionKind::Command => semantic.text.muted,
        SlashActionKind::Skill => semantic.status.warning,
        SlashActionKind::McpSkill => semantic.model_info,
    }
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

    let filtered: Vec<SlashCompletionItem> = if props.prefix.is_empty() {
        props.items.clone()
    } else {
        let matcher = SkimMatcherV2::default();
        let query = props.prefix.to_lowercase();
        let mut scored: Vec<(i64, SlashCompletionItem)> = props
            .items
            .iter()
            .filter_map(|item| {
                let score = matcher.fuzzy_match(&item.label_lowercase, &query)?;
                Some((score, item.clone()))
            })
            .collect();
        // 按模糊匹配分数降序排列
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
        scored.into_iter().map(|(_, item)| item).collect()
    };

    let item_count = filtered.len();
    let filtered_for_handler = filtered.clone();
    let on_select = Arc::clone(&props.on_select);
    let on_cancel = Arc::clone(&props.on_cancel);

    // 弹窗绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 可见窗口（渲染时 skip(scroll_start).take(visible_rows)）——选中项保持在
    // 可视区域上 1/3 处；鼠标命中反推需要与渲染使用同一 scroll_start。
    let sel_idx = clamp_selection(*selection.read(), item_count);
    let popup_h = THEME_ATOM.state().read().component.popup.inline_height;
    let visible_rows = popup_h.saturating_sub(2) as usize; // 减去上下边框
    let scroll_start = if item_count <= visible_rows {
        0
    } else {
        let max_scroll = item_count.saturating_sub(visible_rows);
        sel_idx.saturating_sub(visible_rows / 3).min(max_scroll)
    };

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| {
            // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
            if let Event::Mouse(mouse) = event {
                if let Some(area) = area
                    && let Some(idx) = hit_item(
                        &mouse,
                        area,
                        ListLayout {
                            header_rows: 0,
                            item_rows: 1,
                            footer_rows: 0,
                            visible_items: visible_rows as u16,
                            scroll_start,
                            item_count,
                        },
                    )
                {
                    *selection.write() = idx;
                    if let Some(item) = filtered_for_handler.get(idx).cloned() {
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
                    return EventResult::Consumed;
                }
                // 区域内的左键点击（未命中行）也消费，防止穿透到输入区
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
        },
    );

    let state = THEME_ATOM.state();
    let guard = state.read();
    let popup_tokens = &guard.component.popup;
    let semantic = guard.semantic;

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
            let tier_color = slash_kind_color(&item.kind, &semantic);

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
            Line::from(i18n::tr_args(
                "slash-completion-title",
                &[("name".to_string(), FluentValue::from(props.prefix.as_str()))],
            ))
            .fg(popup_tokens.action_primary)
            .bold(),
        );

    // 可见窗口：只渲染可见区域内的项，避免选中项滚出视野
    // （scroll_start/visible_rows 与鼠标命中测试共用，见 handler 注册处）
    let visible_lines: Vec<Line<'_>> = display_lines
        .into_iter()
        .skip(scroll_start)
        .take(visible_rows)
        .collect();

    let text_render = if empty {
        Paragraph::new(Line::from(i18n::tr("common-no-matches")).fg(semantic.text.muted))
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

#[cfg(test)]
mod tests {
    use super::*;
    use peri_theme::prelude::dark_theme;

    fn semantic() -> peri_theme::semantic::SemanticTokens {
        dark_theme().semantic
    }

    #[test]
    fn test_slash_kind_color_mcp_skill_uses_model_info() {
        let s = semantic();
        assert_eq!(
            slash_kind_color(&SlashActionKind::McpSkill, &s),
            s.model_info,
            "McpSkill 应映射 semantic.model_info"
        );
    }

    /// 回归：既有 Panel/Command/Skill 映射不变（渲染行为除新增色外不变）。
    #[test]
    fn test_slash_kind_color_existing_kinds_unchanged() {
        let s = semantic();
        assert_eq!(
            slash_kind_color(&SlashActionKind::Panel, &s),
            s.border.active
        );
        assert_eq!(
            slash_kind_color(&SlashActionKind::Command, &s),
            s.text.muted
        );
        assert_eq!(
            slash_kind_color(&SlashActionKind::Skill, &s),
            s.status.warning
        );
    }
}
