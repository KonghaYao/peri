#![cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]
//! 宿主遥测装配与关闭报告保有的真实生命周期回归（契约 §3.6、§5.2、§7 P4b、§8）。
//!
//! 每个用例都驱动**真实的** [`crate::host::AcpHostHandle`] 与真实 `LangfuseShutdownOwner`：
//! 交付成败由受控本地 OTLP 端点决定（真实 TCP、运行期可切状态码、可把在途响应阻塞到放行），
//! 暂存能力用真实 `NativeSpool`（临时根），不存在假 store、mockall 或只看枚举转换的断言。
//!
//! 本文件同时固定一条**实测**语义（R3 证据）：`Batcher` 的关闭终态在 worker join 之后不可变
//! （`langfuse-client/src/batcher.rs`：重复调用返回相同终态、不重新发送事件），因此第二次宿主
//! 关闭会真正重入 `finish`（重新排空资源、再次动用部署关闭权），但遥测只会读到同一终态。
//! 契约 §8 R3 期望的「第二次返回 `Complete` 或已完成移交的 `PersistedPending`」在真实路径
//! 不可达，已按契约冲突条目上报；这里如实断言实际行为，不写成期望的假象。

use std::future::Future;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use langfuse_client::spool::native::NativeSpool;
use langfuse_client::spool::{route_fingerprint, SpoolStore};
use langfuse_client::types::TraceBody;
use langfuse_client::{FlushReport, IngestionEvent, LangfuseError};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::session_resources::{SessionResourceResult, SessionStoreShutdownPort};
use peri_config::observability::LangfuseConfig;
use peri_controller::langfuse::{
    LangfuseSession, LangfuseSessionLike, LangfuseShutdownOwner, LangfuseShutdownReport,
};
use serial_test::serial;

use crate::host::{spawn_acp_server, AcpHostHandle, AcpHostShutdownReport, AcpServerConfig};
use crate::provider::{LlmProvider, PeriConfig, ProviderConfig, ProviderModels};
use crate::transport::stdio::StdioTransport;

fn event(id: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some(id.into()),
            name: Some(id.into()),
            ..Default::default()
        },
        metadata: None,
    }
}

/// 有界等待：条件成立立即返回，到期 panic（不靠固定 sleep 断言到达）。
async fn wait_until(what: &str, deadline: Duration, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not reached within {deadline:?}: {what}");
}

// ── 受控 OTLP 端点（真实 TCP + 脚本状态码 + 可阻塞的在途响应）────────────────

/// 放行门：`release` 之前，读完请求的处理线程停在响应之前（受控在途 IO）。
#[derive(Default)]
struct Gate {
    released: Mutex<bool>,
    cv: Condvar,
}

impl Gate {
    fn wait_for_release(&self) {
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.cv.wait(released).unwrap();
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

/// 脚本化 OTLP 端点：状态码可在运行期切换，`gate` 存在时每个请求都阻塞到放行。
///
/// 处理线程按连接 spawn，accept 线程随测试进程结束（测试二进制退出即回收）。
struct ScriptedIngestion {
    url: String,
    status: Arc<AtomicU16>,
    requests: Arc<AtomicUsize>,
    gate: Option<Arc<Gate>>,
}

impl ScriptedIngestion {
    fn start(status: u16) -> Self {
        Self::start_with_gate(status, false)
    }

    fn start_gated(status: u16) -> Self {
        Self::start_with_gate(status, true)
    }

    fn start_with_gate(status: u16, gated: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted ingestion");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let status = Arc::new(AtomicU16::new(status));
        let requests = Arc::new(AtomicUsize::new(0));
        let gate = gated.then(|| Arc::new(Gate::default()));
        let (accept_status, accept_requests, accept_gate) =
            (Arc::clone(&status), Arc::clone(&requests), gate.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (status, requests, gate) = (
                    Arc::clone(&accept_status),
                    Arc::clone(&accept_requests),
                    accept_gate.clone(),
                );
                std::thread::spawn(move || serve(stream, &status, &requests, gate.as_deref()));
            }
        });
        Self {
            url,
            status,
            requests,
            gate,
        }
    }

    fn set_status(&self, status: u16) {
        self.status.store(status, Ordering::SeqCst);
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn release(&self) {
        self.gate.as_ref().expect("gated endpoint").release();
    }
}

/// 一次请求：读完头与 `content-length` 正文，计数，必要时等在途门，再按当前状态码应答。
fn serve(mut stream: TcpStream, status: &AtomicU16, requests: &AtomicUsize, gate: Option<&Gate>) {
    if read_request(&mut stream).is_none() {
        return;
    }
    requests.fetch_add(1, Ordering::SeqCst);
    if let Some(gate) = gate {
        gate.wait_for_release();
    }
    let code = status.load(Ordering::SeqCst);
    let (reason, body, retry) = match code {
        200 => ("OK", "{}", ""),
        // 可重试状态：`Retry-After` 是客户端周期内重试的最短等待，取 1 秒让用例快速收敛。
        503 => ("Service Unavailable", "", "retry-after: 1\r\n"),
        _ => ("Error", "", ""),
    };
    let response = format!(
        "HTTP/1.1 {code} {reason}\r\ncontent-length: {}\r\n{retry}connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

/// 读到请求头结束且正文完整为止；`None` 表示对端提前关闭。
fn read_request(stream: &mut TcpStream) -> Option<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_head_end(&buf) {
            if buf.len() >= end + header_value(&buf[..end], "content-length").unwrap_or(0) {
                return Some(());
            }
        }
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn header_value(head: &[u8], name: &str) -> Option<usize> {
    let head = String::from_utf8_lossy(head);
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

// ── 部署夹具：真实会话 + 真实端口 + 宿主配置 ────────────────────────────────

/// 端点/凭据配置：默认**关闭**暂存（有界内存模式），需要端口的用例显式覆盖。
fn langfuse_config(url: &str) -> LangfuseConfig {
    LangfuseConfig {
        public_key: Some("pk-test".into()),
        secret_key: Some("sk-test".into()),
        host: url.into(),
        batch_max_events: 50,
        batch_flush_interval_secs: 3_600,
        spool_enabled: false,
        ..Default::default()
    }
}

/// 需要真实端口的用例配置：正文内存前缀压到最小，导出必然走文件。
fn spool_config(url: &str) -> LangfuseConfig {
    LangfuseConfig {
        spool_enabled: true,
        payload_memory_threshold_bytes: 64,
        spool_quota_bytes: SPOOL_QUOTA,
        ..langfuse_config(url)
    }
}

const SPOOL_QUOTA: u64 = 8 * 1024 * 1024;

/// 真实 native 端口：临时根 + 与同一凭据/端点一致的 route 摘要。
fn native_spool(tmp: &tempfile::TempDir, url: &str) -> Arc<dyn SpoolStore> {
    let route = route_fingerprint(url, "pk-test", "sk-test").expect("route fingerprint");
    Arc::new(
        NativeSpool::open_at(tmp.path().join("spool-root"), route, SPOOL_QUOTA)
            .expect("native spool"),
    )
}

async fn owned_session(
    config: LangfuseConfig,
    spool: Option<Arc<dyn SpoolStore>>,
) -> (Arc<LangfuseSession>, LangfuseShutdownOwner) {
    LangfuseSession::new_owned(config, "deployment".into(), spool)
        .await
        .expect("deployment session")
}

/// 部署关闭权替身：只记录调用次数，用于区分「真重入」与「命中缓存终态」。
struct CountingClose(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl SessionStoreShutdownPort for CountingClose {
    async fn shutdown(&self) -> SessionResourceResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// 起一个真实宿主：注入共享会话与独占关闭权限，返回宿主与 client 侧写入端。
///
/// 关闭 admission（丢弃写入端 → 传输 EOF）由调用方在事件入队**之后**执行：宿主收不到
/// EOF 就不会开始排空，夹具因此不会与用例的交付观察竞态。
async fn start_host(
    tmp: &tempfile::TempDir,
    session: &Arc<LangfuseSession>,
    owner: LangfuseShutdownOwner,
    close_calls: &Arc<AtomicUsize>,
) -> (AcpHostHandle, tokio::io::DuplexStream) {
    let mut cfg: AcpServerConfig = server_config(tmp).await;
    cfg.langfuse_session = Some(Arc::clone(session));
    cfg.langfuse_shutdown_owner = Some(owner);
    cfg.session_store_shutdown = Some(Box::new(CountingClose(Arc::clone(close_calls))));
    let (transport, input, _output) = duplex_transport();
    let host = spawn_acp_server(Arc::new(transport), cfg);
    (host, input)
}

/// duplex 驱动的 `StdioTransport`（与 stdio 集成测试同形）。
fn duplex_transport() -> (
    StdioTransport,
    tokio::io::DuplexStream,
    tokio::io::DuplexStream,
) {
    let (input_write, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, output_read) = tokio::io::duplex(64 * 1024);
    (
        StdioTransport::from_reader_writer(transport_read, transport_write),
        input_write,
        output_read,
    )
}

/// 最小宿主配置（与其它 host 测试同构：provider 假 key，无外部依赖）。
async fn server_config(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![ProviderConfig {
        id: "a".into(),
        provider_type: "openai".into(),
        api_key: "sk-openai-test".into(),
        models: ProviderModels {
            sonnet: "gpt-4o".into(),
            ..Default::default()
        },
        ..Default::default()
    }];
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .expect("session resources");
    let session_manager = crate::session::SessionManager::new(
        session_resources.clone(),
        provider.clone(),
        Arc::new(peri_config.clone()),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
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
        acp_mcp: None,
        dynamic_mcp: None,
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
        shared_tools: Arc::new(parking_lot::RwLock::new(std::collections::BTreeMap::new())),
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
        stdio_command_filter: true,
    }
}

// ── R3：跨两次真实 host shutdown ────────────────────────────────────────────

/// 第一次关闭遇受控 IO 阻塞（交付尝试被端点拒绝并推迟到允许的重试时刻）报告
/// `TelemetryFailed(Pending)`：保留原 context/owner、不缓存终态；解除阻塞后第二次关闭
/// **真正重入** `finish`（重新排空资源、重新动用部署关闭权）。
///
/// 契约冲突证据：`Batcher` 终态在 worker join 之后不可变（`langfuse-client/src/batcher.rs`：
/// 重复调用返回相同终态、不重新发送事件），因此第二次关闭读到的是同一冻结终态——受控端点
/// 此时已可交付，请求数仍为 1。契约 §8 R3 期望的「第二次返回 `Complete` 或已完成移交的
/// `PersistedPending`」在真实路径不可达，已按契约冲突条目上报。
#[tokio::test]
async fn host_shutdown_reenters_after_telemetry_pending() {
    let tmp = tempfile::TempDir::new().unwrap();
    let endpoint = ScriptedIngestion::start(503);
    // 有界内存模式（无端口）：不可恢复的待交付事件只能报告 Pending，不能冒充移交。
    let (session, owner) = owned_session(langfuse_config(&endpoint.url), None).await;
    let close_calls = Arc::new(AtomicUsize::new(0));
    let (mut host, input) = start_host(&tmp, &session, owner, &close_calls).await;

    session.try_add(event("pending-one")).unwrap();
    // 交付尝试已经到达端点，且解析为「未到期的重试等待」：前缀未终态。
    let flush = session.flush().await;
    assert!(
        matches!(flush, Err(LangfuseError::Pending { ref counts })
            if counts.pending_events == 1 && counts.persisted_pending_events == 0),
        "内存模式的失败交付必须按 Pending 报告: {flush:?}"
    );
    assert!(
        endpoint.requests() >= 1,
        "受控端点应已收到交付尝试: {}",
        endpoint.requests()
    );

    // 传输 EOF：宿主开始关闭；此时该交付仍在等待下一次允许的重试时刻。
    drop(input);
    let first = host.shutdown().await;
    let AcpHostShutdownReport::TelemetryFailed(telemetry) = &first else {
        panic!("第一次关闭应报告遥测待交付: {first:?}");
    };
    let LangfuseShutdownReport::Pending { counts } = telemetry else {
        panic!("内存模式下的未交付积压必须报告 Pending: {telemetry:?}");
    };
    assert_eq!(counts.pending_events, 1);
    assert_eq!(counts.persisted_pending_events, 0);
    assert_eq!(counts.active_tasks, 0);
    assert_eq!(counts.cleanup_pending_objects, 0);
    assert!(!first.is_complete());
    assert!(
        telemetry.retains_ownership(),
        "未收敛的待交付不得被当作终态缓存"
    );
    assert_eq!(
        close_calls.load(Ordering::SeqCst),
        1,
        "资源排空之后才动用部署关闭权"
    );

    // 终态不可变：worker 已 join，重入不会产生新的交付尝试（契约冲突证据）。
    let attempts_at_terminal = endpoint.requests();
    // 解除阻塞：端点改为可交付。上下文仍由宿主持有，第二次关闭必须重新做真实检查。
    endpoint.set_status(200);
    let second = host.shutdown().await;
    assert_eq!(
        close_calls.load(Ordering::SeqCst),
        2,
        "保留的上下文必须真正重入 finish（重复动用关闭权），而不是命中缓存终态"
    );
    assert_eq!(
        endpoint.requests(),
        attempts_at_terminal,
        "契约冲突证据：终态在 worker join 后冻结，重入不重新发送已收敛的交付尝试"
    );
    assert_eq!(second, first, "没有新的交付尝试时读到同一冻结终态");
    assert!(!second.is_complete());
}

// ── P4/P4b 契约用例 ────────────────────────────────────────────────────────

/// 已完成移交的持久积压：报告 `PersistedPending`，不是完成，也不是失败；
/// 正文留给下一 owner（不因关闭被删除），终态可缓存并稳定重放。
#[tokio::test]
async fn host_reports_persisted_pending_without_complete() {
    let tmp = tempfile::TempDir::new().unwrap();
    let endpoint = ScriptedIngestion::start(503);
    let store = native_spool(&tmp, &endpoint.url);
    let (session, owner) =
        owned_session(spool_config(&endpoint.url), Some(Arc::clone(&store))).await;
    let close_calls = Arc::new(AtomicUsize::new(0));
    let (mut host, input) = start_host(&tmp, &session, owner, &close_calls).await;

    session.try_add(event("persisted-one")).unwrap();
    let flush = session.flush().await;
    assert!(
        matches!(flush, Err(LangfuseError::PersistedPending { ref counts })
            if counts.pending_events == 1 && counts.persisted_pending_events == 1),
        "写盘成功不等于交付，必须按持久积压报告: {flush:?}"
    );
    assert_eq!(
        store.usage().expect("usage").objects,
        1,
        "正文应在真实端口上落盘"
    );

    drop(input);
    let report = host.shutdown().await;
    let AcpHostShutdownReport::TelemetryFailed(LangfuseShutdownReport::PersistedPending { counts }) =
        &report
    else {
        panic!("已完成移交的积压应报告 PersistedPending: {report:?}");
    };
    assert!(!report.is_complete());
    assert_eq!(counts.pending_events, 1);
    assert_eq!(counts.persisted_pending_events, 1);
    assert_eq!(counts.active_tasks, 0);
    assert_eq!(counts.cleanup_pending_objects, 0);
    assert_eq!(
        store.usage().expect("usage").objects,
        1,
        "移交后正文属于下一个 owner，关闭不得删除"
    );

    // 已移交的积压本进程不能再推进，因此可以缓存为终态：重复观察稳定且不重复关闭。
    assert_eq!(host.shutdown().await, report);
    assert_eq!(close_calls.load(Ordering::SeqCst), 1);
}

/// 交付资源比发送器晚关闭：在途响应被端点阻塞时宿主不得提前返回，端口里的正文
/// 仍由部署持有；放行并确认交付之后，资源随 join 收敛（对象清零、宿主报完成）。
#[tokio::test]
async fn host_keeps_resources_until_io_joined() {
    let tmp = tempfile::TempDir::new().unwrap();
    let endpoint = ScriptedIngestion::start_gated(200);
    let store = native_spool(&tmp, &endpoint.url);
    let (session, owner) =
        owned_session(spool_config(&endpoint.url), Some(Arc::clone(&store))).await;
    let close_calls = Arc::new(AtomicUsize::new(0));

    let (mut host, input) = start_host(&tmp, &session, owner, &close_calls).await;
    session.try_add(event("in-flight")).unwrap();
    // 真实发送：flush 在途（被门阻塞），交付尝试已抵达端点。
    let sender = Arc::clone(&session);
    tokio::spawn(async move {
        let _ = LangfuseSessionLike::flush(&*sender).await;
    });
    wait_until(
        "受控端点收到在途交付尝试",
        Duration::from_secs(10),
        || endpoint.requests() == 1,
    )
    .await;
    assert_eq!(
        store.usage().expect("usage").objects,
        1,
        "在途交付期间正文仍由部署持有，不得提前释放"
    );

    // 传输 EOF：宿主开始关闭，但发送器还没 join；这里的关闭必须等它收敛。
    drop(input);
    let mut shutdown = Box::pin(host.shutdown());
    std::future::poll_fn(|cx| {
        assert!(
            shutdown.as_mut().poll(cx).is_pending(),
            "在途 IO 未 join 之前宿主不得关闭"
        );
        std::task::Poll::Ready(())
    })
    .await;

    endpoint.release();
    let report = tokio::time::timeout(Duration::from_secs(10), shutdown)
        .await
        .expect("放行后宿主应在期限内收敛");
    assert_eq!(report, AcpHostShutdownReport::Complete);
    assert_eq!(close_calls.load(Ordering::SeqCst), 1);
    assert_eq!(endpoint.requests(), 1);
    assert_eq!(
        store.usage().expect("usage").objects,
        0,
        "交付确认并 join 之后，端口对象必须已清理"
    );
}

/// 共享给 turn 的会话句柄没有关闭能力：它只能添加/flush，关闭权只属于部署关闭权限，
/// 经宿主在排空之后使用；关闭后共享句柄立刻观察到准入关闭。
#[tokio::test]
async fn shared_session_has_no_shutdown_capability() {
    let tmp = tempfile::TempDir::new().unwrap();
    let endpoint = ScriptedIngestion::start(200);
    // 真实装配面：无端口（spool_enabled=false）时不得触碰文件系统。
    let telemetry = super::assemble(Some(langfuse_config(&endpoint.url))).await;
    let session = telemetry.session.clone().expect("deployment session");
    let owner = telemetry
        .shutdown_owner
        .expect("deployment close authority");

    /// turn 侧的使用面：共享句柄做一次真实投递，结果只有交付报告，没有关闭权限。
    async fn turn_like_delivery(
        session: &Arc<dyn LangfuseSessionLike>,
    ) -> Result<FlushReport, LangfuseError> {
        session.try_add(event("turn-shared"))?;
        session.flush().await
    }

    let shared: Arc<dyn LangfuseSessionLike> = Arc::clone(&session) as Arc<dyn LangfuseSessionLike>;
    assert!(
        turn_like_delivery(&shared).await.is_ok(),
        "共享句柄的投递必须真实完成"
    );
    assert_eq!(endpoint.requests(), 1);
    assert!(
        shared.try_add(event("shared-still-open")).is_ok(),
        "共享句柄的投递不等于关闭部署：准入仍开放"
    );

    let close_calls = Arc::new(AtomicUsize::new(0));
    let (mut host, input) = start_host(&tmp, &session, owner, &close_calls).await;
    // 传输 EOF：宿主的最终排空经部署关闭权关闭遥测，尾部事件仍被真实交付。
    drop(input);
    assert_eq!(host.shutdown().await, AcpHostShutdownReport::Complete);
    assert_eq!(close_calls.load(Ordering::SeqCst), 1);
    assert_eq!(endpoint.requests(), 2, "关闭前的尾部事件仍被真实交付");
    assert!(matches!(
        shared.try_add(event("after-close")),
        Err(LangfuseError::ChannelClosed)
    ));
    assert!(matches!(
        turn_like_delivery(&shared).await,
        Err(LangfuseError::ChannelClosed)
    ));
}

/// native 私有根是**进程级** env 事实（macOS 读 `$HOME/Library/Application Support`，
/// 其他 unix 先看 `$XDG_DATA_HOME` 再退到 `$HOME/.local/share`）。同二进制的其它用例会临时
/// 改写 `HOME`（`requests_test` / `mcp_v4_*` 的 HomeDirGuard），并行执行时端口解析会跟着
/// 偶发降级；因此需要真实端口的用例与它们同组 `#[serial]` 互斥，并在临时 HOME 下先建出
/// 本平台的数据根形状。Drop 还原。
struct PrivateDataRootGuard {
    _tmp: tempfile::TempDir,
    home: Option<std::ffi::OsString>,
}

impl PrivateDataRootGuard {
    fn set() -> Self {
        let tmp = tempfile::TempDir::new().unwrap();
        let data = if cfg!(target_os = "macos") {
            tmp.path().join("Library/Application Support")
        } else {
            tmp.path().join(".local/share")
        };
        std::fs::create_dir_all(&data).unwrap();
        let home = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        Self { _tmp: tmp, home }
    }
}

impl Drop for PrivateDataRootGuard {
    fn drop(&mut self) {
        match self.home.take() {
            Some(previous) => std::env::set_var("HOME", previous),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// 装配面决定端口注入：关闭暂存或缺凭据时不给端口（不触碰文件系统），端点无法解析成
/// route 摘要时按有界内存模式降级，绝不提交一个无法使用的端口。
#[tokio::test]
#[serial]
async fn spool_capability_requires_enabled_credentials_and_routable_endpoint() {
    let _root = PrivateDataRootGuard::set();
    let mut config = langfuse_config("http://127.0.0.1:1");
    config.spool_enabled = true;
    assert!(
        super::open_spool_capability(&config).is_some(),
        "开启暂存且端点可解析时应注入真实端口"
    );
    config.spool_enabled = false;
    assert!(
        super::open_spool_capability(&config).is_none(),
        "关闭暂存不得打开任何端口"
    );
    config.spool_enabled = true;
    config.secret_key = None;
    assert!(
        super::open_spool_capability(&config).is_none(),
        "缺凭据时无法冻结 route 上下文，不得提交端口"
    );
    config.secret_key = Some("sk-test".into());
    config.host = "not a routable endpoint".into();
    assert!(
        super::open_spool_capability(&config).is_none(),
        "route 摘要被拒时按有界内存模式降级"
    );
}
