use peri_widgets::BorderedPanel;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use super::AtMentionState;
use crate::ui::theme;

/// 弹窗最大显示行数
pub const MAX_VIEWPORT: usize = 10;

/// 渲染 @ 提及文件候选弹窗
pub fn render_at_mention_popup(f: &mut Frame, state: &AtMentionState, input_area: Rect) {
    if !state.active {
        return;
    }

    // 空目录 / 无匹配时显示占位消息
    if state.candidates.is_empty() {
        if let Some(ref msg) = state.empty_message {
            let popup_height = 3u16; // 内容 1 行 + 边框上下
            let y = input_area.y.saturating_sub(popup_height);
            let popup_area = Rect {
                x: input_area.x,
                y,
                width: input_area.width.min(30), // 占位消息窄即可
                height: popup_height,
            };
            let inner = BorderedPanel::new(Span::styled("", Style::default()))
                .border_style(Style::default().fg(theme::BORDER))
                .render(f, popup_area);
            let text = Span::styled(
                msg.as_str(),
                Style::default().fg(theme::TEXT).add_modifier(Modifier::DIM),
            );
            f.render_widget(Paragraph::new(text), inner);
        }
        return;
    }

    let total = state.candidates.len();
    let viewport = MAX_VIEWPORT.min(total);
    let scroll_offset = state.scroll_offset;
    let visible = &state.candidates[scroll_offset..scroll_offset + viewport];

    let popup_height = viewport as u16 + 2; // 内容 + 边框上下
    let y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: input_area.x,
        y,
        width: input_area.width,
        height: popup_height,
    };

    let inner = BorderedPanel::new(Span::styled("", Style::default()))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, popup_area);

    let content_width = inner.width as usize;
    // "❯ " 或 "  " 占 2 列 + " " 图标占 1 列 + 空格 1 列 = 4 列前缀
    let path_max_width = content_width.saturating_sub(4);

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(vi, cand)| {
            let global_idx = scroll_offset + vi;
            let is_selected = global_idx == state.selected;
            let icon = if cand.is_dir { "/" } else { " " };

            let display = truncate_middle(&cand.name.to_string_lossy(), path_max_width);

            let prefix = if is_selected { "❯ " } else { "  " };
            let style = if is_selected {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };

            Line::from(vec![
                Span::styled(
                    format!("{}{} ", prefix, icon),
                    Style::default().fg(theme::THINKING),
                ),
                Span::styled(display, style),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

/// 中间截断路径，保留前后部分，中间用 "..." 连接
fn truncate_middle(path: &str, max_width: usize) -> String {
    let width = UnicodeWidthStr::width(path);
    if width <= max_width {
        return path.to_string();
    }
    if max_width < 5 {
        // "..." 占 3 列 + 至少前后各 1 列 = 5
        return path.chars().take(max_width).collect();
    }

    let sep = "...";
    let sep_width = UnicodeWidthStr::width(sep);
    let remaining = max_width - sep_width;

    // 前半部分取 ceil(remaining/2) 列宽，后半取 floor(remaining/2)
    let head_width = remaining.div_ceil(2);
    let tail_width = remaining - head_width;

    let head: String = take_width(path, head_width);
    let tail: String = take_width_rev(path, tail_width);

    format!("{}{}{}", head, sep, tail)
}

/// 从字符串头部取指定显示宽度的字符
fn take_width(s: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max_width {
            break;
        }
        result.push(ch);
        w += cw;
    }
    result
}

/// 从字符串尾部取指定显示宽度的字符
fn take_width_rev(s: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut w = 0;
    for ch in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max_width {
            break;
        }
        result.push(ch);
        w += cw;
    }
    result.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short() {
        // 短于 max_width 不截断
        assert_eq!(truncate_middle("abc", 10), "abc");
    }

    #[test]
    fn test_truncate_long() {
        let result = truncate_middle("abcdefghijklmnopqrstuvwxyz", 10);
        let width = UnicodeWidthStr::width(result.as_str());
        assert_eq!(width, 10, "截断后宽度应等于 max_width");
        assert!(result.contains("..."), "应包含省略号");
    }

    #[test]
    fn test_truncate_cjk() {
        // 每个汉字占 2 列，CJK 字符不可拆分，实际宽度可能小于 max_width
        let result = truncate_middle("你好世界测试数据", 10);
        let width = UnicodeWidthStr::width(result.as_str());
        assert!(width <= 10, "CJK 截断后宽度不应超过 max_width");
        assert!(result.contains("..."));
    }

    #[test]
    fn test_truncate_exact() {
        // 刚好等于 max_width
        assert_eq!(truncate_middle("abcde", 5), "abcde");
    }
}
