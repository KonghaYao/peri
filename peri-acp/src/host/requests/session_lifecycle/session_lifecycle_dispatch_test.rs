//! `session/new` 预留根会话 ID（wire：`_meta["peri.rootSessionId"]`）的目标用例。
//!
//! 覆盖部署级 opt-in 的两种结果（打开 = 预留生效；关闭 = 显式拒绝）、默认自动生成
//! UUIDv7、重复/非法/未授权的诊断，以及「不覆盖既有会话」的冲突判定（live 与持久
//! 两侧）。夹具是只读准备 + 无会话环境的宿主：`workspace_assembly = None` ⇒ 会话
//! 创建不启动 MCP/会话环境，用例只断言身份与持久事实。
//!
//! 装配面 opt-in 本身另有一条用例走真实 `assemble_server_config_with_capabilities`
//! （`host_assembly_opt_in_reaches_config`）。

use std::collections::BTreeMap;
use std::sync::Arc;

use peri_middlewares::permission::shared_mode::{PermissionMode, SharedPermissionMode};
use serial_test::serial;
use tempfile::TempDir;

use super::handle_new;
use super::parse_reserved_root_session_id;
use crate::host::AcpServerConfig;
use crate::host::SessionState;
use crate::provider::{LlmProvider, ProviderConfig, ProviderModels};
use crate::transport::types::AcpError;

/// 合法 UUIDv7（v7 版本位 + RFC 4122 variant）。
const RESERVED_V7: &str = "0192f0c4-1c2a-7a5b-8f00-000000000001";
/// 另一条合法 UUIDv7，用于证明默认生成与预留值不同。
const OTHER_V7: &str = "0192f0c4-1c2a-7a5b-8f00-000000000002";
/// 合法 UUIDv4：不是宿主身份口径（拒绝）。
const UUID_V4: &str = "67e55044-10b1-426f-9247-bb680e5fe0c8";

fn make_provider() -> LlmProvider {
    let mut config = crate::provider::PeriConfig::default();
    config.config.active_alias = "sonnet".to_string();
    config.config.providers = vec![ProviderConfig {
        id: "reserved-root-session-test".into(),
        provider_type: "anthropic".into(),
        api_key: "reserved-root-session-test-placeholder".into(),
        models: ProviderModels {
            sonnet: "reserved-root-session-test-model".into(),
            ..Default::default()
        },
        ..Default::default()
    }];
    LlmProvider::from_config(&config).expect("test provider config must resolve")
}

/// 真实 SQLite 门面 + 无会话环境的宿主（`workspace_assembly = None`）。
async fn test_host(tmp: &TempDir, reserved_root_session_id: bool) -> AcpServerConfig {
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let peri_config = crate::provider::PeriConfig::default();
    let provider = make_provider();
    let session_manager = crate::session::SessionManager::new(
        session_resources.clone(),
        provider.clone(),
        Arc::new(peri_config.clone()),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        Some(Arc::new(|| {
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
                as Arc<dyn peri_acp_types::tasks::TaskManager>
        })),
        Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        Vec::new(),
    );
    let (host_task_owner, host_task_spawner) = crate::host::task_scope::HostTaskOwner::new();
    let (mcp_task_owner, _mcp_task_spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    AcpServerConfig {
        reserved_root_session_id,
        model_transport_route: None,
        workspace_assembly: None,
        host_task_owner: Some(host_task_owner),
        host_task_spawner,
        mcp_task_owner: Some(Box::new(mcp_task_owner)),
        provider: Arc::new(parking_lot::RwLock::new(provider)),
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        cron_scheduler: None,
        mcp_pool: None,
        mcp_apps_relay: None,
        mcp_apps_facade: None,
        session_authorization: None,
        acp_mcp: None,
        oauth_event_tx: None,
        oauth_event_rx: None,
        plugin_skill_roots: Vec::new(),
        plugin_command_entries: Vec::new(),
        plugin_face_closed: false,
        plugin_hooks: Vec::new(),
        plugin_hooks_only: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        tool_search_index: Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new()),
        agent_catalog: Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        plugin_manager: Arc::new(peri_middlewares::host_ports::PluginManager),
        settings_hooks: Arc::new(peri_middlewares::host_ports::SettingsHooksLoader),
        shared_tools: Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
        workflow_middleware_factory: Arc::new(
            peri_middlewares::assembly::WorkflowAgentMiddlewareFactory,
        ),
        session_resources: session_resources.clone(),
        session_store_shutdown: None,
        controller: Arc::new(peri_controller::Controller::new(session_resources)),
        langfuse_session: None,
        langfuse_shutdown_owner: None,
        config_source: Arc::new(
            crate::provider::ConfigSource::load_at(
                &tmp.path().join("empty-cwd"),
                tmp.path().join("test_config.json"),
            )
            .unwrap(),
        ),
        session_manager,
        stdio_command_filter: false,
    }
}

/// 会话创建请求：`cwd` = 沙箱目录，可选预留 ID。
fn new_request(cwd: &str, reserved: Option<&str>) -> serde_json::Value {
    let mut params = serde_json::json!({ "cwd": cwd });
    if let Some(reserved) = reserved {
        params["_meta"] = serde_json::json!({ "peri.rootSessionId": reserved });
    }
    params
}

fn created_id(response: &serde_json::Value) -> String {
    response["sessionId"]
        .as_str()
        .expect("session/new response carries sessionId")
        .to_owned()
}

fn assert_error(error: AcpError, code: i64, message: &str) {
    assert_eq!(error.code, code, "unexpected code: {error}");
    assert_eq!(error.message, message, "unexpected message: {error}");
}

/// 预留 ID 生效：响应与持久身份都是预留值，且 live 会话按该 ID 发布。
#[tokio::test]
async fn reserved_root_session_id_is_used_for_creation() {
    let tmp = TempDir::new().unwrap();
    // 存储里的 cwd 是解析后的规范化路径（macOS 的 /var → /private/var）。
    let cwd = tmp.path().canonicalize().unwrap();
    let cwd = cwd.to_string_lossy().into_owned();
    let host = test_host(&tmp, true).await;
    let mut sessions = std::collections::HashMap::new();

    let response = handle_new(&new_request(&cwd, Some(RESERVED_V7)), &host, &mut sessions)
        .await
        .expect("预留 ID 的 session/new 必须成功");

    assert_eq!(created_id(&response), RESERVED_V7);
    assert!(
        sessions.contains_key(RESERVED_V7),
        "live 会话必须按预留 ID 发布"
    );
    let snapshot = host
        .session_resources
        .load_session_snapshot(&RESERVED_V7.to_owned())
        .await
        .expect("预留 ID 的会话必须可读");
    assert_eq!(snapshot.meta.cwd, cwd, "持久身份必须就是本次预留 ID");
}

/// 默认路径：不带 `peri.rootSessionId` 时仍由宿主生成新的 UUIDv7。
#[tokio::test]
async fn missing_reserved_id_generates_fresh_uuid_v7() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    let host = test_host(&tmp, true).await;
    let mut sessions = std::collections::HashMap::new();

    let first = handle_new(&new_request(&cwd, None), &host, &mut sessions)
        .await
        .expect("默认 session/new 必须成功");
    let second = handle_new(&new_request(&cwd, None), &host, &mut sessions)
        .await
        .expect("默认 session/new 必须可重复");

    for response in [&first, &second] {
        let id = created_id(response);
        let parsed = uuid::Uuid::parse_str(&id).expect("默认 ID 必须是 UUID");
        assert_eq!(parsed.get_version_num(), 7, "默认 ID 必须是 UUIDv7");
    }
    assert_ne!(
        created_id(&first),
        created_id(&second),
        "每次创建都是新身份"
    );
    assert_ne!(created_id(&first), OTHER_V7);
}

/// TUI / print / stdio 的默认装配（opt-in 关闭）行为不变：不传键时照常自动生成。
#[tokio::test]
async fn default_deployment_without_opt_in_still_generates_uuid_v7() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    let host = test_host(&tmp, false).await;
    let mut sessions = std::collections::HashMap::new();

    let response = handle_new(&new_request(&cwd, None), &host, &mut sessions)
        .await
        .expect("默认部署的 session/new 必须成功");
    let id = created_id(&response);
    assert_eq!(
        uuid::Uuid::parse_str(&id).unwrap().get_version_num(),
        7,
        "默认装配仍生成 UUIDv7"
    );
}

/// 未 opt-in 却带预留键：显式拒绝（不静默忽略），且不创建任何会话。
#[tokio::test]
async fn reserved_id_is_rejected_without_opt_in() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    let host = test_host(&tmp, false).await;
    let mut sessions = std::collections::HashMap::new();

    let error = handle_new(&new_request(&cwd, Some(RESERVED_V7)), &host, &mut sessions)
        .await
        .expect_err("关闭 opt-in 时必须拒绝预留键");

    assert_error(
        error,
        -32602,
        "peri.rootSessionId is not enabled for this host",
    );
    assert!(sessions.is_empty(), "拒绝必须发生在创建之前");
}

/// 同名 ID 第二次创建：`-32010`，且原会话事实不被覆盖。
#[tokio::test]
async fn duplicate_reserved_id_is_rejected_and_keeps_existing_session() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    let host = test_host(&tmp, true).await;
    let mut sessions = std::collections::HashMap::new();

    handle_new(&new_request(&cwd, Some(RESERVED_V7)), &host, &mut sessions)
        .await
        .expect("首次创建必须成功");
    let before = host
        .session_resources
        .load_session_snapshot(&RESERVED_V7.to_owned())
        .await
        .expect("首次创建后会话可读");

    let error = handle_new(&new_request(&cwd, Some(RESERVED_V7)), &host, &mut sessions)
        .await
        .expect_err("重复预留 ID 必须被拒绝");

    assert_error(error, -32010, "Session id already exists");
    let after = host
        .session_resources
        .load_session_snapshot(&RESERVED_V7.to_owned())
        .await
        .expect("被拒绝的创建不得删除原会话");
    assert_eq!(after.meta.cwd, before.meta.cwd, "原会话元数据不得被改写");
    assert_eq!(
        after.frozen, before.frozen,
        "原会话的 frozen 事实不得被改写"
    );
}

/// 只有持久事实、没有 live 实例（会话曾关闭）：同一 ID 仍不可复用。
#[tokio::test]
async fn reserved_id_conflicting_with_persisted_session_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    let host = test_host(&tmp, true).await;
    let mut sessions = std::collections::HashMap::new();
    handle_new(&new_request(&cwd, Some(RESERVED_V7)), &host, &mut sessions)
        .await
        .expect("首次创建必须成功");
    // 模拟 close：live 视图消失，持久行保留。
    sessions.clear();

    let error = handle_new(&new_request(&cwd, Some(RESERVED_V7)), &host, &mut sessions)
        .await
        .expect_err("持久会话占用同一 ID 时必须拒绝");

    assert_error(error, -32010, "Session id already exists");
}

/// live 冲突优先于任何写入：命中已发布会话即拒绝，存储保持为空。
#[tokio::test]
async fn reserved_id_conflicting_with_live_session_is_rejected_before_write() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    let host = test_host(&tmp, true).await;
    let mut sessions = std::collections::HashMap::new();
    sessions.insert(
        RESERVED_V7.to_owned(),
        SessionState::view_only(RESERVED_V7.to_owned(), RESERVED_V7.to_owned(), cwd.clone()),
    );

    let error = handle_new(&new_request(&cwd, Some(RESERVED_V7)), &host, &mut sessions)
        .await
        .expect_err("live 会话占用同一 ID 时必须拒绝");

    assert_error(error, -32010, "Session id already exists");
    assert!(
        host.session_resources
            .load_frozen_state(&RESERVED_V7.to_owned())
            .await
            .is_err(),
        "live 冲突必须在写库之前拒绝"
    );
}

/// 非法预留值一律 `-32602`：非字符串、空串、非 UUID、非 v7。
#[tokio::test]
async fn reserved_id_validation_rejects_invalid_values() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    let host = test_host(&tmp, true).await;
    let mut sessions = std::collections::HashMap::new();

    for value in [
        serde_json::json!(42),
        serde_json::json!(""),
        serde_json::json!("   "),
        serde_json::json!("not-a-uuid"),
        serde_json::json!(UUID_V4),
    ] {
        let params = serde_json::json!({ "cwd": cwd, "_meta": { "peri.rootSessionId": value } });
        let error = handle_new(&params, &host, &mut sessions)
            .await
            .expect_err("非法预留值必须被拒绝");
        assert_eq!(error.code, -32602, "非法值 {value} 必须是参数错误");
        assert!(
            error.message.starts_with("peri.rootSessionId must be"),
            "非法值 {value} 的诊断必须指向预留键：{error}"
        );
    }
    assert!(sessions.is_empty(), "校验失败不得创建会话");
}

/// 解析器本身：规范化输出与关闭位的判定顺序（未授权先于类型）。
#[test]
fn reserved_id_parser_normalizes_and_gates_on_opt_in() {
    let upper = serde_json::json!({
        "_meta": { "peri.rootSessionId": RESERVED_V7.to_uppercase() }
    });
    assert_eq!(
        parse_reserved_root_session_id(&upper, true)
            .unwrap()
            .as_deref(),
        Some(RESERVED_V7),
        "合法 v7 必须规范化为小写连字符形式"
    );

    let absent = serde_json::json!({ "cwd": "." });
    assert_eq!(
        parse_reserved_root_session_id(&absent, false).unwrap(),
        None
    );

    let closed = serde_json::json!({ "_meta": { "peri.rootSessionId": 42 } });
    assert_error(
        parse_reserved_root_session_id(&closed, false).unwrap_err(),
        -32602,
        "peri.rootSessionId is not enabled for this host",
    );
}

/// 装配面 opt-in 真的到达 `AcpServerConfig`（TUI/print/stdio 默认仍为 `false`）。
#[tokio::test]
#[serial]
async fn host_assembly_opt_in_reaches_config() {
    let tmp = TempDir::new().unwrap();
    let home = HomeDirGuard::set(tmp.path());
    let cwd = tmp.path().canonicalize().unwrap();
    for expected in [false, true] {
        let config = crate::provider::PeriConfig::default();
        let provider = make_provider();
        let cfg = crate::host::assemble::assemble_server_config_with_capabilities(
            crate::host::assemble::HostAssemblyInput {
                provider,
                peri_config: Arc::new(parking_lot::RwLock::new(config)),
                config_source: Arc::new(
                    crate::provider::ConfigSource::load_at(&cwd, tmp.path().join("settings.json"))
                        .unwrap(),
                ),
                permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
                session_resources: peri_agent::resources::open_session_resources_with(Some(
                    tmp.path().join(format!("assembly-{expected}.db")),
                ))
                .await
                .unwrap(),
                workspace_id: None,
                session_store_shutdown: None,
                cwd: cwd.to_string_lossy().into_owned(),
                bare: false,
                drive_cron_tick: false,
                builtin_assembly_mode: None,
                workspace_input: None,
                workspace_resources: None,
                workflow_input: None,
                prepared_plugins: None,
                session_mcp_servers: None,
                builtin_closed: Default::default(),
                skills_face_closed: false,
                plugin_face_closed: false,
                reserved_root_session_id: expected,
            },
            Default::default(),
        )
        .await;
        assert_eq!(
            cfg.reserved_root_session_id, expected,
            "装配输入必须原样落进 AcpServerConfig"
        );
    }
    drop(home);
}

/// HOME 隔离：装配面按 HOME 解析用户级 `.claude`，测试必须挡住真实家目录。
struct HomeDirGuard {
    home: Option<std::ffi::OsString>,
}

impl HomeDirGuard {
    fn set(path: &std::path::Path) -> Self {
        let home = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        Self { home }
    }
}

impl Drop for HomeDirGuard {
    fn drop(&mut self) {
        match self.home.take() {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
