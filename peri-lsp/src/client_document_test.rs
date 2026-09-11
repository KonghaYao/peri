//! 文档同步跨真实握手、writer 背压、连接更换的契约回归。

use super::*;
use std::{future::Future, path::Path, pin::Pin, task::Poll, time::Duration};

const SCRIPT: &str = r#"
binmode STDIN; binmode STDOUT; select STDOUT; $| = 1;
while (1) {
    my $h = '';
    while (my $l = <STDIN>) { last if $l =~ /^\r?\n$/; $h .= $l; }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $body = '';
    while (length($body) < $len) {
        my $n = read(STDIN, my $part, $len - length($body));
        exit 1 unless $n; $body .= $part;
    }
    if ($body =~ /"method":"initialize"/ && $ENV{GATE_INIT}) {
        open my $f, '>', "$ENV{FIXTURE}/initialize" or exit 1; close $f;
        select undef, undef, undef, 0.005 until -e "$ENV{FIXTURE}/release-init";
    }
    if ($body =~ /"method":"pause-input"/) {
        open my $f, '>', "$ENV{FIXTURE}/paused" or exit 1; close $f;
        select undef, undef, undef, 0.005 until -e "$ENV{FIXTURE}/release-input";
    }
    if ($body =~ /"method":"textDocument\//) {
        open my $f, '>>', "$ENV{FIXTURE}/documents" or exit 1;
        print $f $body . "\n"; close $f;
    }
    if ($body =~ /"id":(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}
"#;

fn make_client(dir: &Path, gate_init: bool) -> Arc<LspClient> {
    Arc::new(LspClient::new(
        "documents".into(),
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

async fn wait_until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("真实服务器未到达预期协议边界");
}

async fn poll_pending<T>(future: Pin<&mut (impl Future<Output = T> + ?Sized)>) {
    let mut future = future;
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "队列已满时调用必须等待准入"
        );
        Poll::Ready(())
    })
    .await;
}

fn documents(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("documents"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

// Future 仅用于保持待确认的调用存活；真实 writer 独占已入队帧。
type Sending = Pin<Box<dyn Future<Output = Result<(), LspError>> + Send>>;

async fn block_writer(client: &Arc<LspClient>, dir: &Path) -> Sending {
    client.notify("pause-input", None).await.unwrap();
    wait_until(|| dir.join("paused").exists()).await;
    let dispatcher = client.ready_dispatcher().unwrap();
    let mut first: Sending = Box::pin({
        let client = client.clone();
        async move {
            client
                .notify(
                    "blocked-frame",
                    Some(serde_json::json!({"text":"x".repeat(8 * 1024 * 1024)})),
                )
                .await
        }
    });
    poll_pending(first.as_mut()).await;
    // 首帧已被 writer 取出且阻塞于真实 stdin，随后填满 16 个队列槽位。
    wait_until(|| dispatcher.writer_capacity_for_test() == Some(16)).await;
    first
}

async fn fill_writer(client: &Arc<LspClient>, dir: &Path) -> Vec<Sending> {
    let first = block_writer(client, dir).await;
    let dispatcher = client.ready_dispatcher().unwrap();
    let mut senders = vec![first];
    for _ in 0..16 {
        let mut sender: Sending = Box::pin({
            let client = client.clone();
            async move { client.notify("filler", None).await }
        });
        poll_pending(sender.as_mut()).await;
        senders.push(sender);
    }
    assert_eq!(dispatcher.writer_capacity_for_test(), Some(0));
    senders
}

/// [回归测试] initialize gate 内 NotReady 的文档操作不能留下假缓存。
#[tokio::test]
async fn test_document_sync_not_ready_does_not_hide_the_first_open() {
    let dir = tempfile::tempdir().unwrap();
    let client = make_client(dir.path(), true);
    let start = tokio::spawn({
        let client = client.clone();
        async move { client.start("file:///tmp").await }
    });
    wait_until(|| dir.path().join("initialize").exists()).await;
    let open = client
        .did_open("file:///tmp/open.rs", "rust", "rejected-open")
        .await;
    let change = client
        .did_change("file:///tmp/change.rs", "rejected-change")
        .await;
    std::fs::write(dir.path().join("release-init"), b"").unwrap();
    start.await.unwrap().unwrap();
    client
        .did_open("file:///tmp/open.rs", "rust", "accepted-open")
        .await
        .unwrap();
    client
        .did_change("file:///tmp/change.rs", "accepted-change")
        .await
        .unwrap();
    client.request("barrier", None, 5_000).await.unwrap();
    let records = documents(dir.path());
    client.shutdown().await;
    assert!(matches!(open, Err(LspError::NotReady { .. })));
    assert!(matches!(change, Err(LspError::NotReady { .. })));
    assert_eq!(records.len(), 2, "两次被拒绝的调用均不得吞掉后续首次同步");
    assert_eq!(records[0]["method"], "textDocument/didOpen");
    assert_eq!(
        records[0]["params"]["textDocument"]["text"],
        "accepted-open"
    );
    assert_eq!(records[1]["method"], "textDocument/didOpen");
    assert_eq!(
        records[1]["params"]["textDocument"]["text"],
        "accepted-change"
    );
}

/// [回归测试] 满 writer 队列的准入前取消不能吞 didOpen 或跳过 didChange 版本。
#[tokio::test]
async fn test_document_sync_cancel_before_admission_preserves_cache_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let client = make_client(dir.path(), false);
    client.start("file:///tmp").await.unwrap();
    client
        .did_open("file:///tmp/existing.rs", "rust", "initial")
        .await
        .unwrap();
    let senders = fill_writer(&client, dir.path()).await;
    {
        let mut open = Box::pin(client.did_open("file:///tmp/new.rs", "rust", "cancelled-open"));
        poll_pending(open.as_mut()).await;
    }
    {
        let mut change = Box::pin(client.did_change("file:///tmp/existing.rs", "cancelled-change"));
        poll_pending(change.as_mut()).await;
    }
    drop(senders);
    std::fs::write(dir.path().join("release-input"), b"").unwrap();
    client.request("drained", None, 5_000).await.unwrap();
    client
        .did_open("file:///tmp/new.rs", "rust", "accepted-open")
        .await
        .unwrap();
    client
        .did_change("file:///tmp/existing.rs", "accepted-change")
        .await
        .unwrap();
    client.request("barrier", None, 5_000).await.unwrap();
    let records = documents(dir.path());
    client.shutdown().await;
    assert_eq!(
        records.len(),
        3,
        "准入前取消不得发送，也不得吞掉后续 didOpen"
    );
    assert_eq!(records[1]["method"], "textDocument/didOpen");
    assert_eq!(
        records[1]["params"]["textDocument"]["text"],
        "accepted-open"
    );
    assert_eq!(records[2]["method"], "textDocument/didChange");
    assert_eq!(records[2]["params"]["textDocument"]["version"], 2);
    assert_eq!(
        records[2]["params"]["contentChanges"][0]["text"],
        "accepted-change"
    );
}

/// [回归测试] 旧连接等待队列的调用在 restart 后只能结算旧连接，不能污染新缓存。
#[tokio::test]
async fn test_document_sync_old_connection_cannot_publish_into_restarted_cache() {
    let dir = tempfile::tempdir().unwrap();
    let client = make_client(dir.path(), false);
    client.start("file:///tmp").await.unwrap();
    let senders = fill_writer(&client, dir.path()).await;
    let mut old_open = Box::pin(client.did_open("file:///tmp/restarted.rs", "rust", "old-content"));
    poll_pending(old_open.as_mut()).await;
    client.try_restart("file:///tmp").await.unwrap();
    let old_result = old_open.await;
    drop(senders);
    client
        .did_change("file:///tmp/restarted.rs", "new-content")
        .await
        .unwrap();
    client.request("barrier", None, 5_000).await.unwrap();
    let records = documents(dir.path());
    client.shutdown().await;
    assert!(matches!(old_result, Err(LspError::TransportClosed)));
    assert_eq!(records.len(), 1, "重启后的第一次同步应且仅应发送新内容");
    assert_eq!(records[0]["method"], "textDocument/didOpen");
    assert_eq!(records[0]["params"]["textDocument"]["version"], 1);
    assert_eq!(records[0]["params"]["textDocument"]["text"], "new-content");
}

/// [回归测试] 已入队但尚未写完时取消，didOpen 缓存必须保留以免重复发送。
#[tokio::test]
async fn test_document_sync_cancel_after_admission_keeps_the_first_open() {
    let dir = tempfile::tempdir().unwrap();
    let client = make_client(dir.path(), false);
    client.start("file:///tmp").await.unwrap();
    let blocked = block_writer(&client, dir.path()).await;
    {
        let mut open =
            Box::pin(client.did_open("file:///tmp/admitted.rs", "rust", "admitted-content"));
        poll_pending(open.as_mut()).await;
        assert_eq!(
            client
                .ready_dispatcher()
                .unwrap()
                .writer_capacity_for_test(),
            Some(15),
            "didOpen 已入队但 writer 尚未写入"
        );
    }
    drop(blocked);
    std::fs::write(dir.path().join("release-input"), b"").unwrap();
    client.request("drained", None, 5_000).await.unwrap();
    client
        .did_open("file:///tmp/admitted.rs", "rust", "duplicate-content")
        .await
        .unwrap();
    client.request("barrier", None, 5_000).await.unwrap();
    let records = documents(dir.path());
    client.shutdown().await;
    assert_eq!(records.len(), 1, "取消确认等待不应撤销已经发送的 didOpen");
    assert_eq!(
        records[0]["params"]["textDocument"]["text"],
        "admitted-content"
    );
}
