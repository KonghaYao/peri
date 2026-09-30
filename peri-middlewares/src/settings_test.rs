//! F12 配置位读取测试，配置正文与全局路径由配置数据面提供。

use tempfile::tempdir;

use super::load_disable_bundled_skills_from_path;

struct GlobalConfigGuard(std::path::PathBuf);

impl Drop for GlobalConfigGuard {
    fn drop(&mut self) {
        peri_mcp_config::set_global_config_path(Some(self.0.clone()));
    }
}

/// [回归测试] ACP 指定的配置路径必须同时控制内置技能关闭位。
#[test]
#[serial_test::serial]
fn test_disable_bundled_skills_uses_shared_global_config_path() {
    let _lock = peri_mcp_common::process_env::lock().expect("process env lock");
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("custom-config.json");
    std::fs::write(&settings_path, r#"{"disableBundledSkills":true}"#).unwrap();
    let _guard = GlobalConfigGuard(peri_mcp_config::global_config_path());
    peri_mcp_config::set_global_config_path(Some(settings_path.clone()));

    assert_eq!(super::global_config_path(), settings_path);
    assert!(super::load_disable_bundled_skills());
}

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
    let dir = tempdir().unwrap();
    assert!(!load_disable_bundled_skills_from_path(
        &dir.path().join("missing.json")
    ));
}

#[test]
fn test_load_disable_bundled_skills_handles_read_failure() {
    let dir = tempdir().unwrap();
    assert!(!load_disable_bundled_skills_from_path(dir.path()));
}

#[test]
fn test_nested_disable_bundled_skills_takes_precedence() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"config":{"disableBundledSkills":false},"disableBundledSkills":true}"#,
    )
    .unwrap();
    assert!(!load_disable_bundled_skills_from_path(&settings_path));
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
