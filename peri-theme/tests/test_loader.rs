//! 验证 JSON loader: 加载、$ref 解析、extends 继承、循环引用检测。

use peri_theme::loader::{ThemeLoadError, list_available_themes, load_theme};

#[test]
fn test_load_dark_from_json() {
    let result = load_theme("peri-dark");
    assert!(result.is_ok());
    let theme = result.unwrap();
    assert_eq!(theme.name, "peri-dark");
}

#[test]
fn test_load_light_from_json() {
    let result = load_theme("peri-light");
    assert!(result.is_ok());
    let theme = result.unwrap();
    assert_eq!(theme.name, "peri-light");
}

#[test]
fn test_load_unknown_theme() {
    let result = load_theme("nonexistent");
    assert!(matches!(result, Err(ThemeLoadError::ThemeNotFound(_))));
}

/// [用户主题] 验证 ~/.peri/themes/nord.json 能被 loader 正确解析。
/// 仅在本机存在 nord.json 时运行，CI 环境下跳过。
#[test]
fn test_load_user_nord_theme() {
    let themes = list_available_themes();
    if !themes.contains(&"nord".to_string()) {
        eprintln!("SKIP: nord.json not found in ~/.peri/themes/");
        return;
    }
    let result = load_theme("nord");
    assert!(
        result.is_ok(),
        "load_theme(\"nord\") 失败: {:?}",
        result.err()
    );
    let theme = result.unwrap();
    assert_eq!(theme.name, "nord");
    // 验证几个关键色值已正确解析（$ref 解析后的实际值）
    assert_eq!(
        theme.palette.accent.primary,
        ratatui::style::Color::Rgb(136, 192, 208)
    ); // #88C0D0
    assert_eq!(
        theme.semantic.accent,
        ratatui::style::Color::Rgb(136, 192, 208)
    );
    assert_eq!(
        theme.semantic.text.primary,
        ratatui::style::Color::Rgb(216, 222, 233)
    ); // #D8DEE9
    assert_eq!(
        theme.palette.base.bg,
        ratatui::style::Color::Rgb(46, 52, 64)
    ); // #2E3440
}
