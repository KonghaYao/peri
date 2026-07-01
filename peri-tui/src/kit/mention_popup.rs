//! @mention 文件提醒弹出组件。
//!
//! I18-C：Up/Down 选中索引同步写入 `MENTION_SELECTED_INDEX` atom，
//! InputArea 在 Enter 时读取真实选中文件名（而非仅 prefix）。

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
    pub on_select: Handler<'static, String>,
    pub on_cancel: Handler<'static, ()>,
}

#[component]
pub fn MentionPopup(props: &MentionPopupProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 当前选中项索引——用 atom 共享给 InputArea
    let selection = hooks.use_store(*MENTION_SELECTED_INDEX.get().unwrap());

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
    hooks.use_local_events(move |event: Event| {
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                return;
            }
            match key.code {
                KeyCode::Up => {
                    let mut s = selection.write();
                    *s = s.saturating_sub(1);
                }
                KeyCode::Down => {
                    let mut s = selection.write();
                    if item_count > 0 {
                        *s = (s.saturating_add(1)).min(item_count - 1);
                    }
                }
                _ => {}
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
