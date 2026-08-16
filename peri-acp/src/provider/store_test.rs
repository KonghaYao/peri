use std::io::Write;

use serial_test::serial;

use super::{load_from, save_to, ConfigSource};
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
    // load_from 不存在的路径 → 默认空配置
    let cfg = load_from(&std::path::PathBuf::from("/nonexistent/path/settings.json")).unwrap();
    assert!(cfg.config.providers.is_empty());
}

#[test]
fn test_config_source_load_standalone_ignores_workspace() {
    // --settings 语义：指定文件整体生效，不探测工作区、不合并全局；
    // 写回仍写该文件（读写对称）。
    let tmp = tempfile::tempdir().unwrap();
    // 工作区文件存在但不应被加载
    write_settings(
        &tmp.path().join("ws"),
        r#"{"config": {"active_alias": "sonnet"}}"#,
    );

    let settings_file = tmp.path().join("standalone.json");
    std::fs::write(
        &settings_file,
        r#"{"config": {"active_alias": "haiku", "providers": [{"id": "p1", "type": "openai", "apiKey": "sk-1"}]}}"#,
    )
    .unwrap();

    let source = ConfigSource::load_standalone(settings_file.clone()).unwrap();
    assert!(!source.is_workspace());
    assert!(source.workspace_path().is_none());
    let merged = source.loaded_merged();
    assert_eq!(merged.config.active_alias, "haiku");
    assert_eq!(merged.config.providers.len(), 1);
    assert_eq!(merged.config.providers[0].api_key, "sk-1");

    // 写回仍写该文件（无工作区 → 全量快照落该文件）
    let mut updated = merged.clone();
    updated.config.active_alias = "opus".to_string();
    source.save(&updated).unwrap();
    let reloaded = load_from(&settings_file).unwrap();
    assert_eq!(reloaded.config.active_alias, "opus");
    assert_eq!(reloaded.config.providers.len(), 1);
}

#[test]
fn test_workspace_config_path_does_not_panic() {
    // workspace_config_path 依赖进程 cwd，仅验证不 panic（只读探测场景）
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

// ─── ConfigSource（值对象，无进程级全局态，可并行）─────────────────────────

/// 构造配置源：global 目录 + 可选 workspace 目录（均含 .peri/settings.json）
fn make_source(tmp: &tempfile::TempDir, ws_dir: Option<&str>) -> ConfigSource {
    let global_dir = tmp.path().join("global");
    let global_path = global_dir.join(".peri").join("settings.json");
    let cwd = ws_dir
        .map(|d| tmp.path().join(d))
        .unwrap_or_else(|| tmp.path().join("empty-cwd"));
    if ws_dir.is_none() {
        std::fs::create_dir_all(&cwd).unwrap();
    }
    ConfigSource::load_at(&cwd, global_path).unwrap()
}

/// [P0 契约] 加载与保存共享同一路径决策：`ConfigSource` 在加载时一次性确定
/// 布局，保存复用——工作区配置存在时写回工作区，全局文件保持原样。
///
/// providers 例外说明：merge/extract 均为**整体替换**语义（既定设计，用户
/// 确认保持）——工作区一旦声明 provider，即接管完整列表（含全局条目）。
#[test]
fn test_config_source_save_routes_to_workspace_layered() {
    let tmp = tempfile::tempdir().unwrap();
    // 全局：sonnet + openai-1（含 apiKey）
    write_settings(
        &tmp.path().join("global"),
        r#"{
        "config": {
            "active_alias": "sonnet",
            "providers": [{"id": "openai-1", "type": "openai", "apiKey": "sk-global"}]
        }
    }"#,
    );
    // 工作区：仅覆盖 active_alias
    write_settings(
        &tmp.path().join("ws"),
        r#"{"config": {"active_alias": "haiku"}}"#,
    );
    let source = make_source(&tmp, Some("ws"));
    assert!(source.is_workspace());
    let ws_path = tmp.path().join("ws").join(".peri").join("settings.json");
    let global_path = tmp
        .path()
        .join("global")
        .join(".peri")
        .join("settings.json");

    // 用户在 TUI 中把 active_alias 切回全局值 + 新增一个工作区 provider
    let mut merged = source.loaded_merged();
    merged.config.active_alias = "sonnet".to_string(); // 与全局相同 → 应剔除
    merged.config.providers.push(
        serde_json::from_value(serde_json::json!({
            "id": "ws-provider",
            "type": "openai",
            "apiKey": "sk-workspace"
        }))
        .unwrap(),
    );
    source.save(&merged).unwrap();

    // 工作区文件：active_alias 恒收录（分层豁免，值为保存时生效值）；
    // providers 整体接管（含全局条目）
    let ws_content = std::fs::read_to_string(&ws_path).unwrap();
    let ws_parsed: serde_json::Value = serde_json::from_str(&ws_content).unwrap();
    assert_eq!(
        ws_parsed["config"]["active_alias"], "sonnet",
        "active_alias 恒收录（豁免分层：缺省 opus 与未声明不可区分）"
    );
    let ws_providers = ws_parsed["config"]["providers"].as_array().unwrap();
    assert_eq!(
        ws_providers.len(),
        2,
        "providers 整体接管：含全局 + 新增条目"
    );
    assert_eq!(ws_providers[0]["id"], "openai-1");
    assert_eq!(ws_providers[1]["id"], "ws-provider");

    // 全局文件保持原样（未被动）
    let global_content = std::fs::read_to_string(&global_path).unwrap();
    assert!(
        global_content.contains("sk-global") && !global_content.contains("ws-provider"),
        "全局文件必须保持原样"
    );

    // 分层 roundtrip：重新加载合并结果应等于保存前的 merged
    let reloaded = ConfigSource::load_at(
        &tmp.path().join("ws"),
        tmp.path()
            .join("global")
            .join(".peri")
            .join("settings.json"),
    )
    .unwrap();
    assert_eq!(
        reloaded.loaded_merged(),
        merged,
        "extract 与 merge 应严格互逆"
    );
}

/// 无工作区配置时：save 写回全局文件（唯一事实源）
#[test]
fn test_config_source_save_writes_global_when_no_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    write_settings(
        &tmp.path().join("global"),
        r#"{"config": {"active_alias": "sonnet"}}"#,
    );
    let source = make_source(&tmp, None);
    assert!(!source.is_workspace());
    let global_path = tmp
        .path()
        .join("global")
        .join(".peri")
        .join("settings.json");

    let mut merged = source.loaded_merged();
    merged.config.active_alias = "haiku".to_string();
    source.save(&merged).unwrap();

    let content = std::fs::read_to_string(&global_path).unwrap();
    assert!(
        content.contains("\"haiku\""),
        "无工作区时保存应写回全局文件"
    );
}

/// [Q5 契约] 生产接线等价性：`ConfigSource::load_at` 与 `load()` 的合并语义
/// 一致——meta_harness 逐 key 合并（全局其余 key 保留、同 key 工作区覆盖）。
#[test]
fn test_config_source_load_merges_meta_harness_per_key() {
    let tmp = tempfile::tempdir().unwrap();
    write_settings(
        &tmp.path().join("global"),
        r#"{
        "config": {
            "meta_harness": {
                "01_intro": true,
                "WebMiddleware": false
            }
        }
    }"#,
    );
    write_settings(
        &tmp.path().join("ws"),
        r#"{
        "config": {
            "meta_harness": {
                "01_intro": false,
                "TerminalMiddleware": false
            }
        }
    }"#,
    );
    let source = make_source(&tmp, Some("ws"));

    let map = source
        .loaded_merged()
        .config
        .meta_harness
        .expect("merged 后存在");
    assert_eq!(map.get("01_intro"), Some(&false), "同 key：工作区覆盖全局");
    assert_eq!(
        map.get("TerminalMiddleware"),
        Some(&false),
        "新 key：追加保留"
    );
    assert_eq!(map.get("WebMiddleware"), Some(&false), "全局其余 key 保留");
    assert_eq!(
        source
            .global_config()
            .config
            .meta_harness
            .as_ref()
            .unwrap()
            .get("01_intro"),
        Some(&true),
        "原始全局基准不含工作区覆盖"
    );
}

/// 写回失败传播：目标路径不可写时 save 返回 Err
#[test]
fn test_config_source_save_unwritable_errors() {
    let tmp = tempfile::tempdir().unwrap();
    // 以普通文件为全局路径父目录，create_dir_all 必然失败
    let f = tmp.path().join("f");
    std::fs::write(&f, "not a dir").unwrap();
    let target = f.join("settings.json");
    let source = ConfigSource::load_at(&tmp.path().join("empty-cwd"), target.clone()).unwrap();

    let result = source.save(&PeriConfig::default());
    assert!(result.is_err());
    assert!(!target.exists());
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

/// 重定向 + ConfigSource 全链路：save 写回重定向后的全局路径
#[test]
#[serial]
fn test_redirect_config_path_and_save_roundtrip() {
    let _guard = ConfigPathGuard;
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("global").join("settings.json");
    // tempdir 路径本身是绝对路径，set 后不做相对路径解析
    super::set_global_config_path(Some(target.clone()));
    assert_eq!(super::config_path(), target);

    let source = ConfigSource::load().unwrap();
    source.save(&PeriConfig::default()).unwrap();
    assert!(target.exists());
    // 写入内容必须是合法 JSON（save_to 内部 serde_json::to_string_pretty 已保证）
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
