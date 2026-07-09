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

/// 一个视觉行——逻辑行折行后的一行。选中映射和光标定位依赖 char_range。
#[derive(Debug, Clone)]
pub struct VisualLine {
    /// 来源逻辑行索引（text.split('\n') 中的行号）
    pub source_line: usize,
    /// 本视觉行的文本内容
    pub text: String,
    /// 本视觉行内字符范围（[start, end) 半开区间，全局坐标）
    /// 同时充当第一个字符的全局索引（等价于旧 char_start 字段）
    pub char_range: (usize, usize),
}

/// wrap_text 的返回值
#[derive(Debug, Clone)]
pub struct WrapResult {
    /// 折行后的视觉行列表
    pub visual_lines: Vec<VisualLine>,
    /// 光标所在的视觉行索引
    pub cursor_visual_row: usize,
    /// 光标在视觉行内的视觉列偏移（display width）
    pub cursor_visual_col: usize,
    /// 总视觉行数（等于 visual_lines.len()）
    pub total_visual_rows: usize,
}

/// 将文本按 max_width 做 display-width 感知的折行，返回视觉行列表和光标映射。
///
/// 折行策略：任意字符处断行（overflow-wrap: break-word），保证零宽字符
/// 不单独成行，CJK 双宽字符不打散。max_width 最小 1。
///
/// 光标位置 cursor 为字符索引。返回的 cursor_visual_row/col 是视觉坐标。
pub fn wrap_text(text: &str, cursor: usize, max_width: usize) -> WrapResult {
    let max_width = max_width.max(1);
    let cursor = cursor.min(text.chars().count());

    let mut visual_lines: Vec<VisualLine> = Vec::new();
    let mut cursor_visual_row = 0usize;
    let mut cursor_visual_col = 0usize;
    let mut global_char = 0usize;

    for (source_line, logical_line) in text.split('\n').enumerate() {
        let logical_start = global_char;
        let mut line_char_offset = 0usize; // 本逻辑行内字符偏移

        // 空逻辑行仍然产生一个空视觉行
        if logical_line.is_empty() {
            // 只在 global_char 精确匹配时设置光标（不在 +1 处——避免覆盖依赖）
            if cursor == global_char {
                cursor_visual_row = visual_lines.len();
                cursor_visual_col = 0;
            }
            visual_lines.push(VisualLine {
                source_line,
                text: String::new(),
                char_range: (global_char, global_char),
            });
            global_char += 1; // for \n
            continue;
        }

        let mut current_text = String::new();
        let mut current_width = 0usize;
        let mut segment_char_start = line_char_offset;

        for ch in logical_line.chars() {
            let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);

            // 当前行已有内容，加下一个字会超出 → 断行
            if current_width > 0 && current_width + ch_w > max_width {
                visual_lines.push(VisualLine {
                    source_line,
                    text: std::mem::take(&mut current_text),
                    char_range: (
                        logical_start + segment_char_start,
                        logical_start + line_char_offset,
                    ),
                });
                current_width = 0;
                segment_char_start = line_char_offset;
            }

            current_text.push(ch);
            current_width += ch_w;

            // 光标检查：在视觉行构建过程中匹配全局坐标
            let char_global = logical_start + line_char_offset;
            if cursor == char_global {
                cursor_visual_row = visual_lines.len();
                cursor_visual_col = current_width - ch_w; // 此字符之前
            } else if cursor == char_global + 1 {
                // 光标在这个字符之后
                cursor_visual_row = visual_lines.len();
                cursor_visual_col = current_width;
            }

            line_char_offset += 1;
        }

        // 该行剩余部分
        if !current_text.is_empty() || logical_line.is_empty() {
            // 再次检查光标（可能在行尾）
            let char_global = logical_start + line_char_offset;
            if cursor == char_global {
                cursor_visual_row = visual_lines.len();
                cursor_visual_col = current_width;
            }

            visual_lines.push(VisualLine {
                source_line,
                text: current_text,
                char_range: (
                    logical_start + segment_char_start,
                    logical_start + line_char_offset,
                ),
            });
        }

        global_char += logical_line.chars().count() + 1; // +1 for \n
    }

    // 光标可能在文本末尾（超出所有字符）
    if cursor >= text.chars().count() {
        cursor_visual_row = visual_lines.len().saturating_sub(1);
        cursor_visual_col = visual_lines
            .last()
            .map(|vl| {
                vl.text
                    .chars()
                    .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
    }

    let total_visual_rows = visual_lines.len();

    WrapResult {
        visual_lines,
        cursor_visual_row,
        cursor_visual_col,
        total_visual_rows,
    }
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
    max_width: usize,
    viewport_height: usize,
    loading: bool,
    show_cursor: bool,
) -> Vec<Line<'static>> {
    let viewport_height = viewport_height.max(1);

    if text.is_empty() {
        return if loading {
            vec![Line::from("")]
        } else if !show_cursor {
            if let Some(ph) = placeholder.filter(|s| !s.is_empty()) {
                vec![Line::from(vec![Span::styled(
                    ph.to_string(),
                    placeholder_style,
                )])]
            } else {
                vec![Line::from("")]
            }
        } else if let Some(ph) = placeholder.filter(|s| !s.is_empty()) {
            vec![Line::from(vec![
                Span::styled(" ", cursor_style),
                Span::styled(ph.to_string(), placeholder_style),
            ])]
        } else {
            vec![Line::from(vec![Span::styled(" ", cursor_style)])]
        };
    }

    // 使用 wrap_text 做软换行
    let wrap = wrap_text(text, cursor, max_width);

    // 视口裁剪基于视觉行
    let (start, end) = if wrap.total_visual_rows <= viewport_height {
        (0, wrap.total_visual_rows)
    } else {
        let half_window = viewport_height / 2;
        let center_start = wrap.cursor_visual_row.saturating_sub(half_window);
        let end = (center_start + viewport_height).min(wrap.total_visual_rows);
        let start = end.saturating_sub(viewport_height);
        (start, end)
    };

    let mut result: Vec<Line<'static>> = Vec::with_capacity(end - start);

    for vi in start..end {
        let vl = &wrap.visual_lines[vi];
        let line = &vl.text;
        let line_chars = line.chars().count();
        let is_cursor_line = vi == wrap.cursor_visual_row;

        // 计算选区与本视觉行的重叠区间（全局坐标 → 视觉行内坐标）
        let sel_in_line: Option<(usize, usize)> =
            selection_range.and_then(|(sel_start, sel_end)| {
                let (v_start, _v_end) = vl.char_range;
                let overlap_start = sel_start.saturating_sub(v_start);
                let overlap_end = (sel_end.saturating_sub(v_start)).min(line_chars);
                if overlap_start < overlap_end {
                    Some((overlap_start, overlap_end))
                } else {
                    None
                }
            });

        if is_cursor_line && show_cursor {
            // ── 光标行：光标 + 选区合并渲染 ──
            let target_col = wrap.cursor_visual_col;

            // 将 visual_col 映射到字符位置和字节
            let mut col = 0usize;
            let mut cut_byte = 0usize;
            for (i, ch) in line.char_indices() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if col + cw > target_col {
                    break;
                }
                col += cw;
                cut_byte = i + ch.len_utf8();
            }
            let cursor_end_byte = if cut_byte < line.len() {
                line[cut_byte..]
                    .chars()
                    .next()
                    .map(|c| cut_byte + c.len_utf8())
                    .unwrap_or(line.len())
            } else {
                line.len()
            };

            // 分段构建 spans
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
                let style = if seg_start >= cut_byte && seg_end <= cursor_end_byte {
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

            // 光标在视觉行尾时追加 styled space。
            // CR 修正：移除 !spans.is_empty() 条件——空视觉行（如空逻辑行）
            // 上 spans 为空但光标仍需可见。
            let line_display_w: usize = line
                .chars()
                .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                .sum();
            if target_col >= line_display_w {
                spans.push(Span::styled(" ", cursor_style));
            }
            result.push(Line::from(spans));
        } else if let Some((s_start, s_end)) = sel_in_line {
            // ── 非光标行：仅选区高亮 ──
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
            // ── 纯文本行 ──
            result.push(Line::from(line.to_string()));
        }
    }
    result
}
