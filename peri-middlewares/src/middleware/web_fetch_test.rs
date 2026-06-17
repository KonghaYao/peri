use std::fs;

use super::*;

/// 从落盘提示中提取临时文件路径。
/// 提示格式：`... saved to <path> — use Read tool ...`
fn extract_path_from_hint(hint: &str) -> &str {
    let prefix = "saved to ";
    let suffix = " — use Read";
    let path_start = hint.find(prefix).expect("应包含 'saved to'") + prefix.len();
    let path_end = hint[path_start..]
        .find(suffix)
        .map(|i| path_start + i)
        .unwrap_or(hint.len());
    &hint[path_start..path_end]
}

#[test]
fn test_truncate_content_超限时触发落盘() {
    // 生成 MAX_CONTENT_LINES + 1 行内容
    let lines: Vec<String> = (0..=MAX_CONTENT_LINES)
        .map(|i| format!("line {i}"))
        .collect();
    let full_content = lines.join("\n");
    let result = truncate_content(&full_content, MAX_CONTENT_LINES);
    // 截断提示存在（P2-3: 统一为英文）
    assert!(
        result.contains("Content truncated"),
        "应包含截断提示: {result}"
    );
    // 落盘提示存在
    assert!(result.contains("saved to "), "应包含落盘路径提示: {result}");
    // 验证落盘文件内容与原始完全一致
    let path = extract_path_from_hint(&result);
    let saved = fs::read_to_string(path).expect("落盘文件应存在");
    assert_eq!(saved, full_content, "落盘内容应与原始内容完全一致");
    fs::remove_file(path).ok();
}

#[test]
fn test_truncate_content_未超限时不落盘() {
    let content = "line1\nline2\nline3";
    let result = truncate_content(content, MAX_CONTENT_LINES);
    assert_eq!(result, content, "未超限时应原样返回");
    assert!(
        !result.contains("saved to "),
        "未超限时不应有落盘提示: {result}"
    );
}

#[test]
fn test_truncate_content_行数未超但字节超限_触发落盘() {
    // 单行超大内容（模拟 minified JS），行数不超限但字节远超 MAX_CONTENT_CHARS
    let single_line = "x".repeat(MAX_CONTENT_CHARS + 1000);
    let result = truncate_content(&single_line, MAX_CONTENT_LINES);
    assert!(
        result.contains("exceeds") || result.contains("Content truncated"),
        "应包含字节截断提示: {result}"
    );
    assert!(result.contains("saved to "), "应包含落盘路径提示: {result}");
    // 验证落盘文件内容与原始完全一致
    let path = extract_path_from_hint(&result);
    let saved = fs::read_to_string(path).expect("落盘文件应存在");
    assert_eq!(saved, single_line, "落盘内容应与原始内容完全一致");
    fs::remove_file(path).ok();
}

#[test]
fn test_truncate_content_多行但字节超限_触发落盘() {
    // 多行，行数不超限但总字节超限：1500 行 x 100 字节 = 150000 > MAX_CONTENT_CHARS
    let line_content = "a".repeat(99) + "\n";
    let content = line_content.repeat(1500);
    assert!(
        content.lines().count() <= MAX_CONTENT_LINES,
        "测试数据应行数不超限: actual={}",
        content.lines().count()
    );
    let result = truncate_content(&content, MAX_CONTENT_LINES);
    assert!(result.contains("saved to "), "应包含落盘路径提示: {result}");
    let path = extract_path_from_hint(&result);
    let saved = fs::read_to_string(path).expect("落盘文件应存在");
    assert_eq!(saved, content, "落盘内容应与原始内容完全一致");
    fs::remove_file(path).ok();
}
