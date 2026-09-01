use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
};

use crate::provider::{PeriConfig, ProviderConfig, ProviderModels};
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use async_trait::async_trait;
use peri_acp_types::event_data::PluginSnapshotEntry;
use peri_acp_types::plugin::{InstallScope, InstalledPlugin, PluginManagerPort, PluginOrigin};
use peri_acp_types::ports::WorkflowMiddlewarePort;
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::thread::ThreadMeta;
use peri_agent::thread::FilesystemThreadStore;
use peri_middlewares::permission::shared_mode::{PermissionMode, SharedPermissionMode};
use peri_middlewares::workflow::WorkflowMiddleware;
use peri_workflow::protocol::{AgentRunParams, AgentRunResult, Usage};
use peri_workflow::registry::{WorkflowRun, WorkflowRunStatus, WorkflowTaskResult};
use peri_workflow::runner::AgentExecutor;
use serde_json::{json, Value};
use serial_test::serial;

use super::*;
use crate::provider::LlmProvider;

// ── Mock AcpTransport ─────────────────────────────────────────────────────────

/// 记录全部通知的 mock transport（`Mutex<Vec<(method, payload)>>`，Slice 6
/// 改造：原实现丢弃 `_params`，现记录供 available_commands_update 回调重发
/// 断言）。
#[derive(Default)]
struct MockTransport {
    notifications: std::sync::Mutex<Vec<(String, Value)>>,
}

impl MockTransport {
    fn notifications(&self) -> Vec<(String, Value)> {
        self.notifications.lock().unwrap().clone()
    }
}

#[async_trait]
impl crate::transport::AcpTransport for MockTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
    }
    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.notifications
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(())
    }
    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }
    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

fn make_provider_config(
    id: &str,
    provider_type: &str,
    api_key: &str,
    model: &str,
) -> ProviderConfig {
    ProviderConfig {
        id: id.to_string(),
        provider_type: provider_type.to_string(),
        api_key: api_key.to_string(),
        // 将模型名填入 sonnet 别名（默认 alias）
        models: ProviderModels {
            sonnet: model.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 构造含单个 provider 的 PeriConfig（active_alias=sonnet），供 `LlmProvider::from_config` 使用。
fn make_peri_config_with_provider(provider: ProviderConfig) -> PeriConfig {
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![provider];
    peri_config
}

fn make_server_config(
    peri_config: PeriConfig,
    provider: LlmProvider,
    tmp: &tempfile::TempDir,
) -> AcpServerConfig {
    let thread_store = FilesystemThreadStore::new(tmp.path().join("threads"));
    let arc_thread_store: Arc<dyn peri_acp_types::store::ThreadStore> = Arc::new(thread_store);
    let session_manager = crate::session::SessionManager::new(
        arc_thread_store.clone(),
        provider.clone(),
        Arc::new(peri_config.clone()),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        None,
        // 注入真实 TaskManager 工厂：cancel-bg-task 回归测试依赖 registry 簿记
        Some(Arc::new(|| {
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
                as Arc<dyn peri_acp_types::tasks::TaskManager>
        })),
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
        Vec::new(), // plugin skill roots（C1；测试无）
    );
    let (host_task_owner, host_task_spawner) = crate::host::task_scope::HostTaskOwner::new();
    let (mcp_task_owner, _mcp_task_spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    AcpServerConfig {
        host_task_owner: Some(host_task_owner),
        host_task_spawner,
        mcp_task_owner: Some(Box::new(mcp_task_owner)),
        provider: Arc::new(parking_lot::RwLock::new(provider)),
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        cron_scheduler: None,
        mcp_pool: None,
        mcp_apps_relay: None,
        dynamic_mcp: None,
        oauth_event_tx: None,
        oauth_event_rx: None,
        channel_state: None,
        plugin_skill_roots: Vec::new(),
        plugin_command_entries: Vec::new(),
        plugin_agent_dirs: Vec::new(),
        plugin_hooks: Vec::new(),
        plugin_hooks_only: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        plugin_lsp_servers: Vec::new(),
        tool_search_index: Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new()),
        skills: Arc::new(peri_middlewares::host_ports::SkillsProvider),
        plugin_manager: Arc::new(peri_middlewares::host_ports::PluginManager),
        settings_hooks: Arc::new(peri_middlewares::host_ports::SettingsHooksLoader),
        shared_tools: Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
        workflow_middleware_factory: Arc::new(
            peri_middlewares::assembly::WorkflowAgentMiddlewareFactory,
        ),
        thread_store: arc_thread_store.clone(),
        controller: Arc::new(peri_controller::Controller::new(arc_thread_store)),
        langfuse_session: None,
        config_source: Arc::new(
            // 空 cwd（无工作区配置）+ 显式全局路径：persist_config 写回该路径
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

// ── 测试 ──────────────────────────────────────────────────────────────────────

/// 验证 session/update_config 切换 active profile 的 provider 后 cfg.provider 正确更新
#[tokio::test]
async fn test_update_config_切换provider后cfg_provider更新() {
    // Arrange: 构造两个 provider（a=openai, b=anthropic），初始 sonnet profile 绑定 "a"
    let tmp = tempfile::TempDir::new().unwrap();
    let provider_a = make_provider_config("a", "openai", "sk-openai-test", "gpt-4o");
    let provider_b = make_provider_config("b", "anthropic", "sk-ant-test", "claude-sonnet-4-6");

    let mut peri_config = PeriConfig::default();
    peri_config
        .config
        .profiles
        .get_mut("sonnet")
        .unwrap()
        .provider = "a".to_string();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![provider_a.clone(), provider_b.clone()];

    let initial_provider = LlmProvider::from_config(&peri_config).unwrap();
    assert!(
        matches!(initial_provider, LlmProvider::OpenAi { .. }),
        "初始 provider 应为 OpenAI"
    );

    let cfg = make_server_config(peri_config.clone(), initial_provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    // 构造 update_config 参数：sonnet profile 的 provider 改为 "b"
    let mut updated_config = peri_config.clone();
    updated_config
        .config
        .profiles
        .get_mut("sonnet")
        .unwrap()
        .provider = "b".to_string();

    let params = json!({
        "sessionId": "test-session",
        "config": updated_config,
    });

    // Act: 调用 handle_request
    let result = handle_request(
        "session/update_config",
        &params,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    // Assert: cfg.provider 应切换到 anthropic
    let provider = cfg.provider.read();
    assert!(
        matches!(&*provider, LlmProvider::Anthropic { model, .. } if model == "claude-sonnet-4-6"),
        "切换后 provider 应为 Anthropic claude-sonnet-4-6，实际: display={} model={}",
        provider.display_name(),
        provider.model_name(),
    );
    assert_eq!(
        provider.display_name(),
        "Anthropic",
        "display_name 应为 Anthropic"
    );

    // 验证返回值包含 configOptions
    assert!(
        result.get("configOptions").is_some(),
        "响应应包含 configOptions"
    );
}

/// 验证 session/update_config 空 providers 时返回错误
#[tokio::test]
async fn test_update_config_空providers返回错误() {
    let tmp = tempfile::TempDir::new().unwrap();
    let provider_a = make_provider_config("a", "openai", "sk-openai-test", "gpt-4o");

    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config
        .config
        .profiles
        .get_mut("sonnet")
        .unwrap()
        .provider = "a".to_string();
    peri_config.config.providers = vec![provider_a];

    let initial_provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config.clone(), initial_provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    // 空 providers
    let mut bad_config = PeriConfig::default();
    bad_config.config.providers = vec![];

    let params = json!({
        "sessionId": "test-session",
        "config": bad_config,
    });

    let result = handle_request(
        "session/update_config",
        &params,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "空 providers 应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("providers cannot be empty"),
        "错误消息应提及 providers 为空，实际: {}",
        err.message,
    );
}

/// 验证 session/update_config 不存在的 active_provider_id 返回错误
#[tokio::test]
async fn test_update_config_不存在的provider_id返回错误() {
    let tmp = tempfile::TempDir::new().unwrap();
    let provider_a = make_provider_config("a", "openai", "sk-openai-test", "gpt-4o");

    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config
        .config
        .profiles
        .get_mut("sonnet")
        .unwrap()
        .provider = "a".to_string();
    peri_config.config.providers = vec![provider_a];

    let initial_provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config.clone(), initial_provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    // sonnet profile 的 provider 指向不存在的 provider
    let mut bad_config = peri_config.clone();
    bad_config
        .config
        .profiles
        .get_mut("sonnet")
        .unwrap()
        .provider = "nonexistent".to_string();
    bad_config.config.providers = vec![make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    )];

    let params = json!({
        "sessionId": "test-session",
        "config": bad_config,
    });

    let result = handle_request(
        "session/update_config",
        &params,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "不存在的 provider_id 应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("not found"),
        "错误消息应提及 not found，实际: {}",
        err.message,
    );
}

// ── Rewind RPC 路由测试 ─────────────────────────────────────────────────────

/// 注册一个含 user/ai 消息的 SessionState（字段以 mod.rs 定义为准）。
fn register_session_with_history(
    sessions: &mut HashMap<String, SessionState>,
    cwd: &str,
) -> String {
    let history = vec![
        peri_acp_types::messages::BaseMessage::human("第一轮用户问题"),
        peri_acp_types::messages::BaseMessage::ai("第一轮回答"),
        peri_acp_types::messages::BaseMessage::human("第二轮用户问题"),
    ];
    let sid = "rewind-test-session".to_string();
    sessions.insert(
        sid.clone(),
        SessionState {
            session_id: sid.clone(),
            thread_id: "thread-1".to_string(),
            cwd: cwd.to_string(),
            history,
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: None,
            lsp_pool: None,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            lease: crate::host::lease::WriterLease::acquired("default"),
        },
    );
    sid
}

#[tokio::test]
async fn test_rewind_methods_require_explicit_capability() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
    cfg.session_manager
        .caps_registry()
        .insert(sid.clone(), PeriCaps::default());
    let target = sessions.get(&sid).unwrap().history[0]
        .id()
        .as_uuid()
        .to_string();
    let original: Vec<_> = sessions
        .get(&sid)
        .unwrap()
        .history
        .iter()
        .map(|message| (message.id().as_uuid().to_string(), message.content()))
        .collect();

    for (method, params) in [
        ("session/rewind-candidates", json!({"sessionId": sid})),
        (
            "session/rewind-preview",
            json!({"sessionId": sid, "target_message_id": target}),
        ),
        (
            "session/rewind",
            json!({"sessionId": sid, "target_message_id": target}),
        ),
    ] {
        let error = handle_request(method, &params, &cfg, &mut sessions, &transport)
            .await
            .unwrap_err();
        assert_eq!(error.code, -32601);
        assert_eq!(error.message, "peri.rewind capability not negotiated");
    }
    let current: Vec<_> = sessions
        .get(&sid)
        .unwrap()
        .history
        .iter()
        .map(|message| (message.id().as_uuid().to_string(), message.content()))
        .collect();
    assert_eq!(current, original);
}

/// session/rewind-candidates 路由到 dispatch：返回 user-only 候选。
#[tokio::test]
async fn test_rewind_candidates_routes_to_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );

    let result = handle_request(
        "session/rewind-candidates",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    let value = result.unwrap();
    let messages = value["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "只返回 user 消息");
}

/// session/rewind-preview 路由到 dispatch：返回 file_changes 数组（无工具调用 → 空）。
/// 目标取 history[2]（Human 消息）——与生产口径一致：rewind-candidates 只返回
/// user 消息，AI 消息永远不可能成为回滚目标。
#[tokio::test]
async fn test_rewind_preview_routes_to_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );
    let target_id = sessions.get(&sid).unwrap().history[2]
        .id()
        .as_uuid()
        .to_string();

    let result = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target_id }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    let value = result.unwrap();
    let changes = value["file_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 0, "历史无工具调用 → 空预算");
}

/// session/rewind-preview：目标消息不存在时返回 not found 错误（生产 rewind_preview
/// 按 id 定位，history 之外的 id 一律拒绝）。「仅 AI 消息」场景由候选层保证不可达
/// （rewind-candidates 只返回 user 消息），UI 不可能选中 AI 消息作为目标。
#[tokio::test]
async fn test_rewind_preview_missing_target_returns_not_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );

    let result = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": "00000000-0000-0000-0000-000000000000" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "目标不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("未找到目标消息"),
        "错误消息应提及未找到目标，实际: {}",
        err.message,
    );
}

/// session/rewind 路由到 dispatch：执行回退（无 Write/Edit 时仅截断）。
#[tokio::test]
async fn test_rewind_routes_to_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );
    let target_id = sessions.get(&sid).unwrap().history[0]
        .id()
        .as_uuid()
        .to_string();
    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target_id }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let preview_fingerprint = preview["preview_fingerprint"].as_str().unwrap();

    let result = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target_id,
            "preview_fingerprint": preview_fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert_eq!(result.unwrap()["status"], "executed");

    // P1：rewind 后 SessionState.history 必须截断——它是后续候选/预算查询的
    // 数据源，不写回会导致第二次回退 not found。
    let s = sessions.get(&sid).unwrap();
    assert_eq!(s.history.len(), 0, "回退到第一条后 history 应为空");
}

#[tokio::test]
async fn test_rewind_rejects_stale_preview_without_mutating_history() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );
    let target_id = sessions.get(&sid).unwrap().history[0]
        .id()
        .as_uuid()
        .to_string();
    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target_id }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let preview_fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    sessions
        .get_mut(&sid)
        .unwrap()
        .history
        .push(peri_acp_types::messages::BaseMessage::ai("late answer"));
    let before = sessions.get(&sid).unwrap().history.len();
    let error = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target_id,
            "preview_fingerprint": preview_fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();

    assert!(error.message.contains("preview is stale"));
    assert_eq!(sessions.get(&sid).unwrap().history.len(), before);
}

// ── session/cancel-bg-task 路由测试（issue 2026-08-05）───────────────────

/// [回归测试] cancel-bg-task 对 Workflow 类型任务必须真正 kill（issue 2026-08-05）。
/// 历史 bug：Workflow 注册时固定 `Kill(None)`，cancel() 只 warn 并返回 success——
/// 条目移除但 runner 继续运行。修复后 kill 闭包（生产路径转发
/// WorkflowTaskRegistry::kill）随注册存入条目，cancel() 触发闭包。
/// 本测试用探针闭包在 RPC 层锁定该行为。
#[tokio::test]
async fn test_cancel_bg_task_workflow_invokes_kill_closure() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid = "cancel-bg-session".to_string();
    cfg.session_manager
        .new_session_with_id(&sid, tmp.path().to_str().unwrap())
        .await
        .unwrap();

    let killed = Arc::new(AtomicBool::new(false));
    let killed_clone = killed.clone();
    let registry = &cfg.session_manager.get_session(&sid).unwrap().task_manager;
    registry
        .register(peri_acp_types::tasks::BgTaskRegistration {
            task_id: "wf-run-1".to_string(),
            kind: BgTaskKind::Workflow,
            summary: "wf cancel test".to_string(),
            pid: None,
            kill: Some(Box::new(move || {
                killed_clone.store(true, Ordering::SeqCst);
            })),
        })
        .unwrap();
    assert_eq!(registry.active_count(), 1);

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": sid, "taskId": "wf-run-1" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(
        result.is_ok(),
        "取消 Workflow 任务应返回 success，实际: {:?}",
        result.err()
    );
    assert!(
        killed.load(Ordering::SeqCst),
        "cancel-bg-task 必须触发 kill 闭包（runner 真正被终止），而非仅移除条目"
    );
    assert_eq!(registry.active_count(), 0, "取消后条目应从 registry 移除");
}

/// [回归测试] cancel-bg-task 会话不存在时必须如实报错（issue 2026-08-05）。
/// 历史 bug：静默返回 success，掩盖"取消未生效"。
#[tokio::test]
async fn test_cancel_bg_task_session_not_found_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": "no-such-session", "taskId": "wf-run-1" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "会话不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "错误消息应提及 session not found，实际: {}",
        err.message
    );
}

/// [回归测试] cancel-bg-task 任务不存在时必须如实报错（issue 2026-08-05）。
/// 与 session_not_found 区分（错误消息不同），客户端可据此判断重试策略。
#[tokio::test]
async fn test_cancel_bg_task_task_not_found_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid = "cancel-bg-session".to_string();
    cfg.session_manager
        .new_session_with_id(&sid, tmp.path().to_str().unwrap())
        .await
        .unwrap();

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": sid, "taskId": "no-such-task" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "任务不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("not found"),
        "错误消息应提及 not found，实际: {}",
        err.message
    );
}

// ── workflow/kill_run & workflow/kill_agent sessionId 分发测试（issue 2026-08-05）──

/// Mock workflow executor（仅用于构造 WorkflowMiddleware，不真正执行 agent）
struct MockWorkflowExecutor;

#[async_trait]
impl AgentExecutor for MockWorkflowExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        AgentRunResult::Ok {
            output: serde_json::json!("mock"),
            usage: Usage { output_tokens: 0 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

/// 构造带 workflow_middleware 的 SessionState，返回 middleware 引用（供注册 run 用）。
fn register_session_with_workflow(
    sessions: &mut HashMap<String, SessionState>,
    sid: &str,
    cwd: &str,
) -> Arc<WorkflowMiddleware> {
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockWorkflowExecutor);
    let (notification_tx, _) = tokio::sync::broadcast::channel::<WorkflowTaskResult>(32);
    let mw = Arc::new(WorkflowMiddleware::new(
        executor,
        cwd,
        notification_tx,
        None,
    ));
    sessions.insert(
        sid.to_string(),
        SessionState {
            session_id: sid.to_string(),
            thread_id: format!("thread-{sid}"),
            cwd: cwd.to_string(),
            history: Vec::new(),
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: Some(Arc::clone(&mw) as Arc<dyn WorkflowMiddlewarePort>),
            lsp_pool: None,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            lease: crate::host::lease::WriterLease::acquired("default"),
        },
    );
    mw
}

/// 在 middleware 的 registry 注册一个 Running 的 run（kill_tx 保持 open）。
fn register_run(mw: &Arc<WorkflowMiddleware>, run_id: &str) {
    let (kill_tx, _kill_rx) = tokio::sync::oneshot::channel::<()>();
    let child = tokio::spawn(async {});
    mw.registry()
        .register(WorkflowRun {
            run_id: run_id.to_string(),
            workflow_name: "wf-test".to_string(),
            script_preview: "test".to_string(),
            status: WorkflowRunStatus::Running,
            started_at: std::time::Instant::now(),
            child_handle: child,
            kill_tx: Some(kill_tx),
        })
        .unwrap();
}

/// [回归测试] workflow/kill_run 必须按请求 sessionId 定位 session（issue 2026-08-05）。
/// 历史 bug：`sessions.values().find_map()` 取第一个带 middleware 的 session，
/// 多 session 时可能 kill 错 session（run 在另一 session 却报 killed:true）。
#[tokio::test]
async fn test_kill_run_targets_requested_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp.path().to_str().unwrap();

    let mw_a = register_session_with_workflow(&mut sessions, "sess-a", cwd);
    let mw_b = register_session_with_workflow(&mut sessions, "sess-b", cwd);
    register_run(&mw_a, "run-a");
    register_run(&mw_b, "run-b");

    // run-a 只在 sess-a：请求 sess-b 杀 run-a 必须 killed:false（修复前可能误报 true）
    let resp = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": "sess-b", "runId": "run-a" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["killed"], false, "sess-b 无 run-a，不得误报 killed");

    // 请求 sess-b 杀 run-b → killed:true，且只影响 sess-b 的 registry
    let resp = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": "sess-b", "runId": "run-b" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["killed"], true, "sess-b 的 run-b 应被 kill");
    assert!(
        mw_b.registry().list_runs().is_empty(),
        "sess-b 的 registry 应已移除 run-b"
    );
    assert!(
        !mw_a.registry().list_runs().is_empty(),
        "sess-a 的 registry 不得受影响"
    );

    // 缺失 sessionId → -32602
    let err = handle_request(
        "workflow/kill_run",
        &json!({ "runId": "run-a" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报错，实际: {}",
        err.message
    );

    // session 不存在 → 明确错误（修复前静默返回 killed:false）
    let err = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": "no-such-session", "runId": "run-a" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "会话不存在应报 session not found，实际: {}",
        err.message
    );

    // session 存在但无 workflow middleware → 明确错误
    let sid = register_session_with_history(&mut sessions, cwd);
    let err = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": sid, "runId": "run-a" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "无 middleware 的会话应报错，实际: {}",
        err.message
    );
}

/// [回归测试] workflow/kill_agent 必须按请求 sessionId 定位 session（issue 2026-08-05）。
/// 深层 kill 依赖 runner 内部 active_channels（外部不可注入），此处锁定协议层：
/// 缺失/不存在的 session 如实报错，存在的 session 正常返回 killed 结果。
#[tokio::test]
async fn test_kill_agent_targets_requested_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp.path().to_str().unwrap();

    register_session_with_workflow(&mut sessions, "sess-a", cwd);
    register_session_with_workflow(&mut sessions, "sess-b", cwd);

    // 存在 session：正常返回 killed（sess-b 无该 run 的 active channel → false，不报错）
    let resp = handle_request(
        "workflow/kill_agent",
        &json!({ "sessionId": "sess-b", "runId": "run-x", "agentId": 1 }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["killed"], false);

    // 缺失 sessionId → -32602
    let err = handle_request(
        "workflow/kill_agent",
        &json!({ "runId": "run-x", "agentId": 1 }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报错，实际: {}",
        err.message
    );

    // session 不存在 → 明确错误（修复前静默返回 killed:false）
    let err = handle_request(
        "workflow/kill_agent",
        &json!({ "sessionId": "no-such-session", "runId": "run-x", "agentId": 1 }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "会话不存在应报 session not found，实际: {}",
        err.message
    );
}

/// [回归测试] workflow/resume 必须按请求 sessionId 定位 session（issue 2026-08-05）。
/// 历史 bug：`sessions.values().find_map()` 取第一个带 middleware 的 session，
/// 多 session 时可能 resume 错 session（与 kill_run 同源）。
#[tokio::test]
async fn test_resume_targets_requested_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp.path().to_str().unwrap();

    register_session_with_workflow(&mut sessions, "sess-a", cwd);
    register_session_with_workflow(&mut sessions, "sess-b", cwd);

    // 请求 sess-b + 不存在的 run → 错误来自 sess-b 的 middleware（read_state 失败），
    // 而非 "session not found"——证明分发到了 sess-b 而非第一个 session
    let err = handle_request(
        "workflow/resume",
        &json!({ "sessionId": "sess-b", "runId": "no-such-run" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("Failed to read workflow state"),
        "应分发到 sess-b 的 middleware 并报 read_state 失败，实际: {}",
        err.message
    );

    // 缺失 sessionId → -32602
    let err = handle_request(
        "workflow/resume",
        &json!({ "runId": "no-such-run" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报错，实际: {}",
        err.message
    );

    // session 不存在 → 明确错误（修复前可能误用第一个 session 的 middleware）
    let err = handle_request(
        "workflow/resume",
        &json!({ "sessionId": "no-such-session", "runId": "no-such-run" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "会话不存在应报 session not found，实际: {}",
        err.message
    );
}

// ── session/delete（标准 ACP，agentclientprotocol.com/protocol/v1/session-delete）──

/// 删除后：响应为空对象、线程从 store 移除（load_meta 报错）、活跃会话从
/// sessions 表清理。
#[tokio::test]
async fn test_delete_removes_thread_and_active_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp.path().to_str().unwrap();

    // 真实创建线程（id 即 session id）
    let thread_id = cfg
        .thread_store
        .create_thread(ThreadMeta::new(cwd))
        .await
        .unwrap();
    let sid = thread_id.clone();

    // 活跃会话登记（与 session/new 后的内存态一致）
    register_session_with_workflow(&mut sessions, &sid, cwd);

    let resp = handle_request(
        "session/delete",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/delete 应成功");

    // 标准响应为空对象
    assert_eq!(
        resp,
        serde_json::json!({}),
        "标准 session/delete 响应为 {{}}"
    );

    // 活跃会话已清理
    assert!(
        !sessions.contains_key(&sid),
        "删除后活跃会话应从 sessions 表移除"
    );

    // 线程已从 store 持久化删除（元数据不存在 + 列表不再包含）
    assert!(
        cfg.thread_store.load_meta(&sid).await.is_err(),
        "删除后线程元数据不应存在"
    );
    let remaining = cfg.thread_store.list_threads().await.unwrap();
    assert!(
        !remaining.iter().any(|m| m.id == sid),
        "删除后 session/list 不应再包含该线程"
    );
}

/// 删除不存在的线程：幂等成功（存储层不报错，历史不存在视为已删除）。
#[tokio::test]
async fn test_delete_unknown_session_is_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    let resp = handle_request(
        "session/delete",
        &json!({ "sessionId": "never-existed" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("删除不存在的会话应幂等成功");
    assert_eq!(resp, serde_json::json!({}));
}

/// 缺失 sessionId → -32602 Invalid params。
#[tokio::test]
async fn test_delete_missing_session_id_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    let err = handle_request(
        "session/delete",
        &json!({}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报 -32602，实际: {}",
        err.message
    );
}

// ── session/rename（标准 ACP；stdio 与 TUI 共用统一 host 注册于 requests.rs）──

/// 重命名成功：thread store 持久化标题 + `session/update` 通知携带
/// `SessionInfoUpdate.title` + 响应往返 `{sessionId, title}`。
#[tokio::test]
async fn test_rename_persists_title_and_pushes_session_info_update() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let mock = std::sync::Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let cwd = tmp.path().to_str().unwrap();

    // 真实创建线程（id 即 session id），与 session/new 后的持久层状态一致
    let sid = cfg
        .thread_store
        .create_thread(ThreadMeta::new(cwd))
        .await
        .unwrap();
    let new_title = "重构 ACP 协议".to_string();

    let resp = handle_request(
        "session/rename",
        &json!({ "sessionId": sid, "title": new_title }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/rename 应成功");

    // 标准响应往返
    assert_eq!(resp["sessionId"], sid, "响应 sessionId: {resp}");
    assert_eq!(resp["title"], new_title, "响应 title: {resp}");

    // 持久化：load_meta 标题已更新
    let meta = cfg.thread_store.load_meta(&sid).await.unwrap();
    assert_eq!(meta.title.as_deref(), Some(new_title.as_str()));

    // 通知：session/update 携带 SessionInfoUpdate.title，供标题栏与外部客户端刷新
    let (method, payload) = mock
        .notifications()
        .iter()
        .find(|(m, _)| m == "session/update")
        .cloned()
        .expect("rename 应推送 session/update 通知");
    assert_eq!(method, "session/update");
    assert_eq!(payload["sessionId"], sid);
    assert_eq!(payload["update"]["sessionUpdate"], "session_info_update");
    assert_eq!(payload["update"]["title"], new_title);
}

/// 缺失 sessionId → -32602 Invalid params。
#[tokio::test]
async fn test_rename_missing_session_id_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    let err = handle_request(
        "session/rename",
        &json!({ "title": "无 sessionId" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报 -32602，实际: {}",
        err.message
    );
}

/// 缺失 title → -32602 Invalid params。
#[tokio::test]
async fn test_rename_missing_title_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    let err = handle_request(
        "session/rename",
        &json!({ "sessionId": "some-session" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(
        err.message.contains("missing title"),
        "缺失 title 应报 -32602，实际: {}",
        err.message
    );
}

// ── M2 回归：进程内 session/delete 必须 shutdown LSP pool ────────────────────

/// 记录 shutdown 调用的 mock LSP pool。
struct MockLspPool {
    shutdown_calls: Arc<std::sync::atomic::AtomicU32>,
}

#[async_trait::async_trait]
impl peri_acp_types::ports::LspPoolPort for MockLspPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn shutdown(&self) {
        self.shutdown_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 删除活跃会话（带 lsp_pool）时必须在锁外 shutdown pool，与 stdio 路径一致，
/// 避免 LSP 服务器子进程/read task 残留（M2；此前进程内路径直接丢弃 pool）。
#[tokio::test]
async fn test_delete_active_session_shuts_down_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp.path().to_str().unwrap();

    // 真实创建线程（id 即 session id），与 delete 分支的 thread_store 删除对应
    let sid = cfg
        .thread_store
        .create_thread(ThreadMeta::new(cwd))
        .await
        .unwrap();

    let shutdown_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let pool: Arc<dyn peri_acp_types::ports::LspPoolPort> = Arc::new(MockLspPool {
        shutdown_calls: Arc::clone(&shutdown_calls),
    });

    // 构造带 lsp_pool 的活跃会话（其余字段与 register_session_with_workflow 一致）
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockWorkflowExecutor);
    let (notification_tx, _) = tokio::sync::broadcast::channel::<WorkflowTaskResult>(32);
    let mw = Arc::new(WorkflowMiddleware::new(
        executor,
        cwd,
        notification_tx,
        None,
    ));
    sessions.insert(
        sid.clone(),
        SessionState {
            session_id: sid.clone(),
            thread_id: sid.clone(),
            cwd: cwd.to_string(),
            history: Vec::new(),
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: Some(Arc::clone(&mw) as Arc<dyn WorkflowMiddlewarePort>),
            lsp_pool: Some(pool),
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            lease: crate::host::lease::WriterLease::acquired("default"),
        },
    );

    let resp = handle_request(
        "session/delete",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/delete 应成功");
    assert_eq!(resp, serde_json::json!({}));
    assert!(
        !sessions.contains_key(&sid),
        "删除后活跃会话应从 sessions 表移除"
    );
    assert_eq!(
        shutdown_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "删除活跃会话必须 shutdown LSP pool（M2）"
    );
}

// ── AvailableCommandsUpdate + 注册表回调重发（Slice 6 / DD-5 / Phase 6 A4）──

/// session/new 首发无 mcp 条目；MCP 发现完成后经「发现管线直接写入命令
/// 注册表（A3 `mark_source_completed`）→ **注册表 on_change（投影重建唯一
/// 触发源）**」重发，第二次通知含 mcp 条目（条目级 `_meta.periKind`；
/// update 级 `mcpSkillNames` 镜像键已退役，Phase 6 D1）；注册表
/// 内容变化（unregister）亦触发重发且投影收缩（Phase 6 A4：McpSkillRegistry
/// 挂点已删，命令面变更统一经注册表）。
#[tokio::test]
async fn test_available_commands_update_mcp_callback_resend() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap().to_string();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, &sid).await;

    // 首发：registry 尚未发现 → availableCommands 无 mcp 条目
    let notifications = transport.notifications();
    assert_eq!(notifications.len(), 1, "首发仅一条 session/update 通知");
    assert_eq!(notifications[0].0, "session/update");
    let update0 = &notifications[0].1["update"];
    assert_eq!(update0["sessionUpdate"], "available_commands_update");
    let commands0 = update0["availableCommands"].as_array().unwrap();
    assert!(
        commands0
            .iter()
            .all(|c| c["name"] != "mcp__demo__hello" && c["name"] != "demo:hello"),
        "首发不得含 mcp 条目"
    );

    // 变更命令注册表（A4 后 MCP 条目由发现管线直接写入，不再经
    // McpSkillRegistry on_change 对账——挂点已删）→ 注册表 on_change
    // 触发投影重发（A3 `mark_source_completed` 同语义）。
    let command_registry = cfg
        .session_manager
        .command_registry_for(&sid)
        .expect("session 应持有命令注册表");
    let token: peri_acp_types::mcp_skills::HandleToken = Arc::new(42u32);
    command_registry.mark_source_started("demo", token.clone());
    command_registry.mark_source_completed(
        "demo",
        token,
        vec![peri_acp_types::command::command_route::RouteEntry {
            fullname: "demo:hello".into(),
            aliases: Vec::new(),
            description: "MCP skill hello".into(),
            kind: peri_acp_types::command::command_route::CommandEntryKind::McpSkill,
            category: None,
            args_schema: None,
            handler: Arc::new(crate::session::command::AgentPassthrough),
            provenance: peri_acp_types::command::command_route::CommandProvenance {
                source: peri_acp_types::command::command_route::CommandSource::Mcp {
                    server: "demo".into(),
                },
                // 对齐生产语义（skill_discovery.rs `mcp_route_entries` 产出
                // Discovered；handler 为跨 crate 占位等价——peri-acp 无法
                // 引用 peri-middlewares 的 McpSkillReleaser，用
                // AgentPassthrough 占位，本用例只断言触发源 = 注册表
                // on_change，与 handler/lifecycle 无关）。
                lifecycle: peri_acp_types::command::command_route::CommandLifecycle::Discovered,
            },
        }],
    );

    // 回调经 tokio::spawn 异步发送 → 轮询短等待
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while transport.notifications().len() < 2 {
        assert!(std::time::Instant::now() < deadline, "等待重发通知超时");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // 稳定窗口：异步重发落袋后再等一拍，断言重发**恰一次**（注册表
    // on_change 只触发一次，不得重复重发——A5「重发恰一次断言不变」）
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        transport.notifications().len(),
        2,
        "mark_source_completed 应触发重发恰一次（首发 + 重发）"
    );

    // 静默断言（验收 13 ACP 半边）：除 available_commands_update 外无其它
    // 通知类型
    let notifications = transport.notifications();
    assert!(
        notifications.iter().all(|(m, p)| {
            m == "session/update" && p["update"]["sessionUpdate"] == "available_commands_update"
        }),
        "不得出现其它通知类型，实际: {:?}",
        notifications
            .iter()
            .map(|(m, p)| (m.as_str(), p["update"]["sessionUpdate"].clone()))
            .collect::<Vec<_>>()
    );

    let update1 = &notifications[1].1["update"];
    let commands1 = update1["availableCommands"].as_array().unwrap();
    let hello = commands1
        .iter()
        .find(|c| c["name"] == "demo:hello")
        .expect("第二次通知应含 mcp 条目（demo:hello 全名）");
    assert_eq!(
        hello["_meta"]["periKind"], "mcp_skill",
        "mcp 条目 kind 入条目级 _meta（mcpSkillNames 镜像键已退役）"
    );

    // 触发源 = 注册表：unregister 内置条目 → 重发且投影收缩（不再依赖
    // McpSkillRegistry 直接重发）
    let before = transport.notifications().len();
    assert!(
        command_registry.unregister("core:loop"),
        "unregister 应命中"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let shrunk = transport.notifications().iter().any(|(_, p)| {
            p["update"]["availableCommands"]
                .as_array()
                .map(|a| a.iter().all(|c| c["name"] != "loop"))
                .unwrap_or(false)
        });
        if shrunk {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "等待注册表 on_change 重发超时（before={before}）"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// session/load 与 session/new 同构（决策 B 扩展）：同样预热 MCP skill
/// 发现。pool 存在但无已连接 server（pending）时 prewarm 空跑不 panic、
/// 广播正常发出；已连接 server 的发现行为由 middleware 层单测覆盖
/// （`prewarm_discovery_triggers_idempotent_discovery`）。
#[tokio::test]
async fn test_session_load_prewarms_mcp_discovery_smoke() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp);
    cfg.session_manager.set_pending_caps(PeriCaps::default());
    cfg.mcp_pool = Some(Arc::new(peri_middlewares::mcp::McpClientPool::new_pending()));
    let mut legacy_meta = ThreadMeta::new(tmp.path().to_str().unwrap());
    legacy_meta.id = "s1".to_string();
    cfg.thread_store.create_thread(legacy_meta).await.unwrap();
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/load",
        &json!({ "sessionId": "s1", "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert!(
        result.get("modes").is_some(),
        "session/load 应返回 modes/configOptions"
    );
    // prewarm 空跑路径（pending pool 无已连接 server）不 panic，广播正常发出
    assert!(
        transport.notifications().iter().any(|(m, p)| {
            m == "session/update" && p["update"]["sessionUpdate"] == "available_commands_update"
        }),
        "session/load 应广播 available_commands_update"
    );
}

/// [回归测试] 冷启动加载同一 session 必须恢复创建时的 frozen prompt。
///
/// 历史问题：`session/load` 只恢复消息，却重新扫描当前 CLAUDE.md/skills/date；
/// OpenAI/Cursor 因而在恢复后的首请求看到“新 system + 旧 history”，首轮重建
/// cache prefix、后续轮才恢复命中。
#[tokio::test]
async fn test_session_load_cold_host_restores_original_frozen_prompt() {
    // Arrange
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "FROZEN_PROMPT_V1").unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config.clone(), provider.clone(), &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let created = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    let original_prompt = sessions[&session_id]
        .frozen
        .as_ref()
        .unwrap()
        .system_prompt()
        .to_string();
    let original_claude_md = sessions[&session_id]
        .frozen
        .as_ref()
        .unwrap()
        .claude_md()
        .unwrap()
        .to_string();
    assert!(original_claude_md.contains("FROZEN_PROMPT_V1"));
    cfg.thread_store
        .append_messages(
            &session_id,
            &[peri_acp_types::messages::BaseMessage::human(
                "existing history",
            )],
        )
        .await
        .unwrap();
    drop(cfg);
    drop(sessions);
    std::fs::write(tmp.path().join("CLAUDE.md"), "FROZEN_PROMPT_V2").unwrap();
    let restarted = make_server_config(peri_config, provider, &tmp);
    let mut restored_sessions = HashMap::new();
    // Act
    handle_request(
        "session/load",
        &json!({
            "sessionId": session_id,
            "cwd": tmp.path().to_str().unwrap(),
        }),
        &restarted,
        &mut restored_sessions,
        &transport,
    )
    .await
    .unwrap();
    // Assert
    let restored_prompt = restored_sessions[&session_id]
        .frozen
        .as_ref()
        .unwrap()
        .system_prompt();
    let restored_claude_md = restored_sessions[&session_id]
        .frozen
        .as_ref()
        .unwrap()
        .claude_md()
        .unwrap();
    assert_eq!(restored_prompt, original_prompt);
    assert_eq!(restored_claude_md, original_claude_md);
    assert!(restored_claude_md.contains("FROZEN_PROMPT_V1"));
    assert!(!restored_claude_md.contains("FROZEN_PROMPT_V2"));
}

#[tokio::test]
async fn test_session_load_future_frozen_snapshot_fails_without_overwrite() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config.clone(), provider.clone(), &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let created = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    let future_snapshot = r#"{"version":999,"data":{"must":"remain"}}"#;
    let snapshot_path = tmp
        .path()
        .join("threads")
        .join(&session_id)
        .join("frozen.json");
    tokio::fs::write(&snapshot_path, future_snapshot)
        .await
        .unwrap();
    drop(cfg);
    drop(sessions);

    let restarted = make_server_config(peri_config, provider, &tmp);
    let mut restored_sessions = HashMap::new();
    let error = handle_request(
        "session/load",
        &json!({
            "sessionId": session_id,
            "cwd": tmp.path().to_str().unwrap(),
        }),
        &restarted,
        &mut restored_sessions,
        &transport,
    )
    .await
    .unwrap_err();

    assert!(error
        .message
        .contains("unsupported frozen snapshot version"));
    assert!(restored_sessions.is_empty());
    assert!(restarted.session_manager.get_session(&session_id).is_none());
    assert_eq!(
        tokio::fs::read_to_string(snapshot_path).await.unwrap(),
        future_snapshot,
        "future snapshot must be preserved for a newer binary"
    );
}

/// 核对点 8 覆盖缺口（P2-3）：`set_pending_caps` 带 `ui_commands` 明细 →
/// session/new → 断言 ui 面板条目随 caps 明细出现（name = Level1 裸名、
/// `periKind=panel`、`periCategory=ui`、alias 注入），未协商的默认明细不出现。
#[tokio::test]
async fn test_available_commands_update_ui_entries_from_caps_details() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    // 协商 caps：仅上送两条自定义 ui 明细（大写 name 验证小写归一 + alias 透传）
    cfg.session_manager.set_pending_caps(PeriCaps {
        ui_commands: vec![
            peri_acp_types::command::command_route::UiCommandSpec {
                name: "gallery".into(),
                description: "Open the gallery panel".into(),
                aliases: vec!["gal".into()],
                args: None,
            },
            peri_acp_types::command::command_route::UiCommandSpec {
                name: "Zoom".into(),
                description: "Zoom panel".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    });
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, sid).await;

    let notifications = transport.notifications();
    assert_eq!(notifications.len(), 1, "首发仅一条 session/update 通知");
    let update = &notifications[0].1["update"];
    assert_eq!(update["sessionUpdate"], "available_commands_update");
    let commands = update["availableCommands"].as_array().unwrap();
    let by_name = |n: &str| {
        commands
            .iter()
            .find(|c| c["name"] == n)
            .unwrap_or_else(|| panic!("条目 {n} 应存在: {:?}", commands))
    };
    // ui 条目随 caps 明细出现：name = 裸名 / periKind=panel / periLevel=1 /
    // periCategory=ui（Level1 域归属只经条目级 kind 下发，name 不带域前缀）
    let gallery = by_name("gallery");
    assert_eq!(
        gallery["_meta"]["periKind"], "panel",
        "ui 条目 kind = panel"
    );
    assert_eq!(gallery["_meta"]["periLevel"], 1, "core/ui 域 level = 1");
    assert_eq!(gallery["_meta"]["periCategory"], "ui");
    assert_eq!(
        gallery["_meta"]["periAliases"],
        json!(["gal"]),
        "caps 明细 alias 应透传注入"
    );
    assert_eq!(
        by_name("zoom")["_meta"]["periKind"],
        "panel",
        "name 应小写归一（Zoom → zoom）"
    );
    // 未协商的默认明细不得出现（门控反转：只广播客户端声明的明细）——
    // panel 条目恰为协商的 gallery/zoom 两条（help 未协商不注册；core:clear
    // 的内置裸名条目 kind=command，不构成 panel）
    let panels: Vec<&str> = commands
        .iter()
        .filter(|c| c["_meta"]["periKind"] == "panel")
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        panels,
        ["gallery", "zoom"],
        "仅协商的 ui 明细注册为 panel 条目: {panels:?}"
    );
    // 基座内置仍在（注册表投影，Level1 裸名）
    assert!(
        commands.iter().any(|c| c["name"] == "compact"),
        "基座内置条目应保留"
    );
}

/// 防引用环断言：注册回调后清零外部强引用（持 Weak 观察）→ upgrade 必须
/// 变 None——注册表 on_change 回调不得捕获注册表 Arc 强引用（重发闭包只
/// 持 Weak；Phase 6 A4 后 McpSkillRegistry 不再经本函数挂回调）。
#[tokio::test]
async fn test_available_commands_update_callbacks_do_not_hold_strong_refs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let _cfg = make_server_config(peri_config, provider, &tmp);
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let command_registry = Arc::new(peri_acp_types::command_registry::CommandRegistry::new());
    let weak_cmd = Arc::downgrade(&command_registry);

    crate::host::notify::send_available_commands_update(
        &transport_dyn,
        "anti-cycle-session",
        &PeriCaps::all_enabled(),
        Some(command_registry), // 唯一强引用移入函数，返回后即释放
        false,
    )
    .await;

    // 外部强引用清零后：注册表 on_change 回调只持 Weak(注册表)（重发闭包），
    // 注册表必须能被回收（无引用环）。
    assert!(
        weak_cmd.upgrade().is_none(),
        "注册表应可回收（on_change 回调不得捕获强引用）；upgrade 应为 None"
    );
    assert_eq!(weak_cmd.strong_count(), 0, "注册表 strong_count 应归零");
}

#[tokio::test]
async fn test_mcp_list_requires_negotiated_oauth_capability() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-test-placeholder",
        "gpt-test",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp);
    cfg.session_manager.set_pending_caps(PeriCaps::default());
    cfg.mcp_pool = Some(Arc::new(peri_middlewares::mcp::McpClientPool::new_pending()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let error = handle_request(
        "mcp/list",
        &json!({}),
        &cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32601);
    assert_eq!(error.message, "peri.oauth capability not negotiated");
}

#[tokio::test]
async fn test_mcp_list_returns_bounded_safe_empty_snapshot_when_negotiated() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-test-placeholder",
        "gpt-test",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp);
    cfg.session_manager.set_pending_caps(PeriCaps {
        oauth: true,
        ..PeriCaps::default()
    });
    cfg.mcp_pool = Some(Arc::new(peri_middlewares::mcp::McpClientPool::new_pending()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let response = handle_request(
        "mcp/list",
        &json!({}),
        &cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(response, json!({ "servers": [] }));
    assert!(response.to_string().find("url").is_none());
    assert!(response.to_string().find("error").is_none());
}

#[tokio::test]
async fn test_oauth_start_rejects_missing_flow_id_before_spawning() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-test-placeholder",
        "gpt-test",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp);
    cfg.session_manager.set_pending_caps(PeriCaps {
        oauth: true,
        ..PeriCaps::default()
    });
    cfg.mcp_pool = Some(Arc::new(peri_middlewares::mcp::McpClientPool::new_pending()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let error = handle_request(
        "mcp/oauth_start",
        &json!({ "server_name": "docs" }),
        &cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "missing 'flow_id'");
}

#[tokio::test]
async fn test_safe_oauth_capability_rejects_callback_secrets_over_acp() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-test-placeholder",
        "gpt-test",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp);
    cfg.session_manager.set_pending_caps(PeriCaps {
        oauth: true,
        ..PeriCaps::default()
    });
    cfg.mcp_pool = Some(Arc::new(peri_middlewares::mcp::McpClientPool::new_pending()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let error = handle_request(
        "mcp/oauth_callback",
        &json!({
            "server_name": "docs",
            "flow_id": "flow-1",
            "code": "secret-code",
            "state": "secret-state"
        }),
        &cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32601);
    assert!(!error.message.contains("secret-code"));
    assert!(!error.message.contains("secret-state"));
}

// ── Phase 6 B3：plugin install/uninstall RPC 级投影断言（P2-2）──────────────

/// 测试期重定向 `$HOME`（`handle_request` 内 `claude_dir` 由
/// `dirs_next::home_dir()` 计算，`refresh_plugin_command_entries` 经真实
/// `load_enabled_plugins` 重载）；Drop 时还原。进程级 env 态 →
/// 本组用例全部 `#[serial]`（与 store_test 同组互斥）。
/// Windows 下 `dirs_next::home_dir()` 读 `USERPROFILE`（`HOME` 仅 Unix 生效），
/// 两个变量同步设置以保证隔离。
struct HomeDirGuard {
    home: Option<std::ffi::OsString>,
    #[cfg(windows)]
    userprofile: Option<std::ffi::OsString>,
}

impl HomeDirGuard {
    fn set(path: &Path) -> Self {
        let home = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        #[cfg(windows)]
        {
            let prev = std::env::var_os("USERPROFILE");
            std::env::set_var("USERPROFILE", path);
            Self {
                home,
                userprofile: prev,
            }
        }
        #[cfg(not(windows))]
        Self { home }
    }
}

fn restore_env_var(slot: &mut Option<std::ffi::OsString>, name: &str) {
    match slot.take() {
        Some(v) => std::env::set_var(name, v),
        None => std::env::remove_var(name),
    }
}

impl Drop for HomeDirGuard {
    fn drop(&mut self) {
        restore_env_var(&mut self.home, "HOME");
        #[cfg(windows)]
        restore_env_var(&mut self.userprofile, "USERPROFILE");
    }
}

/// 可编程 `PluginManagerPort` mock：install / uninstall 结果注入，其余方法
/// 空实现（install/uninstall 分支仅消费 install/uninstall + snapshot +
/// cache_dir；`unstable_event` caps 默认关闭，push_plugin_* 不发通知）。
struct MockPluginManager {
    install_result: std::sync::Mutex<Result<InstalledPlugin, String>>,
    uninstall_result: std::sync::Mutex<Result<(), String>>,
}

impl MockPluginManager {
    fn install_ok(id: &str) -> Self {
        Self {
            install_result: std::sync::Mutex::new(Ok(InstalledPlugin {
                id: id.to_string(),
                name: id.to_string(),
                version: "1.0.0".into(),
                marketplace: "test-mkt".into(),
                install_path: PathBuf::from("/tmp/mock-install"),
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            })),
            uninstall_result: std::sync::Mutex::new(Ok(())),
        }
    }
}

#[async_trait]
impl PluginManagerPort for MockPluginManager {
    async fn install(
        &self,
        _name: &str,
        _marketplace: &str,
        _scope: InstallScope,
        _cache_dir: &Path,
        _claude_dir: &Path,
    ) -> Result<InstalledPlugin, String> {
        self.install_result.lock().unwrap().clone()
    }

    async fn uninstall(&self, _plugin_id: &str, _claude_dir: &Path) -> Result<(), String> {
        self.uninstall_result.lock().unwrap().clone()
    }

    fn set_enabled(
        &self,
        _plugin_id: &str,
        _scope: InstallScope,
        _claude_dir: &Path,
        _enable: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    fn cache_dir(&self) -> PathBuf {
        PathBuf::from("/tmp/mock-cache")
    }

    async fn update(
        &self,
        _plugin_id: &str,
        _cache_dir: &Path,
        _claude_dir: &Path,
    ) -> Result<InstalledPlugin, String> {
        Err("mock: unused".into())
    }

    async fn refresh_marketplace(&self, _name: &str) -> Result<usize, String> {
        Err("mock: unused".into())
    }

    async fn cleanup(&self, _claude_dir: &Path) -> Result<usize, String> {
        Err("mock: unused".into())
    }

    async fn marketplace_add(&self, _source: &str) -> Result<String, String> {
        Err("mock: unused".into())
    }

    async fn marketplace_remove(&self, _name: &str) -> Result<(), String> {
        Err("mock: unused".into())
    }

    async fn marketplace_update(&self, _name: &str) -> Result<String, String> {
        Err("mock: unused".into())
    }

    fn marketplace_snapshot(&self) -> Value {
        json!({})
    }

    fn snapshot(&self, _claude_dir: &Path) -> Vec<PluginSnapshotEntry> {
        vec![]
    }
}

/// 在 `{home}/.claude` 布置一个启用中的插件 `ecc`（含命令
/// `commands/deploy.md`），供 `refresh_plugin_command_entries` 重载出
/// `plugin:ecc:deploy`（与 peri-middlewares loader_test 的磁盘形态同构）。
fn seed_plugin_ecc(home: &Path) {
    let claude_dir = home.join(".claude");
    let plugin_dir = claude_dir.join("plugins").join("ecc");
    // 命令文件相对插件根目录（extract_commands: base_dir.join(path)）
    std::fs::create_dir_all(plugin_dir.join("commands")).unwrap();
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"ecc","version":"1.0.0","commands":[{"path":"commands/deploy.md"}]}"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("commands").join("deploy.md"),
        "---\ndescription: Deploy to prod\n---\nBody",
    )
    .unwrap();
    std::fs::create_dir_all(claude_dir.join("plugins")).unwrap();
    let installed_json =
        serde_json::to_string(&peri_middlewares::plugin::types::InstalledPlugins {
            version: 2,
            plugins: vec![InstalledPlugin {
                id: "ecc@test-mkt".into(),
                name: "ecc".into(),
                version: "1.0.0".into(),
                marketplace: "test-mkt".into(),
                install_path: plugin_dir,
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            }],
        })
        .unwrap();
    std::fs::write(
        claude_dir.join("plugins").join("installed_plugins.json"),
        installed_json,
    )
    .unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"enabledPlugins":["ecc@test-mkt"]}"#,
    )
    .unwrap();
}

/// 等待 `session/update` 通知达到目标条数（on_change 经 tokio::spawn
/// 异步发送 → 轮询短等待，对齐 A5 重发测试先例）。仅计数 session/update：
/// 未协商 caps 回退 all_enabled（`unstable_event: true`），install/uninstall
/// 还会发 `peri/unstable_event`（plugin-action-result / plugin-snapshot）。
fn session_update_count(transport: &MockTransport) -> usize {
    transport
        .notifications()
        .iter()
        .filter(|(m, _)| m == "session/update")
        .count()
}

async fn wait_for_session_updates(transport: &MockTransport, target: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while session_update_count(transport) < target {
        assert!(
            std::time::Instant::now() < deadline,
            "等待通知超时（target={target}）"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // 稳定窗口：异步重发落袋后再等一拍（防迟到的重复重发漏判）
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

fn plugin_entries(registry: &peri_acp_types::command::CommandRegistry) -> Vec<String> {
    registry
        .snapshot()
        .iter()
        .filter(|e| e.fullname.to_lowercase().starts_with("plugin:"))
        .map(|e| e.fullname.clone())
        .collect()
}

/// B3 成功分支（install 调用路径 :907）：mock install 成功 + 磁盘重载出
/// `plugin:ecc:deploy` → 注册表投影含新插件命令（provenance 剥离前缀，
/// P0-1 回归）+ 注册表 on_change 触发投影推送**恰一次**。
#[tokio::test]
#[serial]
async fn test_plugin_install_refreshes_plugin_domain_and_pushes_once() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home_guard = HomeDirGuard::set(&home);
    seed_plugin_ecc(&home);

    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp);
    cfg.plugin_manager = Arc::new(MockPluginManager::install_ok("ecc"));
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap().to_string();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, &sid).await;
    assert_eq!(
        transport.notifications().len(),
        1,
        "session/new response 后首发仅 available_commands_update 一条"
    );
    let registry = cfg
        .session_manager
        .command_registry_for(&sid)
        .expect("session 应持有命令注册表");
    assert!(
        plugin_entries(&registry).is_empty(),
        "初始 plugin 域应为空（无插件命令）"
    );

    // Act：plugin/install（mock 成功）
    let resp = handle_request(
        "plugin/install",
        &json!({
            "sessionId": sid,
            "name": "ecc",
            "marketplace": "test-mkt",
        }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(resp["success"], true, "install RPC 应成功");
    assert_eq!(resp["plugin"], "ecc");

    // Assert ①：注册表投影含新插件命令，provenance 剥离 plugin: 前缀
    let entries = plugin_entries(&registry);
    assert_eq!(entries, vec!["plugin:ecc:deploy"]);
    let entry = registry
        .snapshot()
        .into_iter()
        .find(|e| e.fullname == "plugin:ecc:deploy")
        .expect("插件命令应已注册");
    use peri_acp_types::command::command_route::{
        CommandEntryKind, CommandLifecycle, CommandSource as RouteCommandSource,
    };
    assert_eq!(entry.kind, CommandEntryKind::Command);
    assert_eq!(entry.provenance.lifecycle, CommandLifecycle::Connected);
    match &entry.provenance.source {
        RouteCommandSource::Plugin { name } => {
            assert_eq!(name, "ecc", "plugin: 前缀必须剥离，实际: {name}");
        }
        other => panic!("source 应为 Plugin，实际: {other:?}"),
    }

    // Assert ②：注册表 on_change → 投影推送恰一次（首发 + 重发）
    wait_for_session_updates(&transport, 2).await;
    let notifications = transport.notifications();
    let updates: Vec<_> = notifications
        .iter()
        .filter(|(m, _)| m == "session/update")
        .collect();
    assert_eq!(
        updates.len(),
        2,
        "install 后注册表 on_change 应触发投影重发恰一次"
    );
    let commands = updates[1].1["update"]["availableCommands"]
        .as_array()
        .expect("重发载荷应含 availableCommands");
    assert!(
        commands.iter().any(|c| c["name"] == "plugin:ecc:deploy"),
        "投影推送应含插件命令条目"
    );
}

/// B3 失败分支（install 调用路径 :907）：磁盘重载失败（installed_plugins.json
/// 非法 JSON → `load_enabled_plugins` Err）→ 保留空 plugin 域 + 告警，
/// **不阻塞 RPC 回包**（`{success: true}` 仍回），plugin 域无变化不触发推送。
#[tokio::test]
#[serial]
async fn test_plugin_install_reload_failure_keeps_domain_empty_and_does_not_block_rpc() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(claude_dir.join("plugins")).unwrap();
    let _home_guard = HomeDirGuard::set(&home);
    // 重载失败形态：installed_plugins.json 内容非法
    std::fs::write(
        claude_dir.join("plugins").join("installed_plugins.json"),
        "{invalid json",
    )
    .unwrap();

    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp);
    cfg.plugin_manager = Arc::new(MockPluginManager::install_ok("ecc"));
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap().to_string();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, &sid).await;
    let registry = cfg
        .session_manager
        .command_registry_for(&sid)
        .expect("session 应持有命令注册表");

    // Act：plugin/install —— 重载失败不得阻塞回包
    let resp = handle_request(
        "plugin/install",
        &json!({
            "sessionId": sid,
            "name": "ecc",
            "marketplace": "test-mkt",
        }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(resp["success"], true, "重载失败不得阻塞 RPC 回包");

    // Assert：plugin 域保持空 + 无内容变化 → 无投影推送（unstable_event
    // 通知 `peri/unstable_event` 与投影推送 `session/update` 相互独立）
    assert!(
        plugin_entries(&registry).is_empty(),
        "重载失败 → 保留空 plugin 域（过时条目不得残留）"
    );
    // 稳定窗口：若存在异步重发也已落袋（重载失败路径 reconcile 无内容
    // 变化，不应触发任何 on_change）
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        session_update_count(&transport),
        1,
        "plugin 域无内容变化，不得触发投影推送（仅首发一条）"
    );
}

/// B3 注销分支（uninstall 调用路径 :969）：install 预置 plugin 域条目 →
/// 磁盘 enabledPlugins 清空 → uninstall 成功 → stale 条目按名注销，
/// plugin 域为空，RPC 仍 success。
#[tokio::test]
#[serial]
async fn test_plugin_uninstall_removes_stale_plugin_entries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home_guard = HomeDirGuard::set(&home);
    seed_plugin_ecc(&home);

    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp);
    cfg.plugin_manager = Arc::new(MockPluginManager::install_ok("ecc"));
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap().to_string();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, &sid).await;
    let registry = cfg
        .session_manager
        .command_registry_for(&sid)
        .expect("session 应持有命令注册表");

    // 预置：install 成功 → plugin 域含 plugin:ecc:deploy
    handle_request(
        "plugin/install",
        &json!({
            "sessionId": sid,
            "name": "ecc",
            "marketplace": "test-mkt",
        }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(
        plugin_entries(&registry),
        vec!["plugin:ecc:deploy"],
        "install 预置失败"
    );

    // 卸载后磁盘重载面清空（enabledPlugins 空 → 无插件）
    std::fs::write(
        home.join(".claude").join("settings.json"),
        r#"{"enabledPlugins":[]}"#,
    )
    .unwrap();

    // Act：plugin/uninstall（mock 成功）
    let resp = handle_request(
        "plugin/uninstall",
        &json!({
            "sessionId": sid,
            "pluginId": "ecc@test-mkt",
        }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(resp["success"], true, "uninstall RPC 应成功");

    // Assert：stale 条目按名注销 → plugin 域空
    assert!(
        plugin_entries(&registry).is_empty(),
        "uninstall 后 stale 插件命令应全部注销"
    );
    // 通知：首发 + install on_change + uninstall 注销 on_change，各恰一次
    wait_for_session_updates(&transport, 3).await;
    assert_eq!(
        session_update_count(&transport),
        3,
        "install/uninstall 各触发一次投影推送（首发 + 2 次重发）"
    );
}
