use pulldown_cmark::Alignment;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment as RAlignment, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::types::TableData;

// ── 列宽计算 ────────────────────────────────────────────────────────

/// 计算表格各列宽度（含等比例缩放适配 max_width）。
pub(crate) fn compute_table_col_widths(
    headers: &[Vec<Span<'static>>],
    rows: &[Vec<Vec<Span<'static>>>],
    col_count: usize,
    max_width: usize,
) -> Vec<usize> {
    let mut col_widths = vec![0usize; col_count];
    for (i, cell) in headers.iter().enumerate() {
        col_widths[i] = col_widths[i].max(span_width(cell));
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_count {
                col_widths[i] = col_widths[i].max(span_width(cell));
            }
        }
    }
    for w in &mut col_widths {
        *w = (*w).max(3);
    }

    let total_border = 1 + 3 * col_count;
    let needed = total_border + col_widths.iter().sum::<usize>();
    if needed > max_width {
        let available = max_width.saturating_sub(total_border);
        if available > 0 {
            let total: usize = col_widths.iter().sum();
            if total > 0 {
                let mut allocated = 0;
                for (i, w) in col_widths.iter_mut().enumerate() {
                    if i == col_count - 1 {
                        *w = available.saturating_sub(allocated).max(2);
                    } else {
                        *w = (*w * available / total).max(2);
                        allocated += *w;
                    }
                }
            }
        }
    }

    col_widths
}

fn span_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

// ── 公开渲染入口 ────────────────────────────────────────────────────

/// 使用 ratatui `Table` widget 渲染表格到 `Vec<Line>`（CJK 安全）。
pub fn table_data_to_lines(data: &TableData, border_style: Style) -> Vec<Line<'static>> {
    let col_count = data.col_widths.len();
    if col_count == 0 {
        return vec![Line::default()];
    }

    let r_align = |i: usize| match data.alignments.get(i) {
        Some(Alignment::Center) => RAlignment::Center,
        Some(Alignment::Right) => RAlignment::Right,
        _ => RAlignment::Left,
    };

    // 用 ratatui Paragraph 渲染单元格，提取对齐后的纯文本
    let align_cell = |spans: &[Span<'static>], w: u16, align: RAlignment| -> String {
        let text = Text::from(Line::from(spans.to_vec())).alignment(align);
        let para = Paragraph::new(text).alignment(align);
        let area = Rect::new(0, 0, w, 1);
        let mut buf = Buffer::empty(area);
        para.render(area, &mut buf);
        (0..w)
            .map(|x| {
                buf.cell((x, 0))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect()
    };

    let mut out: Vec<Line<'static>> = Vec::new();

    // 顶部边框
    out.push(build_grid_border(
        "┌",
        "┬",
        "┐",
        &data.col_widths,
        border_style,
    ));

    // 表头行
    if !data.headers.is_empty() {
        out.push(build_grid_row(
            &data.headers,
            &data.col_widths,
            &r_align,
            &align_cell,
            border_style,
        ));
        if !data.rows.is_empty() {
            out.push(build_grid_border(
                "├",
                "┼",
                "┤",
                &data.col_widths,
                border_style,
            ));
        }
    }

    // 数据行
    for row in &data.rows {
        out.push(build_grid_row(
            row,
            &data.col_widths,
            &r_align,
            &align_cell,
            border_style,
        ));
    }

    // 底部边框
    out.push(build_grid_border(
        "└",
        "┴",
        "┘",
        &data.col_widths,
        border_style,
    ));

    out
}

// ── 边框/行构建 ─────────────────────────────────────────────────────

fn build_grid_border(
    left: &str,
    cross: &str,
    right: &str,
    col_widths: &[usize],
    style: Style,
) -> Line<'static> {
    let mut parts = vec![Span::styled(left.to_string(), style)];
    for (i, w) in col_widths.iter().enumerate() {
        parts.push(Span::styled("─".repeat(*w + 2), style));
        if i < col_widths.len() - 1 {
            parts.push(Span::styled(cross.to_string(), style));
        }
    }
    parts.push(Span::styled(right.to_string(), style));
    Line::from(parts)
}

fn build_grid_row(
    cells: &[Vec<Span<'static>>],
    col_widths: &[usize],
    r_align: &dyn Fn(usize) -> RAlignment,
    align_cell: &dyn Fn(&[Span<'static>], u16, RAlignment) -> String,
    border_style: Style,
) -> Line<'static> {
    let mut parts = vec![Span::styled("│", border_style)];
    for (i, w) in col_widths.iter().enumerate() {
        let cell_spans = cells.get(i).map(|c| c.as_slice()).unwrap_or(&[]);
        let aligned = align_cell(cell_spans, *w as u16 + 2, r_align(i));
        parts.push(Span::styled(aligned, Style::default()));
        parts.push(Span::styled("│", border_style));
    }
    Line::from(parts)
}
