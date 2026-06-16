/// 按字节截断字符串，确保不拆分 UTF-8 字符边界。
///
/// 与 `&s[..max_bytes]` 不同，此函数会从 `max_bytes` 位置向前搜索
/// 最近的字符边界，避免在多字节字符中间截断。
pub fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_bytes_ascii() {
        let s = "hello world";
        assert_eq!(truncate_bytes(s, 5), "hello");
    }

    #[test]
    fn test_truncate_bytes_within_limit() {
        let s = "hello";
        assert_eq!(truncate_bytes(s, 100), "hello");
    }

    #[test]
    fn test_truncate_bytes_utf8_safe() {
        let s = "你好世界";
        // "你好" = 6 bytes, "你" = 3 bytes each
        assert_eq!(truncate_bytes(s, 6), "你好");
    }

    #[test]
    fn test_truncate_bytes_utf8_mid_character() {
        let s = "你好";
        // 4 bytes — would split in the middle of 好 (position 3)
        let result = truncate_bytes(s, 5);
        assert_eq!(result, "你"); // 回退到字符边界
    }

    #[test]
    fn test_truncate_bytes_empty_string() {
        assert_eq!(truncate_bytes("", 10), "");
    }

    #[test]
    fn test_truncate_bytes_zero_max() {
        assert_eq!(truncate_bytes("hello", 0), "");
    }
}
