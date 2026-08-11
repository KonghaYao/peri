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
    let dispatcher = MessageDispatcher {
        dispatch_state: Arc::new(DispatchState {
            pending: Mutex::new(HashMap::new()),
            notification_handlers: Mutex::new(HashMap::new()),
            on_error: Mutex::new(None),
            stdin: tokio::sync::Mutex::new(None),
        }),
        read_task: Mutex::new(None),
        child: Arc::new(tokio::sync::Mutex::new(None)),
    };

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
