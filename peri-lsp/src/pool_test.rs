//! Tests for pool

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::ports::{LspPoolPort, LspSyncError};
use serde_json::Value;

use super::*;
use crate::config::{LspConfigFile, LspServerConfig};

fn make_config() -> LspConfigFile {
    let mut servers = HashMap::new();
    servers.insert(
        "rust-analyzer".to_string(),
        LspServerConfig {
            name: "rust-analyzer".to_string(),
            command: "rust-analyzer".to_string(),
            args: vec!["--stdio".to_string()],
            env: None,
            extension_to_language: HashMap::from([(".rs".to_string(), "rust".to_string())]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    servers.insert(
        "typescript".to_string(),
        LspServerConfig {
            name: "typescript-language-server".to_string(),
            command: "typescript-language-server".to_string(),
            args: vec!["--stdio".to_string()],
            env: None,
            extension_to_language: HashMap::from([
                (".ts".to_string(), "typescript".to_string()),
                (".tsx".to_string(), "typescriptreact".to_string()),
            ]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    LspConfigFile {
        lsp_servers: servers,
    }
}

/// pool 的 root/cwd：平台临时目录，保证在运行平台上真实存在。
///
/// pool 用 `root_uri` 解码出的路径作为 LSP 子进程的 cwd；测试曾用 `"/tmp"`，
/// Windows 上它解码为 `<当前盘>:\tmp`（通常不存在），spawn 直接失败。
fn fake_pool_root() -> String {
    crate::uri::test_workspace_dir()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn test_extension_routing() {
    let pool = LspServerPool::new(&fake_pool_root(), make_config());
    assert!(pool.server_for_file("/test/main.rs").is_some());
    assert!(pool.server_for_file("/test/index.ts").is_some());
    assert!(pool.server_for_file("/test/App.tsx").is_some());
    assert!(pool.server_for_file("/test/readme.md").is_none());
    assert!(pool.server_for_file("/test/no_ext").is_none());
}

/// 端口替身调用记录（顺序断言用）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncCall {
    ReadyFor,
    DidChange,
    DidSave,
}

/// 端口替身：`ready_for` / `did_change` / `did_save` **显式实现**（A30 冻结端口
/// 无默认实现，替身不得依赖 no-op 兜底），以 `AtomicUsize` 计数并记录调用顺序与
/// 参数——「替身被调用」必须可由计数证明，否则同步链路假绿。
/// 同时充当 downcast 类型不匹配路径的替身。host 侧两个同形替身（H-04）须按本
/// 形状补齐。
#[derive(Default)]
struct StubPool {
    ready: AtomicBool,
    ready_for_calls: AtomicUsize,
    did_change_calls: AtomicUsize,
    did_save_calls: AtomicUsize,
    calls: Mutex<Vec<SyncCall>>,
    changed: Mutex<Vec<(PathBuf, String)>>,
    saved: Mutex<Vec<PathBuf>>,
}

impl StubPool {
    fn with_ready(ready: bool) -> Self {
        Self {
            ready: AtomicBool::new(ready),
            ..Self::default()
        }
    }

    fn ready_for_count(&self) -> usize {
        self.ready_for_calls.load(Ordering::SeqCst)
    }

    fn did_change_count(&self) -> usize {
        self.did_change_calls.load(Ordering::SeqCst)
    }

    fn did_save_count(&self) -> usize {
        self.did_save_calls.load(Ordering::SeqCst)
    }

    fn calls(&self) -> Vec<SyncCall> {
        self.calls.lock().clone()
    }

    fn changed(&self) -> Vec<(PathBuf, String)> {
        self.changed.lock().clone()
    }

    fn saved(&self) -> Vec<PathBuf> {
        self.saved.lock().clone()
    }

    /// 替身按契约回应：ready 才有 Ok，否则 `NoServer`（与真实端口一致）
    fn sync_result(&self) -> Result<(), LspSyncError> {
        if self.ready.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(LspSyncError::NoServer)
        }
    }
}

#[async_trait::async_trait]
impl LspPoolPort for StubPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn shutdown(&self) {}

    fn ready_for(&self, _path: &std::path::Path) -> bool {
        self.ready_for_calls.fetch_add(1, Ordering::SeqCst);
        self.calls.lock().push(SyncCall::ReadyFor);
        self.ready.load(Ordering::SeqCst)
    }

    async fn did_change(&self, path: &std::path::Path, text: &str) -> Result<(), LspSyncError> {
        self.did_change_calls.fetch_add(1, Ordering::SeqCst);
        self.calls.lock().push(SyncCall::DidChange);
        self.changed
            .lock()
            .push((path.to_path_buf(), text.to_string()));
        self.sync_result()
    }

    async fn did_save(&self, path: &std::path::Path) -> Result<(), LspSyncError> {
        self.did_save_calls.fetch_add(1, Ordering::SeqCst);
        self.calls.lock().push(SyncCall::DidSave);
        self.saved.lock().push(path.to_path_buf());
        self.sync_result()
    }
}

/// 端口 downcast 往返：upcast 为 Arc<dyn LspPoolPort> 后经 downcast_arc
/// 还原为同一 LspServerPool 实例（装配面会话级复用前置条件，H1）。
#[test]
fn test_lsp_pool_port_downcast_roundtrip() {
    let pool: Arc<LspServerPool> = Arc::new(LspServerPool::new(&fake_pool_root(), make_config()));
    let port: Arc<dyn LspPoolPort> = pool.clone();
    let restored = match port.downcast_arc::<LspServerPool>() {
        Ok(restored) => restored,
        Err(_) => panic!("类型匹配时应还原成功"),
    };
    assert!(
        Arc::ptr_eq(&pool, &restored),
        "downcast 应还原同一 pool 实例"
    );
    assert!(restored.has_servers());
}

/// 类型不匹配：downcast 失败返回原端口句柄（仍可调用 shutdown）。
#[tokio::test]
async fn test_lsp_pool_port_downcast_mismatch_returns_original() {
    let port: Arc<dyn LspPoolPort> = Arc::new(StubPool::default());
    let err = match port.downcast_arc::<LspServerPool>() {
        Ok(_) => panic!("类型不匹配时应还原失败"),
        Err(p) => p,
    };
    err.shutdown().await;
}

/// L-02 门禁用例（A30）：`ready_for` 反映**路由 server 的真实就绪状态**——
/// 无路由 / 未 ready ⇒ `false` 且 `did_change` / `did_save` 返回 `Err(NoServer)`；
/// 有路由且 ready ⇒ `true`。同时证明 ready 判定与失败的同步调用都不启动
/// language server（无副作用）。
#[tokio::test]
async fn port_ready_for_reflects_routed_server_state() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawns");
    let pool: Arc<LspServerPool> = Arc::new(make_fake_pool(&count_file));
    let port: Arc<dyn LspPoolPort> = pool.clone();

    let routed = dir.path().join("main.rs");
    let unrouted = dir.path().join("readme.md");

    // ① 无路由：没有可路由的 server
    assert!(
        !port.ready_for(&unrouted),
        "无路由文件 ready_for 必须为 false"
    );
    assert_eq!(
        port.did_change(&unrouted, "let x = 1;\n")
            .await
            .unwrap_err(),
        LspSyncError::NoServer
    );
    assert_eq!(
        port.did_save(&unrouted).await.unwrap_err(),
        LspSyncError::NoServer
    );

    // ② 有路由但 client 未就绪（尚未启动）
    assert!(
        !port.ready_for(&routed),
        "服务器未就绪时 ready_for 必须为 false"
    );
    let error = port.did_change(&routed, "let x = 1;\n").await.unwrap_err();
    assert_eq!(error, LspSyncError::NoServer);
    assert_eq!(
        port.did_save(&routed).await.unwrap_err(),
        LspSyncError::NoServer
    );
    // 错误文本只进 debug 日志，不进模型面：不得携带路径 / env / 凭据
    let rendered = format!("{error}|{error:?}");
    assert!(
        !rendered.contains("main.rs"),
        "错误文本不得泄露路径: {rendered}"
    );

    // ③ ready 判定与失败的同步调用都不得拉起 language server
    assert!(
        !count_file.exists(),
        "ready_for / 失败的 did_change / did_save 不得启动任何 language server"
    );

    // ④ 有路由且 ready
    pool.ensure_server_for_file(&routed.to_string_lossy())
        .await
        .unwrap();
    assert!(
        port.ready_for(&routed),
        "路由 server 就绪后 ready_for 必须为 true"
    );

    // ⑤ 同一路由文件在 pool 关闭后重新变为未 ready
    pool.shutdown().await;
    assert!(
        !port.ready_for(&routed),
        "pool 关闭后 ready_for 必须为 false"
    );
}

/// 端口路由与参数（A30）：`did_change` / `did_save` 做 path → `file://` URI 转换、
/// `text` 原样透传，且同一调用内 change 通知先于 save 通知到达服务器；端口替身
/// 以**调用计数与顺序**断言（不以「未报错」替代），确保替身不是静默 no-op。
#[tokio::test]
async fn port_did_change_and_did_save_route_paths() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawns");
    let documents_file = dir.path().join("documents");
    let pool: Arc<LspServerPool> = Arc::new(make_recording_pool(&count_file, &documents_file));
    let port: Arc<dyn LspPoolPort> = pool.clone();

    let path = dir.path().join("main.rs");
    let text = "fn main() { let x = 1; }\n";

    pool.ensure_server_for_file(&path.to_string_lossy())
        .await
        .unwrap();
    assert!(
        port.ready_for(&path),
        "已就绪的路由文件 ready_for 必须为 true"
    );

    // 首次同步按既有语义转 didOpen（`peri-lsp/src/client/documents.rs` did_change
    // 的首次分支，本波不改协议行为）：先让它落盘并清空记录，使被断言的窗口恰好是
    // 「一次 change + 一次 save」。
    port.did_change(&path, "fn main() {}\n")
        .await
        .expect("首次同步必须成功");
    wait_recorded_documents(&documents_file, 1).await;
    std::fs::write(&documents_file, "").unwrap();

    port.did_change(&path, text)
        .await
        .expect("did_change 必须成功");
    port.did_save(&path).await.expect("did_save 必须成功");

    // ① 真实端口：服务器收到的通知序列 = change 后 save，URI 与文本正确
    let documents = wait_recorded_documents(&documents_file, 2).await;
    let methods: Vec<&str> = documents
        .iter()
        .map(|d| d["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        vec!["textDocument/didChange", "textDocument/didSave"],
        "同一调用内必须 change 先于 save"
    );
    assert_eq!(
        documents
            .iter()
            .filter(|d| d["method"] == "textDocument/didChange")
            .count(),
        1,
        "did_change 必须恰好发出一条 change 通知"
    );
    assert_eq!(
        documents
            .iter()
            .filter(|d| d["method"] == "textDocument/didSave")
            .count(),
        1,
        "did_save 必须恰好发出一条 save 通知"
    );

    let expected_uri = path_to_uri(&path);
    assert!(expected_uri.starts_with("file://"));
    assert!(expected_uri.ends_with("/main.rs"));
    for document in &documents {
        assert_eq!(
            document["params"]["textDocument"]["uri"].as_str().unwrap(),
            expected_uri,
            "端口必须把文件系统 path 转成 file:// URI（不是把裸 path 当 URI）"
        );
    }
    assert_eq!(
        documents[0]["params"]["contentChanges"][0]["text"]
            .as_str()
            .unwrap(),
        text,
        "text 必须原样透传"
    );

    // ② 端口替身：调用计数、顺序与参数
    let stub = Arc::new(StubPool::with_ready(true));
    let stub_port: Arc<dyn LspPoolPort> = stub.clone();
    assert!(stub_port.ready_for(&path));
    stub_port
        .did_change(&path, text)
        .await
        .expect("ready 替身的 did_change 必须成功");
    stub_port
        .did_save(&path)
        .await
        .expect("ready 替身的 did_save 必须成功");

    assert_eq!(stub.ready_for_count(), 1, "ready_for 必须被调用恰 1 次");
    assert_eq!(stub.did_change_count(), 1, "did_change 必须被调用恰 1 次");
    assert_eq!(stub.did_save_count(), 1, "did_save 必须被调用恰 1 次");
    assert_eq!(
        stub.calls(),
        vec![SyncCall::ReadyFor, SyncCall::DidChange, SyncCall::DidSave],
        "调用顺序必须是 ready_for → did_change → did_save"
    );
    assert_eq!(
        stub.changed(),
        vec![(path.clone(), text.to_string())],
        "did_change 必须收到原样 path 与 text"
    );
    assert_eq!(
        stub.saved(),
        vec![path.clone()],
        "did_save 必须收到原样 path"
    );

    pool.shutdown().await;
}

#[test]
fn test_case_insensitive_extension() {
    let pool = LspServerPool::new(&fake_pool_root(), make_config());
    assert!(pool.server_for_file("/test/main.RS").is_some());
    assert!(pool.server_for_file("/test/main.TS").is_some());
}

#[test]
fn test_disabled_server() {
    let mut config = make_config();
    config
        .lsp_servers
        .get_mut("rust-analyzer")
        .unwrap()
        .disabled = Some(true);
    let pool = LspServerPool::new(&fake_pool_root(), config);
    assert!(pool.server_for_file("/test/main.rs").is_none());
}

#[test]
fn test_has_servers() {
    let pool = LspServerPool::new(&fake_pool_root(), make_config());
    assert!(pool.has_servers());
}

#[test]
fn test_empty_config() {
    let pool = LspServerPool::new(&fake_pool_root(), LspConfigFile::default());
    assert!(!pool.has_servers());
    assert!(pool.server_for_file("/test/main.rs").is_none());
}

#[tokio::test]
async fn test_ensure_server_for_file_no_match() {
    let pool = LspServerPool::new(&fake_pool_root(), make_config());
    // .md 文件没有匹配的 LSP 服务器
    let result = pool.ensure_server_for_file("/test/readme.md").await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("readme.md"));
}

#[tokio::test]
async fn test_ensure_server_for_file_already_initialized() {
    let dir = tempfile::tempdir().unwrap();
    let count = dir.path().join("spawns");
    let pool = make_fake_pool(&count);
    pool.ensure_server_for_file("/test/main.rs").await.unwrap();
    pool.ensure_server_for_file("/test/main.rs").await.unwrap();
    let spawned = std::fs::read_to_string(count).unwrap().lines().count();
    pool.shutdown().await;
    assert_eq!(spawned, 1, "已经就绪时复用同一进程");
}

/// [回归测试] 单个 client 关闭后，pool 不能用旧 initialized 名称跳过重新握手。
#[tokio::test]
async fn test_ensure_uses_current_client_readiness_after_client_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let count = dir.path().join("spawns");
    let pool = make_fake_pool(&count);
    pool.ensure_server_for_file("/tmp/main.rs").await.unwrap();
    let client = pool.server_for_file("/tmp/main.rs").unwrap();
    client.shutdown().await;
    pool.ensure_server_for_file("/tmp/main.rs").await.unwrap();
    let ready = client.is_ready();
    let spawned = std::fs::read_to_string(count).unwrap().lines().count();
    pool.shutdown().await;
    assert!(ready, "pool 必须依据当前 client 状态重新初始化");
    assert_eq!(spawned, 2, "单服务器关闭后必须启动新的进程");
}

/// perl 编写的极简 LSP 服务器（同 client_test.rs）：
/// 每次 spawn 向 `$PERI_LSP_TEST_COUNT` 追加一行 "spawned"，对带 id 的请求回 result:null
const FAKE_LSP_SCRIPT: &str = r#"open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
print $c "spawned\n";
close $c;
binmode STDIN;
select STDOUT;
$| = 1;
while (1) {
    my $h = '';
    while (1) {
        my $l = <STDIN>;
        last unless defined $l;
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    read(STDIN, $b, $len) == $len or last;
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 构造以 perl fake server 为命令的 LspServerPool（仅 .rs 路由）
fn make_fake_pool(count_file: &std::path::Path) -> LspServerPool {
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_COUNT".to_string(),
        count_file.to_string_lossy().into_owned(),
    );
    let mut servers = HashMap::new();
    servers.insert(
        "fake-lsp".to_string(),
        LspServerConfig {
            name: "fake-lsp".to_string(),
            command: "perl".to_string(),
            args: vec!["-e".to_string(), FAKE_LSP_SCRIPT.to_string()],
            env: Some(env),
            extension_to_language: HashMap::from([(".rs".to_string(), "rust".to_string())]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    LspServerPool::new(
        &fake_pool_root(),
        LspConfigFile {
            lsp_servers: servers,
        },
    )
}

/// 同 `FAKE_LSP_SCRIPT`，另将收到的 `textDocument/*` 通知逐行落盘到
/// `$ENV{PERI_LSP_TEST_DOCUMENTS}`（端口同步的 uri / 顺序 / 文本断言用）。
const FAKE_LSP_SCRIPT_RECORDING_DOCUMENTS: &str = r#"open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
print $c "spawned\n";
close $c;
binmode STDIN;
select STDOUT;
$| = 1;
while (1) {
    my $h = '';
    while (1) {
        my $l = <STDIN>;
        last unless defined $l;
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    while (length($b) < $len) {
        my $n = read(STDIN, my $part, $len - length($b));
        exit 1 unless $n;
        $b .= $part;
    }
    if ($b =~ /"method":"textDocument\//) {
        open my $d, '>>', $ENV{PERI_LSP_TEST_DOCUMENTS} or exit 1;
        print $d $b . "\n";
        close $d;
    }
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 构造记录 `textDocument/*` 通知的 fake pool（仅 .rs 路由）
fn make_recording_pool(
    count_file: &std::path::Path,
    documents_file: &std::path::Path,
) -> LspServerPool {
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_COUNT".to_string(),
        count_file.to_string_lossy().into_owned(),
    );
    env.insert(
        "PERI_LSP_TEST_DOCUMENTS".to_string(),
        documents_file.to_string_lossy().into_owned(),
    );
    let mut servers = HashMap::new();
    servers.insert(
        "fake-lsp".to_string(),
        LspServerConfig {
            name: "fake-lsp".to_string(),
            command: "perl".to_string(),
            args: vec![
                "-e".to_string(),
                FAKE_LSP_SCRIPT_RECORDING_DOCUMENTS.to_string(),
            ],
            env: Some(env),
            extension_to_language: HashMap::from([(".rs".to_string(), "rust".to_string())]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    LspServerPool::new(
        "/tmp",
        LspConfigFile {
            lsp_servers: servers,
        },
    )
}

/// 已落盘的 `textDocument/*` 通知（半行/未完成写入直接跳过）
fn recorded_documents(documents_file: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(documents_file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

/// 等待 fake server 落盘至少 `expected` 条通知：客户端写完成 ≠ 服务器已处理
async fn wait_recorded_documents(documents_file: &std::path::Path, expected: usize) -> Vec<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let documents = recorded_documents(documents_file);
        if documents.len() >= expected {
            return documents;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fake server 未在超时内落盘 {expected} 条 textDocument 通知（实收 {}）",
            documents.len()
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// 同 FAKE_LSP_SCRIPT，另将子进程 PID 写入 `$ENV{PERI_LSP_TEST_PID}`（生命周期断言用）
const FAKE_LSP_SCRIPT_WITH_PID: &str = r#"open my $p, '>', $ENV{PERI_LSP_TEST_PID} or exit 1;
print $p "$$\n";
close $p;
open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
print $c "spawned\n";
close $c;
binmode STDIN;
select STDOUT;
$| = 1;
while (1) {
    my $h = '';
    while (1) {
        my $l = <STDIN>;
        last unless defined $l;
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    read(STDIN, $b, $len) == $len or last;
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 构造记录 PID 的 fake pool（仅 .rs 路由）
fn make_fake_pool_with_pid(
    count_file: &std::path::Path,
    pid_file: &std::path::Path,
) -> LspServerPool {
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_COUNT".to_string(),
        count_file.to_string_lossy().into_owned(),
    );
    env.insert(
        "PERI_LSP_TEST_PID".to_string(),
        pid_file.to_string_lossy().into_owned(),
    );
    let mut servers = HashMap::new();
    servers.insert(
        "fake-lsp".to_string(),
        LspServerConfig {
            name: "fake-lsp".to_string(),
            command: "perl".to_string(),
            args: vec!["-e".to_string(), FAKE_LSP_SCRIPT_WITH_PID.to_string()],
            env: Some(env),
            extension_to_language: HashMap::from([(".rs".to_string(), "rust".to_string())]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    LspServerPool::new(
        &fake_pool_root(),
        LspConfigFile {
            lsp_servers: servers,
        },
    )
}

/// 探活：进程存在返回 true。
/// Unix 用 kill -0；Windows 用 tasklist（/FO CSV /NH，精确匹配 PID 列）。
/// shutdown 路径经 transport.close → tokio Child::kill（start_kill + wait reap），
/// 进程表无僵尸残留，探活失败即已退出。
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    let pid_str = pid.to_string();
    let filter = format!("PID eq {pid}");
    std::process::Command::new("tasklist")
        .args(["/FI", filter.as_str(), "/FO", "CSV", "/NH"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout).lines().any(|line| {
                line.split(',').nth(1).map(|c| c.trim_matches('"')) == Some(pid_str.as_str())
            })
        })
        .unwrap_or(false)
}

/// 生命周期：pool.shutdown() 后 LSP 服务器子进程必须退出（H1 进程泄漏验证）。
#[tokio::test]
async fn test_shutdown_kills_child_process() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let pid_file = dir.path().join("server.pid");
    let pool = make_fake_pool_with_pid(&count_file, &pid_file);

    pool.ensure_server_for_file("/test/main.rs").await.unwrap();
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .expect("fake server 应写出 PID 文件")
        .trim()
        .parse()
        .expect("PID 应为数字");
    assert!(process_alive(pid), "启动后服务器进程应存活（pid={pid}）");

    pool.shutdown().await;

    assert!(
        !process_alive(pid),
        "shutdown 后服务器子进程应退出（pid={pid}）"
    );
}

/// 生命周期：shutdown 清空激活状态；再次 ensure 重新 spawn（不残留旧进程复用）。
#[tokio::test]
async fn test_shutdown_then_ensure_respawns() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let pool = make_fake_pool(&count_file);

    pool.ensure_server_for_file("/test/main.rs").await.unwrap();
    assert!(pool.any_server().is_some());

    pool.shutdown().await;
    assert!(pool.any_server().is_none(), "shutdown 后不能残留就绪服务器");

    pool.ensure_server_for_file("/test/main.rs").await.unwrap();
    let count = std::fs::read_to_string(&count_file)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    assert_eq!(count, 2, "shutdown 后 ensure 应重新 spawn 而非复用已死进程");

    pool.shutdown().await;
}

#[tokio::test]
async fn test_concurrent_ensure_server_for_file_spawns_once() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let pool = make_fake_pool(&count_file);

    let (r1, r2) = tokio::join!(
        pool.ensure_server_for_file("/test/main.rs"),
        pool.ensure_server_for_file("/test/lib.rs"),
    );

    assert!(r1.is_ok(), "第一个 ensure 失败: {:?}", r1.err());
    assert!(r2.is_ok(), "第二个 ensure 失败: {:?}", r2.err());
    let count = std::fs::read_to_string(&count_file)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    assert_eq!(
        count, 1,
        "并发 ensure_server_for_file 只应 spawn 一次子进程"
    );
    assert!(pool.any_server().is_some(), "握手完成后服务器可用");

    pool.shutdown().await;
}

/// [回归测试] 同名动态替换必须关闭旧进程，旧 client Arc 与旧扩展名不能成为第二个 owner。
#[tokio::test]
async fn test_replacing_server_closes_old_client_and_replaces_extension_routes() {
    let dir = tempfile::tempdir().unwrap();
    let count = dir.path().join("spawns");
    let pid_file = dir.path().join("pid");
    let pool = make_fake_pool_with_pid(&count, &pid_file);
    pool.ensure_server_for_file("/test/main.rs").await.unwrap();
    let old = pool.server_for_file("/test/main.rs").unwrap();
    let pid = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    pool.add_server(LspServerConfig {
        name: "fake-lsp".into(),
        command: "perl".into(),
        args: vec!["-e".into(), FAKE_LSP_SCRIPT.into()],
        env: Some(HashMap::from([(
            "PERI_LSP_TEST_COUNT".into(),
            count.to_string_lossy().into_owned(),
        )])),
        extension_to_language: HashMap::from([(".py".into(), "python".into())]),
        initialization_options: None,
        disabled: None,
        max_restarts: None,
        startup_timeout: None,
        source: None,
    })
    .await;
    let current = pool.server_for_file("/test/main.py").unwrap();
    assert!(!old.is_ready());
    assert!(
        !process_alive(pid),
        "外部仍持有旧 Arc 时也必须回收被替换进程"
    );
    assert!(
        pool.server_for_file("/test/main.rs").is_none(),
        "移除已不属于新配置的扩展名"
    );
    assert!(!Arc::ptr_eq(&old, &current));
    assert!(current.is_ready(), "已激活的池自动握手新实例");
    pool.shutdown().await;
}
