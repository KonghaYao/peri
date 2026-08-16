use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::{TuiDivider, TuiNoteLevel, TuiSystemNote, TuiTodoSummary};
use crate::truncate::truncate_by_width;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::style::{Color, Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::helpers::{cont_prefix, first_prefix, sym};

/// §6.6 System event：普通事件单行 divider；warning/error 用符号 + accent。
///
/// - Info：`── {来源文本} ──…`（divider 填满 content 列）。
/// - Warning/Error：`[!|×][gap]{文本}` + 后续行 muted（错误含恢复动作文本）。
pub(super) fn render_system_note_lines(
    data: &TuiSystemNote,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    // 剥离旧版 ✻/⏿ 前缀标记，统一走新符号层
    let clean: Vec<String> = data
        .text
        .lines()
        .map(|l| {
            l.trim_start()
                .trim_start_matches('\u{273b}')
                .trim_start_matches("\u{23bf}")
                .trim_start()
                .to_string()
        })
        .collect();

    match data.level {
        TuiNoteLevel::Info => {
            let label = clean.first().cloned().unwrap_or_default();
            vec![divider_with_label_line(grid, &label)]
        }
        TuiNoteLevel::Warning => {
            let (symbol, color) = (sym().warning, sem.status.warning);
            render_note_lines(grid, symbol, color, &clean)
        }
        TuiNoteLevel::Error => {
            let (symbol, color) = (sym().error, sem.status.error);
            render_note_lines(grid, symbol, color, &clean)
        }
    }
}

fn render_note_lines(
    grid: &GridSpec,
    symbol: &str,
    color: Color,
    clean: &[String],
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut lines = Vec::new();
    for (i, text) in clean.iter().enumerate() {
        if text.is_empty() {
            continue;
        }
        let mut spans = if i == 0 {
            first_prefix(
                grid,
                symbol,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        } else {
            cont_prefix(grid, sem.text.dim)
        };
        let fg = if i == 0 { color } else { sem.text.muted };
        spans.push(Span::styled(
            truncate_by_width(text, grid.content_width()),
            Style::default().fg(fg),
        ));
        lines.push(Line::from(spans));
    }
    if lines.is_empty() {
        // 空 note 兜底：至少渲染一条符号行
        lines.push(Line::from(first_prefix(
            grid,
            symbol,
            Style::default().fg(color),
        )));
    }
    lines
}

/// `── {label} ──…` divider 行（label 为 None 时纯分隔线），填满 content 列。
fn divider_with_label_line(grid: &GridSpec, label: &str) -> Line<'static> {
    let sem = THEME_ATOM.state().read().semantic;
    let content = grid.content_width();
    let inner = match label {
        "" => "\u{2500}\u{2500}".to_string(),
        l => format!("\u{2500}\u{2500} {l}"),
    };
    let inner = truncate_by_width(&inner, content);
    let fill = content.saturating_sub(inner.width());
    let mut spans = first_prefix(grid, "\u{2500}", Style::default().fg(sem.text.dim));
    spans.push(Span::styled(
        format!("{}{}", inner, "\u{2500}".repeat(fill)),
        Style::default().fg(sem.text.dim),
    ));
    Line::from(spans)
}

/// 分隔线：turn 边界 divider（无 label → 纯分隔线；有 label → `── label ──`）。
pub(super) fn render_divider_lines(data: &TuiDivider, grid: &GridSpec) -> Vec<Line<'static>> {
    let label = data.label.as_deref().unwrap_or("");
    vec![divider_with_label_line(grid, label)]
}

/// §6.9 Todo 进度摘要行：`[◼][gap]{3/7 tasks · Running tests}`。
pub(super) fn render_todo_summary_lines(
    data: &TuiTodoSummary,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut spans = first_prefix(grid, "\u{25fc}", Style::default().fg(sem.accent));
    spans.push(Span::styled(
        truncate_by_width(&data.text, grid.content_width()),
        Style::default().fg(sem.text.muted),
    ));
    vec![Line::from(spans)]
}
