use std::collections::HashMap;

use super::{classify_theme_catalog, refresh_theme_catalog_after_download};
use peri_theme::theme::ThemeMode;

/// [回归测试] 下载成功后必须以重新扫描到的 catalog 覆盖旧快照，并按最新列表重新分类，
/// 避免已打开的 ThemePanel 继续显示下载前的主题列表。
#[test]
fn downloaded_catalog_replaces_snapshot_and_reclassifies() {
    let mut catalog = vec!["stale-dark".to_string()];

    refresh_theme_catalog_after_download(&mut catalog, 1, || {
        vec![
            "downloaded-dark".to_string(),
            "downloaded-light".to_string(),
        ]
    });

    assert_eq!(
        catalog,
        vec!["downloaded-dark", "downloaded-light"],
        "下载后的 catalog 应覆盖旧快照"
    );

    let modes = HashMap::from([
        ("downloaded-dark", ThemeMode::Dark),
        ("downloaded-light", ThemeMode::Light),
    ]);
    let (dark, light) = classify_theme_catalog(&catalog, |name| modes.get(name).copied());

    assert_eq!(dark, vec!["downloaded-dark"], "新增深色主题应可见");
    assert_eq!(light, vec!["downloaded-light"], "新增浅色主题应可见");
}

/// [回归测试] 当本次下载没有成功写入时，不应刷新主题目录快照。
#[test]
fn failed_download_does_not_rescan_or_replace_catalog() {
    let mut catalog = vec!["existing-theme".to_string()];
    let mut scan_called = false;

    refresh_theme_catalog_after_download(&mut catalog, 0, || {
        scan_called = true;
        vec!["unexpected-theme".to_string()]
    });

    assert!(!scan_called, "没有成功写入时不应重新扫描主题目录");
    assert_eq!(catalog, vec!["existing-theme"]);
}
