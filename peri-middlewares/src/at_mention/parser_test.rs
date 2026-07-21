//! Tests for parser

use super::*;

#[test]
fn test_extract_plain_path() {
    // 普通路径提取
    let mentions = extract_at_mentions("看看 @src/main.rs 的内容");
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].path, "src/main.rs");
    assert_eq!(mentions[0].line_start, None);
    assert_eq!(mentions[0].line_end, None);
}

#[test]
fn test_extract_quoted_path() {
    // 带引号路径（含空格）
    let mentions = extract_at_mentions("查看 @\"my path/file.rs\" 内容");
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].path, "my path/file.rs");
}

#[test]
fn test_extract_line_range() {
    // 行范围提取
    let mentions = extract_at_mentions("看 @src/main.rs#L10-20");
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].path, "src/main.rs");
    assert_eq!(mentions[0].line_start, Some(10));
    assert_eq!(mentions[0].line_end, Some(20));
}

#[test]
fn test_extract_single_line() {
    // 单行提取
    let mentions = extract_at_mentions("看 @lib.rs#L42");
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].path, "lib.rs");
    assert_eq!(mentions[0].line_start, Some(42));
    assert_eq!(mentions[0].line_end, None);
}

#[test]
fn test_extract_multiple() {
    // 多个提及提取
    let mentions = extract_at_mentions("看 @foo.rs 和 @bar.ts#L5-10 还有 @baz/mod.rs");
    assert_eq!(mentions.len(), 3);
    assert_eq!(mentions[0].path, "foo.rs");
    assert_eq!(mentions[1].path, "bar.ts");
    assert_eq!(mentions[2].path, "baz/mod.rs");
    assert_eq!(mentions[1].line_start, Some(5));
    assert_eq!(mentions[1].line_end, Some(10));
}

#[test]
fn test_deduplicate() {
    // 重复路径去重
    let mentions = extract_at_mentions("@foo.rs 和 @foo.rs");
    assert_eq!(mentions.len(), 1);
}

#[test]
fn test_skip_email_like() {
    // 跳过 email 格式
    let mentions = extract_at_mentions("联系 user@example.com 或 @real/path.rs");
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].path, "real/path.rs");
}

#[test]
fn test_skip_short() {
    // 跳过单字符提及
    let mentions = extract_at_mentions("@a @bc");
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].path, "bc");
}
