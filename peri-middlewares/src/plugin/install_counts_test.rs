//! Tests for install_counts

use super::*;

#[test]
fn test_format_install_count() {
    assert_eq!(format_install_count(0), "0");
    assert_eq!(format_install_count(42), "42");
    assert_eq!(format_install_count(999), "999");
    assert_eq!(format_install_count(1000), "1K");
    assert_eq!(format_install_count(1200), "1.2K");
    assert_eq!(format_install_count(36200), "36.2K");
    assert_eq!(format_install_count(998_999), "999K");
    assert_eq!(format_install_count(999_999), "1M");
    assert_eq!(format_install_count(1_000_000), "1M");
    assert_eq!(format_install_count(1_500_000), "1.5M");
}

#[test]
fn test_load_claude_code_cache_format() {
    let json = r#"{
        "version": 1,
        "fetchedAt": "2026-05-06T06:12:56.730Z",
        "counts": [
            {"plugin": "frontend-design@claude-plugins-official", "unique_installs": 662133},
            {"plugin": "superpowers@claude-plugins-official", "unique_installs": 579903}
        ]
    }"#;

    let dir = std::env::temp_dir().join("peri_test_counts");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(CACHE_FILE);
    std::fs::write(&path, json).unwrap();

    // 直接解析测试
    let cache: CacheFormat = serde_json::from_str(json).unwrap();
    match cache {
        CacheFormat::ClaudeCode { counts, .. } => {
            assert_eq!(counts.len(), 2);
            assert_eq!(counts[0].plugin, "frontend-design@claude-plugins-official");
            assert_eq!(counts[0].unique_installs, 662133);
        }
    }

    // 清理
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}
