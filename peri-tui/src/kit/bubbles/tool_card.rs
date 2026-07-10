//! 工具调用卡片——显示工具名、参数摘要和输出。
//!
//! 折叠/展开规则：Bash/Read/Glob/Grep/AskUserQuestion 默认折叠，
//! AgentResult/ExecuteExtraTool/SearchExtraTools 自动展开，
//! Write/Edit 运行中折叠、完成后展开，错误不折叠。

use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use peri_theme::atoms::THEME_ATOM;

/// 折叠/展开规则（TUI-PAGE.md §2.4.2）。
const COLLAPSED_BY_DEFAULT: &[&str] = &["Bash", "Read", "Glob", "Grep", "AskUserQuestion"];
const AUTO_EXPAND: &[&str] = &["AgentResult", "ExecuteExtraTool", "SearchExtraTools"];
const FORCE_EXPAND_ON_COMPLETE: &[&str] = &["Write", "Edit"];

fn should_collapse(tool_name: &str, is_running: bool, is_error: bool) -> bool {
    if is_error {
        return false;
    }
    if AUTO_EXPAND.contains(&tool_name) {
        return false;
    }
    if FORCE_EXPAND_ON_COMPLETE.contains(&tool_name) {
        return is_running;
    }
    COLLAPSED_BY_DEFAULT.contains(&tool_name)
}

/// 工具卡片属性。
#[derive(Props, Default)]
pub struct ToolCardProps {
    /// 工具名称（原始名，如 "Bash"）。
    pub tool_name: String,
    /// 工具输入/参数摘要。
    pub input_summary: String,
    /// 工具输出摘要。
    pub output_summary: String,
    /// 是否为错误。
    pub is_error: bool,
    /// 是否仍在运行。
    pub is_running: bool,
    /// 运行时长（毫秒）。
    pub running_duration_ms: Option<u64>,
    /// Agent 工具的子工具调用计数。
    pub tool_calls_count: usize,
}

/// 将原始 tool_name 映射为用户友好的显示名。
fn format_tool_name(raw: &str) -> &str {
    match raw {
        "Bash" => "Shell",
        "folder_operations" => "Folder",
        other => other,
    }
}

fn format_running_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1000;
        format!("{}m{}s", mins, secs)
    }
}

/// 截断输出行（取最后 N 行，每行最多 max_chars 字符）。
fn compact_output_lines(output: &str, max_lines: usize, max_chars: usize) -> Vec<String> {
    let lines: Vec<&str> = output.lines().collect();
    let start = if lines.len() > max_lines {
        lines.len() - max_lines
    } else {
        0
    };
    lines[start..]
        .iter()
        .map(|line| {
            if line.chars().count() > max_chars {
                format!("{}...", line.chars().take(max_chars).collect::<String>())
            } else {
                line.to_string()
            }
        })
        .collect()
}

#[component]
pub fn ToolCard(mut hooks: Hooks, props: &ToolCardProps) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let guard = theme_def.read();
    let semantic = &guard.semantic;

    let collapsed = should_collapse(&props.tool_name, props.is_running, props.is_error);

    let indicator = if props.is_running {
        Span::styled("⏳", Style::default().fg(semantic.status.warning))
    } else if props.is_error {
        Span::styled("✗", Style::default().fg(semantic.status.error))
    } else {
        Span::styled("✓", Style::default().fg(semantic.status.success))
    };

    let display_name = format_tool_name(&props.tool_name).to_string();
    let name = Span::styled(
        display_name.clone(),
        Style::default()
            .fg(semantic.text.primary)
            .add_modifier(Modifier::BOLD),
    );

    let mut header_spans = vec![indicator, Span::raw(" "), name];

    let summary = truncate_summary(&props.input_summary, 400);
    if !summary.is_empty() {
        header_spans.push(Span::styled(
            format!(" ({})", summary),
            Style::default().fg(semantic.text.dim),
        ));
    }

    let mut lines: Vec<Line<'static>> = vec![Line::from(header_spans)];

    // Bash/Agent 运行行
    if props.is_running && !props.is_error {
        let duration = props.running_duration_ms.unwrap_or(0);
        if props.tool_name == "Bash" {
            lines.push(Line::from(vec![
                Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                Span::styled(
                    format!("Running ({})", format_running_duration(duration)),
                    Style::default().fg(semantic.text.muted),
                ),
            ]));
        } else if props.tool_name == "Agent" && props.tool_calls_count > 0 {
            lines.push(Line::from(vec![
                Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                Span::styled(
                    format!(
                        "{} tool calls, running {}",
                        props.tool_calls_count,
                        format_running_duration(duration)
                    ),
                    Style::default().fg(semantic.text.muted),
                ),
            ]));
        }
    }

    if collapsed {
        // 折叠态：显示最后 1 行输出摘要
        if !props.output_summary.is_empty() {
            let color = if props.is_error {
                semantic.status.error
            } else {
                semantic.text.muted
            };
            for out_line in compact_output_lines(&props.output_summary, 1, 400) {
                lines.push(Line::from(vec![
                    Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                    Span::styled(out_line, Style::default().fg(color)),
                ]));
            }
        }
    } else if !props.output_summary.is_empty() {
        // 展开态：显示最多 4 行输出摘要（TodoWrite 不限）
        let result_color = if props.is_error {
            semantic.status.error
        } else {
            semantic.text.muted
        };
        let border_color = if props.is_error {
            semantic.status.error
        } else {
            semantic.text.dim
        };
        let max_lines = if props.tool_name == "TodoWrite" {
            usize::MAX
        } else {
            4
        };
        for out_line in compact_output_lines(&props.output_summary, max_lines, 400) {
            lines.push(Line::from(vec![
                Span::styled("  ⎿ ", Style::default().fg(border_color)),
                Span::styled(out_line, Style::default().fg(result_color)),
            ]));
        }
    }

    // 末尾空行（消息间距）
    lines.push(Line::from(""));

    element! {
        View(width: Constraint::Fill(1)) {
            Text(text: Paragraph::new(lines))
        }
    }
}

fn truncate_summary(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}...", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}
