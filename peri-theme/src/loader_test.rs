//! Tests for loader_theme

use super::*;

#[test]
fn test_parse_hex() {
    assert_eq!(
        parse_hex_color("#FFFFFF").unwrap(),
        Color::Rgb(255, 255, 255)
    );
    assert_eq!(parse_hex_color("#000000").unwrap(), Color::Rgb(0, 0, 0));
    assert_eq!(
        parse_hex_color("#4EBA65").unwrap(),
        Color::Rgb(78, 186, 101)
    );
    // #RGB 简写
    assert_eq!(parse_hex_color("#FFF").unwrap(), Color::Rgb(255, 255, 255));
    assert_eq!(parse_hex_color("#000").unwrap(), Color::Rgb(0, 0, 0));
}

#[test]
fn test_parse_hex_invalid() {
    assert!(parse_hex_color("").is_err());
    assert!(parse_hex_color("not-a-color").is_err());
}

#[test]
fn test_resolve_refs_simple() {
    let mut flat = HashMap::new();
    flat.insert("palette.base.bg".to_string(), "$other.bg".to_string());
    flat.insert("other.bg".to_string(), "#000000".to_string());

    let resolved = resolve_refs(&flat, 0).unwrap();
    assert_eq!(resolved.get("palette.base.bg").unwrap(), "#000000");
    assert_eq!(resolved.get("other.bg").unwrap(), "#000000");
}

#[test]
fn test_resolve_refs_chain() {
    let mut flat = HashMap::new();
    flat.insert("a".to_string(), "$b".to_string());
    flat.insert("b".to_string(), "$c".to_string());
    flat.insert("c".to_string(), "#FFFFFF".to_string());

    let resolved = resolve_refs(&flat, 0).unwrap();
    assert_eq!(resolved.get("a").unwrap(), "#FFFFFF");
}

#[test]
fn test_resolve_refs_circular() {
    let mut flat = HashMap::new();
    flat.insert("a".to_string(), "$b".to_string());
    flat.insert("b".to_string(), "$a".to_string());

    let result = resolve_refs(&flat, 0);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ThemeLoadError::CircularRef(_)
    ));
}

#[test]
fn test_resolve_refs_max_depth() {
    let mut flat = HashMap::new();
    for i in 0..12 {
        let key = format!("k{i}");
        let next = format!("$k{}", i + 1);
        flat.insert(key, next);
    }
    flat.insert("k12".to_string(), "#000000".to_string());

    let result = resolve_refs(&flat, 0);
    assert!(result.is_err());
}
