use ratatui::{
    style::Style,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

/// 计算字符串前 char_idx 个字符的显示列宽度（CJK 字符占 2 列）。
pub fn display_width_before(s: &str, char_idx: usize) -> usize {
    s.chars()
        .take(char_idx)
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// 字符索引 → 字节偏移（行内辅助，复用 TextAreaState::char_to_byte 同逻辑）。
fn char_index_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// 把文本按 \n 拆成多行 Line，光标以传入的 style 高亮，选区以 selection_style 高亮。
///
/// I22-B：渲染窗口上限——只渲染 viewport_height 行（由调用方传入，等于 composer 可见行数）。
/// 当总行数超出 viewport_height 时以光标行为中心构建窗口，光标始终落在渲染窗口内。
/// viewport_height 最小值受 clamp(1) 保护以兼容 tests。
///
/// selection_range 为 `(start, end)` 字符索引的半开区间，cursor_style 优先级高于 selection_style。
/// placeholder 非空且 text 为空时，渲染占位符文本（类似 tui-textarea 风格：光标空间 + 占位文）。
pub fn render_multiline_with_cursor(
    text: &str,
    cursor: usize,
    cursor_style: Style,
    selection_range: Option<(usize, usize)>,
    selection_style: Style,
    placeholder: Option<&str>,
    placeholder_style: Style,
    viewport_height: usize,
    loading: bool,
    show_cursor: bool,
) -> Vec<Line<'static>> {
    let viewport_height = viewport_height.max(1);

    if text.is_empty() {
        return if loading {
            vec![Line::from("")]
        } else if !show_cursor {
            // 非聚焦态：无光标，仅渲染占位文本或空行
            if let Some(ph) = placeholder.filter(|s| !s.is_empty()) {
                vec![Line::from(vec![Span::styled(
                    ph.to_string(),
                    placeholder_style,
                )])]
            } else {
                vec![Line::from("")]
            }
        } else if let Some(ph) = placeholder.filter(|s| !s.is_empty()) {
            // 占位符：光标 styled space + 占位文本（tui-textarea 同款实现）
            vec![Line::from(vec![
                Span::styled(" ", cursor_style),
                Span::styled(ph.to_string(), placeholder_style),
            ])]
        } else {
            // 空态光标：styled space 与行尾光标保持一致，避免 ▓ 块与终端默认光标重叠产生"双光标"
            vec![Line::from(vec![Span::styled(" ", cursor_style)])]
        };
    }

    // 把光标位置映射到 (line_idx, col_idx)
    let mut chars_before_cursor = 0usize;
    let mut done = false;
    let mut target_line = 0usize;
    let mut target_col = 0usize;
    for (li, line) in text.split('\n').enumerate() {
        let line_chars = line.chars().count();
        if !done && chars_before_cursor + line_chars >= cursor {
            target_line = li;
            target_col = cursor - chars_before_cursor;
            done = true;
            break;
        }
        chars_before_cursor += line_chars + 1; // +1 for \n
        if chars_before_cursor > cursor + 1 {
            break;
        }
    }
    if !done {
        // 光标在文本末尾
        let total_lines: Vec<&str> = text.split('\n').collect();
        target_line = total_lines.len() - 1;
        target_col = total_lines.last().map(|l| l.chars().count()).unwrap_or(0);
    }

    // I22-B：计算渲染窗口 [start, end)，确保光标行包含在内。
    // 当总行数 <= viewport_height 时展示全部；否则以光标行为中心构建窗口，
    // 并在 end 被末尾钳位后向上扩展 start，保证窗口始终占满 viewport_height 行。
    let total_line_count = text.matches('\n').count() + 1;
    let (start, end) = if total_line_count <= viewport_height {
        (0, total_line_count)
    } else {
        let half_window = viewport_height / 2;
        let center_start = target_line.saturating_sub(half_window);
        let end = (center_start + viewport_height).min(total_line_count);
        let start = end.saturating_sub(viewport_height);
        (start, end)
    };

    // 全局字符索引——遍历时递增，用于将 selection_range（全局坐标）映射到行内坐标。
    let mut global_char_idx = 0usize;

    let mut result: Vec<Line<'static>> = Vec::with_capacity(end - start);
    for (li, line) in text.split('\n').enumerate() {
        // 跳过渲染窗口之前的行
        if li < start {
            global_char_idx += line.chars().count() + 1;
            continue;
        }
        if li >= end {
            break;
        }

        let line_chars = line.chars().count();
        let line_start_global = global_char_idx;

        // 计算选区与本行的重叠区间（行内字符索引的半开区间）
        let sel_in_line: Option<(usize, usize)> =
            selection_range.and_then(|(sel_start, sel_end)| {
                let overlap_start = sel_start.saturating_sub(line_start_global);
                let overlap_end = (sel_end - line_start_global).min(line_chars);
                if overlap_start < overlap_end {
                    Some((overlap_start, overlap_end))
                } else {
                    None
                }
            });

        if li == target_line {
            // ── 光标行：光标 + 选区合并渲染 ──
            // 光标字符位置用 display_width 定位（兼容 CJK 双宽）。
            let visual_col = display_width_before(line, target_col);
            let mut col = 0usize;
            let mut cut_byte = 0usize;
            for (i, ch) in line.char_indices() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if col + cw > visual_col {
                    break;
                }
                col += cw;
                cut_byte = i + ch.len_utf8();
            }
            // 光标字符的字节结束位置
            let cursor_end_byte = if cut_byte < line.len() {
                line[cut_byte..]
                    .chars()
                    .next()
                    .map(|c| cut_byte + c.len_utf8())
                    .unwrap_or(line.len())
            } else {
                line.len()
            };

            // 收集所有分段切点：选区边界 + 光标边界，排序去重后逐段构建 Span。
            let mut split_points: Vec<usize> = vec![0, line.len(), cut_byte, cursor_end_byte];
            if let Some((s_start, s_end)) = sel_in_line {
                split_points.push(char_index_to_byte(line, s_start));
                split_points.push(char_index_to_byte(line, s_end));
            }
            split_points.sort();
            split_points.dedup();

            let mut spans: Vec<Span<'static>> = Vec::new();
            for i in 0..split_points.len() - 1 {
                let seg_start = split_points[i];
                let seg_end = split_points[i + 1];
                if seg_start >= seg_end || seg_start >= line.len() {
                    continue;
                }
                let seg = &line[seg_start..seg_end.min(line.len())];
                if seg.is_empty() {
                    continue;
                }

                // 样式优先级：光标（最高）> 选区 > 默认
                let style = if show_cursor && seg_start >= cut_byte && seg_end <= cursor_end_byte {
                    cursor_style
                } else if let Some((s_start, s_end)) = sel_in_line {
                    let s_s_byte = char_index_to_byte(line, s_start);
                    let s_e_byte = char_index_to_byte(line, s_end);
                    if seg_start >= s_s_byte && seg_end <= s_e_byte {
                        selection_style
                    } else {
                        Style::default()
                    }
                } else {
                    Style::default()
                };

                spans.push(Span::styled(seg.to_string(), style));
            }

            // 光标在行尾时追加 styled space
            if show_cursor && target_col >= line_chars {
                spans.push(Span::styled(" ", cursor_style));
            }
            result.push(Line::from(spans));
        } else {
            // ── 非光标行：仅选区高亮（无选区则纯文本） ──
            if let Some((s_start, s_end)) = sel_in_line {
                let s_s_byte = char_index_to_byte(line, s_start);
                let s_e_byte = char_index_to_byte(line, s_end);
                let mut spans: Vec<Span<'static>> = Vec::new();
                if s_s_byte > 0 {
                    spans.push(Span::raw(line[..s_s_byte].to_string()));
                }
                spans.push(Span::styled(
                    line[s_s_byte..s_e_byte].to_string(),
                    selection_style,
                ));
                if s_e_byte < line.len() {
                    spans.push(Span::raw(line[s_e_byte..].to_string()));
                }
                result.push(Line::from(spans));
            } else {
                result.push(Line::from(line.to_string()));
            }
        }

        global_char_idx += line_chars + 1;
    }
    result
}
