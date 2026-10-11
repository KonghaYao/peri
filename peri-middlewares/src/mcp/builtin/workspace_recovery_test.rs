//! Regression scenarios through the real workspace server and MCP bridge.
//!
//! 观测面是**宿主侧语义**：真实 MCP 桥的取消/超时/期限与进程收尾（无 orphan）、
//! session 级 TaskManager 的登记与回收、以及共享映射的诊断保真烟测。
//! 工具语义与逐工具诊断文本（recovery / path hints / 失败正文）的覆盖在 package 侧
//! （`peri-mcp-workspace` 的 `filesystem/path_hints_test.rs`、`filesystem/*_test.rs`、
//! `terminal_evidence_test.rs`），本文件不重复。
use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::client::{serve_client_auto, McpServiceWrapper};
use crate::mcp::client::{ClientStatus, McpClientHandle};
use crate::mcp::config::ConfigSource;
use crate::mcp::tool_bridge::McpToolBridge;
use crate::mcp::McpClientPool;
use peri_acp_types::tasks::TaskManager;
use peri_agent::tools::{BaseTool, ToolContext};
use peri_mcp_workspace::{WorkspaceInstanceInput, WorkspaceMcpServer};
use rmcp::{
    model::CallToolRequestParams,
    service::{Peer, RoleClient},
    ServiceError,
};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

const SESSION_ID: &str = "workspace-recovery-session";

async fn bridge(pair: &Pair, name: &str, builtin: bool) -> McpToolBridge {
    let tools = pair.peer().list_tools(None).await.unwrap().tools;
    let tool = tools.iter().find(|tool| tool.name == name).unwrap().clone();
    let handle = Arc::new(McpClientHandle {
        name: "workspace".into(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(pair.peer()),
        tools,
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: Default::default(),
        source: builtin.then(|| ConfigSource::Builtin {
            instance: "workspace".into(),
        }),
        url: None,
        skills_capable: false,
    });
    pair.pool
        .clients
        .write()
        .insert("workspace".into(), handle.clone());
    McpToolBridge::new("workspace", &tool, handle).with_output_store(&pair.pool, Some(SESSION_ID))
}

async fn wait_file(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("shell reached its start marker");
}

#[test]
fn unclassified_errors_preserve_diagnostics_without_recovery_authority() {
    let diagnostic =
        "task_id: synthetic-task\npid: 1\ncredential=synthetic-marker\nPermission denied";
    let raw: Box<dyn std::error::Error + Send + Sync> = diagnostic.into();
    let text = peri_mcp_common::result_mapping::failure_text("Bash", raw.as_ref());
    assert_eq!(text, format!("tool `Bash` failed to execute; {diagnostic}"));
    assert!(!text.contains("\nRecovery:"));
    let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "synthetic-io-marker");
    let text = peri_mcp_common::result_mapping::failure_text("Read", &io);
    assert_eq!(io.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(text, format!("tool `Read` failed to execute; {io}"));
    assert!(!text.contains("\nRecovery:"));
}

#[cfg(unix)]
async fn assert_process_gone(pid: i32) {
    // 取消通知最多用 1s 发送，进程组 TERM 到 KILL 还有 2s 宽限；
    // 全套并发运行时给调度和进程回收留出余量。
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !std::process::Command::new("kill")
                .args(["-0", "--", &format!("-{pid}")])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the owned process group must exit");
}

/// [回归测试] Dropping the bridge future cancels the server request and its real shell.
#[cfg(unix)]
#[tokio::test]
async fn cancelled_bridge_stops_process_and_drains_session_ownership() {
    let (dir, cwd) = workspace_dir();
    let manager: Arc<dyn TaskManager> = Arc::new(peri_mcp_common::create_local_task_manager());
    let pair = connect(
        &cwd,
        Some(WorkspaceInstanceInput {
            task_manager: Some(manager.clone()),
            on_bg_complete: None,
        }),
    )
    .await;
    let bash = bridge(&pair, "Bash", true).await;
    let running = tokio::spawn(async move {
        bash.invoke(
            json!({"command":"echo $$ > started.pid.tmp; mv started.pid.tmp started.pid; exec sleep 60", "timeout":120000}),
            ToolContext::new(&[], "").with_session_identity(SESSION_ID, "recovery-turn"),
        )
        .await
    });
    let marker = dir.path().join("started.pid");
    wait_file(&marker).await;
    let pid = std::fs::read_to_string(marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        std::process::Command::new("kill")
            .args(["-0", "--", &format!("-{pid}")])
            .status()
            .unwrap()
            .success(),
        "negative control: process must exist before cancellation"
    );
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    assert_process_gone(pid).await;
    assert_eq!(manager.active_count(), 0);
    assert_eq!(
        manager.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    let read = bridge(&pair, "Read", true).await;
    assert!(read
        .invoke(
            json!({"file_path":"started.pid"}),
            ToolContext::new(&[], &cwd).with_session_identity(SESSION_ID, "recovery-turn")
        )
        .await
        .is_ok());
    pair.shutdown().await;
}

/// [回归测试] The actual 120-second Bash boundary must return its recoverable receipt.
#[cfg(unix)]
#[tokio::test]
async fn maximum_foreground_timeout_retains_logs_and_cancellable_task() {
    let (_dir, cwd) = workspace_dir();
    let manager: Arc<dyn TaskManager> = Arc::new(peri_mcp_common::create_local_task_manager());
    let pair = connect(
        &cwd,
        Some(WorkspaceInstanceInput {
            task_manager: Some(manager.clone()),
            on_bg_complete: None,
        }),
    )
    .await;
    let bash = bridge(&pair, "Bash", true).await;
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(140),
        bash.invoke(
            json!({"command":"echo live-marker; exec sleep 180", "timeout":120000}),
            ToolContext::new(&[], &cwd).with_session_identity(SESSION_ID, "recovery-turn"),
        ),
    )
    .await
    .unwrap()
    .unwrap_err()
    .to_string();
    assert!(started.elapsed() >= Duration::from_secs(120));
    assert!(
        result.contains("task_id: shell-") && result.contains("pid: "),
        "{result}"
    );
    let task = result
        .lines()
        .find_map(|line| line.strip_prefix("task_id: "))
        .unwrap();
    let pid: i32 = result
        .lines()
        .find_map(|line| line.strip_prefix("pid: "))
        .unwrap()
        .parse()
        .unwrap();
    let stdout = result
        .lines()
        .find_map(|line| line.strip_prefix("- Live output: Read the log file "))
        .unwrap()
        .split(" (stderr:")
        .next()
        .unwrap();
    let read = bridge(&pair, "Read", true).await;
    let live_output = read
        .invoke(
            json!({"file_path":stdout}),
            ToolContext::new(&[], &cwd).with_session_identity(SESSION_ID, "recovery-turn"),
        )
        .await;
    let active_count_before_cancel = manager.active_count();
    manager.cancel(task).unwrap();
    assert_process_gone(pid).await;
    assert_eq!(
        manager.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    pair.shutdown().await;
    assert_eq!(active_count_before_cancel, 1);
    assert!(live_output.unwrap().contains("live-marker"));
    assert!(
        result.contains("Command that timed out: echo live-marker; exec sleep 180"),
        "{result}"
    );
}

/// External servers keep the bounded request policy; timeout notifies the real handler.
#[cfg(unix)]
#[tokio::test]
async fn request_timeout_stops_server_shell() {
    let (dir, cwd) = workspace_dir();
    let manager: Arc<dyn TaskManager> = Arc::new(peri_mcp_common::create_local_task_manager());
    let pair = connect(
        &cwd,
        Some(WorkspaceInstanceInput {
            task_manager: Some(manager.clone()),
            on_bg_complete: None,
        }),
    )
    .await;
    let result = crate::mcp::tool_request::call_tool(
        &pair.peer(),
        call(
            "Bash",
            json!({"command":"echo $$ > timeout.pid; exec sleep 60", "timeout":120000}),
        ),
        Some(Duration::from_secs(1)),
    )
    .await;
    assert!(matches!(result, Err(ServiceError::Timeout { .. })));
    let pid = std::fs::read_to_string(dir.path().join("timeout.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_process_gone(pid).await;
    assert_eq!(manager.active_count(), 0);
    assert_eq!(
        manager.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    pair.shutdown().await;
}

/// Reserved-looking names cannot bypass the external MCP deadline without builtin provenance.
#[cfg(unix)]
#[tokio::test]
async fn external_source_keeps_120_second_deadline_and_cancels_execution() {
    let (dir, cwd) = workspace_dir();
    let pair = connect(&cwd, None).await;
    let bash = bridge(&pair, "Bash", false).await;
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(130),
        bash.invoke(
            json!({"command":"echo $$ > external.pid; exec sleep 180", "timeout":120000}),
            ToolContext::new(&[], &cwd).with_session_identity(SESSION_ID, "recovery-turn"),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("调用超时 (120s)"), "{error}");
    assert!(started.elapsed() >= Duration::from_secs(120));
    let pid = std::fs::read_to_string(dir.path().join("external.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_process_gone(pid).await;
    pair.shutdown().await;
}

/// client 侧握手上界（`serve_client_auto` 内建）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界（夹具收尾）。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// 一条已握手的 builtin 链路（client service + 本代关闭所有权）。
struct Pair {
    service: McpServiceWrapper,
    supervisor: BuiltinInstanceSupervisor,
    pool: Arc<McpClientPool>,
}

impl Pair {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    /// 夹具收尾：关闭 client（释放 duplex 写半边）→ 本代监督者按冻结顺序有界收敛。
    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        let _ = self.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    }
}

/// 经**生产**装配函数握手一条 `workspace` 链路（handler 是真 handler，工具是真工具）。
///
/// `input` 即 AW3-11 的 session 级输入：`None` 是顶层三路径 / 1:N 形态的形态（可见但退化），
/// 既有五条线路用例都走该形态；后台任务两条用例显式传 `Some` / `None` 各一遍。
async fn connect(cwd: &str, input: Option<WorkspaceInstanceInput>) -> Pair {
    let pool = Arc::new(McpClientPool::new_empty());
    let manager: Arc<dyn TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager(SESSION_ID, &manager);
    let transport =
        spawn_builtin_transport_with_handler("workspace", WorkspaceMcpServer::new(cwd, input));
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("builtin 握手不得超时（同进程链路）")
        .expect("builtin 握手不得失败");
    Pair {
        service,
        supervisor,
        pool,
    }
}

/// 临时工作目录（**已 canonicalize**）。
///
/// macOS 的 `/var` 是 `/private/var` 的符号链接：不规范化时工具内部的
/// `resolve_path`（`tools/filesystem/mod.rs:27-44` 会 canonicalize）与夹具的
/// `strip_prefix(cwd)` 口径不一致，写回文本会变成绝对路径。规范化后夹具与工具同源。
fn workspace_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("临时目录夹具必须可创建");
    let cwd = dir.path().canonicalize().expect("夹具目录必须可规范化");
    (dir, cwd.to_string_lossy().to_string())
}

/// `tools/call` 请求（`arguments` 必须是 JSON object；缺省 = 空对象）。
fn call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}
