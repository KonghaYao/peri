//! Display-width text helpers shared by the TUI renderers.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// 严格总列宽截断使用的省略号风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ellipsis {
    /// 单个 `…`（1 列）。
    Single,
    /// ASCII 三点（3 列）；预算只剩 1 或 2 列时只输出放得下的 `.`/`..`。
    Ascii,
}

/// 截断到 `max_width` 个终端显示列，省略号计入预算。
///
/// 契约：输出显示宽度 ≤ `max_width`；`max_width == 0` 返回空串。按 grapheme
/// cluster 迭代，CJK 双宽与组合字符不会被从中间切开。旧的前缀预算调用方
/// （省略号额外占列）应改用 [`prefix_by_width`]。
pub fn truncate_to_width(s: &str, max_width: usize) -> String {
    truncate_to_width_with(s, max_width, Ellipsis::Single)
}

/// [`truncate_to_width`] 的可选省略号风格版本。
pub fn truncate_to_width_with(s: &str, max_width: usize, ellipsis: Ellipsis) -> String {
    // 零预算契约优先：零宽组合字符的 `width()` 为 0，不能走下面的原样返回。
    if max_width == 0 {
        return String::new();
    }
    if s.width() <= max_width {
        return s.to_string();
    }
    let marker = match ellipsis {
        Ellipsis::Single => "…".to_string(),
        Ellipsis::Ascii => ".".repeat(max_width.min(3)),
    };
    let mut out = prefix_by_width(s, max_width.saturating_sub(marker.width()));
    out.push_str(&marker);
    out
}

/// 取不超过 `max_width` 列的前缀（不追加省略号）。
pub fn prefix_by_width(s: &str, max_width: usize) -> String {
    let mut width = 0usize;
    let mut out = String::new();
    for g in s.graphemes(true) {
        let w = g.width();
        if width + w > max_width {
            break;
        }
        out.push_str(g);
        width += w;
    }
    out
}

/// 取从 `offset` 列开始、最多 `max_width` 列的片段（不折行、不追加省略号）。
///
/// 输出显示宽度 ≤ `max_width`；宽字素跨左边界时以空格补齐被切掉的半格，
/// 跨右边界时整字素舍弃（与终端裁剪一致）。
///
/// 列宽按字符串口径（[`UnicodeWidthStr::width`]）测量，与 `content_width`
/// 一致。连字（Arabic Lam–Alef、Tifinagh 连接符等）会让前缀宽度在字素边界处
/// 回落，逐字素累加与二分都不可靠；这类串走慢路径逐边界实测。
pub fn slice_by_width(s: &str, offset: usize, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let naive: usize = s.graphemes(true).map(UnicodeWidthStr::width).sum();
    if naive == s.width() {
        return slice_by_grapheme(s, offset, max_width);
    }
    let mut boundaries: Vec<usize> = s.grapheme_indices(true).map(|(byte, _)| byte).collect();
    boundaries.push(s.len());
    let start = boundaries
        .iter()
        .position(|&byte| s[..byte].width() >= offset)
        .unwrap_or(boundaries.len() - 1);
    let start_byte = boundaries[start];
    let blank = s[..start_byte]
        .width()
        .saturating_sub(offset)
        .min(max_width);
    // 余量内尽量多取：以片段自身的实测宽度为准，避免连字把累计值算大/算小。
    let budget = max_width.saturating_sub(blank);
    let mut end_byte = start_byte;
    for &byte in &boundaries[start + 1..] {
        if s[start_byte..byte].width() > budget {
            break;
        }
        end_byte = byte;
    }
    let mut out = " ".repeat(blank);
    out.push_str(&s[start_byte..end_byte]);
    out
}

/// 无跨字素连字时的快路径：前缀宽度单调，逐字素累加即可。
fn slice_by_grapheme(s: &str, offset: usize, max_width: usize) -> String {
    let mut out = String::new();
    let mut column = 0usize;
    let mut emitted = 0usize;
    for grapheme in s.graphemes(true) {
        let width = grapheme.width();
        if column + width <= offset {
            column += width;
            continue;
        }
        if column < offset {
            // 宽字素被左边界切开：可见的半格留空。
            let blank = (column + width - offset).min(max_width);
            out.push_str(&" ".repeat(blank));
            emitted += blank;
            column += width;
            continue;
        }
        if emitted + width > max_width {
            break;
        }
        out.push_str(grapheme);
        emitted += width;
        column += width;
    }
    out
}

/// 取不超过 `max_width` 列的后缀（不追加省略号）。
pub fn suffix_by_width(s: &str, max_width: usize) -> String {
    let mut width = 0usize;
    let mut parts: Vec<&str> = Vec::new();
    for g in s.graphemes(true).rev() {
        let w = g.width();
        if width + w > max_width {
            break;
        }
        parts.push(g);
        width += w;
    }
    parts.into_iter().rev().collect()
}

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
    use super::{
        Ellipsis, prefix_by_width, slice_by_width, suffix_by_width, truncate_to_width,
        truncate_to_width_with, wrap_by_width,
    };
    use unicode_width::UnicodeWidthStr;

    /// 严格模式：输出含省略号不超过预算；0/1 列边界可预测。
    #[test]
    fn strict_truncate_fits_zero_and_one_column() {
        assert_eq!(truncate_to_width("abcdef", 0), "");
        assert_eq!(truncate_to_width("abcdef", 1), "…");
        assert_eq!(truncate_to_width("abcdef", 2), "a…");
        assert_eq!(
            truncate_to_width("啊", 1),
            "…",
            "双宽字符放不下时只留省略号"
        );
        assert_eq!(truncate_to_width("", 0), "");
        assert_eq!(
            truncate_to_width("\u{301}", 0),
            "",
            "零宽组合字符在零预算下仍返回空串"
        );
    }

    /// 刚好填满预算不截断；多一列才补省略号。
    #[test]
    fn strict_truncate_keeps_exact_fit() {
        assert_eq!(truncate_to_width("hello", 5), "hello");
        assert_eq!(truncate_to_width("hello", 4), "hel…");
        assert_eq!(truncate_to_width("你好", 4), "你好");
        assert_eq!(truncate_to_width("你好世", 4), "你…");
        assert_eq!(truncate_to_width("你好世", 5), "你好…");
    }

    /// 组合字符与 ZWJ 序列作为整体保留，省略号仍计入预算。
    #[test]
    fn strict_truncate_keeps_graphemes_whole() {
        let e_acute = "e\u{301}";
        assert_eq!(truncate_to_width(&format!("{e_acute}abc"), 3), "e\u{301}a…");
        assert_eq!(truncate_to_width(&format!("{e_acute}abc"), 2), "e\u{301}…");
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(
            truncate_to_width(&format!("{family}abcd"), 3),
            format!("{family}…")
        );
        for budget in 0..=8 {
            assert!(truncate_to_width(&format!("{family}x{e_acute}"), budget).width() <= budget);
        }
    }

    /// ASCII 三点模式：预算 1/2 列只输出放得下的点。
    #[test]
    fn ascii_ellipsis_shrinks_to_available_columns() {
        let out = |budget| truncate_to_width_with("abcdef", budget, Ellipsis::Ascii);
        assert_eq!(out(0), "");
        assert_eq!(out(1), ".");
        assert_eq!(out(2), "..");
        assert_eq!(out(3), "...");
        assert_eq!(out(4), "a...");
        assert_eq!(out(5), "ab...");
        assert_eq!(out(6), "abcdef");
    }

    /// 前缀/后缀 helper 只按字素取整块，不补省略号。
    #[test]
    fn prefix_and_suffix_stay_grapheme_safe() {
        assert_eq!(prefix_by_width("你好世界", 3), "你");
        assert_eq!(suffix_by_width("你好世界", 3), "界");
        assert_eq!(prefix_by_width("abcdef", 0), "");
        assert_eq!(suffix_by_width("abcdef", 99), "abcdef");
    }

    /// 列切片：不折行、不补省略号；宽字素跨边界时按终端裁剪语义留空/舍弃。
    #[test]
    fn slice_takes_exact_column_window() {
        assert_eq!(slice_by_width("abcdef", 2, 3), "cde");
        assert_eq!(slice_by_width("abcdef", 0, 3), "abc");
        assert_eq!(slice_by_width("abcdef", 5, 3), "f");
        assert_eq!(slice_by_width("abcdef", 6, 3), "");
        assert_eq!(slice_by_width("abcdef", 0, 0), "");
        // 双宽字素跨左边界：被切掉的半格留空；跨右边界：整字素舍弃。
        assert_eq!(slice_by_width("啊bc", 1, 2), " b");
        assert_eq!(slice_by_width("abc啊", 2, 2), "c");
        assert_eq!(slice_by_width("abc啊", 2, 3), "c啊");
        for (offset, width) in [(0, 1), (1, 1), (1, 2), (2, 2), (3, 1)] {
            assert!(
                slice_by_width("a你b好c", offset, width).width() <= width,
                "offset={offset} width={width}"
            );
        }
        // 连字跨字素变窄：字符串宽 5，与 content_width 同口径才可达最右列。
        assert_eq!("لاabcd".width(), 5);
        assert_eq!(slice_by_width("لاabcd", 2, 3), "bcd");
        assert_eq!(slice_by_width("لاabcd", 4, 3), "d");
        assert_eq!(slice_by_width("لاabcd", 0, 3), "لاab");
        // Tifinagh 连接符会让前缀宽度在边界处回落：输出仍不得越界。
        let tifinagh = "\u{2D4F}\u{2D7F}\u{2D3E}abc";
        for (offset, width) in [(0, 1), (1, 1), (1, 2), (2, 2), (3, 3)] {
            let slice = slice_by_width(tifinagh, offset, width);
            assert!(
                slice.width() <= width,
                "offset={offset} width={width} slice={slice:?}"
            );
        }
    }

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
