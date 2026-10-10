//! ACP session task state remains local to ACP. Workspace Bash state is owned
//! by the MCP capability and verified by its Tasks wire tests.

use std::sync::Arc;

use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};

use super::workspace::SessionEnvironment;
use crate::host::assemble::{assemble_server_config, HostAssemblyInput};
use crate::provider::{ConfigSource, LlmProvider, PeriConfig, ProviderConfig, ProviderModels};

// ── 夹具 ─────────────────────────────────────────────────────────────────────

/// 生产同构的 host cfg + 规范化 cwd（`SessionEnvironment::assemble` 的
/// `same_directory` 分支要求 `canonicalize(startup_cwd) == cwd`）。
struct SeamFixture {
    cfg: crate::host::AcpServerConfig,
    cwd: String,
    _tmp: tempfile::TempDir,
}

async fn seam_fixture() -> SeamFixture {
    let tmp = tempfile::TempDir::new().unwrap();
    // macOS 上 `TempDir` 的原始路径（/var/...）与规范化路径（/private/var/...）
    // 不同：装配面的同目录判定用 `canonicalize(startup_cwd) == cwd`，故 cwd 取规范形态。
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();

    let peri_config = make_peri_config_with_provider(make_provider_config());
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let config_source = Arc::new(
        ConfigSource::load_at(
            &tmp.path().join("empty-cwd"),
            tmp.path().join("test_config.json"),
        )
        .expect("夹具配置源"),
    );

    let cfg = assemble_server_config(HostAssemblyInput {
        provider,
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        config_source,
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        session_resources,
        workspace_id: None,
        session_store_shutdown: None,
        cwd: cwd.clone(),
        // bare：跳过插件/settings hooks，会话只建 workspace 池。
        // 本文件验证激活前的输入注入与关闭，初始化受 activation 闸门保护。
        bare: true,
        drive_cron_tick: false,
        workspace_input: None,
        workspace_bash_default_run_in_background: false,
        // 本夹具直接构造顶层装配：资源面输入同样保持未接线（会话路径才装载）。
        workspace_resources: None,
        // 无会话上下文（测试夹具）：A24 关闭集为空集。
        builtin_closed: Default::default(),
        // 宿主技能面关闭位与关闭集同源：本夹具无会话上下文，恒为假。
        skills_face_closed: false,
        plugin_face_closed: false,
        prepared_plugins: None,
        session_mcp_servers: None,
    })
    .await;

    SeamFixture {
        cfg,
        cwd,
        _tmp: tmp,
    }
}

fn make_provider_config() -> ProviderConfig {
    ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn make_peri_config_with_provider(provider: ProviderConfig) -> PeriConfig {
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![provider];
    peri_config
}

#[tokio::test]
async fn session_task_manager_is_local_to_the_acp_session() {
    let fixture = seam_fixture().await;
    let session_id = "session-task-owner";
    let environment = SessionEnvironment::assemble(&fixture.cfg, &fixture.cwd, session_id)
        .await
        .expect("assembly succeeds")
        .expect("workspace environment exists");
    let manager = environment.task_manager();
    environment
        .cfg
        .session_manager
        .ensure_session_with_task_manager(session_id, &fixture.cwd, Some(manager.clone()));
    let stored = &environment
        .cfg
        .session_manager
        .get_session(session_id)
        .expect("session registered")
        .task_manager;
    assert!(Arc::ptr_eq(stored, &manager));
    assert!(environment.shutdown().await);
}
