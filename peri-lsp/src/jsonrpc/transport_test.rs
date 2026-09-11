//! 测试 transport 分发与关闭语义

use std::{collections::HashMap, sync::Arc, time::Duration};

use super::*;

/// 伪 LSP 服务器脚本：发出服务器发起请求 workspace/configuration (id=1)，
/// 然后从 stdin 读客户端响应，校验为 -32601 MethodNotFound（exit 0），否则 exit 1。
/// 用 perl 实现以跨平台（Unix/macOS 预装，Windows 由 Git for Windows 提供；
/// bash 脚本在 Windows Git Bash 下有 CRLF/管道字节语义差异，不可靠）。
const FAKE_SERVER_SCRIPT: &str = r#"binmode STDOUT;
select STDOUT;
$| = 1;
my $body = '{"jsonrpc":"2.0","id":1,"method":"workspace/configuration","params":[]}';
print "Content-Length: " . length($body) . "\r\n\r\n" . $body;
binmode STDIN;
my $h = '';
while (1) {
    my $l = <STDIN>;
    last unless defined $l;
    last if $l =~ /^\r?\n$/;
    $h .= $l;
}
my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
exit 1 unless defined $len;
my $resp = '';
read(STDIN, $resp, $len) == $len or exit 1;
exit($resp =~ /"code"\s*:\s*-32601/ ? 0 : 1);
"#;

#[tokio::test]
async fn test_server_request_unknown_id_receives_method_not_found() {
    // 服务器发起的请求（id 未注册 pending）：必须回 -32601 响应，
    // 而不是静默丢弃——否则服务器同步等待，后续 textDocument 请求排队至超时
    let transport = LspTransport::spawn(
        "perl",
        &["-e".to_string(), FAKE_SERVER_SCRIPT.to_string()],
        &HashMap::new(),
    )
    .expect("启动伪服务器失败");

    let (dispatcher, rx) = MessageDispatcher::new(transport);
    let state = dispatcher.dispatch_state();
    tokio::spawn(async move { run_dispatch_loop(state, rx).await });

    // 伪服务器收到 -32601 响应后自行退出（exit 0），否则 exit 1
    let child = Arc::clone(&dispatcher.child);
    let status = tokio::time::timeout(Duration::from_secs(5), async move {
        child.lock().await.as_mut().unwrap().wait().await
    })
    .await
    .expect("伪服务器未在 5s 内收到 -32601 响应并退出")
    .expect("wait 子进程失败");
    assert!(
        status.success(),
        "伪服务器应收到 -32601 响应（当前为静默丢弃）: {status:?}"
    );

    dispatcher.close().await;
}

#[tokio::test]
async fn test_cancel_request_removes_pending_entry() {
    // 超时/发送失败路径调用 cancel_request 后，pending 不得残留 oneshot sender
    // （此前仅在 transport EOF 时由 reject_all_pending 整体清理）
    let dispatcher = empty_dispatcher();

    // receiver 保持存活，模拟"请求方仍持有 receiver 但已超时放弃"
    let _receiver = dispatcher.register_request(7);
    assert_eq!(dispatcher.dispatch_state().pending_len(), 1);

    dispatcher.cancel_request(7);
    assert_eq!(
        dispatcher.dispatch_state().pending_len(),
        0,
        "cancel_request 应移除 pending 条目"
    );

    // 取消不存在的 id（响应恰好已在途中被 dispatch 移除）应为无副作用 no-op
    dispatcher.cancel_request(7);
}

#[tokio::test]
async fn test_close_kills_child_process() {
    // sleep 伪进程：close() 必须先 kill 子进程再 abort read task，
    // 否则 abort 路径跳过 child.kill()，子进程成为孤儿。
    // Windows 无 sleep 命令，用 PowerShell 的 Start-Sleep 代替。
    #[cfg(unix)]
    let (command, args) = ("sleep", vec!["60".to_string()]);
    #[cfg(windows)]
    let (command, args) = (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "Start-Sleep -Seconds 60".to_string(),
        ],
    );
    let transport = LspTransport::spawn(command, &args, &HashMap::new()).expect("启动失败");
    let (dispatcher, _rx) = MessageDispatcher::new(transport);

    dispatcher.close().await;

    let child = Arc::clone(&dispatcher.child);
    let status = tokio::time::timeout(Duration::from_secs(5), async move {
        child.lock().await.as_mut().unwrap().wait().await
    })
    .await
    .expect("close() 后子进程未在 5s 内退出（孤儿进程）")
    .expect("wait 子进程失败");
    // Unix 上 kill 以信号终止（code() 为 None）；Windows 上 TerminateProcess
    // 的退出码非 0——统一断言"非正常退出"以区分自然结束
    assert!(
        !status.success(),
        "子进程应被 close() 的 kill 终止，而非自然退出: {status:?}"
    );
}

fn disconnected_state() -> DispatchState {
    DispatchState {
        admission: Mutex::new(Admission {
            closed: false,
            pending: HashMap::new(),
            writer: None,
        }),
        notification_handlers: Mutex::new(HashMap::new()),
        on_error: Mutex::new(None),
    }
}

#[tokio::test]
async fn server_request_id_collision_keeps_client_response_pending() {
    let state = disconnected_state();
    let mut rx = state.register_request(7, Arc::new(()));
    state.dispatch(serde_json::json!({"jsonrpc":"2.0","id":7,"method":"workspace/configuration","params":[]}).to_string()).await;
    assert_eq!(
        state.pending_len(),
        1,
        "双向请求 ID 独立，服务器请求不得消费客户端 pending"
    );
    assert!(matches!(
        rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    state
        .dispatch(serde_json::json!({"jsonrpc":"2.0","id":7,"result":{"ready":true}}).to_string())
        .await;
    assert_eq!(
        rx.await.unwrap().unwrap(),
        serde_json::json!({"ready":true})
    );
}

#[tokio::test]
async fn id_without_response_payload_keeps_request_pending() {
    let state = disconnected_state();
    let rx = state.register_request(9, Arc::new(()));
    state
        .dispatch(serde_json::json!({"jsonrpc":"2.0","id":9}).to_string())
        .await;
    assert_eq!(state.pending_len(), 1, "畸形响应不能伪装成成功的 null 结果");
    state
        .dispatch(serde_json::json!({"jsonrpc":"2.0","id":9,"result":null}).to_string())
        .await;
    assert_eq!(rx.await.unwrap().unwrap(), Value::Null);
}

#[tokio::test]
async fn close_rejects_pending_without_an_external_dispatch_loop() {
    let dispatcher = empty_dispatcher();
    let rx = dispatcher.register_request(11);
    dispatcher.close().await;
    let error = tokio::time::timeout(Duration::from_millis(100), rx)
        .await
        .expect("close必须自行释放pending，而非依赖外部消费者")
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, LspError::RequestFailed { .. }));
    assert_eq!(dispatcher.dispatch_state().pending_len(), 0);
}

fn empty_dispatcher() -> MessageDispatcher {
    MessageDispatcher {
        dispatch_state: Arc::new(disconnected_state()),
        writer_task: Mutex::new(None),
        read_task: Mutex::new(None),
        stderr_task: Mutex::new(None),
        dispatch_task: Mutex::new(None),
        close_lock: tokio::sync::Mutex::new(()),
        child: Arc::new(tokio::sync::Mutex::new(None)),
    }
}

fn dispatcher_with_writer(
    stdin: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
) -> MessageDispatcher {
    let dispatcher = empty_dispatcher();
    let (sender, receiver) = mpsc::channel(writer::FRAME_QUEUE_CAPACITY);
    dispatcher.dispatch_state.admission.lock().writer = Some(sender);
    *dispatcher.writer_task.lock() = Some(tokio::spawn(writer::run(
        stdin,
        receiver,
        Arc::downgrade(&dispatcher.dispatch_state),
    )));
    dispatcher
}

#[tokio::test]
async fn owned_request_drop_cancels_only_its_original_registration() {
    let old = empty_dispatcher();
    let new = empty_dispatcher();
    let (old_guard, old_receiver) = old.register_owned_request(1);
    let (_new_guard, mut new_receiver) = new.register_owned_request(1);
    drop(old_guard);
    assert!(old_receiver.await.is_err());
    assert_eq!(old.dispatch_state.pending_len(), 0);
    assert_eq!(new.dispatch_state.pending_len(), 1);
    assert!(matches!(
        new_receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn old_guard_cannot_remove_a_reused_id_or_keep_dispatcher_state_alive() {
    let dispatcher = empty_dispatcher();
    let (old_guard, _old_receiver) = dispatcher.register_owned_request(1);
    let (new_guard, _new_receiver) = dispatcher.register_owned_request(1);
    drop(old_guard);
    assert_eq!(dispatcher.dispatch_state.pending_len(), 1);
    let weak = Arc::downgrade(&dispatcher.dispatch_state);
    drop(dispatcher);
    assert!(
        weak.upgrade().is_none(),
        "request guard must not own dispatcher state or child"
    );
    drop(new_guard);
}

#[tokio::test]
async fn close_rejects_new_owned_and_legacy_registrations() {
    let dispatcher = empty_dispatcher();
    dispatcher.close().await;
    let (_guard, receiver) = dispatcher.register_owned_request(1);
    assert!(matches!(
        receiver.await.unwrap(),
        Err(LspError::TransportClosed)
    ));
    assert!(matches!(
        dispatcher.register_request(2).await.unwrap(),
        Err(LspError::TransportClosed)
    ));
    assert_eq!(dispatcher.dispatch_state.pending_len(), 0);
}

/// [回归测试] 已取得容量不代表已获准发送，close 后旧 permit 必须拒绝入队。
#[tokio::test]
async fn reserved_notification_cannot_be_admitted_after_close() {
    let (stdin, server) = tokio::io::duplex(8);
    let dispatcher = dispatcher_with_writer(stdin);
    let permit = dispatcher.reserve_notification().await.unwrap();
    dispatcher.close().await;
    let result = permit.enqueue(&JsonRpcNotification::new("after-close", None));
    assert!(matches!(result, Err(LspError::TransportClosed)));
    let mut reader = BufReader::new(server);
    assert!(codec::decode_message(&mut reader).await.unwrap().is_none());
}

#[tokio::test]
async fn cancelled_sender_finishes_its_frame_before_the_next_frame() {
    use tokio::io::AsyncReadExt;
    let (stdin, mut server) = tokio::io::duplex(8);
    let dispatcher = Arc::new(dispatcher_with_writer(stdin));
    let request = JsonRpcRequest::new(
        1,
        "first",
        Some(serde_json::json!({"text": "x".repeat(4096)})),
    );
    let expected = serde_json::to_value(&request).unwrap();
    let sender = {
        let dispatcher = Arc::clone(&dispatcher);
        tokio::spawn(async move { dispatcher.send_request(&request).await })
    };
    // Receiving the first header byte proves the writer has started a frame;
    // the tiny pipe makes actual write completion impossible without more reads.
    let mut prefix = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(2), server.read_exact(&mut prefix))
        .await
        .unwrap()
        .unwrap();
    assert!(
        !sender.is_finished(),
        "send success must wait for actual write completion"
    );
    sender.abort();
    let _ = sender.await;
    let next = {
        let dispatcher = Arc::clone(&dispatcher);
        tokio::spawn(async move {
            dispatcher
                .send_notification(&JsonRpcNotification::new("second", None))
                .await
        })
    };
    let mut reader = BufReader::new(prefix.as_slice().chain(server));
    let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
        let first = codec::decode_message(&mut reader).await.unwrap().unwrap();
        let second = codec::decode_message(&mut reader).await.unwrap().unwrap();
        (first, second)
    })
    .await
    .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&first).unwrap(), expected);
    assert_eq!(
        serde_json::from_str::<Value>(&second).unwrap()["method"],
        "second"
    );
    next.await.unwrap().unwrap();
    dispatcher.close().await;
    assert!(dispatcher.writer_task.lock().is_none());
}

#[tokio::test]
async fn close_joins_a_blocked_writer_and_settles_its_sender() {
    use tokio::io::AsyncReadExt;
    let (stdin, mut server) = tokio::io::duplex(8);
    let dispatcher = Arc::new(dispatcher_with_writer(stdin));
    let pending = dispatcher.register_request(1);
    let sender = {
        let dispatcher = Arc::clone(&dispatcher);
        tokio::spawn(async move {
            dispatcher
                .send_notification(&JsonRpcNotification::new(
                    "blocked",
                    Some(serde_json::json!({"text": "x".repeat(4096)})),
                ))
                .await
        })
    };
    let mut byte = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(2), server.read_exact(&mut byte))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), dispatcher.close())
        .await
        .expect("close must not await a blocked stdin write lock");
    assert!(matches!(
        sender.await.unwrap(),
        Err(LspError::TransportClosed)
    ));
    assert!(pending.await.unwrap().is_err());
    assert!(dispatcher.writer_task.lock().is_none());
    assert_eq!(dispatcher.dispatch_state.pending_len(), 0);
    dispatcher.close().await;
}

#[tokio::test]
async fn writer_failure_rejects_pending_and_reports_terminal_error_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (stdin, server) = tokio::io::duplex(8);
    drop(server);
    let dispatcher = dispatcher_with_writer(stdin);
    let pending = dispatcher.register_request(1);
    let calls = Arc::new(AtomicUsize::new(0));
    let on_error_calls = Arc::clone(&calls);
    dispatcher.set_on_error(Box::new(move |_| {
        on_error_calls.fetch_add(1, Ordering::SeqCst);
    }));
    let error = dispatcher
        .send_notification(&JsonRpcNotification::new("broken", None))
        .await
        .unwrap_err();
    assert!(
        matches!(error, LspError::Io(_)),
        "the initiating send retains its IO error"
    );
    assert!(pending.await.unwrap().is_err());
    assert_eq!(dispatcher.dispatch_state.pending_len(), 0);
    assert!(matches!(
        dispatcher
            .send_notification(&JsonRpcNotification::new("later", None))
            .await,
        Err(LspError::TransportClosed)
    ));
    let (tx, rx) = mpsc::unbounded_channel();
    drop(tx);
    run_dispatch_loop(dispatcher.dispatch_state(), rx).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "later EOF must not duplicate the writer error callback"
    );
    dispatcher.close().await;
}

#[tokio::test]
async fn close_reclaims_child_while_its_stdin_body_write_is_blocked() {
    // Read only the header, announce that barrier on stdout, then stop reading.
    // A body much larger than the OS pipe proves close cannot depend on stdin's
    // write lock becoming available. This uses the same perl dependency as the
    // existing bidirectional JSON-RPC test.
    let script = r#"binmode STDIN; binmode STDOUT; $| = 1;
while (defined(my $line = <STDIN>)) { last if $line =~ /^\r?\n$/; }
my $body = '{"jsonrpc":"2.0","method":"header-read"}';
print "Content-Length: " . length($body) . "\r\n\r\n" . $body;
sleep 60;
"#;
    let transport =
        LspTransport::spawn("perl", &["-e".into(), script.into()], &HashMap::new()).unwrap();
    let (dispatcher, mut incoming) = MessageDispatcher::new(transport);
    let dispatcher = Arc::new(dispatcher);
    let send = {
        let dispatcher = Arc::clone(&dispatcher);
        tokio::spawn(async move {
            dispatcher
                .send_notification(&JsonRpcNotification::new(
                    "large",
                    Some(serde_json::json!({"body": "x".repeat(4 * 1024 * 1024)})),
                ))
                .await
        })
    };
    let observed = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .expect("server must observe the frame header")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&observed).unwrap()["method"],
        "header-read"
    );
    assert!(!send.is_finished());
    tokio::time::timeout(Duration::from_secs(5), dispatcher.close())
        .await
        .expect("blocked body write must not hold close hostage");
    assert!(matches!(
        send.await.unwrap(),
        Err(LspError::TransportClosed)
    ));
    assert!(dispatcher.writer_task.lock().is_none());
    assert!(dispatcher.read_task.lock().is_none());
    assert!(dispatcher.stderr_task.lock().is_none());
    assert!(dispatcher
        .child
        .lock()
        .await
        .as_mut()
        .unwrap()
        .try_wait()
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn close_settles_every_admitted_frame_in_a_full_writer_queue() {
    use tokio::io::AsyncReadExt;
    let (stdin, mut server) = tokio::io::duplex(1);
    let dispatcher = dispatcher_with_writer(stdin);
    let sender = dispatcher
        .dispatch_state
        .admission
        .lock()
        .writer
        .clone()
        .unwrap();
    let mut acks = Vec::new();
    let (ack, receiver) = oneshot::channel();
    sender
        .send(writer::Frame {
            body: "first".into(),
            ack,
        })
        .await
        .unwrap();
    acks.push(receiver);
    let mut byte = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(2), server.read_exact(&mut byte))
        .await
        .unwrap()
        .unwrap();
    for _ in 0..writer::FRAME_QUEUE_CAPACITY {
        let (ack, receiver) = oneshot::channel();
        assert!(sender
            .try_send(writer::Frame {
                body: "queued".into(),
                ack
            })
            .is_ok());
        acks.push(receiver);
    }
    assert_eq!(sender.capacity(), 0);
    tokio::time::timeout(Duration::from_secs(2), dispatcher.close())
        .await
        .unwrap();
    for receiver in acks {
        assert!(
            receiver.await.is_err(),
            "each abandoned frame must settle its send waiter"
        );
    }
    assert!(sender.is_closed());
}

#[tokio::test]
async fn begin_close_retains_join_ownership_for_later_close() {
    let (stdin, _server) = tokio::io::duplex(8);
    let dispatcher = Arc::new(dispatcher_with_writer(stdin));
    let another_owner = Arc::clone(&dispatcher);
    let pending = dispatcher.register_request(1);
    dispatcher.begin_close();
    assert!(
        dispatcher.writer_task.lock().is_some(),
        "begin_close must retain the join handle"
    );
    assert!(pending.await.unwrap().is_err());
    assert!(matches!(
        dispatcher.register_request(2).await.unwrap(),
        Err(LspError::TransportClosed)
    ));
    drop(dispatcher);
    another_owner.close().await;
    assert!(another_owner.writer_task.lock().is_none());
}
