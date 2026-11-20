//! 文本工具——面板/弹窗共用的小型文本处理函数。

/// 按 grapheme cluster + 显示列宽换行（CJK 双宽、组合字符不被从中间切开，
/// 不丢内容）。
///
/// 用于弹窗/详情视图中完整展示长 URL 等不可截断的文本；返回的行数即
/// 渲染行数（鼠标命中反推行号契约的依据）。`width == 0` 时原样返回单行——
/// 非空输入也必须终止（旧实现按码点切片，零宽会原地循环）。
pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    peri_tui_components::text::wrap_by_width(s, width)
}

#[cfg(test)]
mod tests {
    use super::wrap_text;

    #[test]
    fn empty_string_single_line() {
        assert_eq!(wrap_text("", 52), vec![String::new()]);
    }

    #[test]
    fn short_text_no_wrap() {
        assert_eq!(wrap_text("abc", 52), vec!["abc".to_string()]);
    }

    #[test]
    fn wraps_at_width() {
        assert_eq!(wrap_text("abcdefgh", 4), vec!["abcd", "efgh"]);
    }

    #[test]
    fn cjk_wraps_by_display_width() {
        // 每个 CJK 字符占 2 显示列：两列宽时四行（旧实现按码点切成两行，每行实际四列）
        assert_eq!(wrap_text("你好世界", 2), vec!["你", "好", "世", "界"]);
    }

    #[test]
    fn cjk_mixed_text_counts_columns_not_chars() {
        // "ab你好" = 1+1+2+2 = 6 列；4 列宽 → "ab你"（4 列）+ "好"
        assert_eq!(wrap_text("ab你好", 4), vec!["ab你", "好"]);
    }

    #[test]
    fn combining_mark_stays_with_base() {
        // e + U+0301 是一个 grapheme（1 列），不能被切成两行
        assert_eq!(wrap_text("e\u{301}x", 1), vec!["e\u{301}", "x"]);
    }

    #[test]
    fn zero_width_terminates_with_original_line() {
        // 非空输入的零宽必须终止（旧实现无限循环）
        assert_eq!(wrap_text("abc", 0), vec!["abc".to_string()]);
        assert_eq!(wrap_text("你好", 0), vec!["你好".to_string()]);
    }

    #[test]
    fn exact_multiple() {
        assert_eq!(wrap_text("abcdef", 3), vec!["abc", "def"]);
    }

    #[test]
    fn width_larger_than_text() {
        assert_eq!(wrap_text("ab", 10), vec!["ab".to_string()]);
    }
}
