//! Tests for loader_theme

use super::*;
use crate::bridge::ThemeDefinitionExt;

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

// ── 消息流语义键（spec §4 表）：缺省兼容 + 默认值 ─────────────────────────

/// 从内置 dark.json 删除消息流新语义键，模拟旧版主题 JSON。
fn dark_json_without_new_keys() -> String {
    let json_str = include_str!("../themes/dark.json");
    let mut value: serde_json::Value = serde_json::from_str(json_str).expect("dark.json valid");
    let semantic = value["semantic"]
        .as_object_mut()
        .expect("dark.json semantic is object");
    semantic.remove("accents");
    semantic.remove("syntax");
    semantic["text"]
        .as_object_mut()
        .expect("semantic.text is object")
        .remove("secondary");
    semantic["surface"]
        .as_object_mut()
        .expect("semantic.surface is object")
        .remove("raised");
    semantic["surface"]
        .as_object_mut()
        .expect("semantic.surface is object")
        .remove("sunken");
    serde_json::to_string_pretty(&value).expect("serialize back")
}

#[test]
fn test_old_theme_json_without_new_keys_still_loads() {
    // 旧版用户主题（无 accents/syntax/secondary/raised/sunken 键）必须可加载，
    // 新字段回退内置 dark 默认值，旧字段值不受影响。
    let old_json = dark_json_without_new_keys();
    let theme = parse_theme_json(&old_json).expect("旧 JSON 无新键仍应可加载");

    let s = theme.semantic;
    // 新字段全部回退 §4 表 dark 默认值
    assert_eq!(
        s.accents.primary,
        Color::Rgb(215, 119, 87),
        "primary 与旧 accent 同值"
    );
    assert_eq!(s.accents.user, Color::Rgb(122, 162, 247));
    assert_eq!(s.accents.assistant, Color::Rgb(187, 154, 247));
    assert_eq!(s.accents.reasoning, Color::Rgb(84, 92, 126));
    assert_eq!(s.accents.tool, Color::Rgb(115, 122, 162));
    assert_eq!(s.syntax.command, Color::Rgb(224, 175, 104));
    assert_eq!(s.syntax.path, Color::Rgb(255, 158, 100));
    assert_eq!(s.text.secondary, Color::Rgb(169, 177, 214));
    assert_eq!(s.surface.raised, Color::Rgb(41, 46, 66));
    assert_eq!(s.surface.sunken, Color::Rgb(26, 27, 38));
    // 旧字段值保持不变
    assert_eq!(s.accent, Color::Rgb(215, 119, 87));
    assert_eq!(s.text.primary, Color::Rgb(255, 255, 255));
    assert_eq!(s.status.success, Color::Rgb(78, 186, 101));
    assert_eq!(s.surface.default, Color::Rgb(0, 0, 0));
}

#[test]
fn test_dark_theme_defaults_match_spec_table() {
    // §4 表逐项断言（dark：Tokyo Night 方向）
    let s = crate::builtin::dark_theme().semantic;

    // 消息流角色强调色
    assert_eq!(
        s.accents.primary, s.accent,
        "accents.primary 与旧 accent 同值同源"
    );
    assert_eq!(s.accents.user, Color::Rgb(122, 162, 247)); // #7AA2F7
    assert_eq!(s.accents.assistant, Color::Rgb(187, 154, 247)); // #BB9AF7
    assert_eq!(s.accents.reasoning, Color::Rgb(84, 92, 126)); // #545C7E
    assert_eq!(s.accents.tool, Color::Rgb(115, 122, 162)); // #737AA2
    // 文字层级
    assert_eq!(s.text.secondary, Color::Rgb(169, 177, 214)); // #A9B1D6
    // 表面
    assert_eq!(s.surface.raised, Color::Rgb(41, 46, 66)); // #292E42
    assert_eq!(s.surface.sunken, Color::Rgb(26, 27, 38)); // #1A1B26
    // 语法语义
    assert_eq!(s.syntax.command, Color::Rgb(224, 175, 104)); // #E0AF68
    assert_eq!(s.syntax.path, Color::Rgb(255, 158, 100)); // #FF9E64
    // 状态（running 按 §4 表更新为 #7DCFFF）
    assert_eq!(s.status.running, Color::Rgb(125, 207, 255)); // #7DCFFF
    assert_eq!(s.status.success, Color::Rgb(78, 186, 101)); // #4EBA65
    assert_eq!(s.status.error, Color::Rgb(255, 107, 128)); // #FF6B80
}

#[test]
fn test_dark_json_loads_new_keys() {
    // 内置 dark.json 携带新键 → 解析后新键值 = JSON 值（非回退）。
    // 注意：不用 load_theme——用户目录 ~/.peri/themes/peri-dark.json 若存在会
    // 优先加载（loader 既有行为），测试必须直测内置 JSON。
    let json_str = include_str!("../themes/dark.json");
    let theme = parse_theme_json(json_str).expect("dark.json parses");
    let s = theme.semantic;
    assert_eq!(s.accents.user, Color::Rgb(122, 162, 247));
    assert_eq!(
        s.accents.primary, s.accent,
        "JSON 中 accents.primary 引用 $semantic.accent"
    );
    assert_eq!(s.syntax.command, Color::Rgb(224, 175, 104));
    assert_eq!(s.text.secondary, Color::Rgb(169, 177, 214));
    assert_eq!(s.surface.raised, Color::Rgb(41, 46, 66));
    assert_eq!(s.surface.sunken, Color::Rgb(26, 27, 38));
    assert_eq!(s.status.running, Color::Rgb(125, 207, 255));
}

#[test]
fn test_light_theme_new_keys_are_readable_equivalents() {
    let s = crate::builtin::light_theme().semantic;
    // light 可读等值：非 Reset、非黑（浅底可读）
    for c in [
        s.accents.user,
        s.accents.assistant,
        s.accents.reasoning,
        s.accents.tool,
        s.text.secondary,
        s.surface.raised,
        s.surface.sunken,
        s.syntax.command,
        s.syntax.path,
        s.status.running,
    ] {
        assert_ne!(c, Color::Reset, "light 新语义键不得为 Reset");
    }
    assert_eq!(s.accents.primary, s.accent);
    assert_eq!(s.status.running, Color::Rgb(30, 136, 200)); // #1E88C8
}

#[test]
fn test_to_peri_colors_maps_new_semantic_fields() {
    // bridge 映射：to_peri_colors 覆盖全部新语义键；to_palette 未扩展（Palette 冻结）
    let theme = crate::builtin::dark_theme();
    let pc = theme.to_peri_colors();
    let s = theme.semantic;
    assert_eq!(pc.accent_user, s.accents.user);
    assert_eq!(pc.accent_assistant, s.accents.assistant);
    assert_eq!(pc.accent_reasoning, s.accents.reasoning);
    assert_eq!(pc.accent_tool, s.accents.tool);
    assert_eq!(pc.text_secondary, s.text.secondary);
    assert_eq!(pc.syntax_command, s.syntax.command);
    assert_eq!(pc.syntax_path, s.syntax.path);
    assert_eq!(pc.surface_raised, s.surface.raised);
    assert_eq!(pc.surface_sunken, s.surface.sunken);
    assert_eq!(pc.status_running, s.status.running);
}

#[test]
fn test_peri_colors_default_all_reset() {
    // 新字段的 Default 必须为 Reset（与既有字段一致）
    let pc = crate::peri_colors::PeriColors::default();
    assert_eq!(pc.accent_user, Color::Reset);
    assert_eq!(pc.accent_assistant, Color::Reset);
    assert_eq!(pc.accent_reasoning, Color::Reset);
    assert_eq!(pc.accent_tool, Color::Reset);
    assert_eq!(pc.text_secondary, Color::Reset);
    assert_eq!(pc.syntax_command, Color::Reset);
    assert_eq!(pc.syntax_path, Color::Reset);
    assert_eq!(pc.surface_raised, Color::Reset);
    assert_eq!(pc.surface_sunken, Color::Reset);
}
