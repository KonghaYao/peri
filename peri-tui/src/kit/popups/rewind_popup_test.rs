//! Tests

use super::*;

#[test]
fn test_truncate_str_short() {
    assert_eq!(truncate_str("hello", 10), "hello");
}

#[test]
fn test_truncate_str_exact() {
    assert_eq!(truncate_str("hello", 5), "hello");
}

#[test]
fn test_truncate_str_long() {
    assert_eq!(truncate_str("hello world", 5), "hello…");
}

#[test]
fn test_truncate_str_cjk() {
    // 中文字符 1 char = 3 bytes；chars().take 计 char 数不 panic
    assert_eq!(truncate_str("你好世界朋友", 4), "你好世界…");
}

#[test]
fn test_role_display_known() {
    assert_eq!(role_display("user"), "U");
    assert_eq!(role_display("assistant"), "A");
    assert_eq!(role_display("system"), "S");
    assert_eq!(role_display("tool"), "T");
}

#[test]
fn test_role_display_unknown() {
    assert_eq!(role_display("custom"), "?");
    assert_eq!(role_display(""), "?");
}

#[test]
fn test_rewind_view_toggle() {
    assert_ne!(RewindView::Messages, RewindView::Files);
    let v = RewindView::Messages;
    match v {
        RewindView::Messages => {}
        RewindView::Files => panic!("expected Messages"),
    }
}
