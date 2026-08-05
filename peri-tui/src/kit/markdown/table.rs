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

/// 计算表格各列宽度。
///
/// 宽度约束策略：先取自然宽度（各列最宽单元格）；超限时不做等比缩放，
/// 而是以「最长不可断词」为每列最小宽度（保证单词/数字/URL 不被拦腰截断），
/// 再按各列超出最小宽度的需求量比例分配剩余预算。
pub(crate) fn compute_table_col_widths(
    headers: &[Vec<Span<'static>>],
    rows: &[Vec<Vec<Span<'static>>>],
    col_count: usize,
    max_width: usize,
) -> Vec<usize> {
    if col_count == 0 {
        return vec![];
    }

    // 1. 自然宽度：每列取 header + 所有数据单元格的最宽值
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

    // 2. 超限时约束：总开销 = 左│ + 2*pad*col + (col-1)*间│ + 右│ = col*(2*pad+1) + 1
    let padding = 1usize;
    let overhead = col_count * (2 * padding + 1) + 1;
    let content_budget = max_width.saturating_sub(overhead);
    let total_content: usize = col_widths.iter().sum();

    if total_content > content_budget && total_content > 0 {
        // 每列最小宽度 = 该列最长不可断词（智能断词保证的单词边界）。
        // 例如 "Catherine"、"$145,000"、"EMP-1001" 各自是不可断整体。
        let mut min_col_widths = vec![1usize; col_count];
        for (i, cell) in headers.iter().enumerate() {
            if i >= col_count {
                break;
            }
            let text = span_plain(cell);
            for (s, e) in cell_words(&text) {
                min_col_widths[i] = min_col_widths[i].max(text[s..e].width());
            }
        }
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                if i >= col_count {
                    break;
                }
                let text = span_plain(cell);
                for (s, e) in cell_words(&text) {
                    min_col_widths[i] = min_col_widths[i].max(text[s..e].width());
                }
            }
        }

        // 所有列先取最小宽度，再把剩余预算按「想要超出最小值的量」比例分配
        let min_total: usize = min_col_widths.iter().sum();
        let extra_budget = content_budget.saturating_sub(min_total);
        let extra_wants: Vec<usize> = col_widths
            .iter()
            .enumerate()
            .map(|(i, &w)| w.saturating_sub(min_col_widths[i]))
            .collect();
        let total_extra_want: usize = extra_wants.iter().sum();

        let mut new_widths = min_col_widths.clone();
        if total_extra_want > 0 && extra_budget > 0 {
            // 按需求量比例分配（floor）
            for (i, &want) in extra_wants.iter().enumerate() {
                let share =
                    (want as f64 * extra_budget as f64 / total_extra_want as f64).floor() as usize;
                new_widths[i] += share;
            }

            // floor 舍入的余量：优先补给「未满足需求」最大的列，不超自然宽度
            let used: usize = new_widths.iter().sum();
            let mut remaining = content_budget.saturating_sub(used);
            let mut indices: Vec<usize> = (0..col_count).collect();
            indices.sort_by(|&a, &b| {
                let unmet_a = col_widths[a].saturating_sub(new_widths[a]);
                let unmet_b = col_widths[b].saturating_sub(new_widths[b]);
                unmet_b.cmp(&unmet_a)
            });
            for &idx in &indices {
                if remaining == 0 {
                    break;
                }
                if new_widths[idx] < col_widths[idx] {
                    new_widths[idx] += 1;
                    remaining -= 1;
                }
            }
        }

        col_widths = new_widths;
    }

    col_widths
}

fn span_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

fn span_plain(spans: &[Span<'_>]) -> String {
    spans.iter().map(|s| s.content.as_ref()).collect()
}

// ── Cell 换行（智能断词 + FirstFit 贪心 + 样式保留） ────────────────

/// 智能断词：返回文本中不可分割词元的 `(start, end)` 字节区间。
///
/// 断点规则：
/// - 空白后遇到非空白 → 断（常规词边界）
/// - 标点/符号后跟字母 → 断（`foo/bar`、`EMP-1001` 可断）
/// - 标点后跟数字，且标点前也是数字 → 断，但 `,` / `.` 除外
///   （`$145,000`、`3.14` 视为数字格式整体，不在中间断）
/// - 含 `://` 的 token 视为 URL，内部一律不断（避免链接被拦腰截断）
///
/// 断点归属：标点可随左词或右词，选 `max(左段宽, 右段宽)` 更小的一侧，
/// 使两侧宽度尽量均衡。
fn cell_words(text: &str) -> Vec<(usize, usize)> {
    // Pass 1: 扫描断点。记录 (break_pos, punct_start)：
    //   break_pos 是标点归左时下一词起点；punct_start 是标点归右时标点起点。
    let mut breaks: Vec<(usize, usize)> = Vec::new();
    {
        let mut in_whitespace = false;
        let mut after_break_char = false;
        let mut prev_is_digit = false; // 前一个字符是否为数字
        let mut digit_before_break = false; // 断点字符前是否为数字
        let mut last_break_ch = '\0';
        let mut break_char_start = 0usize;
        for (idx, ch) in text.char_indices() {
            let is_space = ch == ' ';
            let is_break_char = !is_space && !ch.is_alphanumeric();

            let should_break = if in_whitespace && !is_space {
                true
            } else if after_break_char {
                if ch.is_alphabetic() {
                    true
                } else if ch.is_ascii_digit() && digit_before_break {
                    // digit-punct-digit：仅数字格式标点（, / .）不断
                    last_break_ch != ',' && last_break_ch != '.'
                } else {
                    false
                }
            } else {
                false
            };

            if should_break {
                if in_whitespace {
                    breaks.push((idx, idx));
                } else {
                    breaks.push((idx, break_char_start));
                }
            }

            if is_break_char {
                break_char_start = idx;
                last_break_ch = ch;
                digit_before_break = prev_is_digit;
            }
            prev_is_digit = ch.is_ascii_digit();
            in_whitespace = is_space;
            after_break_char = is_break_char;
        }
    }

    // URL 保护：空格分隔的 token 若含 `://`，其内部断点全部移除
    let url_ranges: Vec<(usize, usize)> = {
        let mut ranges = Vec::new();
        let mut pos = 0usize;
        for token in text.split_whitespace() {
            let start = text[pos..].find(token).unwrap() + pos;
            let end = start + token.len();
            if token.contains("://") {
                ranges.push((start, end));
            }
            pos = end;
        }
        ranges
    };
    breaks.retain(|&(break_pos, _)| {
        !url_ranges
            .iter()
            .any(|&(s, e)| break_pos > s && break_pos < e)
    });

    // Pass 2: 决定每个断点的标点归属侧（minimize max(左, 右)）
    let mut split_positions: Vec<usize> = Vec::with_capacity(breaks.len());
    {
        let len = text.len();
        for (i, &(attach_left, attach_right)) in breaks.iter().enumerate() {
            if attach_left == attach_right {
                split_positions.push(attach_left);
                continue;
            }
            let seg_start = if i == 0 { 0 } else { split_positions[i - 1] };
            let seg_end = if i + 1 < breaks.len() {
                breaks[i + 1].0
            } else {
                len
            };

            let max_attach_left = text[seg_start..attach_left]
                .width()
                .max(text[attach_left..seg_end].width());
            let max_attach_right = text[seg_start..attach_right]
                .width()
                .max(text[attach_right..seg_end].width());
            if max_attach_right < max_attach_left {
                split_positions.push(attach_right);
            } else {
                split_positions.push(attach_left);
            }
        }
    }

    // Pass 3: 按最终切分点产出词元（跳过空词）
    let mut words = Vec::new();
    let mut pos = 0usize;
    let mut idx = 0usize;
    while pos < text.len() {
        let end = if idx < split_positions.len() {
            let e = split_positions[idx];
            idx += 1;
            e
        } else {
            text.len()
        };
        if end > pos {
            words.push((pos, end));
        }
        pos = end.max(pos + 1);
    }
    words
}

/// 超宽词按列宽硬切分段（显示宽度，char 边界安全）。
///
/// Buffer 渲染管线在单元格绘制时对超出行宽的内容直接丢弃，若让超宽词
/// 整词溢出，文本会永久丢失（复制内容残缺）。因此超宽词按列宽切成
/// 多个完整显示的分段，保证内容逐行可见且拼接后完整。
fn chunk_wide_word(text: &str, s: usize, e: usize, width: usize) -> Vec<(usize, usize)> {
    let mut chunks = Vec::new();
    let mut start = s;
    let mut cur = 0usize;
    for (idx, c) in text[s..e].char_indices() {
        let cw = c.width().unwrap_or(0);
        let abs = s + idx;
        if cur > 0 && cur + cw > width {
            chunks.push((start, abs));
            start = abs;
            cur = 0;
        }
        cur += cw;
    }
    if start < e {
        chunks.push((start, e));
    }
    if chunks.is_empty() {
        chunks.push((s, e));
    }
    chunks
}

/// FirstFit 贪心换行：按断词顺序把词元累积进当前行，放不下则换行；
/// 单个词元超出宽度时按列宽分段（内容不丢，见 [`chunk_wide_word`]）。
/// 返回每个视觉行在原文中的字节区间。
fn wrap_text_ranges(text: &str, width: usize) -> Vec<(usize, usize)> {
    if width == 0 {
        return vec![(0, 0)];
    }
    let words = cell_words(text);
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut cur_start: Option<usize> = None;
    let mut cur_width = 0usize;
    let mut cur_end = 0usize;
    for (s, e) in words {
        let w = text[s..e].width();
        if w > width {
            // 超宽词：先 flush 当前行，再逐段独占一行
            if let Some(start) = cur_start.take() {
                lines.push((start, cur_end));
                cur_width = 0;
                cur_end = 0;
            }
            for (cs, ce) in chunk_wide_word(text, s, e, width) {
                lines.push((cs, ce));
            }
            continue;
        }
        let sep = if cur_start.is_some() { 1 } else { 0 };
        if cur_start.is_some() && cur_width + sep + w > width {
            lines.push((cur_start.take().unwrap(), cur_end));
            cur_width = w;
            cur_end = e;
            cur_start = Some(s);
        } else {
            if cur_start.is_none() {
                cur_start = Some(s);
            }
            cur_width += sep + w;
            cur_end = e;
        }
    }
    if let Some(s) = cur_start {
        lines.push((s, cur_end));
    }
    if lines.is_empty() {
        lines.push((0, 0));
    }
    lines
}

/// 按字节区间从 spans 中切片，保留每段原有样式（wrap 跨行不丢格式）。
fn slice_spans(spans: &[Span<'static>], start: usize, end: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for span in spans {
        let span_start = offset;
        let span_end = offset + span.content.len();
        offset = span_end;
        let a = span_start.max(start);
        let b = span_end.min(end);
        if a < b {
            let text = span.content[a - span_start..b - span_start].to_string();
            out.push(Span::styled(text, span.style));
        }
    }
    out
}

fn wrap_cell_line(
    line: Line<'static>,
    max_width: usize,
    alignment: RAlignment,
) -> Vec<Line<'static>> {
    if max_width == 0 {
        return vec![Line::default()];
    }

    let fallback_style = line.spans.first().map(|s| s.style).unwrap_or_default();
    let full_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

    if full_text.width() <= max_width {
        return vec![align_line(line, max_width, alignment)];
    }

    let ranges = wrap_text_ranges(&full_text, max_width);
    let mut lines = Vec::with_capacity(ranges.len());
    for (s, e) in ranges {
        let mut spans = slice_spans(&line.spans, s, e);
        if spans.is_empty() {
            spans.push(Span::styled(String::new(), fallback_style));
        }
        lines.push(align_line(Line::from(spans), max_width, alignment));
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

#[allow(clippy::too_many_arguments)]
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

#[allow(clippy::too_many_arguments)]
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

    // 渲染列宽：parse 时已按 max_width 约束过一次，这里再做兜底缩放
    // （终端极窄时列宽 + 边框可能超出 buffer 宽度，越界会 panic）。
    let wa0: Vec<u16> = data.col_widths.iter().map(|&w| w as u16).collect();
    let cw = wa0.iter().sum::<u16>() + 2 * wa0.len() as u16 + (wa0.len() as u16).saturating_sub(1);
    let tw = (cw + 2).min(max_width as u16).max(4);
    let wa = if cw + 2 > tw {
        let n = wa0.len() as u16;
        // 边框开销：左右 2 + cell padding*2*n + n-1 列间分隔 = 3*n + 1
        let border_overhead: u16 = 3 * n + 1;
        let content_space = tw.saturating_sub(border_overhead);
        // 每列至少 1 宽 → n 列至少需要 n
        if content_space < n {
            return vec![Line::default()];
        }
        let total: u16 = wa0.iter().sum();
        if total == 0 {
            return vec![Line::default()];
        }
        // 按比例分配，每列至少 1，sum 可能超过 content_space
        let mut new_wa: Vec<u16> = wa0
            .iter()
            .map(|&w| ((w as u32 * content_space as u32 / total as u32) as u16).max(1))
            .collect();
        // 二次归一化：如果 sum > content_space，等比例压缩
        let actual: u16 = new_wa.iter().sum();
        if actual > content_space {
            new_wa = new_wa
                .iter()
                .map(|&w| ((w as u32 * content_space as u32 / actual as u32) as u16).max(1))
                .collect();
        }
        new_wa
    } else {
        wa0
    };

    // 换行必须使用最终渲染列宽 wa：若用 parse 时的列宽 wrap，
    // 缩放后的窄列会把内容截断丢弃（Buffer 绘制不保留超宽文本）。
    let wa_usize: Vec<usize> = wa.iter().map(|&w| w as usize).collect();
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
                    lines: wrap_cell_line(Line::from(spans), wa_usize[i], ha(i)),
                }
            })
            .collect();
        rows.push(RenderedRow {
            cells,
            style: theme.header_style,
            is_separator: false,
        });
    }

    if !data.headers.is_empty() {
        rows.push(RenderedRow::separator(theme.border_style));
    }

    for (i, row) in data.rows.iter().enumerate() {
        let cells: Vec<RenderedCell> = (0..n)
            .map(|i| {
                let spans = row.get(i).cloned().unwrap_or_default();
                RenderedCell {
                    lines: wrap_cell_line(Line::from(spans), wa_usize[i], ha(i)),
                }
            })
            .collect();
        rows.push(RenderedRow {
            cells,
            style: theme.row_style,
            is_separator: false,
        });
        // 数据行之间的横向分隔线（最后一行之后不画）
        if i + 1 < data.rows.len() {
            rows.push(RenderedRow::separator(theme.border_style));
        }
    }

    let th = (rows.iter().map(|r| r.height() as usize).sum::<usize>() + 2) as u16;
    let area = Rect::new(0, 0, tw, th);
    let mut buf = Buffer::empty(area);
    render_table_to_buffer(&mut buf, area, &rows, &wa, theme.border_style, 1);
    buffer_to_lines(&buf, area)
}
