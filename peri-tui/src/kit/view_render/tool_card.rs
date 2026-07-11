//! 工具调用卡片渲染（v2 TuiRenderUnit 渲染器）。
//!
//! 包含折叠/展开规则（对应 TUI-TOOLCALL.md §1.3）。

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::kit::tool_display;
use crate::kit::tui_render_unit::{TuiDiffBlock, TuiHunkLineKind};
use peri_theme::atoms::THEME_ATOM;

// ── 折叠/展开规则（TUI-PAGE.md §2.4.2） ────────────────────────────────

pub(crate) const COLLAPSED_BY_DEFAULT: &[&str] =
    &["Bash", "Read", "Glob", "Grep", "AskUserQuestion"];
pub(crate) const AUTO_EXPAND: &[&str] = &["AgentResult", "ExecuteExtraTool", "SearchExtraTools"];
pub(crate) const FORCE_EXPAND_ON_COMPLETE: &[&str] = &["Write", "Edit"];

/// 工具调用卡片渲染（v2 TuiRenderUnit 渲染器）。
pub(crate) fn render_tool_card(
    data: &crate::kit::tui_render_unit::TuiToolCard,
) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let display = tool_display_fn(&data.tool_name, data.is_error, data.is_running);
    let display_name = tool_display::format_tool_name(&data.tool_name).to_string();

    let mut header_spans = vec![
        Span::styled(display.indicator, Style::default().fg(display.color)),
        Span::raw(" "),
        Span::styled(
            display_name,
            Style::default()
                .fg(semantic.text.primary)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let summary = compact_summary(&data.input_summary, 400);
    if !summary.is_empty() {
        header_spans.push(Span::styled(
            format!(" ({})", summary),
            Style::default().fg(semantic.text.dim),
        ));
    }

    let mut lines = vec![Line::from(header_spans)];

    if data.tool_name == "Bash" && data.is_running && !data.is_error {
        let duration = data.running_duration_ms.unwrap_or(0);
        lines.push(Line::from(vec![
            Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
            Span::styled(
                format!("Running ({})", format_running_duration(duration)),
                Style::default().fg(semantic.text.muted),
            ),
        ]));
    }

    // Agent 工具运行行：tool calls 计数 + 运行时长（仿 Shell Running 行）
    if data.tool_name == "Agent" && data.is_running && !data.is_error {
        let duration = data.running_duration_ms.unwrap_or(0);
        let tool_count = data.tool_calls_count;
        if tool_count > 0 {
            lines.push(Line::from(vec![
                Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
                Span::styled(
                    format!(
                        "{} tool calls, running {}",
                        tool_count,
                        format_running_duration(duration)
                    ),
                    Style::default().fg(semantic.text.muted),
                ),
            ]));
        }
    }

    // 折叠/展开判断（纯 UI 决策，对应 TUI-TOOLCALL.md §1.3）
    let collapsed = if data.is_error {
        false // 错误不折叠
    } else if AUTO_EXPAND.contains(&data.tool_name.as_str()) {
        false // AgentResult/ExecuteExtraTool 自动展开
    } else if FORCE_EXPAND_ON_COMPLETE.contains(&data.tool_name.as_str()) {
        data.is_running // Write/Edit 运行中折叠，完成后展开
    } else {
        COLLAPSED_BY_DEFAULT.contains(&data.tool_name.as_str())
    };

    if collapsed {
        if !data.output_summary.is_empty() {
            let color = if data.is_error {
                semantic.status.error
            } else {
                semantic.text.muted
            };
            for out_line in compact_output_lines(&data.output_summary, 1, 400) {
                lines.push(Line::from(vec![
                    Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
                    Span::styled(out_line, Style::default().fg(color)),
                ]));
            }
        }
        return with_message_spacing(lines);
    }

    // 输出摘要
    if !data.output_summary.is_empty() {
        let result_color = if data.is_error {
            semantic.status.error
        } else {
            semantic.text.muted
        };
        let border_color = if data.is_error {
            semantic.status.error
        } else {
            semantic.text.dim
        };
        let max_lines = if data.tool_name == "TodoWrite" {
            usize::MAX
        } else {
            4
        };
        for out_line in compact_output_lines(&data.output_summary, max_lines, 400) {
            lines.push(Line::from(vec![
                Span::styled("  \u{23bf} ".to_string(), Style::default().fg(border_color)),
                Span::styled(out_line, Style::default().fg(result_color)),
            ]));
        }
    }

    // Diff 变更统计（Write/Edit）
    if let Some(ref diff) = data.diff
        && let Some(summary) = diff_change_summary(diff)
    {
        lines.push(Line::from(vec![
            Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
            Span::styled(summary, Style::default().fg(semantic.text.muted)),
        ]));
    }

    with_message_spacing(lines)
}

pub(crate) fn format_running_duration(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let secs = secs % 60;
    if mins > 0 {
        format!("{}min {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

pub(crate) fn diff_change_summary(diff: &TuiDiffBlock) -> Option<String> {
    let adds = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l.kind, TuiHunkLineKind::Add))
        .count();
    let dels = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l.kind, TuiHunkLineKind::Del))
        .count();
    if adds == 0 && dels == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if adds > 0 {
        parts.push(format!("+{}", adds));
    }
    if dels > 0 {
        parts.push(format!("-{}", dels));
    }
    Some(parts.join(" \u{b7} "))
}

struct ToolDisplay {
    indicator: &'static str,
    color: Color,
}

fn tool_display_fn(_tool_name: &str, is_error: bool, is_running: bool) -> ToolDisplay {
    let semantic = THEME_ATOM.state().read().semantic;
    if is_error {
        return ToolDisplay {
            indicator: "\u{25cf}",
            color: semantic.status.error,
        };
    }

    if is_running {
        // 运行中：常量白色 ●。原 RENDER_CALL_COUNT 闪烁逻辑失效（计数器每批次
        // 在 append_entries 末尾 reset 为 0，且 render 层禁止跨帧写 atom 状态）。
        // 运行态视觉信号由 Bash 卡片的 "Running (duration)" 行独立提供。
        return ToolDisplay {
            indicator: "\u{25cf}",
            color: Color::White,
        };
    }

    ToolDisplay {
        indicator: "\u{25cf}",
        color: semantic.status.success,
    }
}

pub(crate) fn trim_trailing_blank_lines(lines: &mut Vec<Line<'static>>) {
    while lines
        .last()
        .is_some_and(|line| line.spans.iter().all(|span| span.content.is_empty()))
    {
        lines.pop();
    }
}

pub(crate) fn with_message_spacing(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    trim_trailing_blank_lines(&mut lines);
    let mut spaced = Vec::with_capacity(lines.len() + 1);
    spaced.push(Line::from(""));
    spaced.extend(lines);
    // 只加头部空行，尾部空行由下一条消息的头部空行提供——避免相邻消息间出现双空行
    spaced
}

pub(crate) fn compact_summary(text: &str, max_chars: usize) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    truncate_str(&joined, max_chars)
}

pub(crate) fn compact_output_lines(text: &str, max_lines: usize, max_chars: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(max_lines)
        .map(|line| truncate_str(line, max_chars))
        .collect();

    let total = text.lines().filter(|line| !line.trim().is_empty()).count();
    if total > max_lines {
        lines.push(format!("\u{2026} {} more lines", total - max_lines));
    }

    lines
}

// ── 工具函数 ──────────────────────────────────────────────────────────────

/// 字符级截断（保证 CJK 安全）。
pub(crate) fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}\u{2026}", truncated)
    }
}
