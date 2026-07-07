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

/// 把文本按 \n 拆成多行 Line，光标以传入的 style 高亮。
///
/// I22-B：渲染窗口上限——只渲染光标附近的 MAX_RENDER_LINES 行。
/// 原实现遍历整个 text 拆分并生成 Line，paste 大文本时每帧分配 O(n) Vec。
/// 现改为以光标行为中心，仅渲染 MAX_RENDER_LINES 行（与 editor_height 上限一致），
/// 渲染成本由 O(总行数) 降为 O(12)。光标始终落在渲染窗口内。
pub fn render_multiline_with_cursor(
    text: &str,
    cursor: usize,
    cursor_style: Style,
    loading: bool,
) -> Vec<Line<'static>> {
    // I22-B：渲染窗口上限。editor_height 最大 12，所以渲染 >12 行是浪费。
    const MAX_RENDER_LINES: usize = 12;

    if text.is_empty() {
        return vec![if loading {
            Line::from("")
        } else {
            // 空态光标：styled space 与行尾光标保持一致，避免 ▓ 块与终端默认光标重叠产生"双光标"
            Line::from(vec![Span::styled(" ", cursor_style)])
        }];
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
    // 当总行数 <= MAX_RENDER_LINES 时展示全部；否则以光标行为中心构建窗口，
    // 并在 end 被末尾钳位后向上扩展 start，保证窗口始终占满 MAX_RENDER_LINES 行
    // （修复验证报告的"光标在末尾时窗口缩到 7 行"shrinkage bug）。
    let total_line_count = text.matches('\n').count() + 1;
    let (start, end) = if total_line_count <= MAX_RENDER_LINES {
        (0, total_line_count)
    } else {
        let half_window = MAX_RENDER_LINES / 2;
        let center_start = target_line.saturating_sub(half_window);
        let end = (center_start + MAX_RENDER_LINES).min(total_line_count);
        let start = end.saturating_sub(MAX_RENDER_LINES);
        (start, end)
    };

    let mut result: Vec<Line<'static>> = Vec::with_capacity(end - start);
    for (li, line) in text.split('\n').enumerate().skip(start).take(end - start) {
        if li == target_line {
            // 用 unicode-width 计算光标所在显示列，确保 CJK 双宽字符定位正确。
            // 与 text_selection.rs:visual_col_to_byte_offset 同策略。
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
            let mut spans: Vec<Span<'static>> = Vec::new();
            if cut_byte > 0 {
                spans.push(Span::raw(line[..cut_byte].to_string()));
            }
            if cut_byte < line.len() {
                // 反色高亮光标所在字符（用户期望的"字反色"行为）
                let next_end = line[cut_byte..]
                    .chars()
                    .next()
                    .map(|c| cut_byte + c.len_utf8())
                    .unwrap_or(line.len());
                spans.push(Span::styled(
                    line[cut_byte..next_end].to_string(),
                    cursor_style,
                ));
                if next_end < line.len() {
                    spans.push(Span::raw(line[next_end..].to_string()));
                }
            } else {
                // 光标在行尾
                spans.push(Span::styled(" ", cursor_style));
            }
            result.push(Line::from(spans));
        } else {
            result.push(Line::from(line.to_string()));
        }
    }
    result
}
