//! M6：插件来源闭合位（`PluginSourceAdmission`）的准入测试。
//!
//! 目标不是「关闭后日志变安静」，而是两条加载路径都真的取不到插件来源：
//! - 聚合路径（技能 / agent roots、命令、hooks、`merge_plugin_mcp_servers`）；
//! - MCP 合并路径（`collect_plugin_mcp_servers`，见
//!   `mcp/config/snapshot_test.rs` 的同批用例）。
//!
//! 「不读插件目录」用**会被严格路径拒绝的目录**证明：关闭时该目录不会被解析。

use std::path::Path;

use super::*;
use crate::plugin::types::{InstallScope, InstalledPlugin, InstalledPlugins, PluginOrigin};
use crate::plugin::PluginSourceAdmission;

/// 安装并启用一个声明了 skills / commands / hooks / mcpServers 的插件。
fn install_plugin_with_every_face(claude_dir: &Path, mcp_servers: serde_json::Value) -> PathBuf {
    let plugin_dir = claude_dir.join("plugins/cache/market/sample/1.0.0");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(plugin_dir.join("commands")).unwrap();
    std::fs::create_dir_all(plugin_dir.join("skills/demo")).unwrap();
    std::fs::create_dir_all(plugin_dir.join("agents")).unwrap();
    std::fs::write(
        plugin_dir.join("commands/hello.md"),
        "---\ndescription: demo command\n---\nBody\n",
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: demo skill\n---\nBody\n",
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin/plugin.json"),
        serde_json::json!({
            "name": "sample",
            "version": "1.0.0",
            "skills": ["./skills"],
            "commands": ["./commands"],
            "agents": [{"path": "./agents", "name": "demo"}],
            "hooks": {"PreToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": "echo hi"}]}]},
            "mcpServers": mcp_servers,
        })
        .to_string(),
    )
    .unwrap();

    std::fs::create_dir_all(claude_dir.join("plugins")).unwrap();
    std::fs::write(
        claude_dir.join("plugins/installed_plugins.json"),
        serde_json::to_string(&InstalledPlugins {
            version: 2,
            plugins: vec![InstalledPlugin {
                id: "sample@market".into(),
                name: "sample".into(),
                version: "1.0.0".into(),
                marketplace: "market".into(),
                install_path: plugin_dir.clone(),
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            }],
        })
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"enabledPlugins":["sample@market"]}"#,
    )
    .unwrap();
    plugin_dir
}

/// 非法插件 MCP 声明（typed 校验失败：`system_mcp_tools` 缺 `system_mcp`）：
/// 严格路径（MCP 启动）必须失败，关闭位下连解析都不应发生。
fn invalid_mcp_servers() -> serde_json::Value {
    serde_json::json!({"broken": {"system_mcp_tools": ["Read"]}})
}

#[test]
#[serial_test::serial]
fn real_loader_preserves_hook_source_trust_for_shared_roots_in_both_orders() {
    struct TrustConfig;
    impl Drop for TrustConfig {
        fn drop(&mut self) {
            peri_config::io::set_global_config_path(None);
        }
    }
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    peri_config::io::set_global_config_path(Some(home.path().join("peri/settings.json")));
    let _trust_config = TrustConfig;
    install_plugin_with_every_face(home.path(), serde_json::json!({}));
    let path = home.path().join("plugins/installed_plugins.json");
    let mut installed: InstalledPlugins =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut project = installed.plugins[0].clone();
    project.scope = InstallScope::Project;
    project.project_path = Some(workspace.path().to_string_lossy().into_owned());
    installed.plugins.push(project);
    let cwd = workspace.path().to_str().unwrap();
    for reverse in [false, true] {
        if reverse {
            installed.plugins.reverse();
        }
        std::fs::write(&path, serde_json::to_vec(&installed).unwrap()).unwrap();
        let data =
            load_enabled_plugins_aggregated_readonly(home.path(), Some(workspace.path())).unwrap();
        assert_eq!(data.all_hooks.len(), 2);
        for hook in &data.all_hooks {
            assert!(data.plugins.iter().any(|plugin| {
                hook.plugin_source.as_ref() == Some(&plugin.scope)
                    && hook.plugin_root == plugin.install_path
            }));
        }
        let trusted = data
            .plugins
            .iter()
            .find(|plugin| plugin.scope.install_scope == InstallScope::Project)
            .unwrap();
        let binding = crate::host_ports::plugin_hook_binding(workspace.path(), trusted)
            .unwrap()
            .unwrap();
        peri_config::trust::grant(&binding).unwrap();
        let admitted =
            crate::host_ports::admit_plugin_hooks(cwd, &data.plugins, data.all_hooks.clone());
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].plugin_source.as_ref(), Some(&trusted.scope));
        let mut unknown = admitted[0].clone();
        unknown.plugin_source = None;
        assert!(
            crate::host_ports::admit_plugin_hooks(cwd, &data.plugins, vec![unknown]).is_empty()
        );
    }
}

#[test]
fn plugin_face_closed_only_tracks_the_plugin_middleware_key() {
    let mut disabled = std::collections::HashSet::new();
    assert!(!crate::assembly::plugin_face_closed(&disabled));
    assert_eq!(
        PluginSourceAdmission::from_disabled(&disabled),
        PluginSourceAdmission::Open
    );

    disabled.insert("SkillsMiddleware".to_string());
    assert!(
        !crate::assembly::plugin_face_closed(&disabled),
        "其它 middleware 关闭不得连带关闭插件来源"
    );

    disabled.insert(crate::assembly::PLUGIN_FACE_CLOSED_KEY.to_string());
    assert!(crate::assembly::plugin_face_closed(&disabled));
    assert_eq!(
        PluginSourceAdmission::from_disabled(&disabled),
        PluginSourceAdmission::Closed
    );
}

#[test]
fn meta_harness_flag_derives_the_same_closure_bit() {
    let closed = std::collections::HashMap::from([("PluginMiddleware".to_string(), false)]);
    assert_eq!(
        PluginSourceAdmission::from_meta_harness(Some(&closed)),
        PluginSourceAdmission::Closed
    );
    let open = std::collections::HashMap::from([("PluginMiddleware".to_string(), true)]);
    assert_eq!(
        PluginSourceAdmission::from_meta_harness(Some(&open)),
        PluginSourceAdmission::Open
    );
    assert_eq!(
        PluginSourceAdmission::from_meta_harness(None),
        PluginSourceAdmission::Open
    );
}

#[test]
fn open_admission_exposes_every_plugin_face() {
    let claude_dir = tempfile::tempdir().unwrap();
    let plugin_dir = install_plugin_with_every_face(
        claude_dir.path(),
        serde_json::json!({"srv": {"command": "run-srv"}}),
    );

    let data = PluginSourceAdmission::Open
        .load_aggregated_readonly(claude_dir.path(), None)
        .unwrap()
        .expect("open admission returns the aggregate");

    assert_eq!(data.plugins.len(), 1);
    assert_eq!(data.scope.len(), data.plugins.len());
    // 来源身份取自安装记录（id/origin/scope），不由名称猜。
    assert_eq!(data.scope[0].plugin_id, "sample@market");
    assert_eq!(
        data.scope[0].source_identity(),
        "plugin:peri-installed/user/sample@market"
    );
    assert!(!data.all_skill_roots.is_empty());
    assert!(!data.all_commands.is_empty());
    assert!(!data.all_hooks.is_empty());
    // 聚合路径的 MCP 合并面（`merge_plugin_mcp_servers`）在开启时确有内容。
    assert!(data.all_mcp_servers.contains_key("plugin:sample:srv"));
    assert_eq!(
        load_enabled_plugins_aggregated(claude_dir.path(), None).plugins[0].install_path,
        plugin_dir
    );
}

#[test]
fn closed_admission_never_reads_the_plugin_directory() {
    let claude_dir = tempfile::tempdir().unwrap();
    install_plugin_with_every_face(claude_dir.path(), invalid_mcp_servers());

    // 开启：宽容聚合仍返回内容（既有产品行为），但严格 MCP 路径必须失败。
    let error = PluginSourceAdmission::Open
        .load_for_mcp(claude_dir.path(), None)
        .expect_err("严格 MCP 路径不得降级为合法配置");
    // 失败发生在清单/MCP 解析层（`system_mcp_tools` 缺 `system_mcp` 是 typed
    // 解析期拒绝），关键事实是「严格路径失败」，不把它降级成空配置。
    assert!(
        matches!(
            error,
            LoaderError::McpConfigInvalid { .. } | LoaderError::ManifestLoadFailed(_)
        ),
        "unexpected loader error: {error}"
    );

    // 关闭：不读目录 ⇒ 既不返回聚合，也不触发严格路径的解析失败。
    assert!(PluginSourceAdmission::Closed
        .load_aggregated_readonly(claude_dir.path(), None)
        .unwrap()
        .is_none());
    let plugins = PluginSourceAdmission::Closed
        .load_for_mcp(claude_dir.path(), None)
        .unwrap();
    assert!(plugins.is_empty());
}

#[test]
fn closed_admission_zeroes_both_mcp_merge_paths() {
    let claude_dir = tempfile::tempdir().unwrap();
    install_plugin_with_every_face(
        claude_dir.path(),
        serde_json::json!({"srv": {"command": "run-srv"}}),
    );

    // 路径 A：聚合面（`merge_plugin_mcp_servers`）。
    let open_plugins = PluginSourceAdmission::Open
        .load_for_mcp(claude_dir.path(), None)
        .unwrap();
    assert!(merge_plugin_mcp_servers(&open_plugins).contains_key("plugin:sample:srv"));
    let closed_plugins = PluginSourceAdmission::Closed
        .load_for_mcp(claude_dir.path(), None)
        .unwrap();
    assert!(merge_plugin_mcp_servers(&closed_plugins).is_empty());

    // 路径 B：MCP 配置合并面（`collect_plugin_mcp_servers`）由准入后的同一份
    // 插件列表喂入（调用点见 `mcp/config.rs` 的 snapshot 与文件两条加载路径，
    // 回归在 `mcp/config/snapshot_test.rs`）；这里证明输入已被同一闭合位归零。
    let mut sources = std::collections::HashMap::new();
    let servers =
        crate::mcp::config::collect_plugin_mcp_servers_for_test(&closed_plugins, &mut sources);
    assert!(servers.is_empty());
    assert!(sources.is_empty());
}

#[test]
fn plugin_scope_distinguishes_user_install_from_project_declaration() {
    let claude_dir = tempfile::tempdir().unwrap();
    install_plugin_with_every_face(
        claude_dir.path(),
        serde_json::json!({"srv": {"command": "run-srv"}}),
    );
    let data = PluginSourceAdmission::Open
        .load_aggregated_readonly(claude_dir.path(), None)
        .unwrap()
        .unwrap();
    let user_scope = data.scope[0].clone();
    assert_eq!(user_scope.install_scope, InstallScope::User);
    assert_eq!(user_scope.project_path, None);

    let project_scope = peri_acp_types::plugin::PluginScope {
        plugin_id: user_scope.plugin_id.clone(),
        origin: user_scope.origin,
        install_scope: InstallScope::Project,
        project_path: Some("/work/demo".to_string()),
    };
    assert_ne!(
        user_scope.source_identity(),
        project_scope.source_identity(),
        "同名插件在不同范围声明必须是不同来源身份"
    );
}
