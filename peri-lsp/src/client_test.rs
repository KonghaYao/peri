//! Tests for LspClient（并发启动互斥）

use std::{collections::HashMap, sync::Arc};

use super::*;
use crate::diagnostics::DiagnosticsRegistry;
use crate::error::LspError;
use crate::protocol::lsp_types::PublishDiagnosticsParams;

/// perl 编写的极简 LSP 服务器：
/// - 每次 spawn 向 `$PERI_LSP_TEST_COUNT` 文件追加一行 "spawned"（用于断言 spawn 次数）
/// - 对任何带 id 的请求回 `{"result":null}`（满足 initialize/shutdown 握手）
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

/// 构造以 perl fake server 为命令的 LspClient
fn make_fake_client(count_file: &std::path::Path) -> LspClient {
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_COUNT".to_string(),
        count_file.to_string_lossy().into_owned(),
    );
    LspClient::new(
        "fake-lsp".to_string(),
        "perl".to_string(),
        vec!["-e".to_string(), FAKE_LSP_SCRIPT.to_string()],
        env,
        None,
        3,
        DEFAULT_STARTUP_TIMEOUT_MS,
        Arc::new(DiagnosticsRegistry::new()),
    )
}

/// 记录 didOpen 通知的 fake server：同 FAKE_LSP_SCRIPT，另将 didOpen 通知的
/// 完整 JSON body 追加到 `$ENV{PERI_LSP_TEST_DIDOPEN}` 文件
const FAKE_LSP_RECORDING_SCRIPT: &str = r#"open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
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
    if ($b =~ /"method"\s*:\s*"textDocument\/didOpen"/) {
        open my $f, '>>', $ENV{PERI_LSP_TEST_DIDOPEN} or next;
        print $f "$b\n";
        close $f;
    }
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 慢响应的 fake server：收到带 id 的请求后 sleep 3 秒再回复。
/// 用于验证 startup_timeout 生效（短超时 + 慢服务器 → initialize 超时）
const SLOW_LSP_SCRIPT: &str = r#"binmode STDIN;
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
        sleep 3;
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 慢响应 + 写入自身 PID 的 fake server（SLOW_LSP_SCRIPT 的 PID 变体）：
/// 启动时把 `$$` 写入 `$ENV{PERI_LSP_TEST_PID}`，供测试断言失败路径
/// 中子进程被主动清理（kill -0 探活）。
const SLOW_LSP_WITH_PID_SCRIPT: &str = r#"open my $p, '>', $ENV{PERI_LSP_TEST_PID} or exit 1;
print $p $$;
close $p;
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
        sleep 3;
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 响应 initialize 后关闭 stdin 的 fake server（initialized 通知失败路径）：
/// 收到带 id 的请求后回复，然后 `close STDIN` 并 sleep 30 保持存活
/// （stdout 仍打开，read task 不会因 EOF 触发清理）——只有 do_start 失败
/// 路径的主动清理能终止它。
const CLOSE_STDIN_AFTER_INIT_SCRIPT: &str = r#"open my $p, '>', $ENV{PERI_LSP_TEST_PID} or exit 1;
print $p $$;
close $p;
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
        close STDIN;
        sleep 30;
    }
}"#;

/// 构造记录 didOpen 通知的 fake client，返回 (client, didOpen 记录文件路径)
fn make_recording_client(dir: &std::path::Path) -> (LspClient, std::path::PathBuf) {
    let count_file = dir.join("spawn_count.txt");
    let didopen_file = dir.join("didopen.txt");
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_COUNT".to_string(),
        count_file.to_string_lossy().into_owned(),
    );
    env.insert(
        "PERI_LSP_TEST_DIDOPEN".to_string(),
        didopen_file.to_string_lossy().into_owned(),
    );
    (
        LspClient::new(
            "fake-lsp".to_string(),
            "perl".to_string(),
            vec!["-e".to_string(), FAKE_LSP_RECORDING_SCRIPT.to_string()],
            env,
            None,
            3,
            DEFAULT_STARTUP_TIMEOUT_MS,
            Arc::new(DiagnosticsRegistry::new()),
        ),
        didopen_file,
    )
}

/// 统计记录文件中的 didOpen 通知次数
fn didopen_count(didopen_file: &std::path::Path) -> usize {
    std::fs::read_to_string(didopen_file)
        .map(|s| s.matches("textDocument/didOpen").count())
        .unwrap_or(0)
}

/// 轮询等待 didOpen 通知到达（通知无响应，需异步等待子进程写入）
async fn wait_for_didopen(didopen_file: &std::path::Path, expected: usize) {
    for _ in 0..100 {
        if didopen_count(didopen_file) >= expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("didOpen 通知未在超时内到达 (expected >= {expected})");
}

/// 读取 fake server 记录的 spawn 次数
fn spawn_count(count_file: &std::path::Path) -> usize {
    std::fs::read_to_string(count_file)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[tokio::test]
async fn test_start_handshake_ok() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let client = make_fake_client(&count_file);

    let result = client.start("file:///tmp").await;
    assert!(result.is_ok(), "start 应完成握手: {:?}", result.err());
    assert_eq!(spawn_count(&count_file), 1);
    assert!(client.is_ready());

    client.shutdown().await;
}

#[tokio::test]
async fn test_start_uses_configured_startup_timeout() {
    // 配置 startup_timeout=200ms + 慢服务器（3s 才响应 initialize）→ 必须触发超时，
    // 且错误携带配置的超时值（证明 do_start 读取了 startup_timeout_ms 而非硬编码 30s）
    let client = LspClient::new(
        "slow-lsp".to_string(),
        "perl".to_string(),
        vec!["-e".to_string(), SLOW_LSP_SCRIPT.to_string()],
        HashMap::new(),
        None,
        3,
        200,
        Arc::new(DiagnosticsRegistry::new()),
    );

    let err = client.start("file:///tmp").await.unwrap_err();
    assert!(
        matches!(
            err,
            LspError::RequestTimeout {
                method: ref m,
                timeout_ms: 200,
            } if m == "initialize"
        ),
        "短超时 + 慢服务器应触发 initialize 超时: {err:?}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_request_timeout_cleans_pending() {
    // 请求超时后，dispatcher 的 pending map 不得残留 oneshot sender
    // （此前仅在 transport EOF 时由 reject_all_pending 整体清理，超时条目会一直残留）
    let client = LspClient::new(
        "slow-lsp".to_string(),
        "perl".to_string(),
        vec!["-e".to_string(), SLOW_LSP_SCRIPT.to_string()],
        HashMap::new(),
        None,
        3,
        100,
        Arc::new(DiagnosticsRegistry::new()),
    );

    // initialize 请求 100ms 超时（慢服务器 3s 才响应）
    let err = client.start("file:///tmp").await.unwrap_err();
    assert!(
        matches!(err, LspError::RequestTimeout { .. }),
        "慢服务器 + 短超时应触发超时: {err:?}"
    );

    // 启动失败路径整体清理 dispatcher（kill 子进程 + abort read task），
    // pending map 中的超时条目随之释放，不残留 oneshot sender
    assert!(
        client.dispatcher.lock().await.is_none(),
        "start 失败后 dispatcher 应被整体清理（含 pending 条目）"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_concurrent_start_spawns_once() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let client = make_fake_client(&count_file);

    let (r1, r2) = tokio::join!(client.start("file:///tmp"), client.start("file:///tmp"));

    assert!(r1.is_ok(), "第一个 start 失败: {:?}", r1.err());
    assert!(r2.is_ok(), "第二个 start 失败: {:?}", r2.err());
    assert_eq!(
        spawn_count(&count_file),
        1,
        "并发 start 只应 spawn 一次子进程"
    );
    assert!(client.is_ready());

    client.shutdown().await;
}

#[tokio::test]
async fn test_did_open_idempotent_with_first_content() {
    // 同一 uri 重复 did_open 只发送一次通知，且携带首次传入的文本
    let dir = tempfile::tempdir().unwrap();
    let (client, didopen_file) = make_recording_client(dir.path());
    client.start("file:///tmp").await.unwrap();

    client
        .did_open("file:///tmp/main.rs", "rust", "fn main() {}")
        .await
        .unwrap();
    client
        .did_open("file:///tmp/main.rs", "rust", "changed content")
        .await
        .unwrap();

    wait_for_didopen(&didopen_file, 1).await;
    assert_eq!(
        didopen_count(&didopen_file),
        1,
        "重复 did_open 不应发送第二次通知"
    );
    let record = std::fs::read_to_string(&didopen_file).unwrap();
    assert!(
        record.contains("fn main() {}"),
        "通知应携带首次传入的文本: {record}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_try_restart_resets_open_cache() {
    // try_restart 后 open_files 缓存清空，同一 uri 再次 did_open 应重新发送
    let dir = tempfile::tempdir().unwrap();
    let (client, didopen_file) = make_recording_client(dir.path());
    client.start("file:///tmp").await.unwrap();

    client
        .did_open("file:///tmp/main.rs", "rust", "v1")
        .await
        .unwrap();
    wait_for_didopen(&didopen_file, 1).await;
    assert_eq!(didopen_count(&didopen_file), 1);

    client.try_restart("file:///tmp").await.unwrap();
    client
        .did_open("file:///tmp/main.rs", "rust", "v2")
        .await
        .unwrap();

    wait_for_didopen(&didopen_file, 2).await;
    let record = std::fs::read_to_string(&didopen_file).unwrap();
    assert_eq!(
        record.matches("textDocument/didOpen").count(),
        2,
        "重启后缓存重置，应再次发送 didOpen"
    );
    assert!(record.contains("v2"), "重启后的通知应携带新文本: {record}");

    client.shutdown().await;
}

#[tokio::test]
async fn test_restart_window_cooldown() {
    // 窗口内连续重启达到 max_restarts 后进入冷却：返回 ServerCrashed 且不再 spawn
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let client = make_fake_client(&count_file);

    client.start("file:///tmp").await.unwrap();
    for _ in 0..3 {
        client.try_restart("file:///tmp").await.unwrap();
    }

    let spawns_before = spawn_count(&count_file);
    let err = client.try_restart("file:///tmp").await.unwrap_err();
    assert!(
        matches!(
            err,
            LspError::ServerCrashed {
                restart_count: 3,
                max_restarts: 3,
                ..
            }
        ),
        "窗口内第 4 次重启应返回 ServerCrashed: {err:?}"
    );
    assert_eq!(
        spawn_count(&count_file),
        spawns_before,
        "冷却期不应 spawn 子进程"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_restart_window_expiry_resets_count() {
    // 窗口过后计数清零、冷却解除：再次重启成功。
    // 窗口不能设太短：Windows 上 spawn 子进程较慢，若单次重启耗时超过窗口，
    // 计数会被提前清零（第 4 次重启意外成功）——2s 窗口保证 3 次重启落在同一窗口
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let mut client = make_fake_client(&count_file);
    client.restart_window = std::time::Duration::from_secs(2);

    client.start("file:///tmp").await.unwrap();
    for _ in 0..3 {
        client.try_restart("file:///tmp").await.unwrap();
    }
    let err = client.try_restart("file:///tmp").await.unwrap_err();
    assert!(matches!(err, LspError::ServerCrashed { .. }));

    // 等待窗口过期（窗口 + 100ms 缓冲），冷却解除
    tokio::time::sleep(client.restart_window + std::time::Duration::from_millis(100)).await;
    client.try_restart("file:///tmp").await.unwrap();
    assert!(client.is_ready(), "窗口过后冷却解除，应能重启成功");

    client.shutdown().await;
}

#[tokio::test]
async fn test_try_restart_clears_diagnostics() {
    // 重启后 DiagnosticsRegistry 应被清空，避免旧诊断残留
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let diagnostics = Arc::new(DiagnosticsRegistry::new());
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_COUNT".to_string(),
        count_file.to_string_lossy().into_owned(),
    );
    let client = LspClient::new(
        "fake-lsp".to_string(),
        "perl".to_string(),
        vec!["-e".to_string(), FAKE_LSP_SCRIPT.to_string()],
        env,
        None,
        3,
        DEFAULT_STARTUP_TIMEOUT_MS,
        Arc::clone(&diagnostics),
    );
    client.start("file:///tmp").await.unwrap();

    diagnostics.handle_publish_diagnostics(&PublishDiagnosticsParams {
        uri: "file:///tmp/main.rs".parse().unwrap(),
        diagnostics: vec![lsp_types::Diagnostic {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 0,
                },
                end: lsp_types::Position {
                    line: 0,
                    character: 1,
                },
            },
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            message: "old error".to_string(),
            source: Some("test".to_string()),
            ..Default::default()
        }],
        version: None,
    });
    assert!(!diagnostics.get_all().is_empty(), "前置条件：诊断应非空");

    client.try_restart("file:///tmp").await.unwrap();
    assert!(diagnostics.get_all().is_empty(), "重启后旧诊断应被清空");

    client.shutdown().await;
}

/// 探活：进程存在返回 true。
/// Unix 用 kill -0；Windows 用 tasklist（/FO CSV /NH，精确匹配 PID 列）。
fn process_alive(pid: &str) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        let filter = format!("PID eq {pid}");
        std::process::Command::new("tasklist")
            .args(["/FI", filter.as_str(), "/FO", "CSV", "/NH"])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|line| line.split(',').nth(1).map(|c| c.trim_matches('"')) == Some(pid))
            })
            .unwrap_or(false)
    }
}

/// 轮询等待子进程退出（探活；close() 已 wait 回收，无 zombie 残留）。
/// pid_file 由伪服务器启动时写入自身 PID。
async fn wait_for_child_exit(pid_file: &std::path::Path) {
    let mut last_pid = String::new();
    for _ in 0..150 {
        let pid = std::fs::read_to_string(pid_file)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !pid.is_empty() {
            last_pid = pid.clone();
            if !process_alive(&pid) {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("子进程 {last_pid} 在超时内未退出（孤儿进程泄漏）");
}

/// 构造写入 PID 文件的慢服务器 client
fn make_pid_tracking_client(
    pid_file: &std::path::Path,
    script: &str,
    startup_timeout_ms: u64,
) -> LspClient {
    let mut env = HashMap::new();
    env.insert(
        "PERI_LSP_TEST_PID".to_string(),
        pid_file.to_string_lossy().into_owned(),
    );
    LspClient::new(
        "pid-lsp".to_string(),
        "perl".to_string(),
        vec!["-e".to_string(), script.to_string()],
        env,
        None,
        3,
        startup_timeout_ms,
        Arc::new(DiagnosticsRegistry::new()),
    )
}

#[tokio::test]
async fn test_start_failure_initialize_kills_child() {
    // do_start 失败路径（initialize 超时）必须清理已 spawn 的子进程与 read task：
    // 此前 dispatcher 残留、子进程 stdin 未关不 EOF，成为孤儿进程
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("server.pid");
    let client = make_pid_tracking_client(&pid_file, SLOW_LSP_WITH_PID_SCRIPT, 200);

    let err = client.start("file:///tmp").await.unwrap_err();
    assert!(
        matches!(
            err,
            LspError::RequestTimeout {
                method: ref m,
                timeout_ms: 200,
            } if m == "initialize"
        ),
        "短超时 + 慢服务器应触发 initialize 超时: {err:?}"
    );
    assert!(
        client.dispatcher.lock().await.is_none(),
        "启动失败后 dispatcher 应被清理（子进程/read task 不残留）"
    );
    wait_for_child_exit(&pid_file).await;
}

#[tokio::test]
async fn test_start_failure_notify_kills_child() {
    // do_start 失败路径（initialized 通知写入失败）必须清理子进程：
    // 服务器响应 initialize 后关闭 stdin 但保持存活（stdout 仍打开，
    // read task 不会因 EOF 触发清理）——只有失败路径的主动清理能终止它
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("server.pid");
    let client = make_pid_tracking_client(&pid_file, CLOSE_STDIN_AFTER_INIT_SCRIPT, 5_000);

    let err = client.start("file:///tmp").await.unwrap_err();
    assert!(
        matches!(err, LspError::Io(_)),
        "stdin 关闭后 initialized 通知应 IO 失败: {err:?}"
    );
    assert!(
        client.dispatcher.lock().await.is_none(),
        "启动失败后 dispatcher 应被清理（子进程/read task 不残留）"
    );
    wait_for_child_exit(&pid_file).await;
}
