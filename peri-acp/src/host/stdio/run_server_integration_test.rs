//! 批 3 先导集成测试：`StdioTransport`（from_reader_writer + duplex）承载
//! `run_acp_server` 的完整「initialize → session/new → 通知」链路。
//!
//! 目标（批 3 Step 1）：wire 兼容的 **live 证明**——stdin 写入 initialize /
//! session/new 的 JSON-RPC 报文，stdout 侧断言 `protocolVersion` /
//! `agentCapabilities` / AvailableCommandsUpdate 通知（`{"method":
//! "session/update"}` 行含 `available_commands_update`。若本文件证明
//! wire/lifecycle 不兼容（initialize 顺序、通知时序、RequestId 往返），批 3
//! 按 §4 差异列停止并报告 blockers。
//!
//! 另含 `session/prompt` 的 wire 形态验证（未知 session 错误响应 + 通知收尾），
//! 证明 stdio `PromptRequest` 的 `prompt` 块数组（ACP v1 ContentBlock 形态）
//! 与统一 prompt 路径兼容（§7 #1/#2 合并分支）。
//!
//! 批 3 Step 5 测试迁移：原 `host/stdio/session/create_test.rs` 的
//! load/resume/fork 会话级 LSP 池断言（H1）与 session/new MCP 发现预热
//! smoke 迁入本文件——经 `run_acp_server_with_sessions`（外部注入共享
//! session map）驱动统一路径，断言从「handler 直调 + StdioContext 内窥」改为
//! 「wire 驱动 + 共享 map 内窥」（`test_delete_removes_thread_*` 与 prewarm
//! smoke 的 load 变体在 `host/requests_test.rs` 已有等价覆盖，不重复）。

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use crate::host::{self, AcpServerConfig};
use crate::provider::{LlmProvider, PeriConfig, ProviderConfig, ProviderModels};
use crate::transport::{stdio::StdioTransport, AcpTransport};

use super::assemble_stdio_config;

struct StdioStartupGuard;

impl Drop for StdioStartupGuard {
    fn drop(&mut self) {
        crate::provider::set_global_config_path(None);
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_stdio_assembly_uses_input_cwd_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let process_workspace = tmp.path().join("process-workspace");
    let input_workspace = tmp.path().join("input-workspace");
    let global_path = tmp.path().join("missing-global.json");
    std::fs::create_dir_all(process_workspace.join(".peri")).unwrap();
    std::fs::create_dir_all(input_workspace.join(".peri")).unwrap();
    std::fs::write(
        process_workspace.join(".peri/settings.json"),
        r#"{"config":{"active_alias":"sonnet","providers":[{"id":"process","type":"openai","apiKey":"not-a-credential"}],"profiles":{"sonnet":{"provider":"process","model":"test-model"}}}}"#,
    )
    .unwrap();
    std::fs::write(
        input_workspace.join(".peri/settings.json"),
        r#"{"config":{"active_alias":"sonnet","providers":[{"id":"input","type":"openai","apiKey":"not-a-credential"}],"profiles":{"sonnet":{"provider":"input","model":"test-model"}}}}"#,
    )
    .unwrap();

    let _guard = StdioStartupGuard;
    crate::provider::set_global_config_path(Some(global_path));

    let assembled = assemble_stdio_config(crate::host::stdio::StdioInput {
        cwd: input_workspace.to_string_lossy().into_owned(),
        permission_mode: peri_acp_types::permission::SharedPermissionMode::new(
            peri_acp_types::permission::PermissionMode::Bypass,
        ),
        session_store: peri_acp_types::session_store::SessionStoreDeployment::local_path(
            tmp.path().join("threads.db"),
        ),
    })
    .await
    .unwrap();

    assert_eq!(
        assembled.config_source.loaded_merged().config.providers[0].id,
        "input"
    );
}

// ── 测试装配（与 requests_test.rs 的 make_server_config 同构）──────────────

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

async fn make_server_config(
    peri_config: PeriConfig,
    provider: LlmProvider,
    tmp: &tempfile::TempDir,
) -> AcpServerConfig {
    make_server_config_with(peri_config, provider, tmp, Vec::new(), None, None).await
}

/// 同 [`make_server_config`]，另注入 `plugin_lsp_servers`（H1 会话级 LSP 池
/// 测试）与 `mcp_pool`（MCP 发现预热 smoke 测试）——与迁移前
/// `create_test.rs::make_stdio_context` 的「装配后显式替换」等价的参数化形态。
async fn make_server_config_with(
    peri_config: PeriConfig,
    provider: LlmProvider,
    tmp: &tempfile::TempDir,
    lsp_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
    mcp_pool: Option<Arc<dyn peri_acp_types::ports::McpPoolPort>>,
    task_manager_factory: Option<crate::session::TaskManagerFactory>,
) -> AcpServerConfig {
    use std::collections::BTreeMap;

    // 生产同形入口：只注入门面（SessionManager 与 Controller 都只持它）。
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let session_manager = crate::session::SessionManager::new(
        session_resources.clone(),
        provider.clone(),
        Arc::new(peri_config.clone()),
        peri_acp_types::permission::SharedPermissionMode::new(
            peri_acp_types::permission::PermissionMode::Bypass,
        ),
        None,
        None,
        None,
        None,
        task_manager_factory.or_else(|| {
            Some(Arc::new(|| {
                Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
                    as Arc<dyn peri_acp_types::tasks::TaskManager>
            }))
        }),
        Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        Vec::new(),
    );
    let (host_task_owner, host_task_spawner) = crate::host::task_scope::HostTaskOwner::new();
    let (mcp_task_owner, _mcp_task_spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    // H-04（A11/A22）：host 级唯一 pool。测试装配与生产同构——**空配置也构造**
    // （`has_servers()` 假 ⇒ 工具面空表但仍 ready），session 只投影它的 `Arc`。
    let lsp_pool: Arc<dyn peri_acp_types::ports::LspPoolPort> =
        peri_mcp_lsp::create_host_lsp_pool(tmp.path().to_str().unwrap(), &lsp_servers);
    AcpServerConfig {
        workspace_assembly: None,
        host_task_owner: Some(host_task_owner),
        host_task_spawner,
        mcp_task_owner: Some(Box::new(mcp_task_owner)),
        provider: Arc::new(parking_lot::RwLock::new(provider)),
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        permission_mode: peri_acp_types::permission::SharedPermissionMode::new(
            peri_acp_types::permission::PermissionMode::Bypass,
        ),
        cron_scheduler: None,
        mcp_pool,
        mcp_apps_relay: None,
        acp_mcp: None,
        dynamic_mcp: None,
        oauth_event_tx: None,
        oauth_event_rx: None,
        plugin_skill_roots: Vec::new(),
        plugin_command_entries: Vec::new(),
        plugin_hooks: Vec::new(),
        plugin_hooks_only: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        plugin_lsp_servers: lsp_servers,
        lsp_pool: Some(lsp_pool),
        tool_search_index: Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new()),
        agent_catalog: Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        plugin_manager: Arc::new(peri_middlewares::host_ports::PluginManager),
        settings_hooks: Arc::new(peri_middlewares::host_ports::SettingsHooksLoader),
        shared_tools: Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
        workflow_middleware_factory: Arc::new(
            peri_middlewares::assembly::WorkflowAgentMiddlewareFactory,
        ),
        session_resources: session_resources.clone(),
        // 测试宿主：不注入部署关闭权（没有部署生命周期）。
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
        stdio_command_filter: true,
    }
}

// ── duplex 驱动辅助（与 transport/stdio_test.rs 对称）───────────────────────

/// 构造 duplex 驱动的 `StdioTransport` + 对应 stdin 写入端 / stdout 读取端。
fn duplex_transport() -> (StdioTransport, DuplexStream, DuplexStream) {
    let (input_write, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, output_read) = tokio::io::duplex(64 * 1024);
    let transport = StdioTransport::from_reader_writer(transport_read, transport_write);
    (transport, input_write, output_read)
}

/// 往 stdin（input 端）写入一行 JSON-RPC 报文。
async fn write_line(stream: &mut DuplexStream, line: &str) {
    stream.write_all(line.as_bytes()).await.unwrap();
    stream.write_all(b"\n").await.unwrap();
}

/// 从 stdout（output 端）读取一行报文（按 `\n` 分帧，与 pump 对称）；超时防挂死。
async fn read_line(stream: &mut DuplexStream) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            assert_eq!(
                stream.read(&mut byte).await.unwrap(),
                1,
                "输出流不应提前关闭: 已读 {buf:?}"
            );
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        String::from_utf8(buf).expect("stdout 应为 UTF-8 JSON")
    })
    .await
    .expect("read_line 超时：服务端未在期限内产生输出")
}

/// 待测试的最小 `AcpServerConfig`（provider 假 key + bare 语义，无外部依赖）。
async fn test_config(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    make_server_config(peri_config, provider, tmp).await
}

/// 最小 LSP 服务器配置（`command: "true"` 立即可退出的假服务器；与迁移前
/// `create_test.rs::make_lsp_config` 同构）。
fn make_lsp_config() -> peri_acp_types::lsp::LspServerConfig {
    peri_acp_types::lsp::LspServerConfig {
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

/// 带 `plugin_lsp_servers` 注入的测试配置（H1 会话级 LSP 池断言）。
async fn test_config_with_lsp(
    tmp: &tempfile::TempDir,
    lsp_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
) -> AcpServerConfig {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    make_server_config_with(peri_config, provider, tmp, lsp_servers, None, None).await
}

/// 带 pending `mcp_pool` 注入的测试配置（MCP 发现预热 smoke：pool 存在但
/// 无已连接 server）。
async fn test_config_with_pending_mcp_pool(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let pool: Arc<dyn peri_acp_types::ports::McpPoolPort> =
        Arc::new(peri_middlewares::mcp::McpClientPool::new_pending());
    make_server_config_with(peri_config, provider, tmp, Vec::new(), Some(pool), None).await
}

#[derive(Default)]
struct RecordingTaskManager {
    cancel_all_calls: std::sync::atomic::AtomicUsize,
}

impl peri_acp_types::tasks::TaskManager for RecordingTaskManager {
    fn is_execution_idle(&self) -> bool {
        true
    }
    fn shutdown(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = peri_acp_types::tasks::TaskShutdownReport> + Send + '_,
        >,
    > {
        self.cancel_all();
        Box::pin(async { peri_acp_types::tasks::TaskShutdownReport::Complete })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn set_event_sender(
        &self,
        _sender: tokio::sync::mpsc::UnboundedSender<peri_acp_types::tasks::BgRegistryEvent>,
        _session_id: String,
    ) {
    }

    fn active_count(&self) -> usize {
        0
    }

    fn register(&self, _request: peri_acp_types::tasks::BgTaskRegistration) -> Result<(), String> {
        Ok(())
    }

    fn complete(
        &self,
        _task_id: &str,
        _result: peri_acp_types::event::BackgroundTaskResult,
    ) -> bool {
        true
    }

    fn cancel(&self, _task_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn cancel_all(&self) {
        self.cancel_all_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn spawn_shell(
        &self,
        _command: String,
        _cwd: String,
        _timeout_ms: Option<u64>,
        _on_bg_complete: Option<peri_acp_types::tasks::OnBgCompleteFn>,
    ) -> Result<peri_acp_types::tasks::BgShellHandle, Box<dyn std::error::Error + Send + Sync>>
    {
        Err("recording task manager does not spawn".into())
    }

    fn finalize_bg_shell(
        &self,
        _on_bg_complete: &Option<peri_acp_types::tasks::OnBgCompleteFn>,
        _task_id: String,
        _prompt_summary: String,
        _success: bool,
        _output: String,
        _duration_ms: u64,
        _timed_out: bool,
        _shell_output: Option<peri_acp_types::event::ShellOutput>,
    ) {
    }
}

struct RecordingLspPool {
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Arc<tokio::sync::Notify>,
    shutdown_calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl peri_acp_types::ports::LspPoolPort for RecordingLspPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn shutdown(&self) {
        self.shutdown_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        self.release.notified().await;
    }

    /// A30：`ready_for` 无默认实现，替身必须显式实现。本替身不承载就绪语义，
    /// 恒 false —— 必须为 false 而不是「恰好为 true」：`ready_for` 是「读文件前的
    /// 唯一前置判定」，恒 false 表示「无可用 server」，调用方不得读文件、不得发通知。
    /// 若将来有调用方在此路径上依赖 true，会立刻暴露成「同步未发生」而不是假绿。
    fn ready_for(&self, _path: &std::path::Path) -> bool {
        false
    }

    /// A30：显式实现（无默认 no-op）。本替身不记录同步事实，但**返回 typed Err**
    /// 而不是假成功 `Ok`：本替身代表「无 server」的池，同步失败是诚实的结论。
    async fn did_change(
        &self,
        _path: &std::path::Path,
        _text: &str,
    ) -> Result<(), peri_acp_types::ports::LspSyncError> {
        Err(peri_acp_types::ports::LspSyncError::NoServer)
    }

    async fn did_save(
        &self,
        _path: &std::path::Path,
    ) -> Result<(), peri_acp_types::ports::LspSyncError> {
        Err(peri_acp_types::ports::LspSyncError::NoServer)
    }
}

struct EofTaskDropSignal(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for EofTaskDropSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

/// 发送 initialize（id=1）并读取响应，断言 protocolVersion/agentCapabilities。
/// 迁移测试的统一前置：统一路径由 run_acp_server 自身保证 initialize 先于
/// 其余请求（§4）；load/resume/fork 依赖 caps registry 已协商。
async fn send_initialize(input: &mut DuplexStream, output: &mut DuplexStream) {
    write_line(
        input,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": 1 }
        })
        .to_string(),
    )
    .await;
    let line = read_line(output).await;
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["id"], 1, "initialize RequestId 往返: {v}");
    assert!(v.get("error").is_none(), "initialize 不应报错: {v}");
    assert_eq!(
        v["result"]["protocolVersion"], 1,
        "protocolVersion 基线: {v}"
    );
}

/// 发送一条请求并持续读取 stdout，直至收到 `id` 匹配的响应（期间的通知行
/// 跳过）；返回 `result` 值。断言响应无 error。
async fn send_request_and_read_result(
    input: &mut DuplexStream,
    output: &mut DuplexStream,
    id: i64,
    method: &str,
    params: Value,
) -> Value {
    write_line(
        input,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        })
        .to_string(),
    )
    .await;
    loop {
        let line = read_line(output).await;
        let v: Value = serde_json::from_str(&line).unwrap();
        if v.get("id").is_some() {
            assert_eq!(v["id"], id, "请求 id 往返: {v}");
            assert!(v.get("error").is_none(), "{method} 不应报错: {v}");
            return v["result"].clone();
        }
        // 通知行（session/update 等）：跳过，继续等待响应
        assert_eq!(v["method"], "session/update", "响应前只应有通知: {v}");
    }
}

/// 等待宿主在 stdin EOF 后优雅退出（LSP pool shutdown 钩子后返回）。
async fn await_server_exit(server_task: tokio::task::JoinHandle<()>, input: DuplexStream) {
    drop(input);
    tokio::time::timeout(std::time::Duration::from_secs(10), server_task)
        .await
        .expect("run_acp_server 应在 stdin EOF 后退出")
        .expect("server task 不应 panic");
}

async fn create_bound_thread_fixture(cfg: &AcpServerConfig, session_id: &str, cwd: &str) {
    let workspace = cfg
        .session_resources
        .resolve_workspace(std::path::Path::new(cwd))
        .await
        .unwrap();
    let frozen = cfg
        .session_manager
        .build_frozen_data(workspace.cwd.to_str().unwrap());
    let encoded = crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap();
    // 门面一次完成 binding/frozen 保存与执行准入，再按正常收尾标 clean。
    let lease = cfg
        .session_resources
        .create_session(&peri_acp_types::session_resources::NewSession {
            thread_id: session_id.to_owned(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: peri_acp_types::session_resources::NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: peri_acp_types::thread::CancelPolicy::default(),
                snapshot_at_message_id: None,
            },
            binding: peri_acp_types::workspace::SessionBinding::from_workspace(&workspace),
            frozen: peri_acp_types::session_resources::FrozenSnapshotBytes::new(encoded),
        })
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
}

// ── 测试 ──────────────────────────────────────────────────────────────────

/// host task scope 已关闭时，prompt request 仍必须收到一次 terminal error response。
#[tokio::test]
async fn test_rejected_prompt_task_returns_terminal_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config(&tmp).await;
    cfg.host_task_owner
        .as_ref()
        .expect("test config should own host task scope")
        .begin_shutdown();
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let server_task = tokio::spawn(host::run_acp_server(Arc::new(transport), cfg));

    write_line(
        &mut input_write,
        &json!({
            "jsonrpc": "2.0",
            "id": "prompt-rejected",
            "method": "session/prompt",
            "params": { "sessionId": "missing", "prompt": [] }
        })
        .to_string(),
    )
    .await;

    let response: Value = serde_json::from_str(&read_line(&mut output_read).await).unwrap();
    assert_eq!(response["id"], "prompt-rejected");
    assert_eq!(response["error"]["code"], -32800);
    assert_eq!(response["error"]["message"], "request cancelled");
    assert!(response.get("result").is_none());

    drop(input_write);
    server_task.await.expect("server task 不应 panic");
}

/// initialize → session/new → AvailableCommandsUpdate 通知：stdout 侧完整断言。
/// 证明 StdioTransport 可承载 run_acp_server 的生命周期链路（wire 兼容 live 证明）。
#[tokio::test]
async fn test_initialize_and_session_new_over_stdio_transport() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config(&tmp).await;
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let server_task = tokio::spawn(host::run_acp_server(transport, cfg));

    // ── initialize ──
    write_line(
        &mut input_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": 1 }
        })
        .to_string(),
    )
    .await;
    let line = read_line(&mut output_read).await;
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["jsonrpc"], "2.0", "信封 jsonrpc 字段: {v}");
    assert_eq!(v["id"], 1, "RequestId 往返保真（Number id）: {v}");
    assert!(v.get("method").is_none(), "响应不应含 method: {v}");
    assert!(v.get("error").is_none(), "initialize 不应报错: {v}");
    assert_eq!(
        v["result"]["protocolVersion"], 1,
        "protocolVersion 基线: {v}"
    );
    let caps = &v["result"]["agentCapabilities"];
    assert!(caps["sessionCapabilities"]["list"].is_object());
    assert!(caps["sessionCapabilities"]["close"].is_object());
    assert!(caps["sessionCapabilities"]["resume"].is_object());
    assert!(caps["sessionCapabilities"]["fork"].is_object());
    assert!(caps["sessionCapabilities"]["delete"].is_object());

    // ── session/new ──
    let cwd = tmp.path().to_str().unwrap();
    write_line(
        &mut input_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": { "cwd": cwd }
        })
        .to_string(),
    )
    .await;

    // session/new 必须先返回 response，让客户端建立 sessionId 路由；随后再推送
    // AvailableCommandsUpdate，避免首次 commands 通知因 session 尚未绑定而丢失。
    let line = read_line(&mut output_read).await;
    let resp: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], 2, "session/new RequestId 往返: {resp}");
    assert!(resp.get("error").is_none(), "session/new 不应报错: {resp}");
    let session_id = resp["result"]["sessionId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("session/new 返回非空 sessionId")
        .to_string();
    assert!(resp["result"]["modes"].is_object(), "modes 存在: {resp}");
    assert!(
        resp["result"]["configOptions"].is_array(),
        "configOptions 存在: {resp}"
    );

    let line = read_line(&mut output_read).await;
    let notif: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(notif["method"], "session/update", "通知 method: {notif}");
    assert_eq!(
        notif["params"]["sessionId"], session_id,
        "commands 通知应属于刚创建的 session: {notif}"
    );
    assert_eq!(
        notif["params"]["update"]["sessionUpdate"], "available_commands_update",
        "AvailableCommandsUpdate 判别字符串（wire 基线）: {notif}"
    );
    assert!(
        notif["params"]["update"]["availableCommands"].is_array(),
        "availableCommands 数组存在: {notif}"
    );

    // ── EOF → 宿主优雅退出（LSP pool shutdown 钩子后返回）──
    drop(input_write);
    tokio::time::timeout(std::time::Duration::from_secs(10), server_task)
        .await
        .expect("run_acp_server 应在 stdin EOF 后退出")
        .expect("server task 不应 panic");
}

#[tokio::test]
async fn test_transport_eof_closes_sessions_and_drains_host_tasks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let task_manager = Arc::new(RecordingTaskManager::default());
    let task_manager_factory: crate::session::TaskManagerFactory = {
        let task_manager = Arc::clone(&task_manager);
        Arc::new(move || Arc::clone(&task_manager) as Arc<dyn peri_acp_types::tasks::TaskManager>)
    };
    let mut cfg = make_server_config_with(
        peri_config,
        provider,
        &tmp,
        Vec::new(),
        None,
        Some(task_manager_factory),
    )
    .await;
    let manager = cfg.session_manager.clone();
    manager
        .new_session_with_id("manager-only", tmp.path().to_str().unwrap())
        .await
        .unwrap();

    let host_shutdown = cfg.host_task_spawner.shutdown_token();
    let (host_started_tx, host_started_rx) = tokio::sync::oneshot::channel();
    let (host_dropped_tx, host_dropped_rx) = tokio::sync::oneshot::channel();
    cfg.host_task_spawner
        .spawn(
            crate::host::task_scope::HostTaskOwnerKind::Host,
            crate::host::task_scope::HostTaskKind::LegacyCancelHook,
            async move {
                let _drop_signal = EofTaskDropSignal(Some(host_dropped_tx));
                let _ = host_started_tx.send(());
                host_shutdown.cancelled().await;
            },
        )
        .unwrap();

    let local_cancel = tokio_util::sync::CancellationToken::new();
    let local_cancel_observer = local_cancel.clone();
    let lsp_release = Arc::new(tokio::sync::Notify::new());
    let (lsp_entered_tx, lsp_entered_rx) = tokio::sync::oneshot::channel();
    let lsp = Arc::new(RecordingLspPool {
        entered: std::sync::Mutex::new(Some(lsp_entered_tx)),
        release: Arc::clone(&lsp_release),
        shutdown_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    // H-04（A11/A22）：EOF 关的是**宿主唯一 pool**（`cfg.lsp_pool`），不再从
    // session 收集——session state 只是同一 `Arc` 的投影。
    let host_pool = Arc::clone(&lsp) as Arc<dyn peri_acp_types::ports::LspPoolPort>;
    cfg.lsp_pool = Some(Arc::clone(&host_pool));
    let (transport, mut input, mut output) = duplex_transport();
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::from([
        (
            "local-only".to_string(),
            crate::host::SessionState {
                session_id: "local-only".to_string(),
                thread_id: "local-only".to_string(),
                cwd: tmp.path().to_string_lossy().into_owned(),
                execution_owner: None,
                environment: None,
                closing: false,
                history: Vec::new(),
                history_payloads: Vec::new(),
                cancel_token: Some(local_cancel),
                frozen: None,
                recall_items: Vec::new(),
                agent_pool: crate::session::agent_pool::AgentPool::new(),
                workflow_middleware: None,
                // session 只投影 host pool 的同一 `Arc`（A11）
                lsp_pool: Some(Arc::clone(&host_pool)),
                title: None,
                tags: Vec::new(),
                continuation_armed: false,
                continuation_epoch: 0,
                continuation_in_flight: false,
                continuation_mq_steering_pending: false,
                lease: crate::host::lease::WriterLease::acquired("default"),
            },
        ),
    ])));
    let server_task = tokio::spawn(host::run_acp_server_with_sessions(
        Arc::new(transport),
        cfg,
        sessions.clone(),
    ));

    host_started_rx.await.unwrap();
    send_initialize(&mut input, &mut output).await;
    drop(input);
    lsp_entered_rx
        .await
        .expect("EOF must reach the local-only LSP shutdown");

    host_dropped_rx
        .await
        .expect("accepted host task must settle before LSP shutdown");
    assert!(local_cancel_observer.is_cancelled());
    assert!(
        task_manager
            .cancel_all_calls
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 1,
        "manager-only TaskManager must receive pre-close cancellation"
    );
    assert!(manager.get_session("manager-only").is_none());

    let lock_sessions = Arc::clone(&sessions);
    let (lock_acquired_tx, lock_acquired_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _guard = lock_sessions.lock().await;
        let _ = lock_acquired_tx.send(());
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), lock_acquired_rx)
        .await
        .expect("SharedSessions must be acquirable while LSP shutdown is awaiting")
        .unwrap();
    lsp_release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), server_task)
        .await
        .expect("run_acp_server must finish after controlled LSP release")
        .expect("server task must not panic");

    assert!(sessions.lock().await.is_empty());
    assert!(manager.session_ids().is_empty());
    assert_eq!(
        lsp.shutdown_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "local-only LSP must be shut down exactly once"
    );
}

/// `session/prompt` wire 形态（stdio `PromptRequest`：`prompt` 块数组）在统一
/// 路径下可用：未知 session → -32602 error envelope；响应后随一条
/// `session/update`（SessionInfoUpdate）收尾（与 TUI 路径同款通知序列）。
#[tokio::test]
async fn test_prompt_wire_shape_unknown_session_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config(&tmp).await;
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let server_task = tokio::spawn(host::run_acp_server(transport, cfg));

    write_line(
        &mut input_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "session/prompt",
            "params": {
                "sessionId": "no-such-session",
                "prompt": [ { "type": "text", "text": "hi" } ]
            }
        })
        .to_string(),
    )
    .await;

    let line = read_line(&mut output_read).await;
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["id"], 7, "prompt RequestId 往返: {v}");
    assert_eq!(
        v["error"]["code"], -32602,
        "未知 session 应报 Invalid params: {v}"
    );
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("session not found"),
        "错误信息应为 session not found: {v}"
    );

    // dispatch_prompt_turn 后 send_session_info_update 收尾通知（与 TUI 同款）
    let line = read_line(&mut output_read).await;
    let notif: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(notif["method"], "session/update");
    assert_eq!(
        notif["params"]["update"]["sessionUpdate"],
        "session_info_update"
    );

    await_server_exit(server_task, input_write).await;
}

/// `session/rename` 经 stdio wire 完整链路：initialize → session/new →
/// session/rename。通知先于响应（`handle_rename` 中
/// `send_session_info_update_with_title` 在响应返回前推送），通知携带
/// `SessionInfoUpdate.title`，响应往返 `{sessionId, title}`，thread store
/// 持久化标题——证明 stdio 与 TUI 共用统一 host 后 rename RPC 对 stdio 生效
/// （请求注册于 `host/requests.rs`）。
#[tokio::test]
async fn test_rename_over_stdio_transport() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config(&tmp).await;
    let session_resources = Arc::clone(&cfg.session_resources);
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let server_task = tokio::spawn(host::run_acp_server(transport, cfg));

    send_initialize(&mut input_write, &mut output_read).await;

    // ── session/new ──
    let cwd = tmp.path().to_str().unwrap();
    write_line(
        &mut input_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": { "cwd": cwd }
        })
        .to_string(),
    )
    .await;
    let line = read_line(&mut output_read).await;
    let resp: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], 2, "session/new RequestId 往返: {resp}");
    assert!(resp.get("error").is_none(), "session/new 不应报错: {resp}");
    let session_id = resp["result"]["sessionId"]
        .as_str()
        .expect("session/new 返回非空 sessionId")
        .to_string();
    // 消费 session/new 后的 available_commands_update 通知
    let line = read_line(&mut output_read).await;
    let notif: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(notif["method"], "session/update", "通知 method: {notif}");

    // ── session/rename ──
    write_line(
        &mut input_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/rename",
            "params": { "sessionId": session_id, "title": "stdio 命名会话" }
        })
        .to_string(),
    )
    .await;

    // 通知先于响应（handle_rename 在返回前推送 SessionInfoUpdate）
    let line = read_line(&mut output_read).await;
    let notif: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        notif["method"], "session/update",
        "rename 通知 method: {notif}"
    );
    assert_eq!(notif["params"]["sessionId"], session_id);
    assert_eq!(
        notif["params"]["update"]["sessionUpdate"], "session_info_update",
        "SessionInfoUpdate 判别字符串: {notif}"
    );
    assert_eq!(notif["params"]["update"]["title"], "stdio 命名会话");

    // 响应往返 {sessionId, title}
    let line = read_line(&mut output_read).await;
    let resp: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], 3, "rename RequestId 往返: {resp}");
    assert!(resp.get("error").is_none(), "rename 不应报错: {resp}");
    assert_eq!(resp["result"]["sessionId"], session_id, "响应: {resp}");
    assert_eq!(resp["result"]["title"], "stdio 命名会话", "响应: {resp}");

    // 持久化：thread store 标题已更新
    let meta = session_resources
        .load_session_meta(&session_id)
        .await
        .unwrap();
    assert_eq!(meta.title.as_deref(), Some("stdio 命名会话"));

    // ── EOF → 宿主优雅退出 ──
    drop(input_write);
    tokio::time::timeout(std::time::Duration::from_secs(10), server_task)
        .await
        .expect("run_acp_server 应在 stdin EOF 后退出")
        .expect("server task 不应 panic");
}

#[path = "langfuse_shutdown_test.rs"]
mod langfuse_shutdown_tests;

#[path = "session_store_shutdown_test.rs"]
mod session_store_shutdown_tests;

#[path = "run_server_session_resources_test.rs"]
mod session_resources_tests;
