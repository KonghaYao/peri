//! `scan_harness_docs` 测试（设计 §2.2）。
//!
//! 用临时目录验证扫描规则：一级 `.md`、key 去扩展名、非 md 忽略、
//! 不递归、单文件读取失败跳过、目录不存在返回空、全文保留。

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::scan_harness_docs;

fn setup(dir: &TempDir) -> String {
    // cwd 本身作为 cwd 参数；.peri/meta 在 cwd 之下
    let cwd = dir.path();
    let meta = cwd.join(".peri").join("meta");
    fs::create_dir_all(&meta).unwrap();
    cwd.to_string_lossy().to_string()
}

#[test]
fn reads_top_level_md_files() {
    let tmp = TempDir::new().unwrap();
    let cwd = setup(&tmp);
    let meta = PathBuf::from(&cwd).join(".peri").join("meta");
    fs::write(meta.join("01_intro.md"), "# intro body").unwrap();
    fs::write(meta.join("05_using_tools.md"), "# tools body").unwrap();

    let docs = scan_harness_docs(&cwd);
    assert_eq!(docs.len(), 2);
    assert_eq!(
        docs.get("01_intro").map(String::as_str),
        Some("# intro body")
    );
    assert_eq!(
        docs.get("05_using_tools").map(String::as_str),
        Some("# tools body")
    );
}

#[test]
fn file_name_without_extension_becomes_key() {
    let tmp = TempDir::new().unwrap();
    let cwd = setup(&tmp);
    let meta = PathBuf::from(&cwd).join(".peri").join("meta");
    fs::write(meta.join("weird.name.md"), "x").unwrap();

    let docs = scan_harness_docs(&cwd);
    assert_eq!(docs.get("weird.name"), Some(&"x".to_string()));
    assert!(!docs.contains_key("weird.name.md"));
}

#[test]
fn ignores_non_md_files() {
    let tmp = TempDir::new().unwrap();
    let cwd = setup(&tmp);
    let meta = PathBuf::from(&cwd).join(".peri").join("meta");
    fs::write(meta.join("01_intro.md"), "a").unwrap();
    fs::write(meta.join("notes.txt"), "b").unwrap();
    fs::write(meta.join("script.sh"), "c").unwrap();
    fs::write(meta.join("README"), "d").unwrap();
    fs::write(meta.join("UPPER.MD"), "e").unwrap(); // 扩展名不精确为 md

    let docs = scan_harness_docs(&cwd);
    assert_eq!(docs.len(), 1);
    assert_eq!(docs.get("01_intro"), Some(&"a".to_string()));
}

#[test]
fn does_not_recurse_into_subdirs() {
    let tmp = TempDir::new().unwrap();
    let cwd = setup(&tmp);
    let meta = PathBuf::from(&cwd).join(".peri").join("meta");
    fs::create_dir_all(meta.join("nested")).unwrap();
    fs::write(meta.join("01_intro.md"), "top").unwrap();
    fs::write(meta.join("nested").join("02_system.md"), "deep").unwrap();
    // 目录名带 .md 后缀也不进入（is_file 过滤）
    fs::create_dir_all(meta.join("03_doing_tasks.md")).unwrap();

    let docs = scan_harness_docs(&cwd);
    assert_eq!(docs.len(), 1);
    assert_eq!(docs.get("01_intro"), Some(&"top".to_string()));
}

#[test]
fn unreadable_file_skipped_others_still_load() {
    let tmp = TempDir::new().unwrap();
    let cwd = setup(&tmp);
    let meta = PathBuf::from(&cwd).join(".peri").join("meta");
    fs::write(meta.join("01_intro.md"), "ok").unwrap();
    // 目录伪装成 bad.md（is_file = false）→ 跳过但不影响其他文件
    fs::create_dir_all(meta.join("bad.md")).unwrap();
    fs::write(meta.join("bad.md").join("inner.md"), "nested").unwrap();

    let docs = scan_harness_docs(&cwd);
    assert_eq!(docs.len(), 1);
    assert_eq!(docs.get("01_intro"), Some(&"ok".to_string()));
}

#[test]
fn missing_dir_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().to_string(); // 无 .peri/meta
    let docs = scan_harness_docs(&cwd);
    assert!(docs.is_empty());
}

#[test]
fn preserves_full_text_with_whitespace() {
    let tmp = TempDir::new().unwrap();
    let cwd = setup(&tmp);
    let meta = PathBuf::from(&cwd).join(".peri").join("meta");
    let body = "  line one\n\nline two  \n# heading\n```rust\nlet x = 1;\n```\n";
    fs::write(meta.join("01_intro.md"), body).unwrap();

    let docs = scan_harness_docs(&cwd);
    assert_eq!(docs.get("01_intro").map(String::as_str), Some(body));
}

#[test]
fn meta_dir_with_md_extension_is_not_a_doc() {
    // 回归：cwd 下存在名为 "meta.md" 的普通文件，不影响 .peri/meta 扫描
    let tmp = TempDir::new().unwrap();
    let cwd = setup(&tmp);
    fs::write(PathBuf::from(&cwd).join("meta.md"), "not scanned").unwrap();
    let docs = scan_harness_docs(&cwd);
    assert!(docs.is_empty());
}
