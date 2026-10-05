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
//! session/new MCP 发现预热 smoke 迁入本文件——经 `run_acp_server_with_sessions`
//! （外部注入共享 session map）驱动统一路径，断言从「handler 直调 + StdioContext
//! 内窥」改为「wire 驱动 + 共享 map 内窥」（`test_delete_removes_thread_*` 与 prewarm
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

    let assembled = assemble_stdio_config(
        crate::host::stdio::StdioInput {
            cwd: input_workspace.to_string_lossy().into_owned(),
            settings_stdin: false,
            permission_mode: peri_acp_types::permission::SharedPermissionMode::new(
                peri_acp_types::permission::PermissionMode::Bypass,
            ),
            session_store: peri_acp_types::session_store::SessionStoreDeployment::local_path(
                tmp.path().join("threads.db"),
            ),
        },
        None,
    )
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
    make_server_config_with(peri_config, provider, tmp, None, None).await
}

/// 同 [`make_server_config`]，另注入 `mcp_pool`（MCP 发现预热 smoke 测试）——与迁移前
/// `create_test.rs::make_stdio_context` 的「装配后显式替换」等价的参数化形态。
async fn make_server_config_with(
    peri_config: PeriConfig,
    provider: LlmProvider,
    tmp: &tempfile::TempDir,
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

/// 带已初始化空 `mcp_pool` 的测试配置（MCP 发现预热 smoke：pool 存在但
/// 无已连接 server）。
async fn test_config_with_empty_mcp_pool(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let pool = Arc::new(peri_middlewares::mcp::McpClientPool::new_pending());
    pool.set_builtin_available(false).unwrap();
    let (status_tx, _) = tokio::sync::watch::channel(peri_middlewares::mcp::McpInitStatus::Pending);
    peri_middlewares::mcp::McpClientPool::run_initialize_bare(pool.clone(), tmp.path(), status_tx)
        .await;
    let pool: Arc<dyn peri_acp_types::ports::McpPoolPort> = pool;
    make_server_config_with(peri_config, provider, tmp, Some(pool), None).await
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

/// 等待宿主在 stdin EOF 后优雅退出。
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
    cfg.session_resources
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

    // ── EOF → 宿主优雅退出 ──
    drop(input_write);
    tokio::time::timeout(std::time::Duration::from_secs(10), server_task)
        .await
        .expect("run_acp_server 应在 stdin EOF 后退出")
        .expect("server task 不应 panic");
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

#[path = "run_server_rename_test.rs"]
mod rename_tests;

#[path = "langfuse_shutdown_test.rs"]
mod langfuse_shutdown_tests;

#[path = "session_store_shutdown_test.rs"]
mod session_store_shutdown_tests;

#[path = "run_server_session_resources_test.rs"]
mod session_resources_tests;
