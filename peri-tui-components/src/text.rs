//! Display-width text helpers shared by the TUI renderers.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Wrap text into lines of at most `max_width` terminal display columns.
///
/// 按 grapheme cluster + display width 折行（CJK 双宽 / emoji ZWJ / combining
/// mark 不被从中间切开）；超宽单词按宽度切分成多行，**不丢内容**（区别于
/// truncate 的省略号截断）。`max_width == 0` 时原样返回单行。
pub fn wrap_by_width(s: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![s.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_width = 0usize;
    for g in s.graphemes(true) {
        let w = g.width();
        if !cur.is_empty() && cur_width + w > max_width {
            lines.push(std::mem::take(&mut cur));
            cur_width = 0;
        }
        // 单个 grapheme 即超宽（罕见）：独占一行也不丢内容
        if w > max_width && cur.is_empty() {
            lines.push(g.to_string());
            continue;
        }
        cur.push_str(g);
        cur_width += w;
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::wrap_by_width;

    #[test]
    fn keeps_short_line() {
        assert_eq!(wrap_by_width("hello", 40), vec!["hello"]);
        assert_eq!(wrap_by_width("hello", 5), vec!["hello"]);
    }

    #[test]
    fn wastes_no_content_for_cjk_and_emoji() {
        let text = "你好世界";
        assert_eq!(wrap_by_width(text, 4), vec!["你好", "世界"]);
        assert_eq!(wrap_by_width("a👩‍👩‍👧b", 2), vec!["a", "👩‍👩‍👧", "b"]);
    }

    #[test]
    fn zero_width_returns_original() {
        assert_eq!(wrap_by_width("x", 0), vec!["x"]);
        assert_eq!(wrap_by_width("", 10), vec![""]);
    }
}
