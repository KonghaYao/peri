//! 文本工具——面板/弹窗共用的小型文本处理函数。

/// 按字符宽度换行（字符级切片，CJK 安全，不截断内容）。
///
/// 用于弹窗/详情视图中完整展示长 URL 等不可截断的文本；返回的行数即
/// 渲染行数（鼠标命中反推行号契约的依据）。
pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let end = (i + width).min(chars.len());
        out.push(chars[i..end].iter().collect());
        i = end;
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
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
    fn cjk_safe_slice() {
        // 每字符切 2 个宽度单位，字符级切片不 panic
        assert_eq!(wrap_text("你好世界", 2), vec!["你好", "世界"]);
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
