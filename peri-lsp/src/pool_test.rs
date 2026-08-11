//! Tests for pool

use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::ports::LspPoolPort;

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

#[test]
fn test_extension_routing() {
    let pool = LspServerPool::new("/tmp", make_config());
    assert!(pool.server_for_file("/test/main.rs").is_some());
    assert!(pool.server_for_file("/test/index.ts").is_some());
    assert!(pool.server_for_file("/test/App.tsx").is_some());
    assert!(pool.server_for_file("/test/readme.md").is_none());
    assert!(pool.server_for_file("/test/no_ext").is_none());
}

/// 类型不匹配的端口实现（downcast 失败路径用）
struct StubPool;

#[async_trait::async_trait]
impl LspPoolPort for StubPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn shutdown(&self) {}
}

/// 端口 downcast 往返：upcast 为 Arc<dyn LspPoolPort> 后经 downcast_arc
/// 还原为同一 LspServerPool 实例（装配面会话级复用前置条件，H1）。
#[test]
fn test_lsp_pool_port_downcast_roundtrip() {
    let pool: Arc<LspServerPool> = Arc::new(LspServerPool::new("/tmp", make_config()));
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
    let port: Arc<dyn LspPoolPort> = Arc::new(StubPool);
    let err = match port.downcast_arc::<LspServerPool>() {
        Ok(_) => panic!("类型不匹配时应还原失败"),
        Err(p) => p,
    };
    err.shutdown().await;
}

#[test]
fn test_case_insensitive_extension() {
    let pool = LspServerPool::new("/tmp", make_config());
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
    let pool = LspServerPool::new("/tmp", config);
    assert!(pool.server_for_file("/test/main.rs").is_none());
}

#[test]
fn test_has_servers() {
    let pool = LspServerPool::new("/tmp", make_config());
    assert!(pool.has_servers());
}

#[test]
fn test_empty_config() {
    let pool = LspServerPool::new("/tmp", LspConfigFile::default());
    assert!(!pool.has_servers());
    assert!(pool.server_for_file("/test/main.rs").is_none());
}

#[tokio::test]
async fn test_ensure_server_for_file_no_match() {
    let pool = LspServerPool::new("/tmp", make_config());
    // .md 文件没有匹配的 LSP 服务器
    let result = pool.ensure_server_for_file("/test/readme.md").await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("readme.md"));
}

#[tokio::test]
async fn test_ensure_server_for_file_already_initialized() {
    let pool = LspServerPool::new("/tmp", make_config());
    // 手动标记为已初始化
    pool.initialized.write().insert("rust-analyzer".to_string());
    // 不应尝试启动
    let result = pool.ensure_server_for_file("/test/main.rs").await;
    assert!(result.is_ok());
    // typescript 仍然未初始化
    assert!(!pool.initialized.read().contains("typescript"));
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
        "/tmp",
        LspConfigFile {
            lsp_servers: servers,
        },
    )
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
        "/tmp",
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

/// 生命周期：shutdown 清空 initialized；再次 ensure 重新 spawn（不残留旧进程复用）。
#[tokio::test]
async fn test_shutdown_then_ensure_respawns() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join("spawn_count.txt");
    let pool = make_fake_pool(&count_file);

    pool.ensure_server_for_file("/test/main.rs").await.unwrap();
    assert_eq!(pool.initialized.read().len(), 1);

    pool.shutdown().await;
    assert!(
        pool.initialized.read().is_empty(),
        "shutdown 后 initialized 应清空"
    );

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
    assert_eq!(pool.initialized.read().len(), 1, "initialized 只插入一次");

    pool.shutdown().await;
}
