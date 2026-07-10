use ratatui::text::Line;

use crate::{theme::Theme, tool_call::ToolCallState};

#[derive(Debug, Clone)]
pub enum BlockRenderStrategy {
    Text {
        content: String,
        streaming: bool,
    },
    ToolCall(ToolCallState),
    SubAgent {
        agent_id: String,
        task_preview: String,
        total_steps: usize,
        collapsed: bool,
        result: Option<String>,
    },
    Thinking {
        char_count: usize,
        expanded: bool,
    },
    SystemNote {
        content: String,
    },
}

pub fn render_block(
    strategy: &BlockRenderStrategy,
    width: usize,
    theme: &dyn Theme,
) -> Vec<Line<'static>> {
    match strategy {
        BlockRenderStrategy::Text { content, .. } => {
            #[cfg(feature = "markdown")]
            {
                use super::highlight::is_diff_content;
                use crate::markdown::ThemeMarkdownAdapter;

                if is_diff_content(content) {
                    let mut lines: Vec<Line<'static>> = Vec::new();
                    for line in content.lines() {
                        lines.push(Line::from(super::highlight::highlight_diff_line(
                            line, theme,
                        )));
                    }
                    if lines.is_empty() {
                        lines.push(Line::raw(content.clone()));
                    }
                    lines
                } else {
                    let md_theme = ThemeMarkdownAdapter(theme);
                    let text = crate::markdown::parse_markdown(content, &md_theme, width);
                    text.lines.into_iter().collect()
                }
            }

            #[cfg(not(feature = "markdown"))]
            {
                let _ = width;
                content.lines().map(|l| Line::raw(l.to_string())).collect()
            }
        }
        BlockRenderStrategy::ToolCall(state) => {
            let _ = width;
            // Use ToolCallWidget to render into lines
            let indicator =
                crate::tool_call::display::format_indicator(state.status.clone(), state.tick);
            let arrow = if state.collapsed { "▸" } else { "▾" };
            let mut header_spans: Vec<ratatui::text::Span<'_>> = vec![
                ratatui::text::Span::styled(
                    format!("{} ", indicator),
                    ratatui::style::Style::default().fg(state.color),
                ),
                ratatui::text::Span::styled(
                    format!("{} ", arrow),
                    ratatui::style::Style::default().fg(state.color),
                ),
                ratatui::text::Span::styled(
                    state.tool_name.clone(),
                    ratatui::style::Style::default()
                        .fg(state.color)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ];
            if !state.args_summary.is_empty() {
                let summary =
                    crate::tool_call::display::format_args_summary(&state.args_summary, 400);
                header_spans.push(ratatui::text::Span::styled(
                    format!("({})", summary),
                    ratatui::style::Style::default().fg(theme.dim()),
                ));
            }
            let mut lines: Vec<Line<'_>> = vec![Line::from(header_spans)];
            if !state.collapsed && !state.result_lines.is_empty() {
                for line in &state.result_lines {
                    lines.push(Line::from(vec![
                        ratatui::text::Span::styled(
                            "  │ ".to_string(),
                            ratatui::style::Style::default().fg(theme.dim()),
                        ),
                        ratatui::text::Span::raw(line.clone()),
                    ]));
                }
                if let Some(omitted) = state.omitted_lines {
                    lines.push(Line::from(vec![ratatui::text::Span::styled(
                        format!("  … ({} more lines)", omitted),
                        ratatui::style::Style::default().fg(theme.dim()),
                    )]));
                }
            }
            lines
        }
        BlockRenderStrategy::SubAgent {
            agent_id,
            collapsed,
            result,
            ..
        } => {
            let arrow = if *collapsed { "▸" } else { "▾" };
            let mut lines: Vec<Line<'_>> = vec![Line::from(vec![
                ratatui::text::Span::styled(
                    format!("{} ", arrow),
                    ratatui::style::Style::default().fg(theme.success()),
                ),
                ratatui::text::Span::raw(agent_id.to_string()),
            ])];
            if !collapsed {
                if let Some(res) = result {
                    for line in res.lines() {
                        lines.push(Line::from(format!("  │ {}", line)));
                    }
                }
            }
            lines
        }
        BlockRenderStrategy::Thinking {
            char_count,
            expanded,
        } => {
            let _ = width;
            let mut lines: Vec<Line<'_>> =
                vec![Line::from(format!("💭 思考 ({} chars)", char_count))];
            if *expanded {
                lines.push(Line::raw("(thinking content)"));
            }
            lines
        }
        BlockRenderStrategy::SystemNote { content } => {
            vec![Line::from(format!("[i] {}", content))]
        }
    }
}
