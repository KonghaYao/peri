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

use super::load_stdio_config_source;

#[test]
fn test_stdio_config_source_uses_input_cwd_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let process_workspace = tmp.path().join("process-workspace");
    let input_workspace = tmp.path().join("input-workspace");
    let global_path = tmp.path().join("missing-global.json");
    std::fs::create_dir_all(process_workspace.join(".peri")).unwrap();
    std::fs::create_dir_all(input_workspace.join(".peri")).unwrap();
    std::fs::write(
        process_workspace.join(".peri/settings.json"),
        r#"{"config":{"providers":[{"id":"process","type":"openai"}]}}"#,
    )
    .unwrap();
    std::fs::write(
        input_workspace.join(".peri/settings.json"),
        r#"{"config":{"providers":[{"id":"input","type":"openai"}]}}"#,
    )
    .unwrap();

    let source = load_stdio_config_source(&input_workspace, global_path);

    assert_eq!(source.loaded_merged().config.providers[0].id, "input");
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

fn make_server_config(
    peri_config: PeriConfig,
    provider: LlmProvider,
    tmp: &tempfile::TempDir,
) -> AcpServerConfig {
    make_server_config_with(peri_config, provider, tmp, Vec::new(), None, None)
}

/// 同 [`make_server_config`]，另注入 `plugin_lsp_servers`（H1 会话级 LSP 池
/// 测试）与 `mcp_pool`（MCP 发现预热 smoke 测试）——与迁移前
/// `create_test.rs::make_stdio_context` 的「装配后显式替换」等价的参数化形态。
fn make_server_config_with(
    peri_config: PeriConfig,
    provider: LlmProvider,
    tmp: &tempfile::TempDir,
    lsp_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
    mcp_pool: Option<Arc<dyn peri_acp_types::ports::McpPoolPort>>,
    task_manager_factory: Option<crate::session::TaskManagerFactory>,
) -> AcpServerConfig {
    use std::collections::BTreeMap;

    let thread_store = peri_agent::thread::FilesystemThreadStore::new(tmp.path().join("threads"));
    let arc_thread_store: Arc<dyn peri_acp_types::store::ThreadStore> = Arc::new(thread_store);
    let session_manager = crate::session::SessionManager::new(
        arc_thread_store.clone(),
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
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
        Vec::new(),
        Vec::new(),
    );
    let (host_task_owner, host_task_spawner) = crate::host::task_scope::HostTaskOwner::new();
    let (mcp_task_owner, _mcp_task_spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    AcpServerConfig {
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
        plugin_lsp_servers: lsp_servers,
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
fn test_config(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    make_server_config(peri_config, provider, tmp)
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
fn test_config_with_lsp(
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
    make_server_config_with(peri_config, provider, tmp, lsp_servers, None, None)
}

/// 带 pending `mcp_pool` 注入的测试配置（MCP 发现预热 smoke：pool 存在但
/// 无已连接 server）。
fn test_config_with_pending_mcp_pool(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let pool: Arc<dyn peri_acp_types::ports::McpPoolPort> =
        Arc::new(peri_middlewares::mcp::McpClientPool::new_pending());
    make_server_config_with(peri_config, provider, tmp, Vec::new(), Some(pool), None)
}

#[derive(Default)]
struct RecordingTaskManager {
    cancel_all_calls: std::sync::atomic::AtomicUsize,
}

impl peri_acp_types::tasks::TaskManager for RecordingTaskManager {
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

// ── 测试 ──────────────────────────────────────────────────────────────────

/// host task scope 已关闭时，prompt request 仍必须收到一次 terminal error response。
#[tokio::test]
async fn test_rejected_prompt_task_returns_terminal_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config(&tmp);
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
    let cfg = test_config(&tmp);
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
    let cfg = make_server_config_with(
        peri_config,
        provider,
        &tmp,
        Vec::new(),
        None,
        Some(task_manager_factory),
    );
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
    let (transport, mut input, mut output) = duplex_transport();
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::from([
        (
            "local-only".to_string(),
            crate::host::SessionState {
                session_id: "local-only".to_string(),
                thread_id: "local-only".to_string(),
                cwd: tmp.path().to_string_lossy().into_owned(),
                history: Vec::new(),
                cancel_token: Some(local_cancel),
                frozen: None,
                recall_items: Vec::new(),
                agent_pool: crate::session::agent_pool::AgentPool::new(),
                workflow_middleware: None,
                lsp_pool: Some(Arc::clone(&lsp) as Arc<dyn peri_acp_types::ports::LspPoolPort>),
                title: None,
                tags: Vec::new(),
                continuation_armed: false,
                continuation_epoch: 0,
                continuation_in_flight: false,
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
    let cfg = test_config(&tmp);
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

// ── 批 3 Step 5 迁移：会话级 LSP 池（H1）与 MCP 发现预热（原 create_test.rs）──
//
// 原断言经 `run_acp_server_with_sessions`（外部注入共享 session map）驱动
// 统一路径：wire 请求 + map 内窥，语义与迁移前（handler 直调 + StdioContext
// 内窥）等价。`test_delete_removes_thread_*` 与 prewarm smoke 的 load 变体
// 已在 `host/requests_test.rs` 有等价覆盖，不重复迁移。

/// session/load 分支创建会话级 LSP 池（H1：跨 turn 复用；此前置 None 走临时
/// 实例路径，LSP 服务器子进程跨 turn 泄漏）。
#[tokio::test]
async fn test_load_creates_session_scoped_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config_with_lsp(&tmp, vec![make_lsp_config()]);
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let server_task = tokio::spawn(host::run_acp_server_with_sessions(
        transport,
        cfg,
        sessions.clone(),
    ));

    send_initialize(&mut input_write, &mut output_read).await;
    let result = send_request_and_read_result(
        &mut input_write,
        &mut output_read,
        2,
        "session/load",
        json!({ "sessionId": "load-test-session", "cwd": tmp.path().to_str().unwrap() }),
    )
    .await;
    assert!(
        result["modes"].is_object() && result["configOptions"].is_array(),
        "load 响应应含 modes/configOptions: {result}"
    );

    let sessions = sessions.lock().await;
    let info = sessions
        .get("load-test-session")
        .expect("load 应注册 session");
    assert!(
        info.lsp_pool.is_some(),
        "load 分支应创建会话级 LSP 池（H1 跨 turn 复用）"
    );
    drop(sessions);
    await_server_exit(server_task, input_write).await;
}

/// session/resume 分支（新 session）同样创建会话级 LSP 池。
#[tokio::test]
async fn test_resume_creates_session_scoped_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config_with_lsp(&tmp, vec![make_lsp_config()]);
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let server_task = tokio::spawn(host::run_acp_server_with_sessions(
        transport,
        cfg,
        sessions.clone(),
    ));

    send_initialize(&mut input_write, &mut output_read).await;
    let _result = send_request_and_read_result(
        &mut input_write,
        &mut output_read,
        2,
        "session/resume",
        json!({ "sessionId": "resume-test-session", "cwd": tmp.path().to_str().unwrap() }),
    )
    .await;

    let sessions = sessions.lock().await;
    let info = sessions
        .get("resume-test-session")
        .expect("resume 应注册 session");
    assert!(
        info.lsp_pool.is_some(),
        "resume 分支应创建会话级 LSP 池（H1 跨 turn 复用）"
    );
    drop(sessions);
    await_server_exit(server_task, input_write).await;
}

/// session/fork 分支创建的新 session 同样携带会话级 LSP 池。
#[tokio::test]
async fn test_fork_creates_session_scoped_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config_with_lsp(&tmp, vec![make_lsp_config()]);
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    // 前置：注册带非空历史的 source session（fork 要求 source history 非空）
    sessions.lock().await.insert(
        "fork-source-session".to_string(),
        crate::host::SessionState {
            session_id: "fork-source-session".to_string(),
            thread_id: "fork-source-session".to_string(),
            cwd: tmp.path().to_string_lossy().into_owned(),
            history: vec![peri_acp_types::messages::BaseMessage::human("hello")],
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
    let server_task = tokio::spawn(host::run_acp_server_with_sessions(
        transport,
        cfg,
        sessions.clone(),
    ));

    send_initialize(&mut input_write, &mut output_read).await;
    let result = send_request_and_read_result(
        &mut input_write,
        &mut output_read,
        2,
        "session/fork",
        json!({ "sessionId": "fork-source-session", "cwd": tmp.path().to_str().unwrap() }),
    )
    .await;
    let forked_id = result["sessionId"]
        .as_str()
        .expect("fork 响应应含新 sessionId: {result}")
        .to_string();
    assert_ne!(forked_id, "fork-source-session");

    let sessions = sessions.lock().await;
    let forked = sessions.get(&forked_id).expect("fork 应注册新 session");
    assert!(
        forked.lsp_pool.is_some(),
        "fork 分支应创建会话级 LSP 池（H1 跨 turn 复用）"
    );
    drop(sessions);
    await_server_exit(server_task, input_write).await;
}

/// 无 LSP 配置时 load 分支不创建池（与 create_session_lsp_pool 的 None 语义
/// 一致，装配面不注册 LSP 中间件）。
#[tokio::test]
async fn test_load_without_lsp_config_has_no_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config(&tmp); // plugin_lsp_servers = []
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let server_task = tokio::spawn(host::run_acp_server_with_sessions(
        transport,
        cfg,
        sessions.clone(),
    ));

    send_initialize(&mut input_write, &mut output_read).await;
    let _result = send_request_and_read_result(
        &mut input_write,
        &mut output_read,
        2,
        "session/load",
        json!({ "sessionId": "no-lsp-session", "cwd": tmp.path().to_str().unwrap() }),
    )
    .await;

    let sessions = sessions.lock().await;
    let info = sessions.get("no-lsp-session").expect("load 应注册 session");
    assert!(
        info.lsp_pool.is_none(),
        "无 LSP 配置时不应创建池（与 new 分支一致）"
    );
    drop(sessions);
    await_server_exit(server_task, input_write).await;
}

/// session/new 预热 MCP skill 发现 smoke：pool 存在但无已连接 server（pending）
/// 时 prewarm 空跑不 panic、响应正常（已连接 server 的发现行为由 middleware
/// 层单测覆盖）。
#[tokio::test]
async fn test_new_prewarms_mcp_discovery_smoke() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config_with_pending_mcp_pool(&tmp);
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let server_task = tokio::spawn(host::run_acp_server_with_sessions(
        transport,
        cfg,
        sessions.clone(),
    ));

    send_initialize(&mut input_write, &mut output_read).await;
    let result = send_request_and_read_result(
        &mut input_write,
        &mut output_read,
        2,
        "session/new",
        json!({ "cwd": tmp.path().to_str().unwrap() }),
    )
    .await;
    assert!(
        result["sessionId"].as_str().is_some_and(|s| !s.is_empty()),
        "session/new 应返回 sessionId: {result}"
    );
    // prewarm 空跑路径（pending pool 无已连接 server）不 panic

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
    let cfg = test_config(&tmp);
    let thread_store = cfg.thread_store.clone();
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
    let meta = thread_store.load_meta(&session_id).await.unwrap();
    assert_eq!(meta.title.as_deref(), Some("stdio 命名会话"));

    // ── EOF → 宿主优雅退出 ──
    drop(input_write);
    tokio::time::timeout(std::time::Duration::from_secs(10), server_task)
        .await
        .expect("run_acp_server 应在 stdin EOF 后退出")
        .expect("server task 不应 panic");
}
