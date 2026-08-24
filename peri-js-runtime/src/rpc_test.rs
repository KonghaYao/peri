use std::sync::Arc;

use super::*;

async fn make_channel() -> RpcChannel {
    let mut child = tokio::process::Command::new("node")
        .args(["-e", "setTimeout(() => {}, 60_000);"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn perl failed");
    RpcChannel::new(
        child.stdin.take().expect("stdin 应为 piped"),
        4 * 1024 * 1024,
    )
}

#[tokio::test]
async fn test_notification_writes_newline_and_flushes_frame() {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut child = tokio::process::Command::new("node")
        .args([
            "-e",
            "process.stdin.once('data', data => process.stdout.write(data));",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn perl failed");
    let channel = RpcChannel::new(
        child.stdin.take().expect("stdin 应为 piped"),
        4 * 1024 * 1024,
    );
    let stdout = child.stdout.take().expect("stdout 应为 piped");

    channel
        .send_notification("test/event", serde_json::json!({"value": 1}))
        .await
        .unwrap();

    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        BufReader::new(stdout).read_line(&mut line),
    )
    .await
    .expect("完整 NDJSON frame 应被及时 flush")
    .unwrap();
    assert!(line.ends_with('\n'));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "test/event",
            "params": {"value": 1}
        })
    );
}

#[tokio::test]
async fn test_drain_pending_settles_waiting_request_with_reason() {
    let channel = Arc::new(make_channel().await);
    let request_channel = Arc::clone(&channel);
    let request = tokio::spawn(async move {
        request_channel
            .send_request("test/start", serde_json::json!({}))
            .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while channel.pending_requests.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("request 应先登记 pending");
    channel.drain_pending("process exited");

    let error = request.await.unwrap().unwrap_err();
    assert_eq!(error.code(), "PROTOCOL_ERROR");
    assert!(channel.pending_requests.is_empty());
}
