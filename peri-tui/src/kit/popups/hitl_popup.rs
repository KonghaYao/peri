//! ratatui-kit HitlPopup component.
//!
//! HITL (Human-in-the-Loop) 审批弹窗：从 `HITL_PENDING` atom 读取真实工具调用
//! 信息（tool_name + tool_input + batch），渲染待审批的具体内容。
//!
//! I21-A：替换原 mock_approval() 写死 "FileWrite" 假数据——现在 popup 展示
//! agent 实际触发的工具调用，用户能据此判断该不该授权。
//!
//! ## 用户路径
//!
//! - **Enter**：approve（关闭 popup——目前 ACP server 通过批次重新发起来吸收
//!   审批；未来可通过新增 HITL_RESPONSE_TX channel 接入审批 RPC）
//! - **Esc**：reject（同上，由全局 Esc 链 + close_popup 处理）

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

use crate::kit::atoms::HITL_PENDING;
use crate::kit::popup_overlay::close_popup;
use crate::kit::theme;

#[component]
pub fn HitlPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let pending_store = hooks.use_store(*HITL_PENDING.get().unwrap());
    let pending = pending_store.read().clone();
    let _ = pending_store;

    hooks.use_local_events(move |event: Event| {
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
            && (key.modifiers, key.code) == (KeyModifiers::NONE, KeyCode::Enter)
        {
            // Enter：approve——关闭 popup（HITL_PENDING 由 close_popup 清空）
            close_popup();
        }
    });

    let mut lines: Vec<Line<'_>> = Vec::new();

    match &pending {
        None => {
            // 理论上不会渲染此分支——POPUP_KIND=Hitl 暗示 HITL_PENDING 已写入
            lines.push(Line::from(""));
            lines.push(
                Line::from("  No pending approval request.")
                    .fg(theme::MUTED)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from("  Esc: close").fg(theme::DIM));
        }
        Some(hp) => {
            lines.push(Line::from(""));
            // 工具名行
            lines.push(
                Line::from(format!("  Tool: {}", hp.tool_name))
                    .fg(theme::SAGE)
                    .bold(),
            );
            lines.push(Line::from(""));

            // tool_input 字段渲染——序列化为 pretty JSON 后按行展示
            // 限制前 8 个字段避免超长 input 撑爆 popup 高度
            let input_str = serde_json::to_string_pretty(&hp.tool_input)
                .unwrap_or_else(|_| "<non-serializable>".to_string());
            let char_count = input_str.chars().count();
            let max_chars = 400;
            let truncated_str: String = input_str.chars().take(max_chars).collect();
            let display_str = if char_count > max_chars {
                format!("{}...", truncated_str)
            } else {
                truncated_str
            };

            for line in display_str.lines().take(8) {
                lines.push(Line::from(format!("    {}", line)).fg(theme::TEXT));
            }
            if display_str.lines().count() > 8 || char_count > max_chars {
                lines.push(
                    Line::from(format!("    ... ({} chars total)", char_count)).fg(theme::DIM),
                );
            }

            // 批次附加工具（如有）
            if let Some(batch) = &hp.batch
                && !batch.is_empty()
            {
                lines.push(Line::from(""));
                lines.push(
                    Line::from(format!("  Batch ({} more):", batch.len())).fg(theme::WARNING),
                );
                for tp in batch.iter().take(4) {
                    lines.push(
                        Line::from(format!("    - {} ({})", tp.tool_name, tp.tool_id))
                            .fg(theme::MUTED),
                    );
                }
                if batch.len() > 4 {
                    lines.push(
                        Line::from(format!("    ... and {} more", batch.len() - 4)).fg(theme::DIM),
                    );
                }
            }

            lines.push(Line::from(""));
            lines.push(Line::from("  Enter: approve  |  Esc: reject").fg(theme::DIM));
        }
    }

    let text_render = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Approval Required ").fg(theme::WARNING).bold().centered(),
            width: Constraint::Length(60),
            height: Constraint::Length(16),
        ) {
            Text(text: text_render)
        }
    )
}
