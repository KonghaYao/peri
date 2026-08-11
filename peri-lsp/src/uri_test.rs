//! Tests for uri

use std::path::Path;

use super::{path_to_uri, uri_to_path};

#[test]
fn test_path_to_uri_spaces() {
    assert_eq!(
        path_to_uri(Path::new("/Users/a b.rs")),
        "file:///Users/a%20b.rs"
    );
}

#[test]
fn test_path_to_uri_chinese() {
    assert_eq!(
        path_to_uri(Path::new("/tmp/中文 文件.rs")),
        "file:///tmp/%E4%B8%AD%E6%96%87%20%E6%96%87%E4%BB%B6.rs"
    );
}

#[test]
fn test_path_to_uri_reserved_chars() {
    assert_eq!(
        path_to_uri(Path::new("/tmp/a#b?c%.rs")),
        "file:///tmp/a%23b%3Fc%25.rs"
    );
}

#[test]
fn test_path_to_uri_relative() {
    let uri = path_to_uri(Path::new("src/main.rs"));
    let abs = std::path::absolute("src/main.rs").unwrap();
    assert_eq!(uri, format!("file://{}", abs.display()));
}

#[test]
fn test_path_to_uri_relative_with_dotdot() {
    let uri = path_to_uri(Path::new("some/dir/../file.rs"));
    let cwd = std::env::current_dir().unwrap();
    let expect = format!("file://{}/some/dir/../file.rs", cwd.display());
    assert_eq!(uri, expect);
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
    let cases = ["/Users/a b.rs", "/tmp/中文 文件.rs", "/tmp/a#b?c%.rs"];
    for case in cases {
        assert_eq!(uri_to_path(&path_to_uri(Path::new(case))), case);
    }
}
