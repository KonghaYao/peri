use super::*;
use std::path::Path;

// 文件 gate 只协调真实 wire 边界；watchdog 不作为正确性计时断言。
const SCRIPT: &str = r#"
open my $pid, '>', "$ENV{FIXTURE}/pid" or exit 1;
print $pid $$; close $pid;
binmode STDIN;
select STDOUT; $| = 1;
while (1) {
    my $h = '';
    while (my $l = <STDIN>) {
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    read(STDIN, $b, $len) == $len or last;
    if ($b =~ /"method":"initialize"/ && $ENV{GATE_INIT}) {
        open my $f, '>', "$ENV{FIXTURE}/initialize" or exit 1;
        close $f;
        select undef, undef, undef, 0.005 until -e "$ENV{FIXTURE}/release";
    }
    if ($b =~ /"method":"hang"/) {
        open my $f, '>', "$ENV{FIXTURE}/request" or exit 1;
        close $f;
        next;
    }
    if ($b =~ /"method":"shutdown"/ && $ENV{GATE_SHUTDOWN}) {
        open my $f, '>', "$ENV{FIXTURE}/shutdown" or exit 1;
        close $f;
        select undef, undef, undef, 0.005 until -e "$ENV{FIXTURE}/release-shutdown";
    }
    if ($b =~ /"id":(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}
"#;

fn make_client(dir: &Path, gate_init: bool) -> Arc<LspClient> {
    Arc::new(LspClient::new(
        "gated".into(),
        "perl".into(),
        vec!["-e".into(), SCRIPT.into()],
        HashMap::from([
            ("FIXTURE".into(), dir.to_string_lossy().into_owned()),
            ("GATE_INIT".into(), if gate_init { "1" } else { "0" }.into()),
        ]),
        None,
        3,
        5_000,
        Arc::new(DiagnosticsRegistry::new()),
    ))
}

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("真实服务器未到达预期协议边界");
}

/// [回归测试] initialize 响应被 gate 时，第二个 start 不得看到提前发布的 Running。
#[tokio::test]
async fn test_start_waits_for_initialize_before_publishing_readiness() {
    let dir = tempfile::tempdir().unwrap();
    let client = make_client(dir.path(), true);
    let first = tokio::spawn({
        let client = client.clone();
        async move { client.start("file:///tmp").await }
    });
    wait_for_file(&dir.path().join("initialize")).await;
    let premature_ready = client.is_ready();
    let second = client.start("file:///tmp");
    tokio::pin!(second);
    let premature_return = tokio::select! {
        biased;
        _ = &mut second => true,
        _ = tokio::task::yield_now() => false,
    };
    std::fs::write(dir.path().join("release"), b"").unwrap();
    first.await.unwrap().unwrap();
    if !premature_return {
        second.await.unwrap();
    }
    client.shutdown().await;
    assert!(!premature_ready, "initialize 完成前不能发布可用状态");
    assert!(!premature_return, "并发 start 必须等待同一握手完成");
}

/// [回归测试] 丢弃调用方 future 也应取消其原 dispatcher 中的 pending 登记。
#[tokio::test]
async fn test_cancelled_request_releases_pending_registration() {
    let dir = tempfile::tempdir().unwrap();
    let client = make_client(dir.path(), false);
    client.start("file:///tmp").await.unwrap();
    let request = tokio::spawn({
        let client = client.clone();
        async move { client.request("hang", None, 30_000).await }
    });
    wait_for_file(&dir.path().join("request")).await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let pending = client
        .connection
        .read()
        .dispatcher
        .as_ref()
        .unwrap()
        .dispatch_state()
        .pending_len();
    let next = client.request("next", None, 5_000).await.unwrap();
    client.shutdown().await;
    assert_eq!(pending, 0, "取消请求后不能等待整条连接关闭才移除登记");
    assert_eq!(next, Value::Null, "取消旧请求后连接仍能处理完整的新请求");
}

/// [回归测试] 启动调用被取消时撤回其注册，Drop 保底回收真实子进程。
#[cfg(unix)]
#[tokio::test]
async fn test_cancelled_start_releases_connection_and_child() {
    let dir = tempfile::tempdir().unwrap();
    let client = make_client(dir.path(), true);
    let start = tokio::spawn({
        let client = client.clone();
        async move { client.start("file:///tmp").await }
    });
    wait_for_file(&dir.path().join("initialize")).await;
    let pid = std::fs::read_to_string(dir.path().join("pid")).unwrap();
    start.abort();
    assert!(start.await.unwrap_err().is_cancelled());
    assert!(client.connection.read().dispatcher.is_none());
    assert!(!client.is_ready());
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while std::process::Command::new("kill")
            .args(["-0", &pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("启动取消后子进程应被回收");
    std::fs::write(dir.path().join("release"), b"").unwrap();
    client.start("file:///tmp").await.unwrap();
    assert!(client.is_ready(), "取消后的下一次启动可重新完成握手");
    client.shutdown().await;
}

/// [回归测试] 关闭等待被取消时，即使旧请求持有连接 Arc 也须结算 pending 并可重试回收。
#[tokio::test]
async fn test_cancelled_shutdown_rejects_old_requests_and_can_finish_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = make_client(dir.path(), false);
    Arc::get_mut(&mut client)
        .unwrap()
        .env
        .insert("GATE_SHUTDOWN".into(), "1".into());
    client.start("file:///tmp").await.unwrap();
    let request = tokio::spawn({
        let client = client.clone();
        async move { client.request("hang", None, 30_000).await }
    });
    wait_for_file(&dir.path().join("request")).await;
    let shutdown = tokio::spawn({
        let client = client.clone();
        async move { client.shutdown().await }
    });
    wait_for_file(&dir.path().join("shutdown")).await;
    shutdown.abort();
    assert!(shutdown.await.unwrap_err().is_cancelled());
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), request)
        .await
        .expect("取消关闭等待后原请求必须立即结算")
        .unwrap();
    assert!(
        matches!(result, Err(LspError::RequestFailed { .. })),
        "原请求不应一直等待自身30秒超时: {result:?}"
    );
    client.shutdown().await;
    assert_eq!(client.state(), ServerState::Stopped);
    assert!(
        client.connection.read().dispatcher.is_none(),
        "重试关闭完成join后释放注册"
    );
}
