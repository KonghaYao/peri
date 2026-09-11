//! 公共 loader API 在独立 HOME 中验证，避免本机主题影响测试或污染环境。

use peri_theme::loader::{ThemeLoadError, list_available_themes, load_theme};
use ratatui::style::Color;

#[test]
fn public_loader_uses_isolated_home() {
    const CHILD_MARKER: &str = "PERI_THEME_LOADER_TEST_CHILD";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let home = tempfile::tempdir().unwrap();
        let themes = home.path().join(".peri/themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(
            themes.join("nord.json"),
            serde_json::json!({
                "name": "nord", "extends": "peri-dark",
                "semantic": { "accent": "#88C0D0", "text": { "primary": "#D8DEE9" } },
                "palette.base.bg": "#2E3440"
            })
            .to_string(),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "public_loader_uses_isolated_home", "--nocapture"])
            .env(CHILD_MARKER, "1")
            .env("HOME", home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated loader tests failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    assert_eq!(load_theme("peri-dark").unwrap().name, "peri-dark");
    assert_eq!(load_theme("peri-light").unwrap().name, "peri-light");
    assert_eq!(
        load_theme("dark").unwrap(),
        load_theme("peri-dark").unwrap()
    );
    assert_eq!(
        load_theme("light").unwrap(),
        load_theme("peri-light").unwrap()
    );
    assert!(matches!(
        load_theme("nonexistent"),
        Err(ThemeLoadError::ThemeNotFound(_))
    ));
    assert_eq!(list_available_themes(), ["nord", "peri-dark", "peri-light"]);
    let nord = load_theme("nord").unwrap();
    assert_eq!(nord.name, "nord");
    assert_eq!(nord.palette.accent.primary, Color::Rgb(136, 192, 208));
    assert_eq!(nord.semantic.accent, Color::Rgb(136, 192, 208));
    assert_eq!(nord.semantic.text.primary, Color::Rgb(216, 222, 233));
    assert_eq!(nord.palette.base.bg, Color::Rgb(46, 52, 64));
}
