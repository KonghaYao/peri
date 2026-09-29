//! Tests for mod_fs

use std::fs;

use super::*;

#[test]
fn test_resolve_path_relative() {
    let cwd = std::env::temp_dir().to_string_lossy().to_string();
    let result = resolve_path(&cwd, "test.txt");
    // 父目录存在，应被规范化
    assert!(result.is_absolute());
    assert!(result.to_string_lossy().ends_with("test.txt"));
    // 不应包含未解析的 ..
    assert!(!result.to_string_lossy().contains(".."));
}

#[test]
fn test_resolve_path_absolute() {
    // 使用临时文件测试绝对路径解析，避免 Unix 特定路径依赖
    let tmp = std::env::temp_dir();
    let tmp_file = tmp.join("resolve_test_absolute.txt");
    std::fs::write(&tmp_file, "test").ok();
    let result = resolve_path(&tmp.to_string_lossy(), &tmp_file.to_string_lossy());
    assert!(result.is_absolute());
    assert!(result.exists());
    let _ = std::fs::remove_file(&tmp_file);
}

#[test]
fn test_resolve_path_traversal_canonicalized() {
    // 在 cwd 中创建子目录，然后用 .. 遍历出去
    let tmp = std::env::temp_dir();
    let sub = tmp.join("resolve_test_sub");
    let _ = fs::create_dir_all(&sub);

    let cwd = sub.to_string_lossy().to_string();
    let result = resolve_path(&cwd, "../resolve_test_file.txt");
    // 结果不应包含 ..，应该是 tmp/resolve_test_file.txt
    assert!(!result.to_string_lossy().contains(".."));
    assert!(result.to_string_lossy().contains("resolve_test_file.txt"));

    let _ = fs::remove_dir(&sub);
}
