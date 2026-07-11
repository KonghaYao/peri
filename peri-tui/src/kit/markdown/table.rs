use pulldown_cmark::Alignment;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment as RAlignment, Rect},
    style::Style,
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::types::TableData;
use ratatui_kit::components::TableTheme;

// ── 列宽计算 ────────────────────────────────────────────────────────

/// 计算表格各列宽度（CJK 最小宽度保底 + 公平比例分配）。
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

    // 每列最小 8 个英文字符宽度
    for w in &mut col_widths {
        *w = (*w).max(8);
    }

    // 总开销：左│ + 2*pad*col + (col-1)*间│ + 右│ = 3*col + 1
    let overhead = 3 * col_count + 1;
    let needed = overhead + col_widths.iter().sum::<usize>();
    if needed <= max_width {
        return col_widths;
    }

    let available = max_width.saturating_sub(overhead);
    if available == 0 {
        return (0..col_count).map(|_| 1).collect();
    }

    let total: usize = col_widths.iter().sum();
    if total == 0 {
        return col_widths;
    }

    // 公平分配：floor → 缺口排序 → remainder
    let mut alloc: Vec<usize> = col_widths
        .iter()
        .enumerate()
        .map(|(_, &w)| ((w * available / total).max(1)).min(w).max(2))
        .collect();

    let allocated: usize = alloc.iter().sum();
    let mut remainder = available.saturating_sub(allocated);
    if remainder > 0 {
        let mut deficit: Vec<(usize, usize)> = (0..col_count)
            .map(|i| (i, col_widths[i].saturating_sub(alloc[i])))
            .collect();
        deficit.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
        for (i, _) in &deficit {
            if remainder == 0 {
                break;
            }
            if alloc[*i] < col_widths[*i] {
                alloc[*i] += 1;
                remainder -= 1;
            }
        }
    }
    if remainder > 0 {
        for w in alloc.iter_mut().take(remainder) {
            *w += 1;
        }
    }

    alloc
}

fn span_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

// ── Cell 换行（复刻 ratatui-kit） ──────────────────────────────────

fn wrap_cell_line(
    line: Line<'static>,
    max_width: usize,
    alignment: RAlignment,
) -> Vec<Line<'static>> {
    if max_width == 0 {
        return vec![Line::default()];
    }

    let style = line.spans.first().map(|s| s.style).unwrap_or_default();
    let full_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

    if full_text.width() <= max_width {
        return vec![align_line(
            Line::styled(full_text, style),
            max_width,
            alignment,
        )];
    }

    let mut lines = Vec::new();
    let mut byte_pos = 0;
    while byte_pos < full_text.len() {
        let mut cur_width = 0usize;
        let mut content_end = byte_pos;

        for (i, c) in full_text[byte_pos..].char_indices() {
            let cw = c.width().unwrap_or(0);
            if content_end > byte_pos && cur_width + cw > max_width {
                break;
            }
            cur_width += cw;
            content_end = byte_pos + i + c.len_utf8();
        }

        let mut break_at = content_end;
        for (i, c) in full_text[byte_pos..content_end].char_indices().rev() {
            if c.is_whitespace() {
                break_at = byte_pos + i;
                break;
            }
        }
        if break_at <= byte_pos {
            break_at = content_end;
        }

        let segment = full_text[byte_pos..break_at].trim();
        if !segment.is_empty() {
            lines.push(align_line(
                Line::styled(segment.to_string(), style),
                max_width,
                alignment,
            ));
        }

        byte_pos = break_at;
        while byte_pos < full_text.len()
            && full_text[byte_pos..]
                .chars()
                .next()
                .unwrap()
                .is_whitespace()
        {
            byte_pos += full_text[byte_pos..].chars().next().unwrap().len_utf8();
        }
    }

    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

fn align_line(line: Line<'static>, max_width: usize, alignment: RAlignment) -> Line<'static> {
    let width = line.width();
    let pad = max_width.saturating_sub(width);
    match alignment {
        RAlignment::Left => line,
        RAlignment::Center => {
            let left = pad / 2;
            let mut spans = vec![Span::raw(" ".repeat(left))];
            spans.extend(line.spans);
            spans.push(Span::raw(" ".repeat(pad - left)));
            Line::from(spans)
        }
        RAlignment::Right => {
            let mut spans = vec![Span::raw(" ".repeat(pad))];
            spans.extend(line.spans);
            Line::from(spans)
        }
    }
}

// ── 渲染结构 ───────────────────────────────────────────────────────

struct RenderedCell {
    lines: Vec<Line<'static>>,
}
struct RenderedRow {
    cells: Vec<RenderedCell>,
    style: Style,
    is_separator: bool,
}

impl RenderedRow {
    fn separator(style: Style) -> Self {
        Self {
            cells: vec![],
            style,
            is_separator: true,
        }
    }
    fn height(&self) -> u16 {
        if self.is_separator {
            1
        } else {
            self.cells
                .iter()
                .map(|c| c.lines.len() as u16)
                .max()
                .unwrap_or(1)
                .max(1)
        }
    }
}

// ── 渲染管线（复刻 ratatui-kit render_table） ──────────────────────

fn render_table_to_buffer(
    buf: &mut Buffer,
    area: Rect,
    rows: &[RenderedRow],
    col_widths: &[u16],
    border_style: Style,
    cell_pad: u16,
) {
    let mut y = area.y;
    render_hline(
        buf,
        area.x,
        y,
        col_widths,
        '┌',
        '┬',
        '┐',
        border_style,
        cell_pad,
    );
    y += 1;
    for (ri, row) in rows.iter().enumerate() {
        if y >= area.bottom() {
            break;
        }
        if row.is_separator {
            if ri > 0 && ri < rows.len() - 1 {
                render_hline(
                    buf,
                    area.x,
                    y,
                    col_widths,
                    '├',
                    '┼',
                    '┤',
                    border_style,
                    cell_pad,
                );
                y += 1;
            }
            continue;
        }
        for li in 0..row.height() {
            if y >= area.bottom() {
                break;
            }
            render_row_line(buf, area.x, y, row, li, col_widths, border_style, cell_pad);
            y += 1;
        }
    }
    if y < area.bottom() {
        render_hline(
            buf,
            area.x,
            y,
            col_widths,
            '└',
            '┴',
            '┘',
            border_style,
            cell_pad,
        );
    }
}

fn render_hline(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    widths: &[u16],
    l: char,
    m: char,
    r: char,
    s: Style,
    pad: u16,
) {
    let mut cx = x;
    put(buf, cx, y, l, s);
    cx += 1;
    for (i, &w) in widths.iter().enumerate() {
        for _ in 0..w + pad * 2 {
            put(buf, cx, y, '─', s);
            cx += 1;
        }
        if i + 1 < widths.len() {
            put(buf, cx, y, m, s);
            cx += 1;
        }
    }
    put(buf, cx, y, r, s);
}

fn render_row_line(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    row: &RenderedRow,
    li: u16,
    widths: &[u16],
    bs: Style,
    pad: u16,
) {
    let mut cx = x;
    put(buf, cx, y, '│', bs);
    cx += 1;
    for (i, &w) in widths.iter().enumerate() {
        for _ in 0..pad {
            put(buf, cx, y, ' ', row.style);
            cx += 1;
        }
        let cl = row
            .cells
            .get(i)
            .and_then(|c| c.lines.get(li as usize))
            .cloned()
            .unwrap_or_default();
        render_cell_line(buf, cx, y, w, cl, row.style);
        cx += w;
        for _ in 0..pad {
            put(buf, cx, y, ' ', row.style);
            cx += 1;
        }
        if i + 1 < widths.len() {
            put(buf, cx, y, '│', bs);
            cx += 1;
        }
    }
    put(buf, cx, y, '│', bs);
}

/// 渲染 Cell 行，CJK 感知——双宽字符第 2 列填空防残影。
fn render_cell_line(buf: &mut Buffer, x: u16, y: u16, width: u16, line: Line<'static>, rs: Style) {
    let mut off = 0u16;
    for span in line.spans {
        let s = rs.patch(span.style);
        for c in span.content.chars() {
            let cw = c.width().unwrap_or(0) as u16;
            if off + cw > width {
                return;
            }
            put(buf, x + off, y, c, s);
            if cw == 2 && off + 1 < width {
                put(buf, x + off + 1, y, ' ', s);
            }
            off += cw;
        }
    }
    while off < width {
        put(buf, x + off, y, ' ', rs);
        off += 1;
    }
}

fn put(buf: &mut Buffer, x: u16, y: u16, c: char, style: Style) {
    let cell = &mut buf[(x, y)];
    cell.set_char(c);
    cell.set_style(style);
}

// ── Buffer → Vec<Line>（CJK 安全——跳过双宽字符的 ghost cell） ─────

fn buffer_to_lines(buf: &Buffer, area: Rect) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(area.height as usize);
    for y in 0..area.height {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut text = String::new();
        let mut cur: Option<Style> = None;
        let mut prev_was_double = false;

        for x in 0..area.width {
            if prev_was_double {
                prev_was_double = false;
                continue;
            }

            let cell = buf.cell((x, y));
            let ch = cell.map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '));
            let style = cell.map(|c| c.style()).unwrap_or_default();

            if ch.width().unwrap_or(0) > 1 {
                prev_was_double = true;
            }

            match cur {
                Some(s) if s == style => text.push(ch),
                _ => {
                    if !text.is_empty() {
                        spans.push(Span::styled(
                            std::mem::take(&mut text),
                            cur.unwrap_or_default(),
                        ));
                    }
                    text.push(ch);
                    cur = Some(style);
                }
            }
        }
        if !text.is_empty() {
            spans.push(Span::styled(text, cur.unwrap_or_default()));
        }
        lines.push(Line::from(spans));
    }
    lines
}

// ── 公开 API ───────────────────────────────────────────────────────

pub fn table_data_to_lines(
    data: &TableData,
    theme: &TableTheme,
    max_width: usize,
) -> Vec<Line<'static>> {
    let n = data.col_widths.len();
    if n == 0 {
        return vec![Line::default()];
    }

    let wa: Vec<u16> = data.col_widths.iter().map(|&w| w as u16).collect();
    let ha = |i: usize| match data.alignments.get(i) {
        Some(Alignment::Center) => RAlignment::Center,
        Some(Alignment::Right) => RAlignment::Right,
        _ => RAlignment::Left,
    };

    let mut rows = Vec::new();

    if !data.headers.is_empty() {
        let cells: Vec<RenderedCell> = (0..n)
            .map(|i| {
                let spans = data.headers.get(i).cloned().unwrap_or_default();
                RenderedCell {
                    lines: wrap_cell_line(Line::from(spans), data.col_widths[i], ha(i)),
                }
            })
            .collect();
        rows.push(RenderedRow {
            cells,
            style: theme.header_style,
            is_separator: false,
        });
    }

    if !data.headers.is_empty() && !data.rows.is_empty() {
        rows.push(RenderedRow::separator(theme.border_style));
    }

    for row in &data.rows {
        let cells: Vec<RenderedCell> = (0..n)
            .map(|i| {
                let spans = row.get(i).cloned().unwrap_or_default();
                RenderedCell {
                    lines: wrap_cell_line(Line::from(spans), data.col_widths[i], ha(i)),
                }
            })
            .collect();
        rows.push(RenderedRow {
            cells,
            style: theme.row_style,
            is_separator: false,
        });
    }

    let cw = wa.iter().sum::<u16>() + 2 * wa.len() as u16 + (wa.len() as u16).saturating_sub(1);
    let tw = (cw + 2).min(max_width as u16).max(4);
    let th = (rows.iter().map(|r| r.height() as usize).sum::<usize>() + 2) as u16;
    let area = Rect::new(0, 0, tw, th);
    let mut buf = Buffer::empty(area);
    render_table_to_buffer(&mut buf, area, &rows, &wa, theme.border_style, 1);
    buffer_to_lines(&buf, area)
}
