use std::io::Write;

use serde_json::Value;
use serial_test::serial;

use super::{load_from, save_to, ConfigSource};
use crate::app::PeriConfig;

fn revision(source: &ConfigSource) -> crate::ConfigurationRevision {
    source
        .snapshot()
        .map(|snapshot| snapshot.revision())
        .unwrap_or_else(|| {
            crate::ConfigurationSnapshot::resolve(
                crate::ConfigurationScope::new(
                    std::env::temp_dir(),
                    source.global_path().to_owned(),
                )
                .unwrap(),
                Default::default(),
            )
            .unwrap()
            .revision()
        })
}

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
    source.save(revision(&source), &updated).unwrap();
    let reloaded = load_from(&settings_file).unwrap();
    assert_eq!(reloaded.config.active_alias, "opus");
    assert_eq!(reloaded.config.providers.len(), 1);
}

#[test]
fn injected_settings_replace_file_inputs_and_cannot_be_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let global = tmp.path().join("settings.json");
    std::fs::write(&global, "invalid global JSON").unwrap();
    write_settings(tmp.path(), "invalid workspace JSON");
    let source = ConfigSource::load_injected_at(
        tmp.path(),
        global.clone(),
        r#"{"config":{"active_alias":"sonnet","providers":[{"id":"injected","type":"openai","apiKey":"test"}],"profiles":{"sonnet":{"provider":"injected","model":"test-model"}}}}"#.to_owned(),
    )
    .unwrap();
    assert_eq!(source.loaded_merged().config.active_alias, "sonnet");
    assert_eq!(source.loaded_merged().config.providers[0].id, "injected");
    assert!(source
        .save(revision(&source), &source.loaded_merged())
        .is_err());
    assert_eq!(
        std::fs::read_to_string(global).unwrap(),
        "invalid global JSON"
    );
}

#[test]
fn standalone_inline_settings_are_isolated_across_concurrent_loads() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_path_buf();
    let global = cwd.join("global.json");
    let project = cwd.join(".mcp.json");
    std::fs::write(&global, "invalid global JSON").unwrap();
    write_settings(&cwd, "invalid workspace JSON");
    std::fs::write(&project, "invalid project JSON").unwrap();

    std::thread::scope(|scope| {
        for (alias, provider_id) in [("sonnet", "first"), ("haiku", "second")] {
            let cwd = &cwd;
            let global = &global;
            scope.spawn(move || {
                let settings = format!(
                    r#"{{"config":{{"active_alias":"{alias}","providers":[{{"id":"{provider_id}","type":"openai","apiKey":"test"}}]}}}}"#
                );
                let source = ConfigSource::load_standalone_inline_at(
                    cwd,
                    global.clone(),
                    settings,
                )
                .unwrap();
                assert_eq!(source.loaded_merged().config.active_alias, alias);
                assert_eq!(source.loaded_merged().config.providers[0].id, provider_id);
                assert!(source.snapshot().unwrap().inputs().project.is_none());
                assert!(source
                    .save(revision(&source), &source.loaded_merged())
                    .is_err());
            });
        }
    });

    assert_eq!(
        std::fs::read_to_string(global).unwrap(),
        "invalid global JSON"
    );
    assert_eq!(
        std::fs::read_to_string(project).unwrap(),
        "invalid project JSON"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join(".peri/settings.json")).unwrap(),
        "invalid workspace JSON"
    );
    assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 3);
}

#[test]
fn standalone_inline_invalid_settings_do_not_write_files() {
    let tmp = tempfile::tempdir().unwrap();
    let global = tmp.path().join("global.json");
    assert!(
        ConfigSource::load_standalone_inline_at(tmp.path(), global, "not JSON".to_owned(),)
            .is_err()
    );
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
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
    source.save(revision(&source), &merged).unwrap();

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
    source.save(revision(&source), &merged).unwrap();

    let content = std::fs::read_to_string(&global_path).unwrap();
    assert!(
        content.contains("\"haiku\""),
        "无工作区时保存应写回全局文件"
    );
}

/// [回归] cwd 与全局配置目录同一（用户在 `~` 下启动 peri）：`{cwd}/.peri/settings.json`
/// 与 `~/.peri/settings.json` 是同一个文件，不能同时充当「全局」与「工作区」两层。
///
/// 此前该场景被判为工作区模式，保存只写「相对全局基准的差异字段」回同一文件，
/// 未改动的字段（providers/apiKey 等）被整份丢弃。
#[test]
fn test_save_in_home_cwd_keeps_global_config_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    write_settings(
        &home,
        r#"{
        "config": {
            "active_alias": "sonnet",
            "providers": [{"id": "openai-1", "type": "openai", "apiKey": "sk-global"}]
        }
    }"#,
    );
    let global_path = home.join(".peri").join("settings.json");

    let source = ConfigSource::load_at(&home, global_path.clone()).unwrap();
    assert!(
        !source.is_workspace(),
        "工作区路径与全局路径同一文件时不得判为工作区模式"
    );

    let mut merged = source.loaded_merged();
    merged.config.active_alias = "haiku".to_string();
    source.save(revision(&source), &merged).unwrap();

    let saved = load_from(&global_path).unwrap();
    assert_eq!(saved.config.active_alias, "haiku", "改动字段应落盘");
    assert_eq!(
        saved.config.providers.len(),
        1,
        "未改动的 providers 不得被差异回写丢弃"
    );
    assert_eq!(saved.config.providers[0].api_key, "sk-global");
}

/// [回归] 同一文件经符号链接抵达（macOS `$HOME` 为链接、`/var` → `/private/var`
/// 等）时同样不得判为两层——判定按规范化真实路径，不按字面路径。
#[cfg(unix)]
#[test]
fn test_symlinked_global_path_is_not_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    write_settings(
        &home,
        r#"{"config": {"active_alias": "sonnet", "providers": [{"id": "p1", "type": "openai", "apiKey": "sk-global"}]}}"#,
    );
    let link = tmp.path().join("home-link");
    std::os::unix::fs::symlink(&home, &link).unwrap();

    // 全局路径经链接指向 `{cwd}/.peri/settings.json` 本身
    let source = ConfigSource::load_at(&home, link.join(".peri").join("settings.json")).unwrap();
    assert!(
        !source.is_workspace(),
        "符号链接指向同一文件时不得判为工作区两层"
    );

    let mut merged = source.loaded_merged();
    merged.config.active_alias = "haiku".to_string();
    source.save(revision(&source), &merged).unwrap();

    let saved = load_from(&home.join(".peri").join("settings.json")).unwrap();
    assert_eq!(saved.config.active_alias, "haiku");
    assert_eq!(saved.config.providers.len(), 1, "未改动字段不得被丢弃");
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
                "WorkspaceMiddleware": false
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
        map.get("WorkspaceMiddleware"),
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
    let f = tmp.path().join("f");
    let target = f.join("settings.json");
    let source = ConfigSource::load_at(&tmp.path().join("empty-cwd"), target.clone()).unwrap();
    std::fs::write(&f, "not a dir").unwrap();

    let result = source.save(revision(&source), &PeriConfig::default());
    assert!(result.is_err());
    assert!(!target.exists());
}

// ─── set_global_config_path 重定向（进程级全局态，全部 #[serial]）──────────

#[test]
#[serial]
fn test_set_global_config_path_none_keeps_default() {
    let _guard = ConfigPathGuard;
    super::set_global_config_path(None);
    let expected = peri_mcp_config::global_config_path();
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
    source
        .save(revision(&source), &PeriConfig::default())
        .unwrap();
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

#[test]
fn config_source_rejects_corrupt_workspace_even_without_global_file() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("workspace");
    write_settings(&cwd, "{preserve-broken-original");
    let path = cwd.join(".peri/settings.json");
    let global = tmp.path().join("missing/settings.json");
    let source = ConfigSource::load_at_lenient(&cwd, global.clone());
    assert!(source.reload_merged().is_err());
    assert!(source
        .save(revision(&source), &PeriConfig::default())
        .is_err());
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "{preserve-broken-original"
    );
    assert!(!global.exists());
}

#[test]
fn config_source_reload_preserves_current_workspace_fields_and_chosen_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("workspace");
    write_settings(&cwd, r#"{"config":{"language":"en"}}"#);
    let source = ConfigSource::load_at(&cwd, tmp.path().join("global.json")).unwrap();
    write_settings(
        &cwd,
        r#"{"$schema":"local-schema","config":{"language":"zh-CN","custom-setting":42}}"#,
    );
    let mut current = source.reload_merged().unwrap();
    assert_eq!(current.config.language.as_deref(), Some("zh-CN"));
    assert_eq!(current.config.extra["custom-setting"], 42);
    current.config.active_alias = "sonnet".into();
    source.save(revision(&source), &current).unwrap();
    let stored = load_from(source.workspace_path().unwrap()).unwrap();
    assert_eq!(stored.schema.as_deref(), Some("local-schema"));
    assert_eq!(stored.config.extra["custom-setting"], 42);
    assert_eq!(stored.config.active_alias, "sonnet");
}

#[test]
fn config_source_refuses_stale_global_baseline_without_copying_credentials() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("workspace");
    let global = tmp.path().join("global.json");
    std::fs::write(
        &global,
        r#"{"config":{"providers":[{"id":"p","type":"openai","apiKey":"old-key"}]}}"#,
    )
    .unwrap();
    write_settings(&cwd, r#"{"config":{}}"#);
    let source = ConfigSource::load_at(&cwd, global.clone()).unwrap();
    let old_workspace = std::fs::read(source.workspace_path().unwrap()).unwrap();
    std::fs::write(
        &global,
        r#"{"config":{"providers":[{"id":"p","type":"openai","apiKey":"new-key"}]}}"#,
    )
    .unwrap();
    let mut stale = source.loaded_merged();
    stale.config.language = Some("zh-CN".into());
    assert!(source.save(revision(&source), &stale).is_err());
    assert!(source
        .save(revision(&source), &source.reload_merged().unwrap())
        .is_err());
    assert_eq!(
        std::fs::read(source.workspace_path().unwrap()).unwrap(),
        old_workspace
    );
}

#[test]
#[serial]
fn config_path_uses_shared_data_plane_authority() {
    let _guard = ConfigPathGuard;
    let tmp = tempfile::tempdir().unwrap();
    let shared_path = tmp.path().join("shared.json");
    peri_mcp_config::set_global_config_path(Some(shared_path.clone()));
    assert_eq!(super::config_path(), shared_path);

    let acp_path = tmp.path().join("acp.json");
    super::set_global_config_path(Some(acp_path.clone()));
    assert_eq!(peri_mcp_config::global_config_path(), acp_path);
}

#[test]
fn config_io_works_without_workspace_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nested/settings.json");
    let mut config = PeriConfig::default();
    config.config.active_alias = "sonnet".into();
    save_to(&config, &path).unwrap();
    assert_eq!(load_from(&path).unwrap(), config);
}

#[test]
fn load_from_directory_propagates_read_failure() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(load_from(tmp.path()).is_err());
}

#[cfg(unix)]
#[test]
fn load_from_symlink_loop_propagates_probe_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    std::os::unix::fs::symlink(&path, &path).unwrap();
    assert!(load_from(&path).is_err());
}

#[cfg(unix)]
#[test]
fn workspace_probe_failure_prevents_lenient_source_from_writing_global() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let peri_dir = cwd.join(".peri");
    std::os::unix::fs::symlink(&peri_dir, &peri_dir).unwrap();
    let global_path = tmp.path().join("global.json");
    let original = r#"{"config":{"active_alias":"sonnet","disableBundledSkills":true}}"#;
    std::fs::write(&global_path, original).unwrap();

    assert!(ConfigSource::load_at(&cwd, global_path.clone()).is_err());
    let source = ConfigSource::load_at_lenient(&cwd, global_path.clone());
    assert_eq!(source.loaded_merged().config.active_alias, "sonnet");
    assert!(source.snapshot().is_none());
    assert!(
        source
            .resource_configuration()
            .unwrap()
            .disable_bundled_skills
    );
    assert!(source.reload_merged().is_err());
    assert!(source
        .save(revision(&source), &PeriConfig::default())
        .is_err());
    assert_eq!(std::fs::read_to_string(global_path).unwrap(), original);
}

/// fail-closed：**没有已发布快照**的 lenient 源不投影任何 flag。
///
/// 构造：工作区探测失败（符号链接自环）⇒ `load_at` 失败 ⇒ `load_at_lenient` 返回
/// `authority: None` 的降级源；global 文件本身含 `config.betas` 覆盖。内存草稿视图
/// 仍带该覆盖（前置断言证明本用例有判别力），但 `beta_flags()` 必须返回空投影——
/// 设计「快照缺失、未覆盖与未知 id 一律按 false，配置面不可用不得导致能力意外开启」
/// 的字面口径。
#[cfg(unix)]
#[test]
fn lenient_source_without_snapshot_yields_empty_beta_flags() {
    use peri_acp_types::beta_flags::FULL_ASYNC_TOOLS;

    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let peri_dir = cwd.join(".peri");
    std::os::unix::fs::symlink(&peri_dir, &peri_dir).unwrap();
    let global_path = tmp.path().join("global.json");
    std::fs::write(
        &global_path,
        format!(
            r#"{{"config":{{"active_alias":"sonnet","betas":{{"{FULL_ASYNC_TOOLS}":true}}}}}}"#
        ),
    )
    .unwrap();

    assert!(ConfigSource::load_at(&cwd, global_path.clone()).is_err());
    let source = ConfigSource::load_at_lenient(&cwd, global_path);
    assert!(
        source.snapshot().is_none(),
        "前置：lenient 源没有已发布快照"
    );
    assert_eq!(
        source.loaded_merged().config.betas.get(FULL_ASYNC_TOOLS),
        Some(true),
        "前置：内存草稿视图确实带覆盖（否则本用例没有判别力）"
    );

    let flags = source.beta_flags();
    assert!(
        flags.is_empty(),
        "无已发布快照必须返回空投影（fail-closed，不按草稿视图回升）：{flags:?}"
    );
    assert!(!flags.is_enabled(FULL_ASYNC_TOOLS));
}

#[cfg(unix)]
#[test]
fn lenient_source_without_selected_policy_does_not_use_default_global_config() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let peri_dir = cwd.join(".peri");
    std::os::unix::fs::symlink(&peri_dir, &peri_dir).unwrap();

    let source = ConfigSource::load_at_lenient(&cwd, tmp.path().join("missing.json"));
    assert!(source.snapshot().is_none());
    assert!(source.resource_configuration().is_none());
}

#[cfg(unix)]
#[test]
fn same_file_probe_failure_prevents_lenient_source_from_writing_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("workspace");
    let original = r#"{"config":{"active_alias":"sonnet"}}"#;
    write_settings(&cwd, original);
    let global_path = tmp.path().join("global.json");
    std::os::unix::fs::symlink(&global_path, &global_path).unwrap();

    assert!(ConfigSource::load_at(&cwd, global_path.clone()).is_err());
    let source = ConfigSource::load_at_lenient(&cwd, global_path);
    assert!(source.reload_merged().is_err());
    assert!(source
        .save(revision(&source), &PeriConfig::default())
        .is_err());
    assert_eq!(
        std::fs::read_to_string(cwd.join(".peri/settings.json")).unwrap(),
        original
    );
}

#[test]
fn config_source_exposes_raw_layers_and_preserves_top_level_siblings() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("workspace");
    let global_path = tmp.path().join("global.json");
    std::fs::write(
        &global_path,
        r#"{"config":{"active_alias":"opus"},"langfuse":{"enabled":true}}"#,
    )
    .unwrap();
    write_settings(
        &cwd,
        r#"{"config":{"language":"en"},"mcpServers":{"local":{"command":"tool"}}}"#,
    );

    let source = ConfigSource::load_at(&cwd, global_path.clone()).unwrap();
    assert_eq!(source.cwd(), cwd);
    assert!(source.raw_global().unwrap().contains("langfuse"));
    assert!(source.raw_workspace().unwrap().contains("mcpServers"));
    let mut merged = source.loaded_merged();
    merged.config.language = Some("zh-CN".into());
    source.save(revision(&source), &merged).unwrap();

    let global: Value =
        serde_json::from_str(&std::fs::read_to_string(global_path).unwrap()).unwrap();
    let workspace: Value =
        serde_json::from_str(&std::fs::read_to_string(source.workspace_path().unwrap()).unwrap())
            .unwrap();
    assert_eq!(global["langfuse"]["enabled"], true);
    assert_eq!(workspace["mcpServers"]["local"]["command"], "tool");
    assert_eq!(workspace["config"]["language"], "zh-CN");
}

#[test]
fn save_to_preserves_unknown_top_level_siblings() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    std::fs::write(&path, r#"{"config":{},"mcpServers":{"x":{}},"other":17}"#).unwrap();
    let mut config = PeriConfig::default();
    config.config.active_alias = "sonnet".into();

    save_to(&config, &path).unwrap();

    let saved: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(saved["mcpServers"]["x"], serde_json::json!({}));
    assert_eq!(saved["other"], 17);
    assert_eq!(saved["config"]["active_alias"], "sonnet");
}
