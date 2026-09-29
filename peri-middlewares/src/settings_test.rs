//! F12 配置位读取测试（迁移自 `skills/mod_test.rs`：W4b 后本模块是宿主唯一的
//! 配置读取点，`peri-middlewares/src/skills/` 不再含任何 `std::fs` 调用）。

use tempfile::tempdir;

use super::load_disable_bundled_skills_from_path;

// ─── 配置位读取（F12 宿主适配器）───────────────────────────────────────────

#[test]
fn test_load_disable_bundled_skills_defaults_false_when_missing() {
    // settings.json 无 disableBundledSkills 字段时返回 false
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(&settings_path, r#"{"config": {"other": 1}}"#).unwrap();
    let value = load_disable_bundled_skills_from_path(&settings_path);
    assert!(!value);
}

#[test]
fn test_load_disable_bundled_skills_reads_true() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"config": {"disableBundledSkills": true}}"#,
    )
    .unwrap();
    let value = load_disable_bundled_skills_from_path(&settings_path);
    assert!(value, "disableBundledSkills=true 时应返回 true");
}

#[test]
fn test_load_disable_bundled_skills_reads_false_explicit() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"config": {"disableBundledSkills": false}}"#,
    )
    .unwrap();
    let value = load_disable_bundled_skills_from_path(&settings_path);
    assert!(!value, "显式 false 时应返回 false");
}

#[test]
fn test_load_disable_bundled_skills_handles_missing_file() {
    assert!(!load_disable_bundled_skills_from_path(
        std::path::Path::new("/nonexistent.json")
    ));
}

#[test]
fn test_load_disable_bundled_skills_reads_flat_true() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(&settings_path, r#"{"disableBundledSkills": true}"#).unwrap();
    let value = load_disable_bundled_skills_from_path(&settings_path);
    assert!(value, "扁平 JSON disableBundledSkills=true 时应返回 true");
}

#[test]
fn test_load_disable_bundled_skills_handles_malformed_json() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(&settings_path, "not json at all {{{").unwrap();
    let value = load_disable_bundled_skills_from_path(&settings_path);
    assert!(!value, "非法 JSON 时保守返回 false");
}
