//! 来源与继承契约：仅使用本测试创建的目录，绝不读取真实 HOME。

use std::path::Path;

use super::*;
use ratatui::style::Color;

fn write_theme(directory: &Path, name: &str, json: serde_json::Value) {
    std::fs::write(directory.join(format!("{name}.json")), json.to_string()).unwrap();
}

#[test]
fn inherits_before_resolving_references_and_preserves_child_identity() {
    let directory = tempfile::tempdir().unwrap();
    write_theme(
        directory.path(),
        "parent",
        serde_json::json!({
            "name": "Parent", "extends": "dark", "mode": "high-contrast",
            "semantic": { "text": { "primary": "#123456" } }
        }),
    );
    write_theme(
        directory.path(),
        "child",
        serde_json::json!({
            "extends": "parent", "semantic.text.primary": "#abcdef",
            "component": { "input": { "session_title_palette": ["#abc"] } }
        }),
    );
    let theme = ThemeSources::new(Some(directory.path().to_path_buf()))
        .load("child")
        .unwrap();
    assert_eq!(theme.name, "unnamed");
    assert_eq!(theme.mode, crate::theme::ThemeMode::HighContrast);
    assert_eq!(theme.semantic.text.primary, Color::Rgb(171, 205, 239));
    assert_eq!(theme.palette.base.fg, theme.semantic.text.primary);
    assert_eq!(theme.component.panel.title, theme.semantic.text.primary);
    assert_eq!(
        theme.component.input.session_title_palette[0],
        Color::Rgb(170, 187, 204)
    );
    assert_eq!(
        theme.component.input.session_title_palette[1],
        crate::builtin::dark_theme()
            .component
            .input
            .session_title_palette[1]
    );
}

#[test]
fn user_sources_override_builtins_for_root_and_parent_lookups() {
    let directory = tempfile::tempdir().unwrap();
    write_theme(
        directory.path(),
        "peri-dark",
        serde_json::json!({
            "name": "custom-dark", "extends": "light", "semantic": { "accent": "#123456" }
        }),
    );
    write_theme(
        directory.path(),
        "child",
        serde_json::json!({
            "name": "Child", "extends": "peri-dark"
        }),
    );
    let sources = ThemeSources::new(Some(directory.path().to_path_buf()));
    let custom = sources.load("peri-dark").unwrap();
    assert_eq!(custom.name, "custom-dark");
    assert_eq!(custom.mode, crate::theme::ThemeMode::Light);
    let child = sources.load("child").unwrap();
    assert_eq!(child.name, "Child");
    assert_eq!(child.semantic.accent, Color::Rgb(18, 52, 86));
    assert_eq!(child.mode, crate::theme::ThemeMode::Light);
    assert_eq!(sources.list(), ["child", "peri-dark", "peri-light"]);
}

#[test]
fn rejects_cyclic_missing_and_overdeep_parents() {
    let directory = tempfile::tempdir().unwrap();
    let sources = ThemeSources::new(Some(directory.path().to_path_buf()));
    for (name, parent) in [
        ("a", "b"),
        ("b", "a"),
        ("self", "self"),
        ("missing", "absent"),
    ] {
        write_theme(
            directory.path(),
            name,
            serde_json::json!({ "extends": parent }),
        );
    }
    for name in ["a", "self"] {
        assert!(matches!(
            sources.load(name),
            Err(ThemeLoadError::CircularRef(_))
        ));
    }
    assert!(
        matches!(sources.load("missing"), Err(ThemeLoadError::ThemeNotFound(name)) if name == "absent")
    );
    for index in 0..10 {
        let parent = if index == 9 {
            "dark".to_string()
        } else {
            format!("chain{}", index + 1)
        };
        write_theme(
            directory.path(),
            &format!("chain{index}"),
            serde_json::json!({ "extends": parent }),
        );
    }
    assert!(
        sources.load("chain0").is_ok(),
        "ten parent edges are allowed"
    );
    write_theme(
        directory.path(),
        "extra",
        serde_json::json!({ "extends": "chain0" }),
    );
    assert!(matches!(
        sources.load("extra"),
        Err(ThemeLoadError::CircularRef(_))
    ));
    // 失败的活动链不能污染后续加载。
    assert!(sources.load("chain0").is_ok());
}

#[test]
fn invalid_user_documents_do_not_fall_back_to_builtin() {
    let directory = tempfile::tempdir().unwrap();
    let sources = ThemeSources::new(Some(directory.path().to_path_buf()));
    std::fs::write(directory.path().join("peri-dark.json"), "not JSON").unwrap();
    assert!(matches!(
        sources.load("peri-dark"),
        Err(ThemeLoadError::ParseError(_))
    ));
    for invalid in [
        serde_json::json!([]),
        serde_json::json!({ "extends": null }),
        serde_json::json!({ "extends": 42 }),
    ] {
        write_theme(directory.path(), "invalid", invalid);
        assert!(matches!(
            sources.load("invalid"),
            Err(ThemeLoadError::ParseError(_))
        ));
    }
}

#[test]
fn rejects_paths_in_inherited_theme_names() {
    let directory = tempfile::tempdir().unwrap();
    let sources = ThemeSources::new(Some(directory.path().to_path_buf()));
    for name in [
        "",
        " ",
        ".",
        "..",
        "../outside",
        "/tmp/theme",
        "sub/theme",
        "sub\\theme",
        "C:theme",
    ] {
        write_theme(
            directory.path(),
            "child",
            serde_json::json!({ "extends": name }),
        );
        assert!(matches!(
            sources.load("child"),
            Err(ThemeLoadError::ParseError(_))
        ));
    }
}

#[test]
fn root_lookup_retains_relative_and_absolute_paths() {
    let directory = tempfile::tempdir().unwrap();
    let user_dir = directory.path().join("themes");
    std::fs::create_dir(&user_dir).unwrap();
    write_theme(
        directory.path(),
        "outside",
        serde_json::json!({ "name": "outside", "extends": "dark" }),
    );
    let sources = ThemeSources::new(Some(user_dir));
    assert_eq!(sources.load("../outside").unwrap().name, "outside");
    assert_eq!(
        sources
            .load(directory.path().join("outside").to_str().unwrap())
            .unwrap()
            .name,
        "outside"
    );
}

#[cfg(unix)]
#[test]
fn user_sources_retain_dotfiles_symlinks_for_root_and_parent() {
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write_theme(
        outside.path(),
        "dotfiles",
        serde_json::json!({ "name": "dotfiles", "extends": "light" }),
    );
    std::os::unix::fs::symlink(
        outside.path().join("dotfiles.json"),
        directory.path().join("linked.json"),
    )
    .unwrap();
    let sources = ThemeSources::new(Some(directory.path().to_path_buf()));
    assert_eq!(sources.load("linked").unwrap().name, "dotfiles");
    assert_eq!(sources.list(), ["linked", "peri-dark", "peri-light"]);
    write_theme(
        directory.path(),
        "child",
        serde_json::json!({ "extends": "linked" }),
    );
    assert_eq!(
        sources.load("child").unwrap().mode,
        crate::theme::ThemeMode::Light
    );
}
