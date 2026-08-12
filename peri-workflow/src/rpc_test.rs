use super::*;

#[test]
fn test_duplicate_active_agent_id_preserves_original_cancel_handle() {
    let pending = DashMap::new();
    let (first_tx, mut first_rx) = oneshot::channel();
    let (duplicate_tx, _duplicate_rx) = oneshot::channel();
    let key = ("run-1".to_string(), 7);

    assert!(insert_pending_agent(
        &pending,
        key.clone(),
        PendingAgent {
            rpc_id: Some(1),
            cancel_tx: first_tx,
            token: 0,
        },
    ));
    assert!(!insert_pending_agent(
        &pending,
        key.clone(),
        PendingAgent {
            rpc_id: Some(2),
            cancel_tx: duplicate_tx,
            token: 1,
        },
    ));

    let (_, original) = pending.remove(&key).unwrap();
    original.cancel_tx.send(()).unwrap();
    assert_eq!(first_rx.try_recv(), Ok(()));
}

/// 构造带真实 stdin 管道的 RpcChannel（perl 长驻子进程；跨平台：Unix 预装
/// perl，Windows 由 Git for Windows 提供）。perl 60s 后自然退出，不留永久孤儿。
async fn make_rpc_channel() -> RpcChannel {
    let mut child = tokio::process::Command::new("perl")
        .args(["-e", "sleep 60"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn perl failed");
    let stdin = child.stdin.take().expect("stdin 应为 piped");
    RpcChannel::new(stdin)
}

/// [回归测试] 旧注册用过期 token 注销时，不得移除同 key 的新实例
/// （此前无条件 remove 会清空新实例的 cancel 句柄 → 后续 kill 漏杀）。
#[tokio::test]
async fn test_deregister_agent_stale_token_preserves_new_instance() {
    let ch = make_rpc_channel().await;

    let (cancel_rx_1, token_1) = ch.register_agent("run-1", 7, Some(1)).unwrap();
    assert!(ch.deregister_agent("run-1", 7, token_1));
    drop(cancel_rx_1); // 模拟第一实例完成

    // 同 key 的新实例注册（同 agentId 重试/新调用）
    let (cancel_rx_2, token_2) = ch.register_agent("run-1", 7, Some(2)).unwrap();
    // 旧实例迟到注销：token 不匹配 → false，且不触碰新实例
    assert!(!ch.deregister_agent("run-1", 7, token_1));
    // kill 句柄仍指向新实例：kill 成功且 cancel 被触发
    assert!(ch.kill_agent("run-1", 7).await);
    assert_eq!(cancel_rx_2.await, Ok(()), "kill 应触发新实例的 cancel");
    // kill 已取走注册：新实例注销返回 false（响应已由 kill 分支发送）
    assert!(!ch.deregister_agent("run-1", 7, token_2));
}

/// [回归测试] kill 后 deregister 返回 false：kill 分支已发 error response，
/// task 不得再发成功响应（防双重 JSON-RPC 响应）。
#[tokio::test]
async fn test_deregister_after_kill_returns_false() {
    let ch = make_rpc_channel().await;

    let (cancel_rx, token) = ch.register_agent("run-1", 8, Some(3)).unwrap();
    assert!(ch.kill_agent("run-1", 8).await);
    assert_eq!(cancel_rx.await, Ok(()), "kill 应触发 cancel");
    assert!(
        !ch.deregister_agent("run-1", 8, token),
        "kill 后注册已被取走，注销应返回 false"
    );
    // 条目已移除：重复 kill 返回 false
    assert!(!ch.kill_agent("run-1", 8).await);
}

#[test]
fn test_parse_message_response() {
    let raw = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
    let msg = parse_message(raw).unwrap();
    match msg {
        ParsedMessage::Response { id, result, .. } => {
            assert_eq!(id, 1);
            assert!(result.is_some());
        }
        _ => panic!("expected Response"),
    }
}

#[test]
fn test_parse_message_request_with_id() {
    let raw = r#"{"jsonrpc":"2.0","id":100,"method":"agent/run","params":{"prompt":"hi"}}"#;
    let msg = parse_message(raw).unwrap();
    match msg {
        ParsedMessage::Request { id, method, .. } => {
            assert_eq!(id, Some(100));
            assert_eq!(method, "agent/run");
        }
        _ => panic!("expected Request"),
    }
}

#[test]
fn test_parse_message_notification_no_id() {
    let raw = r#"{"jsonrpc":"2.0","method":"progress/event","params":{"type":"run_started"}}"#;
    let msg = parse_message(raw).unwrap();
    match msg {
        ParsedMessage::Request { id, method, .. } => {
            assert!(id.is_none());
            assert_eq!(method, "progress/event");
        }
        _ => panic!("expected Request (notification)"),
    }
}

#[test]
fn test_parse_message_invalid_json_returns_none() {
    assert!(parse_message("not json").is_none());
}

#[test]
fn test_parse_message_error_response() {
    let raw = r#"{"jsonrpc":"2.0","id":5,"error":{"code":-32000,"message":"aborted"}}"#;
    let msg = parse_message(raw).unwrap();
    match msg {
        ParsedMessage::Response { id, result, error } => {
            assert_eq!(id, 5);
            assert!(result.is_none());
            assert!(error.is_some());
            assert_eq!(error.unwrap().code, -32000);
        }
        _ => panic!("expected Response"),
    }
}

#[test]
fn test_parse_message_response_null_result() {
    let raw = r#"{"jsonrpc":"2.0","id":3,"result":null}"#;
    let msg = parse_message(raw).unwrap();
    match msg {
        ParsedMessage::Response { id, result, .. } => {
            assert_eq!(id, 3);
            assert!(result.is_some()); // null is still Some(Value::Null)
        }
        _ => panic!("expected Response"),
    }
}

#[test]
fn test_parse_message_no_method_no_result_returns_none() {
    // 既无 method 也无 result/error → 丢弃
    let raw = r#"{"jsonrpc":"2.0","id":7}"#;
    assert!(parse_message(raw).is_none());
}

#[test]
fn test_parse_message_empty_string_returns_none() {
    assert!(parse_message("").is_none());
}
