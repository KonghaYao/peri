use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{app::App, ui::theme};

/// 浅色背景色（不影响整体终端背景，只在文字区域可见）
const HEADER_BG: ratatui::style::Color = theme::USER_BG;

/// 渲染 sticky human message header
pub fn render_sticky_header(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }

    let msg = match &app.session_mgr.current().metadata.last_human_message {
        Some(m) => m,
        None => return,
    };

    // 可用宽度（留 padding）
    let width = area.width.saturating_sub(4).max(1) as usize;
    // 可显示内容行数
    let max_lines = area.height.max(1) as usize;

    // 将消息文本按宽度分多行
    let wrapped_lines = wrap_message(msg, width, max_lines);

    // 每行文字都有浅背景，无分隔线
    let bg_style = Style::default().bg(HEADER_BG);
    let text_style = Style::default().fg(theme::TEXT).bg(HEADER_BG);
    let label_style = Style::default()
        .fg(theme::ACCENT)
        .add_modifier(Modifier::BOLD)
        .bg(HEADER_BG);

    let lines: Vec<Line> = wrapped_lines
        .into_iter()
        .map(|text| {
            Line::from(vec![
                Span::styled("❯ ", label_style),
                Span::styled(text, text_style),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(Text::from(lines)).style(bg_style);
    f.render_widget(paragraph, area);
}

/// 根据终端宽度估算消息占用的视觉行数（用于 Layout 高度计算）
pub(super) fn estimate_header_lines(msg: &str, width: u16) -> usize {
    if width == 0 {
        return 1;
    }
    let width = width as usize;
    let display_width = UnicodeWidthStr::width(msg);
    let lines = display_width.div_ceil(width);
    lines.clamp(1, 3)
}

/// 将消息文本按显示列宽分多行（用于渲染）。
///
/// CJK 字符占 2 列，ASCII 占 1 列。使用 `unicode_width` 计算每字符的
/// 实际显示宽度，确保中文/英文混排时换行位置正确。
fn wrap_message(msg: &str, max_width: usize, max_lines: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![];
    }

    let mut result: Vec<String> = Vec::new();
    let mut line_start = 0usize; // 当前行起始字符索引（char 偏移）
    let mut line_width = 0usize; // 当前行已累计的显示列宽
    let chars: Vec<(usize, char)> = msg.char_indices().collect();

    let mut i = 0;
    while i < chars.len() && result.len() < max_lines {
        let (_, ch) = chars[i];
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);

        // 下一个字符会导致本行溢出
        if line_width + cw > max_width {
            // 查找本行内的断词点（向前搜索空格/全角空格）
            let break_at = if let Some(space_pos) = chars[line_start..i]
                .iter()
                .rposition(|(_, c)| c.is_ascii_whitespace() || *c == '　')
            {
                let abs_pos = line_start + space_pos;
                // 跳过断词空格
                if abs_pos + 1 < chars.len() {
                    abs_pos + 1
                } else {
                    i // 断词点后无内容，整行输出
                }
            } else {
                // 无双词点，硬截断
                i
            };

            let line_text: String = chars[line_start..break_at]
                .iter()
                .map(|(_, c)| *c)
                .collect();
            result.push(line_text);

            line_start = break_at;
            // 跳过行首空格
            while line_start < chars.len()
                && (chars[line_start].1.is_ascii_whitespace() || chars[line_start].1 == '　')
            {
                line_start += 1;
            }
            i = line_start;
            line_width = 0;
            continue;
        }

        line_width += cw;
        i += 1;
    }

    // 输出最后一行（剩余内容）
    if line_start < chars.len() && result.len() < max_lines {
        let line_text: String = chars[line_start..].iter().map(|(_, c)| *c).collect();
        result.push(line_text);
    }

    // 截断时在最后一行末尾加 …
    if i < chars.len() && result.len() == max_lines {
        if let Some(last) = result.last_mut() {
            let trimmed = last.trim_end();
            if !trimmed.is_empty() && !trimmed.ends_with('…') {
                let char_count = trimmed.chars().count();
                if char_count > 2 {
                    let suffix_start = char_count.saturating_sub(2);
                    let suffix: String = trimmed.chars().skip(suffix_start).collect();
                    let prefix: String = trimmed.chars().take(suffix_start).collect();
                    *last = format!("{}{}…", prefix, suffix.chars().next().unwrap_or(' '));
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_header_lines_cjk() {
        // "你好世界" = 4 字符，但显示宽度 = 8 列
        // width=8 → 正好 1 行
        assert_eq!(estimate_header_lines("你好世界", 8), 1);
        // width=4 → 8/4 = 2 行
        assert_eq!(estimate_header_lines("你好世界", 4), 2);
        // "你好世界你好世界" = 8 字符，显示宽度 = 16 列，width=5 → 16/5=4 行 → clamp 到 3
        assert_eq!(estimate_header_lines("你好世界你好世界", 5), 3);
    }

    #[test]
    fn test_estimate_header_lines_ascii() {
        // "hello" = 5 字符，显示宽度 = 5 列
        assert_eq!(estimate_header_lines("hello", 10), 1);
        assert_eq!(estimate_header_lines("hello world this is a test", 10), 3);
    }

    #[test]
    fn test_wrap_message_cjk() {
        // "你好世界" 占 8 列，max_width=6 → 应折行为 2 行
        let lines = wrap_message("你好世界", 6, 3);
        assert_eq!(lines.len(), 2, "CJK 8 列在 6 列宽度下应折为 2 行");
    }

    #[test]
    fn test_wrap_message_cjk_no_overflow() {
        // max_width=8，"你好"=4 列，"世界"=4 列，都不溢出
        let lines = wrap_message("你好你好世界世界", 6, 5);
        for line in &lines {
            let w = UnicodeWidthStr::width(line.as_str());
            assert!(w <= 6, "每行显示宽度应 ≤6，实际 {line:?} = {w} 列");
        }
    }

    #[test]
    fn test_wrap_message_mixed_cjk_ascii() {
        // "你好Hello" = 2+2+5 = 9 列
        let lines = wrap_message("你好Hello你好Hello", 8, 5);
        for line in &lines {
            let w = UnicodeWidthStr::width(line.as_str());
            assert!(w <= 8, "每行显示宽度应 ≤8，实际 {line:?} = {w} 列");
        }
    }

    #[test]
    fn test_wrap_message_word_break() {
        // ASCII 空格断词仍正常工作
        let lines = wrap_message("hello world foo bar baz", 10, 5);
        assert!(!lines.is_empty());
        for line in &lines {
            let w = UnicodeWidthStr::width(line.as_str());
            assert!(w <= 10, "每行显示宽度应 ≤10，实际 {line:?} = {w} 列");
        }
    }
}
