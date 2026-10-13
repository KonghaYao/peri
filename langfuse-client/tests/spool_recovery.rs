#![cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]

//! 真实磁盘 spool 的恢复与生命周期断言（契约 §5.2、§8 的 `tests/spool_recovery.rs` 行）。
//!
//! 覆盖五件事，全部落在真实文件系统状态与真实进程上，不看进程内部字段、不以「只判 Ok」代替：
//! 1. `live_retry_wait_cannot_be_taken_over`：活跃 owner 持 `retry_wait` 的 claim 时，另一进程既
//!    不能领取也不能删除；owner 崩溃后新 owner 补发**逐字节相同**的正文，`accepted_at`/`expires_at`
//!    不因重试或重启被重置；
//! 2. `crash_while_sending_resumes_identical_bytes`：子进程在请求在途时被 `SIGKILL`，磁盘上留下
//!    `sending` 对象；重启补发同一份正文，且磁盘正文与线包正文逐字节相同（事件不丢、允许重复）；
//! 3. `half_written_residue_is_never_sent`：tmp 半写残留不可见、启动即清扫、绝不被发送；
//! 4. `corrupt_object_is_removed_without_send_and_next_round_recovers`：损坏描述符被隔离删除，本轮
//!    不发送任何对象，下一轮恢复合格对象，事件不丢；
//! 5. `expired_retry_wait_terminates_without_send`：`retry_wait` 到期按原 TTL 终结，不发送、按
//!    终态清理。
//!
//! 「崩溃」一律由**真实子进程**执行：`std::env::current_exe()` 重新执行本测试二进制，随后用
//! `Child::kill`（`SIGKILL`）终止；不用单进程对象重建冒充崩溃，也不凭 PID 文件判断存活。服务端是
//! 本文件内的本地 TCP fixture：真实读请求字节、真实回 503/200、真实保持连接；不依赖公网或真实
//! Langfuse。用例 1、2、4、5 各起一个子进程并按真实事件（磁盘对象、请求字节）推进，5 号用例按
//! 契约需要跨越真实 TTL，故自然耗时约数秒。

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use langfuse_client::client::ExportConfig;
use langfuse_client::config::{BatcherConfig, TelemetryConfig};
use langfuse_client::spool::native::NativeSpool;
use langfuse_client::spool::{
    route_fingerprint, ObjectId, ObjectKind, ObjectMeta, RouteFingerprint, SpoolError, SpoolState,
    SpoolStore, SpoolUsage,
};
use langfuse_client::types::{GenerationBody, TraceBody};
use langfuse_client::{Batcher, IngestionEvent, LangfuseClient, ShutdownReport};
use sha2::{Digest, Sha256};

const PK: &str = "pk-spool-recovery";
const SK: &str = "sk-spool-recovery";
/// 端口配额：用例正文都很小，8 MiB 足够且不依赖宿主机可用空间。
const QUOTA_BYTES: u64 = 8 * 1024 * 1024;
/// 正文落盘阈值：1 字节即越界，强制正文以磁盘对象供给（恢复路径只处理文件正文）。
const FILE_BACKED_BYTES: usize = 1;
/// 等待真实事件的统一上限；到点即失败，不用固定 sleep 赌时序。
const WAIT: Duration = Duration::from_secs(20);
/// 子进程侧自保上限：父进程忘记放行也不会留下悬挂进程。
const CHILD_WAIT: Duration = Duration::from_secs(30);

// 子进程协议：同一测试二进制以这些环境变量重新执行自身，扮演 [`child_helper`]。
const CHILD_HELPER: &str = "child_helper";
const CHILD_ROOT: &str = "PERI_SPOOL_RECOVERY_ROOT";
const CHILD_URL: &str = "PERI_SPOOL_RECOVERY_URL";
const CHILD_RELEASE: &str = "PERI_SPOOL_RECOVERY_RELEASE";
const CHILD_TTL_MS: &str = "PERI_SPOOL_RECOVERY_TTL_MS";
const CHILD_MODE: &str = "PERI_SPOOL_RECOVERY_MODE";
const CHILD_MARKER: &str = "PERI_SPOOL_RECOVERY_MARKER";
/// 子进程默认角色：把一批事件持久到磁盘并保持持有 claim。
const CHILD_PUBLISH: &str = "publish";

// ─── 事件 fixture ───────────────────────────────────────────────────

fn trace_event(id: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some(format!("trace-{id}")),
            name: Some(id.into()),
            ..Default::default()
        },
        metadata: None,
    }
}

fn generation_event(id: &str) -> IngestionEvent {
    IngestionEvent::GenerationCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: GenerationBody {
            id: Some(format!("gen-{id}")),
            trace_id: Some("019a0000-0000-7000-8000-000000000001".into()),
            name: Some(id.into()),
            start_time: Some("2026-01-01T00:00:00Z".into()),
            end_time: Some("2026-01-01T00:00:01Z".into()),
            input: Some(serde_json::json!({"messages": ["payload"]})),
            output: Some(serde_json::json!("done")),
            ..Default::default()
        },
        metadata: None,
    }
}

/// 两条 span：`event_count` 与「过期/恢复事件数」的断言都按它计量。
fn events() -> Vec<IngestionEvent> {
    vec![trace_event("recovery-1"), generation_event("recovery-2")]
}

const EVENT_COUNT: u64 = 2;

// ─── 本地 HTTP fixture ──────────────────────────────────────────────

/// 一条请求记录：按 `Content-Length` 读到的正文与是否读满。
#[derive(Clone)]
struct Request {
    body: Vec<u8>,
    /// 正文读满（`Content-Length` 全部到齐）；半途断开的记录不计入交付断言。
    complete: bool,
}

const RESPONSE_OK: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
    content-length: 2\r\nconnection: close\r\n\r\n{}";
/// 可恢复错误：429/502/503/504 之一；`retry-after` 让重试窗口对用例可观察。
const RESPONSE_503: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\ncontent-type: text/plain\r\n\
    retry-after: 1\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

/// 服务端行为：正常回 200，或在读满正文后停住（请求在途，客户端拿不到确认）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Policy {
    Respond,
    StallAfterBody,
}

struct Shared {
    healthy: AtomicBool,
    stall: AtomicBool,
    requests: Mutex<Vec<Request>>,
}

struct Server {
    url: String,
    shared: Arc<Shared>,
    join: tokio::task::JoinHandle<()>,
}

impl Server {
    fn start(policy: Policy) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local server");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let url = format!("http://{}", listener.local_addr().expect("local addr"));
        let shared = Arc::new(Shared {
            healthy: AtomicBool::new(true),
            stall: AtomicBool::new(policy == Policy::StallAfterBody),
            requests: Mutex::new(Vec::new()),
        });
        let serving = Arc::clone(&shared);
        let join = tokio::spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("adopt listener");
            while let Ok((stream, _)) = listener.accept().await {
                let serving = Arc::clone(&serving);
                tokio::spawn(async move {
                    serve(stream, &serving).await;
                });
            }
        });
        Self { url, shared, join }
    }

    /// 该连接是否回 200；`false` 一律回可恢复的 503。
    fn set_healthy(&self, healthy: bool) {
        self.shared.healthy.store(healthy, Ordering::SeqCst);
    }

    /// 放行被停住的响应（含已由崩溃进程放弃的连接）。
    fn release(&self) {
        self.shared.stall.store(false, Ordering::SeqCst);
    }

    fn records(&self) -> Vec<Request> {
        self.shared
            .requests
            .lock()
            .expect("request records")
            .clone()
    }

    /// 正文读满的请求数：请求在途判定的真实依据。
    fn complete(&self) -> usize {
        self.records()
            .iter()
            .filter(|request| request.complete)
            .count()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.join.abort();
    }
}

async fn serve(mut stream: tokio::net::TcpStream, shared: &Shared) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        let mut chunk = [0u8; 8192];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut body = buffer[head_end + 4..].to_vec();
    let length = header(&head, "content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .expect("每个 OTLP 请求都必须带 Content-Length");
    let mut complete = true;
    while body.len() < length {
        let mut chunk = [0u8; 8192];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => {
                complete = false;
                break;
            }
            Ok(read) => body.extend_from_slice(&chunk[..read]),
        }
    }
    body.truncate(length.min(body.len()));
    shared
        .requests
        .lock()
        .expect("request records")
        .push(Request { body, complete });
    if !complete {
        return;
    }
    // 请求在途：正文已读满但确认尚未发出（真实保持连接，直到用例放行）。
    while shared.stall.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let response = if shared.healthy.load(Ordering::SeqCst) {
        RESPONSE_OK
    } else {
        RESPONSE_503
    };
    let _ = stream.write_all(response).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

/// 头部逐字查找（HTTP 头名大小写不敏感）。
fn header(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

// ─── Batcher / spool 装配 ───────────────────────────────────────────

fn telemetry(ttl: Duration) -> TelemetryConfig {
    TelemetryConfig {
        payload_memory_threshold_bytes: FILE_BACKED_BYTES,
        spool_ttl: ttl,
        ..Default::default()
    }
}

fn batcher_config(ttl: Duration) -> BatcherConfig {
    BatcherConfig {
        queue_capacity: 8,
        flush_interval: Duration::from_secs(30),
        telemetry: telemetry(ttl),
        ..Default::default()
    }
}

/// 导出配置：短退避便于观察周期内重试；编码与 wire 上限保持默认（必须与 telemetry 一致）。
fn client(url: &str) -> LangfuseClient {
    LangfuseClient::new(PK, SK, url, 3)
        .with_export_config(ExportConfig {
            initial_retry_delay: Duration::from_millis(20),
            max_retry_delay: Duration::from_millis(40),
            ..Default::default()
        })
        .expect("export config")
}

fn route(url: &str) -> RouteFingerprint {
    route_fingerprint(url, PK, SK).expect("route fingerprint")
}

fn spool_at(root: &Path, url: &str) -> Arc<dyn SpoolStore> {
    Arc::new(
        NativeSpool::open_at(root.to_path_buf(), route(url), QUOTA_BYTES).expect("native spool"),
    )
}

fn batcher(spool: Arc<dyn SpoolStore>, url: &str, ttl: Duration) -> Batcher {
    Batcher::try_new(client(url), batcher_config(ttl), Some(spool)).expect("batcher")
}

// ─── 磁盘观察与等待 ─────────────────────────────────────────────────

/// 契约的墙钟口径：Unix 毫秒（与 Batcher 的 `wall_ms` 同源）。
fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("wall clock")
        .as_millis() as i64
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// route 部署目录 `<root>/peri/telemetry-spool/v1/<route-hex>`；用例 root 由 tempdir 独占，
/// 同一 root 只有一个 route（endpoint 与凭据固定），故目录名可唯一确定。
fn route_dir(root: &Path) -> PathBuf {
    let base = root.join("peri/telemetry-spool/v1");
    let mut found: Vec<PathBuf> = fs::read_dir(&base)
        .expect("spool version dir")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    found.sort();
    assert_eq!(found.len(), 1, "用例 root 只应有一个 route 目录");
    found.pop().expect("route dir")
}

/// 与适配器一致的对象目录：`objects/<首字节>/<id hex>`（桶号取自对象 ID 首字节）。
fn object_dir(root: &Path, id_hex: &str) -> PathBuf {
    let bucket = &id_hex[..2];
    route_dir(root).join("objects").join(bucket).join(id_hex)
}

/// 逐字节读取已发布正文（只经公开端口，不触碰宿主路径拼接）。
fn read_object_body(store: &Arc<dyn SpoolStore>, id: ObjectId) -> Vec<u8> {
    let mut reader = store.open(id).expect("open spool body");
    let mut body = Vec::new();
    reader.read_to_end(&mut body).expect("read spool body");
    body
}

/// 轮询用枚举：删除与插入期间允许重复；对象正在被删除时端口可能瞬时返回 `Corrupt`
/// （正文已删、描述尚在），此处只当作「本轮没有稳定页」，不当成失败也不当成空页。
fn try_list(store: &Arc<dyn SpoolStore>) -> Option<Vec<ObjectMeta>> {
    match store.list(None, 64) {
        Ok(page) => Some(page.entries),
        Err(SpoolError::Corrupt { .. }) => None,
        Err(error) => panic!("list spool page: {error}"),
    }
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !ready() {
        assert!(Instant::now() < deadline, "等待超时: {what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// 子进程步进度文件：与标记同目录（`<marker>.steps`），逐行「Unix 毫秒 + 步骤」。
///
/// 子进程若卡住（而非 panic），stderr 只会停在最后一句，且管道要等父进程收尸才读得到；
/// 步进度是**边写边可见**的：父进程超时那一刻就能断言它停在哪一步、已经走了多久。
fn step_log_path(marker: &Path) -> PathBuf {
    marker.with_extension("steps")
}

fn record_step(path: &Path, step: &str) {
    let line = format!("{} {step}\n", wall_ms());
    let mut file = match fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => file,
        Err(_) => return,
    };
    let _ = std::io::Write::write_all(&mut file, line.as_bytes());
}

/// 等子进程写出标记：子进程先退出就立刻带上它的 stderr 失败，不白等满 20 秒。
///
/// 子进程卡住时父进程仍会等到超时，但那时 `子进程 stderr` 与步进度已写明卡在哪一步。
async fn wait_child_marker(probe: &mut ChildProcess, marker: &Path, what: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        if marker.exists() {
            return;
        }
        if let Some(status) = probe.exited() {
            probe.dump_stderr();
            panic!("{what}: 子进程提前退出（{status}）且未写出标记");
        }
        if Instant::now() >= deadline {
            let steps = fs::read_to_string(step_log_path(marker))
                .unwrap_or_else(|error| format!("<无步进度记录: {error}>"));
            probe.dump_stderr();
            panic!("等待超时: {what}（子进程步进度:\n{steps}）");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 等磁盘上没有可见对象：只认**成功枚举出的空页**，删除途中的瞬时错误继续等。
async fn wait_empty(store: &Arc<dyn SpoolStore>, what: &str) {
    wait_for(
        what,
        || matches!(try_list(store), Some(entries) if entries.is_empty()),
    )
    .await;
}

/// 轮询真实磁盘对象（`SpoolStore::list`），谓词成立时返回当时的元数据。
async fn wait_for_object(
    store: &Arc<dyn SpoolStore>,
    what: &str,
    mut ready: impl FnMut(&ObjectMeta) -> bool,
) -> ObjectMeta {
    let deadline = Instant::now() + WAIT;
    let mut history: Vec<String> = Vec::new();
    loop {
        let entries = try_list(store);
        if let Some(meta) = entries.iter().flatten().find(|meta| ready(meta)) {
            return meta.clone();
        }
        for meta in entries.iter().flatten() {
            let line = format!(
                "{:?}/attempts={}/events={}/due={:?}",
                meta.state, meta.attempts, meta.event_count, meta.next_attempt_at_ms
            );
            if history.last() != Some(&line) {
                history.push(line);
            }
        }
        assert!(
            Instant::now() < deadline,
            "等待超时: {what}；状态历史 {history:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// 正文读满的请求（`Content-Length` 全部到齐）；崩溃进程留下的半截请求不计入交付断言。
fn complete_requests(server: &Server) -> Vec<Request> {
    server
        .records()
        .into_iter()
        .filter(|request| request.complete)
        .collect()
}

// ─── 真实子进程 ─────────────────────────────────────────────────────

/// 跨进程 helper：只在父进程设置了环境变量时工作；被测试框架单独执行时直接返回。
///
/// 角色是「另一个真实进程」：用自己的 `NativeSpool` 打开同一 root，把对象持久为
/// `retry_wait`/`sending` 后保持持有 claim，直到父进程放行或直接 `SIGKILL`。
#[tokio::test]
#[ignore = "跨进程 helper：由本文件的恢复用例以子进程重新执行"]
async fn child_helper() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    let url = std::env::var(CHILD_URL).expect("child url");
    let mode = std::env::var(CHILD_MODE).unwrap_or_else(|_| CHILD_PUBLISH.to_string());
    // 探针角色的步进度落盘：装配（`open_at` 的启动扫描与账本重建）在首句 stderr 之前，
    // 只有先落盘才分得清「卡在开库」与「卡在领取」。
    let steps = (mode == "probe_claim")
        .then(|| PathBuf::from(std::env::var_os(CHILD_MARKER).expect("child marker")))
        .map(|marker| step_log_path(&marker));
    if let Some(steps) = &steps {
        record_step(steps, "start: runtime up");
    }
    let spool = spool_at(Path::new(&root), &url);
    if let Some(steps) = &steps {
        record_step(steps, "opened spool");
    }
    if mode == "probe_claim" {
        // 跨进程探针：对别的 owner 持有的对象领取，必须看到 claim=None（判据是 OS 文件锁）。
        let steps = steps.as_ref().expect("probe steps");
        let page = spool.list(None, 64).expect("child list");
        record_step(steps, "listed");
        let meta = page.entries.first().expect("child sees a persisted object");
        let claimed = spool.claim(meta.id).expect("child claim").is_some();
        record_step(steps, "claimed");
        let marker = PathBuf::from(std::env::var_os(CHILD_MARKER).expect("child marker"));
        publish_marker(&marker, format!("claimed={claimed}").as_bytes());
        return;
    }

    let release = PathBuf::from(std::env::var_os(CHILD_RELEASE).expect("child release"));
    let ttl_ms: u64 = std::env::var(CHILD_TTL_MS)
        .expect("child ttl")
        .parse()
        .expect("child ttl number");
    let batcher = batcher(spool, &url, Duration::from_millis(ttl_ms));
    for event in events() {
        batcher.try_add(event).expect("child admit");
    }
    // 一次 flush 后对象已持久（`sending`/`retry_wait`）；句柄留在重试队列里，进程活着就持有 claim。
    // 本轮确认的具体形态（待交付/在途）由父进程按磁盘状态判定，子进程不替它下结论。
    let _ = batcher.flush().await;
    wait_for_release(&release).await;
}

/// 原子落盘标记：先写旁路文件再 rename，父进程不会读到写了一半的标记。
fn publish_marker(path: &Path, content: &[u8]) {
    let staging = path.with_extension("writing");
    fs::write(&staging, content).expect("marker content");
    fs::rename(&staging, path).expect("marker rename");
}

/// 子进程侧等待放行；到点自行退出，父进程不会永久等待。
async fn wait_for_release(release: &Path) {
    let deadline = Instant::now() + CHILD_WAIT;
    while Instant::now() < deadline {
        if release.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

struct ChildProcess {
    child: Option<Child>,
    stderr: Option<std::process::ChildStderr>,
    release: PathBuf,
}

impl ChildProcess {
    /// `SIGKILL`：不执行任何本地清理，磁盘状态即崩溃现场。
    fn kill_and_wait(mut self) {
        let mut child = self.child.take().expect("child handle");
        let _ = child.kill();
        let _ = child.wait();
        self.dump_stderr();
    }

    /// 子进程在被杀之前写下的诊断（例如 panic 消息）：只在非空时转出。
    fn dump_stderr(&mut self) {
        let Some(stream) = self.stderr.as_mut() else {
            return;
        };
        let mut text = String::new();
        if stream.read_to_string(&mut text).is_ok() && !text.trim().is_empty() {
            eprintln!("子进程 stderr:\n{text}");
        }
    }

    /// 子进程是否已退出；用来把「子进程先失败」与「父进程等待不足」分开，而不是白等满 20 秒。
    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // 用例 panic 也要结束子进程，避免留下持锁进程干扰后续断言。
            let _ = fs::write(&self.release, b"release");
            let _ = child.kill();
            let _ = child.wait();
            self.stderr = child.stderr.take();
            self.dump_stderr();
        }
    }
}

/// 子进程描述：角色（publish/probe_claim）、真实临时 root、父进程服务端 URL 与放行/标记文件。
struct ChildSpec<'a> {
    root: &'a Path,
    url: &'a str,
    mode: &'a str,
    ttl: Duration,
    release: PathBuf,
    marker: Option<PathBuf>,
}

impl<'a> ChildSpec<'a> {
    fn publish(root: &'a Path, url: &'a str, ttl: Duration, release: PathBuf) -> Self {
        Self {
            root,
            url,
            mode: CHILD_PUBLISH,
            ttl,
            release,
            marker: None,
        }
    }

    fn probe_claim(root: &'a Path, url: &'a str, marker: PathBuf) -> Self {
        Self {
            root,
            url,
            mode: "probe_claim",
            ttl: Duration::from_secs(60),
            release: marker.with_extension("release"),
            marker: Some(marker),
        }
    }
}

/// 以当前测试二进制重新执行自身，扮演 [`child_helper`]（真实进程、真实文件锁）。
fn spawn_child(spec: ChildSpec<'_>) -> ChildProcess {
    let exe = std::env::current_exe().expect("current test binary");
    let mut command = Command::new(exe);
    command
        .arg(CHILD_HELPER)
        .args(["--exact", "--ignored", "--test-threads=1"])
        .env(CHILD_ROOT, spec.root)
        .env(CHILD_URL, spec.url)
        .env(CHILD_MODE, spec.mode)
        .env(CHILD_RELEASE, &spec.release)
        .env(CHILD_TTL_MS, spec.ttl.as_millis().to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(marker) = &spec.marker {
        command.env(CHILD_MARKER, marker);
    }
    let mut child = command.spawn().expect("spawn child test process");
    let stderr = child.stderr.take();
    ChildProcess {
        child: Some(child),
        stderr,
        release: spec.release,
    }
}

// ─── 用例 ───────────────────────────────────────────────────────────

/// 契约 §8「3：跨进程 retry_wait」：活跃 owner 持 claim 时另一进程既不能接管也不能删除；owner 崩溃
/// 后新 owner 用同一份磁盘正文续发（重试与重启都不重置 `accepted_at`/`expires_at`）。
///
/// 磁盘状态的真实语义：live 路径在重试等待期间保持 `sending`（等待只在内存），`retry_wait` 由恢复
/// 路径在续发被延后时持久写入；本用例两段都断言到，且第二段的 claim 持有者是本进程。
#[tokio::test]
async fn live_retry_wait_cannot_be_taken_over() {
    const TTL_MS: u64 = 10_000;
    let dir = tempfile::TempDir::new().expect("temp dir");
    let server = Server::start(Policy::Respond);
    server.set_healthy(false);
    let store = spool_at(dir.path(), &server.url);
    // 第一段：真实子进程作为活跃 owner，首个 503 后进入重试等待并保持 claim。
    let child = spawn_child(ChildSpec::publish(
        dir.path(),
        &server.url,
        Duration::from_millis(TTL_MS),
        dir.path().join("child.release"),
    ));
    wait_for("子进程首个在途尝试", || server.complete() >= 1).await;
    let meta = wait_for_object(&store, "子进程持有的对象", |meta| {
        matches!(meta.state, SpoolState::Sending | SpoolState::RetryWait) && meta.attempts >= 1
    })
    .await;
    assert_eq!(meta.kind, ObjectKind::Export);
    assert_eq!(meta.event_count, EVENT_COUNT);
    assert_eq!(
        meta.expires_at_ms - meta.accepted_at_ms,
        TTL_MS as i64,
        "TTL 必须等于配置值"
    );
    let sent = complete_requests(&server);
    let wire = sent[0].body.clone();

    // 活跃 owner：另一进程既不能领取也不能删除（判据是 OS 文件锁，不是 PID 存活）。
    assert!(
        store.claim(meta.id).expect("claim").is_none(),
        "活跃 owner 持有的对象不得被另一进程接管"
    );
    match store.delete(meta.id) {
        Err(SpoolError::Busy) => {}
        other => panic!("活跃对象删除必须报 Busy，实际 {other:?}"),
    }

    // 真实崩溃：SIGKILL，丢弃子进程全部本地状态。
    child.kill_and_wait();
    let after = store.inspect(meta.id).expect("崩溃后仍能读元数据");
    assert_eq!(
        after.accepted_at_ms, meta.accepted_at_ms,
        "重启不得重置首次接受时间"
    );
    assert_eq!(after.expires_at_ms, meta.expires_at_ms, "重启不得重置 TTL");
    assert!(after.attempts >= meta.attempts, "attempts 不得回退");

    // 第二段：本进程接手，续发仍失败 ⇒ 恢复路径把 retry_wait 与下次允许时间持久化。
    let batcher = batcher(
        Arc::clone(&store),
        &server.url,
        Duration::from_millis(TTL_MS),
    );
    let retry = wait_for_object(&store, "恢复路径持久化的 retry_wait", |meta| {
        meta.state == SpoolState::RetryWait
    })
    .await;
    let due = retry
        .next_attempt_at_ms
        .expect("retry_wait 必须写下次允许时间");
    assert!(due > wall_ms(), "下次允许时间必须在未来");
    assert_eq!(
        retry.accepted_at_ms, meta.accepted_at_ms,
        "重试不得重置 TTL 起点"
    );
    assert_eq!(retry.expires_at_ms, meta.expires_at_ms, "重试不得延寿");
    assert!(retry.attempts > meta.attempts, "续发必须累计 attempts");
    assert_eq!(batcher.stats().recovered_events, EVENT_COUNT);

    // 跨进程探针：retry_wait 期间 claim 仍归本进程，另一个真实进程只能看到 claim=None。
    let marker = dir.path().join("probe.marker");
    // 记上线时刻与当时磁盘规模：子进程的装配耗时只能对着这两个量解释。
    let visible = try_list(&store).map_or(0, |entries| entries.len());
    record_step(
        &step_log_path(&marker),
        &format!("parent: spawning probe child (visible entries={visible})"),
    );
    let mut probe = spawn_child(ChildSpec::probe_claim(
        dir.path(),
        &server.url,
        marker.clone(),
    ));
    wait_child_marker(&mut probe, &marker, "跨进程探针结论").await;
    assert_eq!(
        fs::read_to_string(&marker).expect("probe marker").trim(),
        "claimed=false",
        "retry_wait 期间不得被另一进程接管"
    );
    probe.kill_and_wait();

    // 放行服务端：本进程按同一份正文交付并清理。
    server.set_healthy(true);
    wait_empty(&store, "retry_wait 到期后交付").await;
    let all = complete_requests(&server);
    assert!(all.len() > sent.len(), "续发必须真实发生");
    assert!(
        all.iter().all(|request| request.body == wire),
        "每次尝试都必须重发同一份正文（逐字节相等，不允许改写或续传）"
    );
    assert_eq!(
        batcher.shutdown().await.expect("shutdown"),
        ShutdownReport::Complete
    );
    assert!(
        store.list(None, 64).expect("交付后清理").entries.is_empty(),
        "交付后正文与描述必须清理"
    );
}

/// 契约 §8「P1/P4：崩溃恢复」的 sending 边界：子进程在请求在途时被 `SIGKILL`，磁盘上留下
/// `sending` 对象；重启补发同一份正文，且磁盘正文与线包正文逐字节相同（事件不丢）。
#[tokio::test]
async fn crash_while_sending_resumes_identical_bytes() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let server = Server::start(Policy::StallAfterBody);
    let store = spool_at(dir.path(), &server.url);
    let child = spawn_child(ChildSpec::publish(
        dir.path(),
        &server.url,
        Duration::from_secs(60),
        dir.path().join("child.release"),
    ));
    // 服务端读到完整正文 ⇒ 请求已在途，磁盘上对象已 checkpoint 为 sending。
    wait_for("首个在途请求", || server.complete() >= 1).await;
    let meta = wait_for_object(&store, "sending 对象", |meta| {
        meta.state == SpoolState::Sending && meta.attempts >= 1
    })
    .await;
    let wire = complete_requests(&server)[0].body.clone();
    assert_eq!(
        read_object_body(&store, meta.id),
        wire,
        "磁盘 spool 正文必须与线包正文逐字节相同"
    );

    // 真实崩溃：确认从未送达（连接被停住），对象只能靠恢复补发。
    child.kill_and_wait();
    let after = store.inspect(meta.id).expect("崩溃后仍能读元数据");
    assert_eq!(after.state, SpoolState::Sending, "在途状态必须原样留在盘上");
    assert_eq!(after.expires_at_ms, meta.expires_at_ms);
    assert_eq!(read_object_body(&store, meta.id), wire, "崩溃不得改写正文");

    // 新 owner 补发：同一份字节，允许重复，不允许丢失或改写。
    server.release();
    let batcher = batcher(Arc::clone(&store), &server.url, Duration::from_secs(60));
    wait_empty(&store, "补发并清理").await;
    assert_eq!(batcher.stats().recovered_events, EVENT_COUNT);
    let all = complete_requests(&server);
    assert!(all.len() >= 2, "补发必须真实发生");
    assert!(
        all.iter().all(|request| request.body == wire),
        "补发必须与在途正文逐字节相同（SHA 相同）"
    );
    assert_eq!(sha256(&all[1].body), sha256(&wire));
    assert_eq!(
        batcher.shutdown().await.expect("shutdown"),
        ShutdownReport::Complete
    );
}

/// 契约 §5.2「writing」：tmp 半写残留不可见、启动即清扫，绝不被当成可发送对象。
#[tokio::test]
async fn half_written_residue_is_never_sent() {
    const RESIDUE: &str = "00112233445566778899aabbccddeeff";
    let dir = tempfile::TempDir::new().expect("temp dir");
    let server = Server::start(Policy::Respond);
    // 第一次 open 只为按适配器真实布局铺出部署目录（不手写 route 摘要）。
    drop(spool_at(dir.path(), &server.url));
    let tmp = route_dir(dir.path()).join("tmp");
    let part = tmp.join(format!("{RESIDUE}.part"));
    let lock = tmp.join(format!("{RESIDUE}.lock"));
    fs::write(&part, b"half written object body").expect("part residue");
    fs::write(&lock, b"").expect("lock residue");
    assert!(part.exists() && lock.exists(), "残留必须先真实存在");

    // 重启：启动清扫回收无主残留，半写正文永远不会进入可见集合。
    let store = spool_at(dir.path(), &server.url);
    assert!(!part.exists(), "无主半写残留必须被启动清扫");
    assert!(!lock.exists(), "残留锁必须一并清扫");
    let batcher = batcher(Arc::clone(&store), &server.url, Duration::from_secs(60));
    assert_eq!(
        batcher.shutdown().await.expect("shutdown"),
        ShutdownReport::Complete
    );
    assert_eq!(server.complete(), 0, "半写残留绝不能被发送");
    assert_eq!(batcher.stats().recovered_events, 0);
    assert!(store.list(None, 64).expect("list").entries.is_empty());
    assert_eq!(
        store.usage().expect("usage"),
        SpoolUsage {
            reserved_bytes: 0,
            stored_bytes: 0,
            objects: 0,
        },
        "清扫后配额账本必须归零"
    );
}

/// 契约 §8「P1/P4：崩溃恢复」的损坏隔离：损坏描述符被端口隔离删除，本轮不发送任何对象；
/// 下一轮恢复合格对象并逐字节交付，事件不丢。
#[tokio::test]
async fn corrupt_object_is_removed_without_send_and_next_round_recovers() {
    const LONG_TTL: Duration = Duration::from_secs(60);
    let dir = tempfile::TempDir::new().expect("temp dir");
    let server = Server::start(Policy::Respond);
    server.set_healthy(false);
    let store = spool_at(dir.path(), &server.url);
    let child = spawn_child(ChildSpec::publish(
        dir.path(),
        &server.url,
        LONG_TTL,
        dir.path().join("child.release"),
    ));
    // 先由真实子进程留下一个持久合格的 Export（首个 503 后进入重试等待，claim 归子进程）。
    wait_for("子进程首个在途尝试", || server.complete() >= 1).await;
    let valid = wait_for_object(&store, "子进程持久对象", |meta| {
        meta.kind == ObjectKind::Export
            && matches!(meta.state, SpoolState::Sending | SpoolState::RetryWait)
            && meta.attempts >= 1
    })
    .await;
    let wire = complete_requests(&server)[0].body.clone();
    child.kill_and_wait();
    let sent_before = complete_requests(&server).len();

    // 手写不可解析/未知版本的描述符（对象身份由目录名决定，正文与描述都留在真实布局里）。
    let corrupt_hex = "00ffeeddccbbaa9988776655443322ff";
    let corrupt = object_dir(dir.path(), corrupt_hex);
    fs::create_dir_all(&corrupt).expect("corrupt object dir");
    fs::write(corrupt.join("body"), b"corrupt body bytes").expect("corrupt body");
    fs::write(corrupt.join("meta"), br#"{"version":99,"state":"ready"}"#).expect("corrupt meta");

    // 第一轮：端口在枚举时就地隔离并删除损坏条目；本轮扫描因此中止，不发任何对象。
    let round1 = batcher(Arc::clone(&store), &server.url, LONG_TTL);
    wait_for("损坏条目被隔离删除", || !corrupt.exists()).await;
    let survivors = store.list(None, 64).expect("list").entries;
    assert_eq!(survivors.len(), 1, "合格对象必须原样留在盘上");
    assert_eq!(survivors[0].id, valid.id);
    assert_eq!(survivors[0].state, valid.state);
    assert_eq!(
        complete_requests(&server).len(),
        sent_before,
        "损坏条目绝不能被发送，本轮也不得发送任何对象"
    );
    assert_eq!(round1.stats().recovered_events, 0);
    assert_eq!(
        round1.shutdown().await.expect("shutdown"),
        ShutdownReport::Complete
    );

    // 下一轮：合格对象按磁盘正文补发，事件不丢。
    server.set_healthy(true);
    let round2 = batcher(Arc::clone(&store), &server.url, LONG_TTL);
    wait_empty(&store, "合格对象补发并清理").await;
    let delivered = complete_requests(&server);
    assert_eq!(
        delivered.len(),
        sent_before + 1,
        "除合格对象的一次补发外不得出现别的请求"
    );
    assert_eq!(
        delivered.last().expect("补发请求").body,
        wire,
        "补发必须是同一份磁盘正文"
    );
    assert_eq!(round2.stats().recovered_events, EVENT_COUNT);
    assert_eq!(
        round2.stats().corrupt_objects,
        0,
        "损坏条目由端口隔离，不经恢复重建"
    );
    assert_eq!(
        round2.shutdown().await.expect("shutdown"),
        ShutdownReport::Complete
    );
}

/// 契约 §5.2「retry_wait / TTL 先到」：`retry_wait` 超过原 TTL 后按终态清理，不发送；
/// TTL 从首次接受时间起算，重试与重启都不延寿（本用例自然耗时数秒）。
#[tokio::test]
async fn expired_retry_wait_terminates_without_send() {
    const TTL_MS: u64 = 2_500;
    let dir = tempfile::TempDir::new().expect("temp dir");
    let server = Server::start(Policy::Respond);
    server.set_healthy(false);
    let store = spool_at(dir.path(), &server.url);
    let child = spawn_child(ChildSpec::publish(
        dir.path(),
        &server.url,
        Duration::from_millis(TTL_MS),
        dir.path().join("child.release"),
    ));
    wait_for("子进程首个在途尝试", || server.complete() >= 1).await;
    let meta = wait_for_object(&store, "子进程持有的对象", |meta| {
        matches!(meta.state, SpoolState::Sending | SpoolState::RetryWait) && meta.attempts >= 1
    })
    .await;
    assert_eq!(
        meta.expires_at_ms - meta.accepted_at_ms,
        TTL_MS as i64,
        "TTL 从首次接受时间起算"
    );
    child.kill_and_wait();
    let remaining = meta.expires_at_ms - wall_ms();
    assert!(
        remaining > 0,
        "必须在原 TTL 内观察到 retry_wait（剩余 {remaining} ms）"
    );

    // 原 TTL 到期后：新 owner 不得发送，只能按终态清理。
    server.set_healthy(true);
    tokio::time::sleep(Duration::from_millis(remaining as u64 + 300)).await;
    let sent_before = complete_requests(&server).len();
    let batcher = batcher(
        Arc::clone(&store),
        &server.url,
        Duration::from_millis(TTL_MS),
    );
    wait_empty(&store, "超期对象清理").await;
    assert_eq!(
        complete_requests(&server).len(),
        sent_before,
        "超期对象绝不能被发送"
    );
    assert_eq!(
        batcher.stats().expired_events,
        EVENT_COUNT,
        "过期按事件数记账"
    );
    assert_eq!(
        batcher.stats().recovered_events,
        0,
        "超期对象不进入恢复交付"
    );
    assert_eq!(
        batcher.shutdown().await.expect("shutdown"),
        ShutdownReport::Complete
    );
    assert_eq!(
        store.usage().expect("usage").objects,
        0,
        "终态清理后磁盘不得残留对象"
    );
}
