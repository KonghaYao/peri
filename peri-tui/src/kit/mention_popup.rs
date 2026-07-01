//! @mention 文件提醒弹出组件。
//!
//! Phase 5：完整交互——通过 `use_local_events` 处理 Esc/Enter/Up/Down，
//! 按 prefix 过滤 items，渲染为 Border 面板。

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

use crate::ui::theme;

#[derive(Default, Props)]
pub struct MentionPopupProps {
    pub prefix: String,
    pub items: Vec<String>,
    pub on_select: Handler<'static, String>,
    pub on_cancel: Handler<'static, ()>,
}

#[component]
pub fn MentionPopup(props: &MentionPopupProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 当前选中项索引
    let selection = hooks.use_state(|| 0usize);

    // 按 prefix 过滤
    let filtered: Vec<String> = props
        .items
        .iter()
        .filter(|item| {
            props.prefix.is_empty() || item.to_lowercase().contains(&props.prefix.to_lowercase())
        })
        .cloned()
        .collect();

    let item_count = filtered.len();

    // 键盘事件处理
    hooks.use_local_events({
        let sel = selection;
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Up => {
                        let mut s = sel.write();
                        *s = s.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        let mut s = sel.write();
                        if item_count > 0 {
                            *s = (s.saturating_add(1)).min(item_count - 1);
                        }
                    }
                    // Phase 5 注：Enter/Esc 回调由外部 state_machine 全局处理，
                    // 此处仅管理选中索引。Phase 8 接入 on_select/on_cancel Handler。
                    _ => {}
                }
            }
        }
    });

    // 构建渲染文本
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
            height: Constraint::Length((filtered.len().max(1) + 2).min(10) as u16),
        ) {
            Text(text: text_render)
        }
    )
}
