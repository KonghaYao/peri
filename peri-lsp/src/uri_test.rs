//! Tests for uri

use std::path::Path;

use super::{path_to_uri, uri_to_path};

/// 绝对路径编码断言：Unix 用 `/` 前缀路径，Windows 用盘符路径。
/// 两者验证同一套编码规则（空格→%20、非 ASCII→UTF-8 字节、保留字符→%XX）。
#[cfg(unix)]
fn assert_abs_path_uri(path: &str, expect: &str) {
    assert_eq!(path_to_uri(Path::new(path)), expect);
}

#[cfg(windows)]
fn assert_abs_path_uri(path: &str, expect: &str) {
    // Windows 上无盘符的 `/...` 会落到当前盘根，与 Unix 语义不同；
    // 用盘符绝对路径验证同样的编码行为。标准 file URI 形式：
    // file:///C:/...（空 authority、正斜杠分隔）
    assert_eq!(path_to_uri(Path::new(path)), expect);
}

#[test]
fn test_path_to_uri_spaces() {
    #[cfg(unix)]
    assert_abs_path_uri("/Users/a b.rs", "file:///Users/a%20b.rs");
    #[cfg(windows)]
    assert_abs_path_uri("C:\\Users\\a b.rs", "file:///C:/Users/a%20b.rs");
}

#[test]
fn test_path_to_uri_chinese() {
    #[cfg(unix)]
    assert_abs_path_uri(
        "/tmp/中文 文件.rs",
        "file:///tmp/%E4%B8%AD%E6%96%87%20%E6%96%87%E4%BB%B6.rs",
    );
    #[cfg(windows)]
    assert_abs_path_uri(
        "C:\\tmp\\中文 文件.rs",
        "file:///C:/tmp/%E4%B8%AD%E6%96%87%20%E6%96%87%E4%BB%B6.rs",
    );
}

#[test]
fn test_path_to_uri_reserved_chars() {
    #[cfg(unix)]
    assert_abs_path_uri("/tmp/a#b?c%.rs", "file:///tmp/a%23b%3Fc%25.rs");
    #[cfg(windows)]
    assert_abs_path_uri("C:\\tmp\\a#b?c%.rs", "file:///C:/tmp/a%23b%3Fc%25.rs");
}

#[test]
fn test_path_to_uri_relative() {
    // 相对路径基于 cwd 绝对化 + 编码；roundtrip 应还原绝对化后的路径
    let uri = path_to_uri(Path::new("src/main.rs"));
    let abs = std::path::absolute("src/main.rs").unwrap();
    assert!(uri.starts_with("file://"));
    assert_eq!(uri_to_path(&uri), abs.to_string_lossy().as_ref());
}

#[test]
fn test_path_to_uri_relative_with_dotdot() {
    let uri = path_to_uri(Path::new("some/dir/../file.rs"));
    let abs = std::path::absolute("some/dir/../file.rs").unwrap();
    assert!(uri.starts_with("file://"));
    assert_eq!(uri_to_path(&uri), abs.to_string_lossy().as_ref());
}

#[test]
fn test_path_to_uri_already_prefix() {
    let uri = "file:///Users/a%20b.rs";
    assert_eq!(path_to_uri(Path::new(uri)), uri);
}

#[test]
fn test_uri_to_path_basic() {
    assert_eq!(uri_to_path("file:///Users/a%20b.rs"), "/Users/a b.rs");
}

#[test]
fn test_uri_to_path_chinese() {
    assert_eq!(
        uri_to_path("file:///tmp/%E4%B8%AD%E6%96%87%20%E6%96%87%E4%BB%B6.rs"),
        "/tmp/中文 文件.rs"
    );
}

#[test]
fn test_uri_to_path_no_prefix() {
    assert_eq!(uri_to_path("/plain/path"), "/plain/path");
}

#[test]
fn test_uri_to_path_invalid_percent_kept() {
    assert_eq!(uri_to_path("file:///tmp/a%zz%2"), "/tmp/a%zz%2");
}

#[test]
fn test_path_uri_roundtrip() {
    // 用例本身是 Unix 风格路径，Windows 下会按盘符语义绝对化（如 /Users → D:\Users）；
    // roundtrip 断言只要求 path_to_uri/uri_to_path 对同一绝对化路径互逆
    let cases = ["/Users/a b.rs", "/tmp/中文 文件.rs", "/tmp/a#b?c%.rs"];
    for case in cases {
        let abs = std::path::absolute(case).unwrap();
        let abs_str = abs.to_string_lossy().into_owned();
        assert_eq!(uri_to_path(&path_to_uri(&abs)), abs_str);
    }
}
