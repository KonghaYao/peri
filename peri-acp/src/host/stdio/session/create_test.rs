//! Tests for session create handlers（H1：load/resume/fork 分支会话级 LSP 池）。
//!
//! stdio handler 的 `Responder`/`ConnectionTo` 由 agent-client-protocol 内部
//! 构造、无法在测试中直接实例化，故经双端 builder 驱动 handler：
//! agent 端 `Agent.builder().connect_to(channel_b)` 作为 server（spawn 运行，
//! 服务器 future 在连接关闭前不会结束，不 await），client 端
//! `Client.builder().connect_with(channel_a, main_fn)` 经 `block_task()` 等待
//! 响应（单端 connect_with 时对端 channel 无人消费消息，请求/响应无法回环）。

use std::sync::Arc;

use agent_client_protocol::{
    schema::v1::{
        DeleteSessionRequest, DeleteSessionResponse, ForkSessionRequest, ForkSessionResponse,
        LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse,
        ResumeSessionRequest, ResumeSessionResponse,
    },
    Agent, Channel, Client, ConnectionTo,
};
use peri_acp_types::lsp::LspServerConfig;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::ports::McpPoolPort;
use peri_acp_types::store::ThreadStore;
use peri_agent::thread::FilesystemThreadStore;

use super::*;
use crate::host::stdio::session::control;
use crate::provider::{LlmProvider, PeriConfig, ProviderConfig, ProviderModels};

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

/// 构造测试用 StdioContext：走统一装配 `assemble_server_config(bare: true)`
/// （与 init.rs 同源；bare 跳过插件/全局 settings hooks/MCP 后台初始化/
/// 孤儿插件清理，仅装配最小中间件集 + SessionManager）。
///
/// prewarm（MCP 发现）与 LSP 池测试需要注入自定义 `mcp_pool` /
/// `lsp_servers`，装配完成后显式替换 `cfg.mcp_pool` / `cfg.plugin_lsp_servers`
/// 两个字段（bare 装配下 mcp_pool 恒为 None；plugin_lsp_servers 仅含全局
/// settings.json 合并结果，测试以参数为准）。
async fn make_stdio_context(
    tmp: &tempfile::TempDir,
    lsp_servers: Vec<LspServerConfig>,
    mcp_pool: Option<Arc<dyn McpPoolPort>>,
) -> Arc<StdioContext> {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let permission_mode = peri_middlewares::permission::shared_mode::SharedPermissionMode::new(
        peri_middlewares::permission::shared_mode::PermissionMode::Bypass,
    );
    let thread_store: Arc<dyn ThreadStore> =
        Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
    // 显式 tmp 路径 + 空配置：测试不读进程/开发者配置（config_source 本批
    // 无 handler 消费，仅装配面注入）。
    let config_source = Arc::new(crate::provider::ConfigSource::load_at_lenient(
        tmp.path(),
        tmp.path().join("test_config.json"),
    ));

    let mut cfg =
        crate::host::assemble::assemble_server_config(crate::host::assemble::HostAssemblyInput {
            provider,
            peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
            config_source,
            permission_mode,
            thread_store,
            cwd: tmp.path().to_string_lossy().into_owned(),
            bare: true,
            drive_cron_tick: false,
        })
        .await;
    // 测试注入（见函数注释）
    cfg.mcp_pool = mcp_pool;
    cfg.plugin_lsp_servers = lsp_servers;

    Arc::new(StdioContext {
        cfg,
        sessions: parking_lot::RwLock::new(std::collections::HashMap::new()),
    })
}

// ── 测试 ──────────────────────────────────────────────────────────────────

/// session/new 预热 MCP skill 发现（决策 B 扩展，stdio 装配面）：pool 存在
/// 但无已连接 server（pending）时 prewarm 空跑不 panic、响应正常；已连接
/// server 的发现行为由 middleware 层单测覆盖
/// （`prewarm_discovery_triggers_idempotent_discovery`）。
#[tokio::test]
async fn test_new_prewarms_mcp_discovery_smoke() {
    let tmp = tempfile::TempDir::new().unwrap();
    let pool: Arc<dyn McpPoolPort> = Arc::new(peri_middlewares::mcp::McpClientPool::new_pending());
    let ctx = make_stdio_context(&tmp, vec![], Some(pool)).await;
    let (channel_a, channel_b) = Channel::duplex();

    let ctx_for_handler = Arc::clone(&ctx);
    let server = Agent
        .builder()
        .on_receive_request(
            {
                let ctx = ctx_for_handler;
                async move |req: NewSessionRequest, responder, cx: ConnectionTo<Client>| {
                    handle_new(&ctx, req, responder, cx).await
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
                let _resp: NewSessionResponse = cx
                    .send_request(NewSessionRequest::new(tmp.path().to_str().unwrap()))
                    .block_task()
                    .await?;
                Ok(())
            },
        )
        .await;

    assert!(result.is_ok(), "handle_new 应成功: {result:?}");
    // prewarm 空跑路径（pending pool 无已连接 server）不 panic
}

/// load 分支与 session/new 一致创建会话级 LSP 池（H1：跨 turn 复用；
/// 此前置 None 走临时实例路径，LSP 服务器子进程跨 turn 泄漏）。
#[tokio::test]
async fn test_load_creates_session_scoped_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = make_stdio_context(&tmp, vec![make_lsp_config()], None).await;
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
    let ctx = make_stdio_context(&tmp, vec![make_lsp_config()], None).await;
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
    let ctx = make_stdio_context(&tmp, vec![make_lsp_config()], None).await;
    // 前置：注册带非空历史的 source session（fork 要求 source history 非空）
    {
        let mut sessions = ctx.sessions.write();
        sessions.insert(
            "fork-source-session".to_string(),
            crate::host::SessionState {
                session_id: "fork-source-session".to_string(),
                thread_id: "fork-source-session".to_string(),
                cwd: tmp.path().to_string_lossy().into_owned(),
                history: vec![BaseMessage::human("hello")],
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
                // 会话创建方即 writer（§6；测试源 session 同样建立 lease）
                lease: crate::host::lease::WriterLease::acquired("default"),
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
    let ctx = make_stdio_context(&tmp, vec![], None).await;
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
    let ctx = make_stdio_context(&tmp, Vec::new(), None).await;
    let (channel_a, channel_b) = Channel::duplex();

    // 先创建线程（session/new 等价物），取得真实 thread id
    let meta = peri_acp_types::thread::ThreadMeta::new(tmp.path().to_str().unwrap());
    let thread_id = ctx.cfg.thread_store.create_thread(meta).await.unwrap();
    let sid = thread_id.clone();

    let ctx_for_handler = Arc::clone(&ctx);
    let server = Agent
        .builder()
        .on_receive_request(
            {
                let ctx = ctx_for_handler;
                async move |req: DeleteSessionRequest, responder, _cx: ConnectionTo<Client>| {
                    control::handle_delete(&ctx, &req.session_id.0, responder).await
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
        ctx.cfg.thread_store.load_meta(&sid).await.is_err(),
        "删除后线程元数据不应存在"
    );
}
