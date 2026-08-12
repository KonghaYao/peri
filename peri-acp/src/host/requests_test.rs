use std::{
    collections::{BTreeMap, HashMap},
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
};

use crate::provider::{PeriConfig, ProviderConfig, ProviderModels};
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use async_trait::async_trait;
use peri_acp_types::ports::WorkflowMiddlewarePort;
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::thread::ThreadMeta;
use peri_agent::thread::FilesystemThreadStore;
use peri_middlewares::hitl::shared_mode::{PermissionMode, SharedPermissionMode};
use peri_middlewares::workflow::WorkflowMiddleware;
use peri_workflow::protocol::{AgentRunParams, AgentRunResult, Usage};
use peri_workflow::registry::{WorkflowRun, WorkflowRunStatus, WorkflowTaskResult};
use peri_workflow::runner::AgentExecutor;
use serde_json::{json, Value};

use super::*;
use crate::provider::LlmProvider;

// ── Mock AcpTransport ─────────────────────────────────────────────────────────

/// 丢弃所有发送操作的 mock transport
struct MockTransport;

#[async_trait]
impl crate::transport::AcpTransport for MockTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
    }
    async fn send_notification(&self, _method: &str, _params: Value) -> Result<(), AcpError> {
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
        // 注入真实 TaskManager 工厂：cancel-bg-task 回归测试依赖 registry 簿记
        Some(Arc::new(|| {
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
                as Arc<dyn peri_acp_types::tasks::TaskManager>
        })),
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
    );
    AcpServerConfig {
        provider: Arc::new(parking_lot::RwLock::new(provider)),
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        cron_scheduler: None,
        mcp_pool: None,
        channel_state: None,
        plugin_skill_roots: Vec::new(),
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
        config_path: tmp.path().join("test_config.json"),
        session_manager,
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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());

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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());

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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
    let target_id = sessions.get(&sid).unwrap().history[0]
        .id()
        .as_uuid()
        .to_string();

    let result = handle_request(
        "session/rewind",
        &json!({ "sessionId": sid, "target_message_id": target_id }),
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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

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
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

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
