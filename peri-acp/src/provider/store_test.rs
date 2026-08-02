use std::io::Write;

use super::load_from;

/// 在临时目录创建 .peri/settings.json
fn write_settings(dir: &std::path::Path, content: &str) {
    let peri_dir = dir.join(".peri");
    std::fs::create_dir_all(&peri_dir).unwrap();
    let mut f = std::fs::File::create(peri_dir.join("settings.json")).unwrap();
    f.write_all(content.as_bytes()).unwrap();
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
