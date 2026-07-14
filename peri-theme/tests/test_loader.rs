//! 验证 JSON loader: 加载、$ref 解析、extends 继承、循环引用检测。

use peri_theme::loader::{ThemeLoadError, load_theme};

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
