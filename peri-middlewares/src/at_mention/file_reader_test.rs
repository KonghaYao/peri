//! Tests for file_reader

use std::fs;

use tempfile::tempdir;

use super::*;

#[test]
fn test_read_file_full_content() {
    // 读取完整文件内容
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("test.rs"), "fn main() {}\n").unwrap();
    let result = read_file_content(dir.path(), "test.rs", None, None).unwrap();
    assert_eq!(result.content, "fn main() {}");
    assert!(!result.truncated);
    assert!(!result.is_dir);
}

#[test]
fn test_read_file_line_range() {
    // 读取指定行范围
    let dir = tempdir().unwrap();
    let content = "line1\nline2\nline3\nline4\nline5\n";
    fs::write(dir.path().join("test.txt"), content).unwrap();
    let result = read_file_content(dir.path(), "test.txt", Some(2), Some(4)).unwrap();
    assert_eq!(result.content, "line2\nline3\nline4");
    assert_eq!(result.line_start, Some(2));
    assert_eq!(result.line_end, Some(4));
}

#[test]
fn test_read_directory() {
    // 读取目录列表
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "").unwrap();
    fs::create_dir(dir.path().join("subdir")).unwrap();
    let result = read_file_content(dir.path(), ".", None, None).unwrap();
    assert!(result.is_dir);
    assert!(result.content.contains("a.txt"));
    assert!(result.content.contains("subdir/"));
}

#[test]
fn test_read_nonexistent_file() {
    // 不存在的文件返回 None
    let dir = tempdir().unwrap();
    let result = read_file_content(dir.path(), "nope.rs", None, None);
    assert!(result.is_none());
}

#[test]
fn test_path_traversal_blocked() {
    // 路径穿越被拒绝
    let dir = tempdir().unwrap();
    let result = read_file_content(dir.path(), "../../../etc/passwd", None, None);
    assert!(result.is_none());
}

#[test]
fn test_truncation() {
    // 超过 MAX_LINES 截断
    let dir = tempdir().unwrap();
    let lines: Vec<String> = (0..2500).map(|i| format!("line{i}")).collect();
    fs::write(dir.path().join("big.txt"), lines.join("\n")).unwrap();
    let result = read_file_content(dir.path(), "big.txt", None, None).unwrap();
    assert!(result.truncated);
    assert!(result.content.ends_with("... (truncated)"));
}
