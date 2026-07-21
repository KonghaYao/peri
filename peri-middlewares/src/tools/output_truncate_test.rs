//! Tests for output_truncate

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
