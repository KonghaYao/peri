//! Tests for session create handlers（H1：load/resume/fork 分支会话级 LSP 池）。
//!
//! stdio handler 的 `Responder`/`ConnectionTo` 由 agent-client-protocol 内部
//! 构造、无法在测试中直接实例化，故经双端 builder 驱动 handler：
//! agent 端 `Agent.builder().connect_to(channel_b)` 作为 server（spawn 运行，
//! 服务器 future 在连接关闭前不会结束，不 await），client 端
//! `Client.builder().connect_with(channel_a, main_fn)` 经 `block_task()` 等待
//! 响应（单端 connect_with 时对端 channel 无人消费消息，请求/响应无法回环）。

use std::collections::BTreeMap;
use std::sync::Arc;

use agent_client_protocol::{
    schema::v1::{
        DeleteSessionRequest, DeleteSessionResponse, ForkSessionRequest, ForkSessionResponse,
        LoadSessionRequest, LoadSessionResponse, ResumeSessionRequest, ResumeSessionResponse,
    },
    Agent, Channel, Client, ConnectionTo,
};
use parking_lot::RwLock;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::lsp::LspServerConfig;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::ports::{SkillsPort, ToolSearchPort};
use peri_acp_types::store::ThreadStore;
use peri_agent::thread::FilesystemThreadStore;

use super::*;
use crate::host::stdio::session::control;
use crate::provider::{LlmProvider, PeriConfig, ProviderConfig, ProviderModels};
use crate::session::SessionManager;

// ── 辅助：构造测试用 StdioContext（仿 init.rs 装配 + requests_test.rs 配置） ──

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
        models: ProviderModels {
            sonnet: model.to_string(),
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

fn make_lsp_config() -> LspServerConfig {
    LspServerConfig {
        name: "test-lsp".to_string(),
        command: "true".to_string(),
        args: Vec::new(),
        env: None,
        extension_to_language: std::collections::HashMap::new(),
        initialization_options: None,
        disabled: None,
        max_restarts: None,
        startup_timeout: None,
        source: None,
    }
}

fn make_stdio_context(
    tmp: &tempfile::TempDir,
    lsp_servers: Vec<LspServerConfig>,
) -> Arc<StdioContext> {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let permission_mode = peri_middlewares::hitl::shared_mode::SharedPermissionMode::new(
        peri_middlewares::hitl::shared_mode::PermissionMode::Bypass,
    );
    let cron_scheduler: Arc<dyn CronSchedulerPort> = Arc::new(
        peri_middlewares::cron::CronSchedulerPortHandle(Arc::new(parking_lot::Mutex::new(
            peri_middlewares::cron::CronScheduler::new(tokio::sync::mpsc::unbounded_channel().0),
        ))),
    );
    let tool_search_index: Arc<dyn ToolSearchPort> =
        Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new());
    let skills: Arc<dyn SkillsPort> = Arc::new(peri_middlewares::host_ports::SkillsProvider);
    let workflow_middleware_factory =
        peri_middlewares::assembly::default_workflow_middleware_factory();
    let thread_store: Arc<dyn ThreadStore> =
        Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
    let shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn peri_agent::tools::BaseTool>>>> =
        Arc::new(RwLock::new(BTreeMap::new()));

    let session_manager = SessionManager::new(
        thread_store.clone(),
        provider.clone(),
        Arc::new(peri_config.clone()),
        permission_mode.clone(),
        None,
        Some(cron_scheduler.clone()),
        Some(Arc::new(|| {
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
                as Arc<dyn peri_acp_types::tasks::TaskManager>
        })),
        skills.clone(),
    );

    Arc::new(StdioContext {
        provider: Arc::new(RwLock::new(provider)),
        peri_config: RwLock::new(peri_config),
        permission_mode,
        cron_scheduler,
        mcp_pool: None,
        channel_state: None,
        plugin_skill_roots: Vec::new(),
        plugin_agent_dirs: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        plugin_lsp_servers: lsp_servers,
        tool_search_index,
        skills,
        shared_tools,
        workflow_middleware_factory,
        sessions: RwLock::new(std::collections::HashMap::new()),
        thread_store: thread_store.clone(),
        controller: Arc::new(peri_controller::Controller::new(thread_store.clone())),
        langfuse_session: None,
        session_manager,
    })
}

// ── 测试 ──────────────────────────────────────────────────────────────────

/// load 分支与 session/new 一致创建会话级 LSP 池（H1：跨 turn 复用；
/// 此前置 None 走临时实例路径，LSP 服务器子进程跨 turn 泄漏）。
#[tokio::test]
async fn test_load_creates_session_scoped_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = make_stdio_context(&tmp, vec![make_lsp_config()]);
    let (channel_a, channel_b) = Channel::duplex();

    let ctx_for_handler = Arc::clone(&ctx);
    let server = Agent
        .builder()
        .on_receive_request(
            {
                let ctx = ctx_for_handler;
                async move |req: LoadSessionRequest, responder, cx: ConnectionTo<Client>| {
                    handle_load(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(channel_b);
    let _server_task = tokio::spawn(server);

    let result = Client
        .builder()
        .connect_with(
            channel_a,
            async move |cx: ConnectionTo<Agent>| -> Result<(), agent_client_protocol::Error> {
                let _resp: LoadSessionResponse = cx
                    .send_request(LoadSessionRequest::new("load-test-session", tmp.path()))
                    .block_task()
                    .await?;
                Ok(())
            },
        )
        .await;

    assert!(result.is_ok(), "handle_load 应成功: {result:?}");
    let sessions = ctx.sessions.read();
    let info = sessions
        .get("load-test-session")
        .expect("load 应注册 session");
    assert!(
        info.lsp_pool.is_some(),
        "load 分支应创建会话级 LSP 池（H1 跨 turn 复用）"
    );
}

/// resume 分支（新 session）同样创建会话级 LSP 池。
#[tokio::test]
async fn test_resume_creates_session_scoped_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = make_stdio_context(&tmp, vec![make_lsp_config()]);
    let (channel_a, channel_b) = Channel::duplex();

    let ctx_for_handler = Arc::clone(&ctx);
    let server = Agent
        .builder()
        .on_receive_request(
            {
                let ctx = ctx_for_handler;
                async move |req: ResumeSessionRequest, responder, cx: ConnectionTo<Client>| {
                    handle_resume(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(channel_b);
    let _server_task = tokio::spawn(server);

    let result = Client
        .builder()
        .connect_with(
            channel_a,
            async move |cx: ConnectionTo<Agent>| -> Result<(), agent_client_protocol::Error> {
                let _resp: ResumeSessionResponse = cx
                    .send_request(ResumeSessionRequest::new("resume-test-session", tmp.path()))
                    .block_task()
                    .await?;
                Ok(())
            },
        )
        .await;

    assert!(result.is_ok(), "handle_resume 应成功: {result:?}");
    let sessions = ctx.sessions.read();
    let info = sessions
        .get("resume-test-session")
        .expect("resume 应注册 session");
    assert!(
        info.lsp_pool.is_some(),
        "resume 分支应创建会话级 LSP 池（H1 跨 turn 复用）"
    );
}

/// fork 分支创建的新 session 同样携带会话级 LSP 池。
#[tokio::test]
async fn test_fork_creates_session_scoped_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = make_stdio_context(&tmp, vec![make_lsp_config()]);
    // 前置：注册带非空历史的 source session（fork 要求 source history 非空）
    {
        let mut sessions = ctx.sessions.write();
        sessions.insert(
            "fork-source-session".to_string(),
            SessionInfo {
                session_id: "fork-source-session".to_string(),
                thread_id: "fork-source-session".to_string(),
                cwd: tmp.path().to_string_lossy().into_owned(),
                history: vec![BaseMessage::human("hello")],
                cancel_token: None,
                frozen: None,
                agent_pool: crate::session::agent_pool::AgentPool::new(),
                workflow_middleware: None,
                lsp_pool: None,
            },
        );
    }

    let (channel_a, channel_b) = Channel::duplex();
    let ctx_for_handler = Arc::clone(&ctx);
    let server = Agent
        .builder()
        .on_receive_request(
            {
                let ctx = ctx_for_handler;
                async move |req: ForkSessionRequest, responder, cx: ConnectionTo<Client>| {
                    handle_fork(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(channel_b);
    let _server_task = tokio::spawn(server);

    let result = Client
        .builder()
        .connect_with(
            channel_a,
            async move |cx: ConnectionTo<Agent>| -> Result<(), agent_client_protocol::Error> {
                let resp: ForkSessionResponse = cx
                    .send_request(ForkSessionRequest::new("fork-source-session", tmp.path()))
                    .block_task()
                    .await?;
                let _ = resp.session_id; // 新 session id 由 store 生成
                Ok(())
            },
        )
        .await;

    assert!(result.is_ok(), "handle_fork 应成功: {result:?}");
    let sessions = ctx.sessions.read();
    let forked = sessions
        .iter()
        .find(|(id, _)| id.as_str() != "fork-source-session")
        .map(|(_, s)| s)
        .expect("fork 应注册新 session");
    assert!(
        forked.lsp_pool.is_some(),
        "fork 分支应创建会话级 LSP 池（H1 跨 turn 复用）"
    );
}

/// 无 LSP 配置时 load 分支不创建池（与 create_session_lsp_pool 的
/// None 语义一致，装配面不注册 LSP 中间件）。
#[tokio::test]
async fn test_load_without_lsp_config_has_no_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = make_stdio_context(&tmp, vec![]);
    let (channel_a, channel_b) = Channel::duplex();

    let ctx_for_handler = Arc::clone(&ctx);
    let server = Agent
        .builder()
        .on_receive_request(
            {
                let ctx = ctx_for_handler;
                async move |req: LoadSessionRequest, responder, cx: ConnectionTo<Client>| {
                    handle_load(&ctx, req, responder, cx).await
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(channel_b);
    let _server_task = tokio::spawn(server);

    let result = Client
        .builder()
        .connect_with(
            channel_a,
            async move |cx: ConnectionTo<Agent>| -> Result<(), agent_client_protocol::Error> {
                let _resp: LoadSessionResponse = cx
                    .send_request(LoadSessionRequest::new("no-lsp-session", tmp.path()))
                    .block_task()
                    .await?;
                Ok(())
            },
        )
        .await;

    assert!(result.is_ok(), "handle_load 应成功: {result:?}");
    let sessions = ctx.sessions.read();
    let info = sessions.get("no-lsp-session").expect("load 应注册 session");
    assert!(
        info.lsp_pool.is_none(),
        "无 LSP 配置时不应创建池（与 new 分支一致）"
    );
}

// ── session/delete（标准 ACP，agentclientprotocol.com/protocol/v1/session-delete）──

/// 双端 builder 驱动：客户端发 DeleteSessionRequest，验证空响应 + 线程持久化删除。
#[tokio::test]
async fn test_delete_removes_thread_and_responds_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = make_stdio_context(&tmp, Vec::new());
    let (channel_a, channel_b) = Channel::duplex();

    // 先创建线程（session/new 等价物），取得真实 thread id
    let meta = peri_acp_types::thread::ThreadMeta::new(tmp.path().to_str().unwrap());
    let thread_id = ctx.thread_store.create_thread(meta).await.unwrap();
    let sid = thread_id.clone();

    let ctx_for_handler = Arc::clone(&ctx);
    let server = Agent
        .builder()
        .on_receive_request(
            {
                let ctx = ctx_for_handler;
                async move |req: DeleteSessionRequest, responder, _cx: ConnectionTo<Client>| {
                    control::handle_delete(&ctx, &req.session_id.0).await;
                    let _ = responder.respond(DeleteSessionResponse::new());
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(channel_b);
    let _server_task = tokio::spawn(server);

    // 闭包外 clone：async move 会整体捕获 sid
    let sid_for_req = sid.clone();
    let result = Client
        .builder()
        .connect_with(
            channel_a,
            async move |cx: ConnectionTo<Agent>| -> Result<(), agent_client_protocol::Error> {
                let _resp: DeleteSessionResponse = cx
                    .send_request(DeleteSessionRequest::new(sid_for_req))
                    .block_task()
                    .await?;
                Ok(())
            },
        )
        .await;

    assert!(result.is_ok(), "handle_delete 应成功: {result:?}");
    // 线程已持久化删除（元数据消失）
    assert!(
        ctx.thread_store.load_meta(&sid).await.is_err(),
        "删除后线程元数据不应存在"
    );
}
