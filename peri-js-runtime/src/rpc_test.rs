use std::sync::Arc;

use super::*;

#[test]
fn rpc_failure_debug_and_display_preserve_payload() {
    let error = JsonRpcError {
        code: -32000,
        message: "JavaScript token=fixture failed".into(),
        data: Some(serde_json::json!({"cause": "cwd=/fixture\nTLS reset"})),
    };
    let rendered = crate::JsRuntimeError::RpcResponse(error).to_string();
    assert!(rendered.contains("token=fixture"));
    assert!(rendered.contains("cwd=/fixture"));
    assert!(rendered.contains("TLS reset"));
}

async fn make_channel() -> (RpcChannel, tokio::process::Child) {
    let mut child = tokio::process::Command::new("node")
        .args(["-e", "setTimeout(() => {}, 60_000);"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn node failed");
    let channel = RpcChannel::new(
        child.stdin.take().expect("stdin 应为 piped"),
        4 * 1024 * 1024,
    );
    (channel, child)
}

#[tokio::test]
async fn test_notification_writes_newline_and_flushes_frame() {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut child = tokio::process::Command::new("node")
        .args([
            "-e",
            "process.stdout.write('ready\\n'); process.stdin.once('data', data => process.stdout.write(data));",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn node failed");
    let channel = RpcChannel::new(
        child.stdin.take().expect("stdin 应为 piped"),
        4 * 1024 * 1024,
    );
    let stdout = child.stdout.take().expect("stdout 应为 piped");
    let mut reader = BufReader::new(stdout);

    // 就绪信号与 echo 分开计时：解释器冷启动（Windows CI 高负载下可达数秒）不
    // 属于「写入即 flush」的语义，却会吃掉同一份预算。行为断言仍是严格的 1s。
    let mut ready = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(120),
        reader.read_line(&mut ready),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "node 应在启动预算内就绪: {error}; child status: {:?}",
            child.try_wait()
        )
    })
    .unwrap();
    assert_eq!(ready, "ready\n");

    channel
        .send_notification("test/event", serde_json::json!({"value": 1}))
        .await
        .unwrap();

    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        reader.read_line(&mut line),
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
    child.kill().await.unwrap();
}

#[tokio::test]
async fn test_drain_pending_settles_waiting_request_with_reason() {
    let (channel, mut child) = make_channel().await;
    let channel = Arc::new(channel);
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
    assert!(matches!(error, crate::JsRuntimeError::RpcResponse(_)));
    assert!(channel.pending_requests.is_empty());
    child.kill().await.unwrap();
}
