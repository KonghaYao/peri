use std::io::Write;

use serial_test::serial;

use super::{load_from, save, save_to};
use crate::provider::config::PeriConfig;

/// 在临时目录创建 .peri/settings.json
fn write_settings(dir: &std::path::Path, content: &str) {
    let peri_dir = dir.join(".peri");
    std::fs::create_dir_all(&peri_dir).unwrap();
    let mut f = std::fs::File::create(peri_dir.join("settings.json")).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

/// RAII guard：测试结束时复位全局配置路径重定向，
/// 防止断言失败后残留全局态污染其他测试。
struct ConfigPathGuard;

impl Drop for ConfigPathGuard {
    fn drop(&mut self) {
        super::set_global_config_path(None);
    }
}

#[test]
fn test_load_global_only_no_workspace() {
    // load() 的合并行为依赖 std::env::current_dir()，
    // 在单元测试中 mock cwd 不实际。
    // 这里验证 load_from 行为不变。
    let cfg = load_from(&std::path::PathBuf::from("/nonexistent/path/settings.json")).unwrap();
    assert!(cfg.config.providers.is_empty());
}

#[test]
fn test_workspace_config_path_does_not_panic() {
    // workspace_config_path 依赖 current_dir，集成测试中不做断言
    // 只验证函数不 panic
    let _ = super::workspace_config_path();
}

#[test]
fn test_merge_global_and_workspace_via_load_from() {
    // 模拟全局 + 工作区双文件合并：
    // 全局配置有 provider，工作区只覆盖 active_alias
    let tmp = tempfile::tempdir().unwrap();
    let global_dir = tmp.path().join("global");
    let ws_dir = tmp.path().join("workspace");

    // 写全局配置
    let global_content = r#"{
        "config": {
            "active_alias": "sonnet",
            "active_provider_id": "openai-1",
            "providers": [{"id": "openai-1", "type": "openai", "apiKey": "sk-global"}]
        }
    }"#;
    write_settings(&global_dir, global_content);

    // 写工作区配置
    let ws_content = r#"{
        "config": {
            "active_alias": "haiku"
        }
    }"#;
    write_settings(&ws_dir, ws_content);

    // 加载全局
    let global_path = global_dir.join(".peri").join("settings.json");
    let mut global = load_from(&global_path).unwrap();

    // 加载工作区并合并
    let ws_path = ws_dir.join(".peri").join("settings.json");
    let workspace = load_from(&ws_path).unwrap();
    global.config.merge_overrides(workspace.config);

    // 验证工作区字段覆盖
    assert_eq!(global.config.active_alias, "haiku");
    // 全局字段保留（旧 active_provider_id 被 extra 吸收，不回写）
    assert!(global.config.extra.contains_key("active_provider_id"));
    assert_eq!(global.config.providers.len(), 1);
    assert_eq!(global.config.providers[0].api_key, "sk-global");
    // profiles 未被工作区定义 → 保留全局默认
    assert_eq!(global.config.profiles.sonnet.effort, "xhigh");
}

// ─── set_global_config_path 重定向（进程级全局态，全部 #[serial]）──────────

#[test]
#[serial]
fn test_set_global_config_path_none_keeps_default() {
    let _guard = ConfigPathGuard;
    super::set_global_config_path(None);
    let expected = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".peri")
        .join("settings.json");
    assert_eq!(super::config_path(), expected);
}

#[test]
#[serial]
fn test_redirect_config_path_and_save_roundtrip() {
    let _guard = ConfigPathGuard;
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("global").join("settings.json");
    // tempdir 路径本身是绝对路径，set 后不做相对路径解析
    super::set_global_config_path(Some(target.clone()));
    assert_eq!(super::config_path(), target);

    save(&PeriConfig::default()).unwrap();
    assert!(target.exists());
    // 写入内容必须是合法 JSON（save 内部 serde_json::to_string_pretty 已保证，
    // 此处防御性验证文件可解析）
    let content = std::fs::read_to_string(&target).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(parsed.is_object());
}

#[test]
#[serial]
fn test_redirect_load_reads_override_file() {
    let _guard = ConfigPathGuard;
    // 测试 cwd 是 peri-acp 包根，无 ./.peri/ 目录，
    // 工作区 merge 不介入，load() 只读重定向后的全局文件。
    let tmp = tempfile::tempdir().unwrap();
    let content = r#"{
        "config": {
            "active_alias": "sonnet",
            "providers": [{"id": "openai-1", "type": "openai", "apiKey": "sk-redirect"}]
        }
    }"#;
    write_settings(tmp.path(), content);
    let target = tmp.path().join(".peri").join("settings.json");
    super::set_global_config_path(Some(target.clone()));

    let cfg = super::load().unwrap();
    assert_eq!(cfg.config.active_alias, "sonnet");
    assert_eq!(cfg.config.providers.len(), 1);
    assert_eq!(cfg.config.providers[0].api_key, "sk-redirect");
}

#[test]
#[serial]
fn test_redirect_save_unwritable_errors() {
    let _guard = ConfigPathGuard;
    let tmp = tempfile::tempdir().unwrap();
    // 以普通文件为父目录，create_dir_all 必然失败
    let f = tmp.path().join("f");
    std::fs::write(&f, "not a dir").unwrap();
    let target = f.join("settings.json");
    super::set_global_config_path(Some(target.clone()));

    let result = save(&PeriConfig::default());
    assert!(result.is_err());
    assert!(!target.exists());
}

#[test]
#[serial]
fn test_redirect_absolutizes_relative_path() {
    let _guard = ConfigPathGuard;
    super::set_global_config_path(Some(std::path::PathBuf::from("settings.json")));
    let resolved = super::config_path();
    let cwd = std::env::current_dir().unwrap();
    assert!(resolved.is_absolute());
    assert_eq!(resolved, cwd.join("settings.json"));
}

#[test]
#[serial]
fn test_redirect_save_to_unaffected_by_override() {
    // save_to 显式指定路径，不经过 config_path()，
    // 重定向设置后行为不变（防御显式路径语义不被全局态污染）。
    let _guard = ConfigPathGuard;
    let tmp = tempfile::tempdir().unwrap();
    super::set_global_config_path(Some(tmp.path().join("override").join("settings.json")));
    let explicit = tmp.path().join("explicit").join("settings.json");
    save_to(&PeriConfig::default(), &explicit).unwrap();
    assert!(explicit.exists());
    assert!(!tmp.path().join("override").join("settings.json").exists());
}
