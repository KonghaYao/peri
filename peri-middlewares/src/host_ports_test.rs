//! `bind_agent_catalog_from_pool` 的定点绑定回归（P0：冻结渲染早于 turn 级 bind）。
//!
//! 正例走**真实线路夹具**（`AgentFaceFixture`：生产 handler + 生产池句柄），断言
//! 绑定后端口目录非空——即会话创建期的冻结渲染确实能拿到候选；同时钉住关闭集
//! 派生的本地面关闭位（与 turn 级 bind 同形）。反例只钉「不适用 ⇒ false 且不
//! panic」，不需要线路。

use std::collections::HashSet;
use std::sync::Arc;

use peri_acp_types::hooks::SettingsHooksPort;
use peri_acp_types::ports::{AgentCatalogPort, McpPoolPort};

use super::{
    bind_agent_catalog_from_pool, AgentCatalogProvider, NoopAgentCatalog, SettingsHooksLoader,
};
use crate::mcp::agent_face_fixture::AgentFaceFixture;

/// 正例：池与端口都是本 crate 实现 ⇒ 绑定成立，且绑定后目录非空。
///
/// 同一条线路做**差分对照**：只差关闭集是否命中 `SubAgentMiddleware`——命中 ⇒
/// 本地面关闭 ⇒ 空目录（证明关闭位随本函数进入 registry，不是恒 false/恒 true）。
#[tokio::test]
async fn bind_agent_catalog_from_pool_exposes_catalog_and_honours_closure() {
    const AGENT: &str = "frozen-prompt-probe";
    const SESSION: &str = "frozen-prompt-probe-session";

    let dir = tempfile::tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join(format!("{AGENT}.md")),
        format!(
            "---\nname: {AGENT}\ndescription: Frozen prompt probe\nmodel: haiku\n---\n\nProbe.\n"
        ),
    )
    .unwrap();
    // 夹具在 `connect` 时快照 `resources/list`，定义必须先落盘。
    let fixture = AgentFaceFixture::connect(dir.path()).await;
    let pool: Arc<dyn McpPoolPort> = fixture.pool.clone();

    let open_port: Arc<dyn AgentCatalogPort> = Arc::new(AgentCatalogProvider::new());
    assert!(
        bind_agent_catalog_from_pool(&open_port, &pool, SESSION, &HashSet::new()),
        "池与端口都是本 crate 实现 ⇒ 绑定必须成立"
    );
    let ids: Vec<String> = open_port.catalog(false).into_iter().map(|e| e.id).collect();
    assert_eq!(
        ids,
        vec![AGENT.to_string()],
        "绑定后目录必须非空——这是会话创建期冻结渲染的前提"
    );

    // 关闭集命中 ⇒ 本地面关闭（与 `assembly/preparation.rs` 的 turn 级 bind 同形）。
    let closed_port: Arc<dyn AgentCatalogPort> = Arc::new(AgentCatalogProvider::new());
    let mut disabled = HashSet::new();
    disabled.insert(crate::assembly::SUB_AGENT_FACE_CLOSED_KEY.to_string());
    assert!(
        bind_agent_catalog_from_pool(&closed_port, &pool, SESSION, &disabled),
        "关闭集不影响绑定成立与否，只决定本地面开关"
    );
    assert!(
        closed_port.catalog(false).is_empty(),
        "关闭集命中 ⇒ 本地面关闭 ⇒ 空目录（X4/J5：不得回落磁盘）"
    );
}

/// 反例：端口不是本 crate 实现 ⇒ 返回 `false` 且不 panic（不回落降级实例）。
///
/// 池用真实 `McpClientPool`（`new_pending` 即可，downcast 成功），确保拒绝来自
/// **端口**分支而不是池分支。
#[test]
fn bind_agent_catalog_from_pool_rejects_foreign_port() {
    let pool: Arc<dyn McpPoolPort> = Arc::new(crate::mcp::McpClientPool::new_pending());
    let foreign: Arc<dyn AgentCatalogPort> = Arc::new(NoopAgentCatalog);
    assert!(
        !bind_agent_catalog_from_pool(&foreign, &pool, "foreign-port-session", &HashSet::new()),
        "端口非 `AgentCatalogProvider` ⇒ 不适用，返回 false"
    );
    assert!(
        foreign.catalog(true).is_empty(),
        "不适用路径不得产生任何目录内容"
    );
}

// === H4 信任准入（项目 / local settings hooks） ===

/// 全局配置路径 guard：测试结束还原（与 `settings_test.rs` 同形）。
struct GlobalConfigGuard(std::path::PathBuf);

impl Drop for GlobalConfigGuard {
    fn drop(&mut self) {
        peri_config::io::set_global_config_path(Some(self.0.clone()));
    }
}

const PROJECT_HOOK: &str =
    r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo hook"}]}]}}"#;
const PROJECT_HOOK_CHANGED: &str =
    r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo changed"}]}]}}"#;

fn write_project_settings(workspace: &std::path::Path, content: &str) {
    let path = workspace.join(".claude/settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// 非交互默认拒绝 → 显式授权放行 → 来源变化失效 → 撤销回到拒绝。
#[test]
#[serial_test::serial]
fn settings_hooks_admission_requires_explicit_workspace_bound_trust() {
    use peri_config::trust::SettingsSourceKind;

    let _lock = peri_mcp_common::process_env::lock().expect("process env lock");
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let _guard = GlobalConfigGuard(peri_config::io::global_config_path());
    peri_config::io::set_global_config_path(Some(home.path().join("peri").join("settings.json")));

    let cwd = workspace.path().to_str().unwrap();
    write_project_settings(workspace.path(), PROJECT_HOOK);
    let port = SettingsHooksLoader;

    assert!(
        port.project(cwd).is_empty(),
        "非交互默认拒绝：未授权项目 hooks 不得进入装配"
    );

    let binding =
        peri_config::trust::settings_binding(workspace.path(), SettingsSourceKind::Project)
            .unwrap()
            .unwrap();
    peri_config::trust::grant(&binding).unwrap();
    assert_eq!(port.project(cwd).len(), 1, "显式授权后项目 hooks 进入装配");

    // local 来源不得借用 project 授权（来源身份含 scope）
    let local_path = workspace.path().join(".claude/settings.local.json");
    std::fs::write(&local_path, PROJECT_HOOK).unwrap();
    assert!(
        port.local(cwd).is_empty(),
        "project 授权不得被 local 来源借用"
    );

    // 来源摘要变化（项目改写 settings.json）⇒ 旧授权失效
    write_project_settings(workspace.path(), PROJECT_HOOK_CHANGED);
    assert!(
        port.project(cwd).is_empty(),
        "来源变化后旧授权必须失效，不得继续装配"
    );

    let rebound =
        peri_config::trust::settings_binding(workspace.path(), SettingsSourceKind::Project)
            .unwrap()
            .unwrap();
    peri_config::trust::grant(&rebound).unwrap();
    assert_eq!(port.project(cwd).len(), 1, "重新授权新摘要后放行");

    assert!(
        peri_config::trust::revoke(&rebound.workspace, &rebound.source).unwrap(),
        "撤销命中已授权记录"
    );
    assert!(
        port.project(cwd).is_empty(),
        "撤销后必须立即回到拒绝（未信任不执行）"
    );
}

/// 未信任来源不阻断 port 的其余来源视图接口（global 不参与信任判定）。
#[test]
#[serial_test::serial]
fn global_source_view_is_not_gated_by_workspace_trust() {
    let _lock = peri_mcp_common::process_env::lock().expect("process env lock");
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let _guard = GlobalConfigGuard(peri_config::io::global_config_path());
    peri_config::io::set_global_config_path(Some(home.path().join("peri").join("settings.json")));

    write_project_settings(workspace.path(), PROJECT_HOOK);
    let port = SettingsHooksLoader;
    let cwd = workspace.path().to_str().unwrap();
    assert!(port.project(cwd).is_empty());

    // global 是用户机器级配置：本测试只要求接口可用且不因项目未授权而报错/panic。
    let _ = port.global();
}

// ── M6/H4：插件来源 hooks 的信任准入 ─────────────────────────────────────────

mod plugin_hook_trust {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use peri_acp_types::hooks::{HookEvent, HooksConfig, RegisteredHook};
    use peri_acp_types::plugin::{
        InstallScope, LoadedPlugin, PluginManifest, PluginOrigin, PluginScope,
    };
    use serial_test::serial;

    use super::super::{admit_plugin_hooks, plugin_hook_binding};

    struct Fixture {
        _home: tempfile::TempDir,
        workspace: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let workspace = tempfile::tempdir().unwrap();
            // 信任文件跟随选中 global 配置：<temp>/peri/settings.json → <temp>/peri/hook-trust.json
            peri_config::io::set_global_config_path(Some(home.path().join("peri/settings.json")));
            Self {
                _home: home,
                workspace,
            }
        }

        fn cwd(&self) -> String {
            self.workspace.path().to_string_lossy().into_owned()
        }
    }

    fn hooks_config(command: &str) -> HooksConfig {
        let rule: peri_acp_types::hooks::HookMatchRule =
            serde_json::from_value(serde_json::json!({
                "matcher": "*",
                "hooks": [{"type": "command", "command": command}],
            }))
            .unwrap();
        HashMap::from([(HookEvent::PreToolUse, vec![rule])])
    }

    fn plugin(root: &std::path::Path, command: &str) -> LoadedPlugin {
        LoadedPlugin {
            name: "sample".to_string(),
            version: "1.0.0".to_string(),
            install_path: root.to_path_buf(),
            manifest: PluginManifest {
                name: "sample".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                author: None,
                commands: None,
                agents: None,
                skills: None,
                hooks: None,
                mcp_servers: None,
                output_styles: None,
                options: None,
                settings: None,
                extra: serde_json::json!({}),
            },
            commands: vec![],
            skills_roots: vec![],
            agents_dirs: vec![],
            mcp_servers: HashMap::new(),
            data_path: root.join("data"),
            hooks_config: Some(hooks_config(command)),
            marketplace: "market".to_string(),
            scope: PluginScope {
                plugin_id: "sample@market".to_string(),
                origin: PluginOrigin::PeriInstalled,
                install_scope: InstallScope::User,
                project_path: None,
            },
        }
    }

    fn registered(root: &std::path::Path) -> RegisteredHook {
        RegisteredHook {
            hook: serde_json::from_str(r#"{"type":"command","command":"echo hi"}"#).unwrap(),
            event: HookEvent::PreToolUse,
            matcher: None,
            plugin_name: "sample".to_string(),
            plugin_id: "sample@market".to_string(),
            plugin_source: Some(PluginScope {
                plugin_id: "sample@market".to_string(),
                origin: PluginOrigin::PeriInstalled,
                install_scope: InstallScope::User,
                project_path: None,
            }),
            plugin_root: root.to_path_buf(),
            plugin_data_dir: PathBuf::from("/tmp/sample-data"),
            plugin_options: HashMap::new(),
        }
    }

    /// 默认拒绝 → 显式授权放行 → hooks 配置变化后授权失效（同一 workspace）。
    #[test]
    #[serial]
    fn plugin_hooks_are_denied_by_default_and_invalidated_by_config_change() {
        let fixture = Fixture::new();
        let root = fixture.workspace.path().join("plugin-root");
        std::fs::create_dir_all(&root).unwrap();
        let cwd = fixture.cwd();

        let untrusted = plugin(&root, "echo first");
        assert_eq!(
            admit_plugin_hooks(
                &cwd,
                std::slice::from_ref(&untrusted),
                vec![registered(&root)]
            )
            .len(),
            0,
            "未授权来源的插件 hooks 必须整组不执行"
        );

        let binding = plugin_hook_binding(std::path::Path::new(&cwd), &untrusted)
            .unwrap()
            .expect("workspace 可规范化");
        assert!(
            binding
                .source
                .starts_with("plugin-hooks:plugin:peri-installed/user/sample@market"),
            "来源身份必须取自安装记录: {}",
            binding.source
        );
        peri_config::trust::grant(&binding).unwrap();
        assert_eq!(
            admit_plugin_hooks(
                &cwd,
                std::slice::from_ref(&untrusted),
                vec![registered(&root)]
            )
            .len(),
            1,
            "显式授权后同一来源必须放行"
        );

        // hooks 配置变化（摘要变化）⇒ 既有授权失效，回到默认拒绝。
        let mutated = plugin(&root, "echo changed");
        assert_eq!(
            admit_plugin_hooks(
                &cwd,
                std::slice::from_ref(&mutated),
                vec![registered(&root)]
            )
            .len(),
            0,
            "插件 hooks 内容变化后授权必须失效"
        );

        // 撤销同样立即生效。
        let rebinding = plugin_hook_binding(std::path::Path::new(&cwd), &mutated)
            .unwrap()
            .unwrap();
        peri_config::trust::grant(&rebinding).unwrap();
        assert_eq!(
            admit_plugin_hooks(
                &cwd,
                std::slice::from_ref(&mutated),
                vec![registered(&root)]
            )
            .len(),
            1
        );
        assert!(peri_config::trust::revoke(&rebinding.workspace, &rebinding.source).unwrap());
        assert_eq!(
            admit_plugin_hooks(
                &cwd,
                std::slice::from_ref(&mutated),
                vec![registered(&root)]
            )
            .len(),
            0,
            "revoke 后必须回到默认拒绝"
        );
    }

    /// 来源身份不可得的 hook（plugin_root 不在本批插件里）必须 fail-closed。
    #[test]
    #[serial]
    fn hook_without_known_source_identity_is_denied() {
        let fixture = Fixture::new();
        let root = fixture.workspace.path().join("plugin-root");
        std::fs::create_dir_all(&root).unwrap();
        let cwd = fixture.cwd();
        let known = plugin(&root, "echo");
        let binding = plugin_hook_binding(std::path::Path::new(&cwd), &known)
            .unwrap()
            .unwrap();
        peri_config::trust::grant(&binding).unwrap();

        let foreign = registered(&fixture.workspace.path().join("unknown-root"));
        assert_eq!(
            admit_plugin_hooks(&cwd, &[known], vec![foreign]).len(),
            0,
            "无来源身份的 hook 不得静默放行"
        );
    }
}
