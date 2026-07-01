//! ratatui-kit RewindPopup component.
//!
//! 回退变更弹窗：显示文件变更列表，支持 Up/Down 选择、Tab 切换、Enter 确认、Esc 取消。
//!
//! Phase 7：完整 UI + 键盘导航。Phase 8 接入 on_confirm/on_cancel Handler。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
        widgets::Paragraph,
    },
};

use crate::ui::theme;

/// 文件变更项
struct FileChange {
    path: &'static str,
    status: &'static str,
    lines_added: usize,
    lines_removed: usize,
}

fn mock_changes() -> Vec<FileChange> {
    vec![
        FileChange {
            path: "src/services/user_service.rs",
            status: "modified",
            lines_added: 23,
            lines_removed: 5,
        },
        FileChange {
            path: "tests/user_service_test.rs",
            status: "added",
            lines_added: 45,
            lines_removed: 0,
        },
        FileChange {
            path: "src/models/user.rs",
            status: "modified",
            lines_added: 12,
            lines_removed: 3,
        },
    ]
}

#[component]
pub fn RewindPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let changes = mock_changes();
    let change_count = changes.len();

    // 当前选中的文件索引
    let selection = hooks.use_state(|| 0usize);

    hooks.use_local_events({
        let sel = selection;
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match (key.modifiers, key.code) {
                    (KeyModifiers::NONE, KeyCode::Up) => {
                        let mut s = sel.write();
                        *s = s.saturating_sub(1);
                    }
                    (KeyModifiers::NONE, KeyCode::Down) => {
                        let mut s = sel.write();
                        if change_count > 0 {
                            *s = (s.saturating_add(1)).min(change_count - 1);
                        }
                    }
                    (KeyModifiers::NONE, KeyCode::Tab) => {
                        let mut s = sel.write();
                        if change_count > 0 {
                            *s = (*s + 1) % change_count;
                        }
                    }
                    (KeyModifiers::SHIFT, KeyCode::BackTab)
                    | (KeyModifiers::NONE, KeyCode::BackTab) => {
                        let mut s = sel.write();
                        *s = s.checked_sub(1).unwrap_or(change_count - 1);
                    }
                    // Phase 8: Enter → on_confirm, Esc → on_cancel
                    _ => {}
                }
            }
        }
    });

    let sel_idx = *selection.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(""));

    // 渲染文件变更列表
    for (i, change) in changes.iter().enumerate() {
        let is_selected = i == sel_idx;

        // 状态标签颜色
        let status_color = match change.status {
            "added" => theme::SAGE,
            "deleted" => theme::ERROR,
            _ => theme::WARNING,
        };

        let prefix = if is_selected { "> " } else { "  " };
        let status_str = format!(
            "{} (+{}/{}-)",
            change.status, change.lines_added, change.lines_removed
        );

        if is_selected {
            lines.push(
                Line::from(format!("{}{}", prefix, change.path))
                    .fg(theme::THINKING)
                    .bold(),
            );
            lines.push(Line::from(format!("  └ {}", status_str)).fg(status_color));
        } else {
            lines.push(Line::from(format!("{}{}", prefix, change.path)).fg(theme::TEXT));
            lines.push(Line::from(format!("  └ {}", status_str)).fg(theme::MUTED));
        }

        lines.push(Line::from(""));
    }

    // 底部操作提示
    lines.push(Line::from(""));
    lines.push(
        Line::from("  Up/Down: select  |  Tab: switch  |  Enter: confirm  |  Esc: cancel")
            .fg(theme::DIM),
    );

    let text_render = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Rewind Changes ").fg(theme::WARNING).bold().centered(),
            width: Constraint::Length(56),
            height: Constraint::Length(14),
        ) {
            Text(text: text_render)
        }
    )
}
