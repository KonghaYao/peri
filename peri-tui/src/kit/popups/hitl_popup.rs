//! ratatui-kit HitlPopup component.
//!
//! HITL (Human-in-the-Loop) 审批弹窗：显示待审批的工具调用详情。
//!
//! Phase 7：完整 UI + 键盘导航。Phase 8 接入 on_approve/on_reject Handler。

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

use crate::kit::theme;

/// 工具调用审批 mock 数据
struct ToolApproval {
    tool_name: &'static str,
    params: Vec<(&'static str, &'static str)>,
}

fn mock_approval() -> ToolApproval {
    ToolApproval {
        tool_name: "FileWrite",
        params: vec![
            ("path", "src/services/user_service.rs"),
            ("mode", "create"),
            ("content", "pub struct UserService { ... }"),
        ],
    }
}

#[component]
pub fn HitlPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let approval = mock_approval();

    // Phase 8: Enter → on_approve, Esc → on_reject
    hooks.use_local_events({
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match (key.modifiers, key.code) {
                    (KeyModifiers::NONE, KeyCode::Enter) => {
                        // Phase 8: on_approve.call(())
                    }
                    (KeyModifiers::NONE, KeyCode::Esc) => {
                        // Phase 8: on_reject.call(())
                    }
                    _ => {}
                }
            }
        }
    });

    // 构建渲染行
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(""));
    // 工具名行
    lines.push(
        Line::from(format!("  Tool: {}", approval.tool_name))
            .fg(theme::SAGE)
            .bold(),
    );
    lines.push(Line::from(""));

    // 参数行
    for (key, val) in &approval.params {
        lines.push(Line::from(format!("    {} = {}", key, val)).fg(theme::TEXT));
    }
    lines.push(Line::from(""));

    // 底部操作提示
    lines.push(Line::from(""));
    lines.push(Line::from("  Enter: approve  |  Esc: reject").fg(theme::DIM));

    let text_render = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Approval Required ").fg(theme::WARNING).bold().centered(),
            width: Constraint::Length(54),
            height: Constraint::Length(14),
        ) {
            Text(text: text_render)
        }
    )
}
