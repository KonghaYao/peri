use peri_widgets::BorderedPanel;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

use crate::{app::App, ui::theme};

/// HITL 批量确认弹窗（底部展开区）
pub(crate) fn render_hitl_popup(f: &mut Frame, app: &App, area: Rect) {
    let Some(crate::app::InteractionPrompt::Approval(prompt)) =
        &app.session_mgr.current().agent.interaction_prompt
    else {
        return;
    };

    let lc = &app.services.lc;
    let item_count = prompt.items.len();
    let popup_area = area;

    let title = if item_count == 1 {
        lc.tr("hitl-single-title")
    } else {
        lc.tr("hitl-batch-title")
    };

    let inner = BorderedPanel::new(Span::styled(
        title,
        Style::default()
            .fg(theme::THINKING)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::WARNING))
    .render(f, popup_area);
    let max_width = inner.width as usize;

    // 渲染每个工具调用项
    let mut lines: Vec<Line> = Vec::new();

    for (i, (item, &approved)) in prompt.items.iter().zip(prompt.approved.iter()).enumerate() {
        let is_cursor = i == prompt.cursor;

        // 状态图标和颜色
        let (status_icon, status_color) = if approved {
            ("✓", theme::SAGE)
        } else {
            ("✗", theme::ERROR)
        };

        // 光标高亮
        let cursor_indicator = if is_cursor { "❯ " } else { "  " };
        let _row_style = Style::default();

        // 工具名行
        lines.push(Line::styled(
            format!(
                "{}{} {}  {}",
                cursor_indicator,
                status_icon,
                item.tool_name,
                if approved {
                    lc.tr("hitl-approved")
                } else {
                    lc.tr("hitl-rejected")
                }
            ),
            if is_cursor {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(status_color)
            },
        ));

        // 参数预览行
        let input_preview = format_input_preview(&item.input, max_width.saturating_sub(6));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(input_preview, Style::default().fg(theme::MUTED)),
        ]));
    }

    // 底部：多项时显示统计摘要（快捷键由状态栏统一负责）
    if item_count > 1 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            lc.tr_args(
                "hitl-summary",
                &[
                    (
                        "approved".into(),
                        (prompt.approved.iter().filter(|&&v| v).count() as i64).into(),
                    ),
                    (
                        "rejected".into(),
                        (prompt.approved.iter().filter(|&&v| !v).count() as i64).into(),
                    ),
                ],
            ),
            Style::default().fg(theme::MUTED),
        )));
    }

    let para = Paragraph::new(Text::from(lines));
    f.render_widget(para, inner);
}

fn format_input_preview(input: &serde_json::Value, max_len: usize) -> String {
    let s = match input {
        serde_json::Value::Object(map) => {
            let key = ["command", "file_path", "pattern", "path"]
                .iter()
                .find(|k| map.contains_key(**k))
                .copied()
                .or_else(|| map.keys().next().map(|k| k.as_str()));

            if let Some(k) = key {
                if let Some(v) = map.get(k) {
                    let val = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    format!("{k}={val}")
                } else {
                    input.to_string()
                }
            } else {
                "{}".to_string()
            }
        }
        other => other.to_string(),
    };

    if s.chars().count() > max_len && max_len > 1 {
        format!("{}…", s.chars().take(max_len - 1).collect::<String>())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use peri_middlewares::hitl::BatchItem; // P4b: runtime dependency, input: Value field

    use crate::app::{App, HitlBatchPrompt, InteractionPrompt};
    include!("hitl_test.rs");
}
