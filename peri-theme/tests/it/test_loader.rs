//! 公共 loader API 在独立 HOME 中验证，避免本机主题影响测试或污染环境。

use peri_theme::loader;
use peri_theme::loader::ThemeLoadError;
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
            .args([
                "--exact",
                "test_loader::public_loader_uses_isolated_home",
                "--nocapture",
            ])
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

    assert_eq!(loader::load_theme("peri-dark").unwrap().name, "peri-dark");
    assert_eq!(loader::load_theme("peri-light").unwrap().name, "peri-light");
    assert_eq!(
        loader::load_theme("dark").unwrap(),
        loader::load_theme("peri-dark").unwrap()
    );
    assert_eq!(
        loader::load_theme("light").unwrap(),
        loader::load_theme("peri-light").unwrap()
    );
    assert!(matches!(
        loader::load_theme("nonexistent"),
        Err(ThemeLoadError::ThemeNotFound(_))
    ));
    assert_eq!(
        loader::list_available_themes(),
        ["nord", "peri-dark", "peri-light"]
    );
    let nord = loader::load_theme("nord").unwrap();
    assert_eq!(nord.name, "nord");
    assert_eq!(nord.palette.accent.primary, Color::Rgb(136, 192, 208));
    assert_eq!(nord.semantic.accent, Color::Rgb(136, 192, 208));
    assert_eq!(nord.semantic.text.primary, Color::Rgb(216, 222, 233));
    assert_eq!(nord.palette.base.bg, Color::Rgb(46, 52, 64));
}
