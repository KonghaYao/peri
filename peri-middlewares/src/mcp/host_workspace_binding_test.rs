//! 宿主 workspace 绑定的契约测试（S2）。
//!
//! 覆盖两种绑定状态（`Remote` / `Absent`）与失败路径：
//!
//! - `Remote`：发现与工具注入走该 HTTP MCP；builtin workspace 不装配。
//! - `Absent`：不装配本机 workspace、不探测本机目录、不回退到会话声明。
//! - 未绑定：既有 bare 行为（builtin workspace 保留）不变。
//! - 错误可诊断；日志/Debug/错误消息不出现 header 值（token），URL 只投影到
//!   scheme/host/port。
//!
//! 设计文档 §1.4 的 `session_injection_test.rs` 三项断言合并到本文件：v5 单出口下
//! 「builtin 开 + 注入 workspace」的实际合并顺序由
//! [`builtin_enabled_injection_keeps_builtin_single_exit_capability`] 记录。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::watch;

use super::HeaderValueSource;
use super::HostWorkspaceBinding;
use super::HostWorkspaceBindingError;
use super::WorkspaceDescriptor;
use super::WorkspaceHeader;
use super::HOST_WORKSPACE_SERVER_NAME;
use crate::mcp::client::McpClientPool;
use crate::mcp::config::ConfigSource;
use crate::mcp::config::McpServerConfig;
use crate::mcp::McpInitStatus;

// ───────────────────────────── 夹具：最小 HTTP MCP server ─────────────────────────────

/// 每个请求的观测记录（方法 + `Authorization` 头）。
type Requests = Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>;

struct FixtureServer {
    url: String,
    requests: Requests,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_request(socket: &mut TcpStream) -> Option<(serde_json::Value, Option<String>)> {
    let mut request = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let size = socket.read(&mut chunk).await.ok()?;
        if size == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..size]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
    let authorization = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_owned())
    });
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let size = socket.read(&mut chunk).await.ok()?;
        if size == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..size]);
    }
    let message = serde_json::from_slice(&request[header_end..header_end + content_length]).ok()?;
    Some((message, authorization))
}

async fn handle_request(mut socket: TcpStream, requests: Requests) {
    let Some((message, authorization)) = read_request(&mut socket).await else {
        return;
    };
    let method = message["method"].as_str().unwrap_or_default().to_owned();
    requests
        .lock()
        .expect("fixture request log")
        .push((method.clone(), authorization));
    if message.get("id").is_none() {
        let _ = socket
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    }
    let response_body = match method.as_str() {
        "server/discover" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "resultType": "complete",
                "supportedVersions": ["2026-07-28"],
                "capabilities": {"tools": {}, "resources": {}},
                "serverInfo": {"name": "host-workspace-fixture", "version": "1.0.0"},
                "ttlMs": 0,
                "cacheScope": "private"
            }
        }),
        "initialize" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "protocolVersion": "2026-07-28",
                "capabilities": {"tools": {}, "resources": {}},
                "serverInfo": {"name": "host-workspace-fixture", "version": "1.0.0"}
            }
        }),
        "tools/list" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {"tools": [
                {"name": "Bash", "description": "Fixture shell", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "Read", "description": "Fixture read", "inputSchema": {"type": "object", "properties": {}}}
            ]}
        }),
        "resources/list" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {"resources": []}
        }),
        "ping" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {}
        }),
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "error": {"code": -32601, "message": "Method not found"}
        }),
    };
    let body = response_body.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = socket.write_all(response.as_bytes()).await;
}

async fn start_fixture_server() -> FixtureServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let requests: Requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let task = tokio::spawn({
        let requests = requests.clone();
        async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let requests = requests.clone();
                tokio::spawn(handle_request(socket, requests));
            }
        }
    });
    FixtureServer {
        url,
        requests,
        task,
    }
}

impl FixtureServer {
    fn request_count(&self) -> usize {
        self.requests.lock().expect("fixture request log").len()
    }

    fn authorization_headers(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("fixture request log")
            .iter()
            .filter_map(|(_, authorization)| authorization.clone())
            .collect()
    }

    fn methods(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("fixture request log")
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }
}

// ───────────────────────────── 夹具：快照 / 运行 ─────────────────────────────

fn snapshot_with(
    scope_dir: &Path,
    global: Option<&str>,
    environment: &[(&str, &str)],
) -> Arc<peri_config::ConfigurationSnapshot> {
    let scope = peri_config::ConfigurationScope::try_new(
        scope_dir.to_owned(),
        scope_dir.join("settings.json"),
    )
    .unwrap();
    Arc::new(
        peri_config::ConfigurationSnapshot::resolve(
            scope,
            peri_config::ConfigurationInputs {
                global: global.map(str::to_owned),
                environment: environment
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap(),
    )
}

fn bind_snapshot(pool: &Arc<McpClientPool>, snapshot: Arc<peri_config::ConfigurationSnapshot>) {
    pool.bind_workspace_scope(peri_acp_types::workspace::WorkspaceId::new())
        .unwrap();
    pool.set_configuration_snapshot(snapshot).unwrap();
}

async fn run_bare(pool: Arc<McpClientPool>, cwd: &Path) -> McpInitStatus {
    let (status, received) = watch::channel(McpInitStatus::Pending);
    McpClientPool::run_initialize_bare(pool, cwd, status).await;
    let settled = received.borrow().clone();
    settled
}

async fn run_full(pool: Arc<McpClientPool>, cwd: &Path) -> McpInitStatus {
    let (status, received) = watch::channel(McpInitStatus::Pending);
    let plugins = cwd.join("no-plugins");
    McpClientPool::run_initialize(pool, cwd, &plugins, status, None).await;
    let settled = received.borrow().clone();
    settled
}

fn remote_binding(url: &str, token: &str) -> HostWorkspaceBinding {
    HostWorkspaceBinding::remote(WorkspaceDescriptor::workspace(
        url,
        vec![WorkspaceHeader::memory("Authorization", token)],
    ))
}

fn tool_names(pool: &McpClientPool, server: &str) -> Vec<String> {
    let mut names: Vec<String> = pool
        .client(server)
        .map(|handle| {
            handle
                .tools
                .iter()
                .map(|tool| tool.name.to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

// ───────────────────────────── Remote：发现与工具注入 ─────────────────────────────

#[tokio::test]
async fn remote_descriptor_discovers_over_http_and_withdraws_builtin_workspace() {
    let fixture = start_fixture_server().await;
    let directory = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    bind_snapshot(&pool, snapshot_with(directory.path(), None, &[]));
    pool.set_host_workspace_binding(remote_binding(&fixture.url, "Bearer fixture-token"))
        .unwrap();

    let status = run_bare(pool.clone(), directory.path()).await;
    assert_eq!(status, McpInitStatus::Ready { total: 1 });

    let configs = pool.configs.read();
    let workspace = configs
        .get(HOST_WORKSPACE_SERVER_NAME)
        .expect("workspace 条目");
    assert_eq!(workspace.url.as_deref(), Some(fixture.url.as_str()));
    assert_eq!(workspace.source, Some(ConfigSource::WorkspaceRemote));
    assert_eq!(workspace.system_mcp, Some(true));
    assert_eq!(workspace.system_mcp_tools, None);
    // descriptor 存在时不再装配本机 builtin workspace（无 `peri` 单出口）。
    assert!(!configs.contains_key(peri_acp_types::builtin_mcp::BUILTIN_V5_SERVER_NAME));
    drop(configs);
    assert!(pool
        .client(peri_acp_types::builtin_mcp::BUILTIN_V5_SERVER_NAME)
        .is_none());
    crate::mcp::builtin::assert_no_builtin_session_collision(&pool.configs.read(), true);

    // 发现走的是该 HTTP MCP：工具清单来自 fixture。
    assert_eq!(
        tool_names(&pool, HOST_WORKSPACE_SERVER_NAME),
        vec!["Bash", "Read"]
    );
    assert_eq!(
        pool.capability_server_name("workspace").as_deref(),
        Some(HOST_WORKSPACE_SERVER_NAME),
        "workspace 能力的落点必须是宿主绑定的远端条目"
    );
    assert!(fixture
        .methods()
        .iter()
        .any(|method| method == "tools/list"));
    assert_eq!(
        fixture.authorization_headers().first().map(String::as_str),
        Some("Bearer fixture-token"),
        "descriptor header 必须原样进入 HTTP 请求"
    );
    assert_eq!(
        pool.workspace_source(),
        Some(ConfigSource::WorkspaceRemote),
        "绑定后、初始化前的来源判定也必须走宿主绑定"
    );
}

#[tokio::test]
async fn binding_is_visible_as_workspace_source_before_initialization() {
    let directory = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    bind_snapshot(&pool, snapshot_with(directory.path(), None, &[]));
    pool.set_host_workspace_binding(remote_binding("http://127.0.0.1:9/mcp", "Bearer t"))
        .unwrap();
    // 未初始化：configs 仍为空，但来源事实已由 typed 输入给出。
    assert!(pool.configs.read().is_empty());
    assert_eq!(pool.workspace_source(), Some(ConfigSource::WorkspaceRemote));
}

#[tokio::test]
async fn host_binding_overrides_session_workspace_declaration() {
    let fixture = start_fixture_server().await;
    let directory = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    bind_snapshot(&pool, snapshot_with(directory.path(), None, &[]));
    let declared: McpServerConfig =
        serde_json::from_value(serde_json::json!({"url": "http://127.0.0.1:1/session-mcp"}))
            .unwrap();
    pool.set_session_servers(HashMap::from([(
        HOST_WORKSPACE_SERVER_NAME.to_owned(),
        declared,
    )]))
    .unwrap();
    pool.set_host_workspace_binding(remote_binding(&fixture.url, "Bearer host-token"))
        .unwrap();

    let status = run_bare(pool.clone(), directory.path()).await;
    assert_eq!(status, McpInitStatus::Ready { total: 1 });
    assert_eq!(
        pool.configs
            .read()
            .get(HOST_WORKSPACE_SERVER_NAME)
            .and_then(|config| config.url.clone()),
        Some(fixture.url.clone())
    );
}

// ───────────────────────────── Absent：无本机 workspace ─────────────────────────────

#[tokio::test]
async fn absent_binding_starts_no_local_workspace() {
    let directory = tempfile::tempdir().unwrap();
    // 故意用一个不存在的 cwd：若仍装配本机 workspace，就会走到目录绑定/资源探测。
    let cwd = directory.path().join("missing-workspace-root");
    let pool = Arc::new(McpClientPool::new_pending());
    bind_snapshot(&pool, snapshot_with(directory.path(), None, &[]));
    pool.set_host_workspace_binding(HostWorkspaceBinding::absent())
        .unwrap();

    let status = run_bare(pool.clone(), &cwd).await;
    assert_eq!(status, McpInitStatus::Ready { total: 0 });
    assert!(pool.configs.read().is_empty());
    assert!(pool.clients().is_empty());
    assert!(pool.client(HOST_WORKSPACE_SERVER_NAME).is_none());
    assert!(pool
        .client(peri_acp_types::builtin_mcp::BUILTIN_V5_SERVER_NAME)
        .is_none());
    assert_eq!(pool.workspace_source(), None);
}

#[tokio::test]
async fn absent_binding_drops_session_workspace_without_fallback() {
    let fixture = start_fixture_server().await;
    let directory = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    bind_snapshot(&pool, snapshot_with(directory.path(), None, &[]));
    let declared: McpServerConfig =
        serde_json::from_value(serde_json::json!({"url": fixture.url})).unwrap();
    pool.set_session_servers(HashMap::from([(
        HOST_WORKSPACE_SERVER_NAME.to_owned(),
        declared,
    )]))
    .unwrap();
    pool.set_host_workspace_binding(HostWorkspaceBinding::absent())
        .unwrap();

    let status = run_bare(pool.clone(), directory.path()).await;
    assert_eq!(status, McpInitStatus::Ready { total: 0 });
    assert!(pool.configs.read().is_empty());
    assert!(pool.client(HOST_WORKSPACE_SERVER_NAME).is_none());
    assert_eq!(fixture.request_count(), 0, "不得回退到会话声明");
}

// ───────────────────────────── 未绑定：既有行为不变 ─────────────────────────────

#[test]
fn bare_config_keeps_builtin_workspace_only_when_unbound() {
    let directory = tempfile::tempdir().unwrap();
    let snapshot = snapshot_with(directory.path(), None, &[]);

    let with_local =
        crate::mcp::config::load_bare_config_from_snapshot_with_mode_and_local_workspace(
            &snapshot, None, true,
        )
        .unwrap();
    assert!(
        crate::mcp::builtin::builtin_workspace_capability_enabled(&with_local.mcp_servers),
        "未绑定时 builtin workspace 能力必须保留（既有 bare 行为）"
    );
    assert!(!with_local
        .mcp_servers
        .contains_key(HOST_WORKSPACE_SERVER_NAME));

    let without_local =
        crate::mcp::config::load_bare_config_from_snapshot_with_mode_and_local_workspace(
            &snapshot, None, false,
        )
        .unwrap();
    assert!(without_local.mcp_servers.is_empty());
    assert!(!crate::mcp::builtin::builtin_workspace_capability_enabled(
        &without_local.mcp_servers
    ));
}

// ───────────────────────────── 非 bare：builtin 关闭组合契约（§1.4） ─────────────────────────────

#[tokio::test]
async fn builtin_closed_injected_workspace_is_single_remote_surface() {
    let fixture = start_fixture_server().await;
    let directory = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    bind_snapshot(
        &pool,
        snapshot_with(directory.path(), None, &[("PERI_MCP_BUILTIN", "off")]),
    );
    let mut declared: McpServerConfig =
        serde_json::from_value(serde_json::json!({"url": fixture.url})).unwrap();
    declared.source = Some(ConfigSource::WorkspaceRemote);
    declared.system_mcp = Some(true);
    pool.set_session_servers(HashMap::from([(
        HOST_WORKSPACE_SERVER_NAME.to_owned(),
        declared,
    )]))
    .unwrap();

    let status = run_full(pool.clone(), directory.path()).await;
    assert_eq!(status, McpInitStatus::Ready { total: 1 });
    let configs = pool.configs.read();
    assert_eq!(configs.len(), 1, "builtin 关闭时只有注入的 workspace");
    let workspace = configs.get(HOST_WORKSPACE_SERVER_NAME).unwrap();
    assert_eq!(workspace.source, Some(ConfigSource::WorkspaceRemote));
    assert_eq!(workspace.system_mcp, Some(true));
    assert!(!configs.contains_key(peri_acp_types::builtin_mcp::BUILTIN_V5_SERVER_NAME));
}

/// 设计 §1.4 断言 2 的实际行为记录：v5 单出口下 builtin 开启时，会话注入的远端
/// `workspace` 覆盖同名条目，但 `peri` 仍保留 workspace 能力（两个 workspace 面）。
///
/// S2 不修改非 bare 合并顺序（设计允许的处置）：swarm 路径用 `builtin_mcp:false`
/// 规避这个组合（见上一个测试）。若未来给非 bare 路径补 capability 撤下逻辑，
/// 本测试必须同步更新为「无 workspace 能力」。
#[tokio::test]
async fn builtin_enabled_injection_keeps_builtin_single_exit_capability() {
    let fixture = start_fixture_server().await;
    let directory = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    bind_snapshot(
        &pool,
        snapshot_with(
            directory.path(),
            Some(r#"{"mcpServers":{"peri":{"disabled":true}}}"#),
            &[],
        ),
    );
    let mut declared: McpServerConfig =
        serde_json::from_value(serde_json::json!({"url": fixture.url})).unwrap();
    declared.source = Some(ConfigSource::WorkspaceRemote);
    declared.system_mcp = Some(true);
    pool.set_session_servers(HashMap::from([(
        HOST_WORKSPACE_SERVER_NAME.to_owned(),
        declared,
    )]))
    .unwrap();

    let status = run_full(pool.clone(), directory.path()).await;
    assert_eq!(status, McpInitStatus::Ready { total: 1 });
    let configs = pool.configs.read();
    assert_eq!(
        configs
            .get(HOST_WORKSPACE_SERVER_NAME)
            .and_then(|config| config.url.clone()),
        Some(fixture.url.clone()),
        "同名注入条目覆盖 builtin"
    );
    assert!(
        configs.contains_key(peri_acp_types::builtin_mcp::BUILTIN_V5_SERVER_NAME),
        "既有合并顺序保留 builtin 单出口"
    );
    assert!(
        crate::mcp::builtin::builtin_workspace_capability_enabled(&configs),
        "既有合并顺序不会撤下 builtin workspace 能力（设计 §1.4 记录的差异）"
    );
}

// ───────────────────────────── 失败路径 ─────────────────────────────

#[tokio::test]
async fn binding_rejects_non_workspace_name_without_leaking_headers() {
    let pool = McpClientPool::new_pending();
    let error = pool
        .set_host_workspace_binding(HostWorkspaceBinding::remote(WorkspaceDescriptor::new(
            "web",
            "http://127.0.0.1:9/mcp",
            vec![WorkspaceHeader::memory(
                "Authorization",
                "Bearer secret-value",
            )],
        )))
        .unwrap_err();
    assert_eq!(
        error,
        HostWorkspaceBindingError::NameNotWorkspace {
            name: "web".to_owned()
        }
    );
    assert!(!error.to_string().contains("secret-value"));
}

#[tokio::test]
async fn binding_rejects_invalid_url_and_duplicate_headers() {
    let pool = McpClientPool::new_pending();
    let error = pool
        .set_host_workspace_binding(HostWorkspaceBinding::remote(
            WorkspaceDescriptor::workspace("workspace.example.test/mcp", vec![]),
        ))
        .unwrap_err();
    assert_eq!(error, HostWorkspaceBindingError::InvalidUrl);
    assert!(!error.to_string().contains("workspace.example.test"));

    let error = pool
        .set_host_workspace_binding(HostWorkspaceBinding::remote(
            WorkspaceDescriptor::workspace(
                "http://127.0.0.1:9/mcp",
                vec![
                    WorkspaceHeader::memory("Authorization", "Bearer a"),
                    WorkspaceHeader::memory("authorization", "Bearer b"),
                ],
            ),
        ))
        .unwrap_err();
    assert_eq!(
        error,
        HostWorkspaceBindingError::DuplicateHeader {
            name: "authorization".to_owned()
        }
    );
    assert!(!error.to_string().contains("Bearer"));

    let error = pool
        .set_host_workspace_binding(HostWorkspaceBinding::remote(
            WorkspaceDescriptor::workspace(
                "http://127.0.0.1:9/mcp",
                vec![WorkspaceHeader::memory("", "value")],
            ),
        ))
        .unwrap_err();
    assert_eq!(error, HostWorkspaceBindingError::EmptyHeaderName);
}

#[tokio::test]
async fn missing_env_header_reports_variable_name_not_value() {
    let pool = McpClientPool::new_pending();
    let var = "PERI_S2_HOST_WORKSPACE_MISSING_TOKEN";
    assert!(std::env::var_os(var).is_none(), "测试前置：变量不存在");
    let error = pool
        .set_host_workspace_binding(HostWorkspaceBinding::remote(
            WorkspaceDescriptor::workspace(
                "http://127.0.0.1:9/mcp",
                vec![WorkspaceHeader::from_env("Authorization", var)],
            ),
        ))
        .unwrap_err();
    assert_eq!(
        error,
        HostWorkspaceBindingError::MissingHeaderEnvironment {
            header: "Authorization".to_owned(),
            var: var.to_owned(),
        }
    );
    let message = error.to_string();
    assert!(message.contains(var));
    assert!(message.contains("Authorization"));
}

#[tokio::test]
async fn env_header_value_is_resolved_once_at_bind_time() {
    let fixture = start_fixture_server().await;
    let directory = tempfile::tempdir().unwrap();
    let var = "PERI_S2_HOST_WORKSPACE_ENV_TOKEN";
    std::env::set_var(var, "Bearer env-token-value");
    let pool = Arc::new(McpClientPool::new_pending());
    bind_snapshot(&pool, snapshot_with(directory.path(), None, &[]));
    let binding = HostWorkspaceBinding::remote(WorkspaceDescriptor::workspace(
        &fixture.url,
        vec![WorkspaceHeader::from_env("Authorization", var)],
    ));
    pool.set_host_workspace_binding(binding).unwrap();
    // 绑定后变量消失不影响本次绑定：值已在内存里。
    std::env::remove_var(var);

    let status = run_bare(pool, directory.path()).await;
    assert_eq!(status, McpInitStatus::Ready { total: 1 });
    assert_eq!(
        fixture.authorization_headers().first().map(String::as_str),
        Some("Bearer env-token-value")
    );
}

#[tokio::test]
async fn binding_window_closes_after_initialization_started() {
    let directory = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    bind_snapshot(
        &pool,
        snapshot_with(directory.path(), None, &[("PERI_MCP_BUILTIN", "off")]),
    );
    let status = run_bare(pool.clone(), directory.path()).await;
    assert_eq!(status, McpInitStatus::Ready { total: 0 });
    let error = pool
        .set_host_workspace_binding(HostWorkspaceBinding::absent())
        .unwrap_err();
    assert_eq!(error, HostWorkspaceBindingError::WindowClosed);
}

#[tokio::test]
async fn second_binding_is_rejected() {
    let pool = McpClientPool::new_pending();
    pool.set_host_workspace_binding(HostWorkspaceBinding::absent())
        .unwrap();
    let error = pool
        .set_host_workspace_binding(remote_binding("http://127.0.0.1:9/mcp", "Bearer t"))
        .unwrap_err();
    assert_eq!(error, HostWorkspaceBindingError::AlreadySet);
}

#[tokio::test]
async fn unreachable_remote_workspace_fails_without_leaking_token() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let directory = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    bind_snapshot(&pool, snapshot_with(directory.path(), None, &[]));
    let token = "Bearer unreachable-secret-token";
    pool.set_host_workspace_binding(remote_binding(
        &format!("http://127.0.0.1:{port}/mcp"),
        token,
    ))
    .unwrap();

    let status = run_bare(pool.clone(), directory.path()).await;
    let McpInitStatus::Failed(message) = status else {
        panic!("不可达的 workspace 必须显式失败，实际 {status:?}");
    };
    assert!(message.contains(HOST_WORKSPACE_SERVER_NAME));
    assert!(
        !message.contains("unreachable-secret-token"),
        "错误消息不得带出 header 值: {message}"
    );
    assert!(matches!(
        pool.client(HOST_WORKSPACE_SERVER_NAME)
            .map(|handle| handle.status.clone()),
        Some(crate::mcp::ClientStatus::Failed(_))
    ));
}

#[test]
fn debug_redacts_header_values_and_url_tail() {
    let binding = HostWorkspaceBinding::remote(WorkspaceDescriptor::workspace(
        "http://127.0.0.1:1234/mcp?token=url-secret",
        vec![
            WorkspaceHeader::memory("Authorization", "Bearer debug-secret"),
            WorkspaceHeader::from_env("X-Api-Key", "PERI_S2_DEBUG_TOKEN_VAR"),
        ],
    ));
    let debug = format!("{binding:?}");
    assert!(!debug.contains("debug-secret"));
    assert!(!debug.contains("url-secret"));
    assert!(debug.contains("Authorization"));
    assert!(debug.contains("PERI_S2_DEBUG_TOKEN_VAR"));
    assert!(debug.contains("http://127.0.0.1:1234/***"));
    // 环境变量来源只出现在变量名位，不出现值。
    assert!(matches!(
        WorkspaceHeader::from_env("Authorization", "VAR").value,
        HeaderValueSource::Env { .. }
    ));
}
