//! Tests for output_persist

use super::*;

#[test]
fn test_persist_writes_file_and_returns_hint() {
    let content = "line1\nline2\nline3";
    let hint = persist_truncated_output(content);
    // 提示应包含文件名
    assert!(
        hint.contains("peri-tool-output-"),
        "hint should contain filename: {hint}"
    );
    // 提示应引导用户使用 Read 工具
    assert!(
        hint.contains("Read"),
        "hint should guide to use Read tool: {hint}"
    );
    // 从提示中提取文件路径并验证内容
    let prefix = "saved to ";
    let suffix = " — use Read";
    let path_start = hint.find(prefix).unwrap() + prefix.len();
    let path_end = hint[path_start..]
        .find(suffix)
        .map(|i| path_start + i)
        .unwrap_or(hint.len());
    let path = &hint[path_start..path_end];
    let saved = fs::read_to_string(path).unwrap();
    assert_eq!(saved, content);
    fs::remove_file(path).ok();
}

#[test]
fn test_persist_empty_string() {
    let hint = persist_truncated_output("");
    // 空内容也应生成包含路径的提示
    assert!(
        hint.contains("Read"),
        "empty content should also produce hint: {hint}"
    );
    // 验证空文件确实被写入，并清理
    let prefix = "saved to ";
    let suffix = " — use Read";
    let path_start = hint.find(prefix).unwrap() + prefix.len();
    let path_end = hint[path_start..]
        .find(suffix)
        .map(|i| path_start + i)
        .unwrap_or(hint.len());
    let path = &hint[path_start..path_end];
    let saved = fs::read_to_string(path).unwrap();
    assert_eq!(saved, "");
    fs::remove_file(path).ok();
}
