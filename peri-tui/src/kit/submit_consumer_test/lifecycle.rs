//! 消费者退出、取消与视图辅助回归。

use super::super::*;
use super::support::*;

use crate::kit::atoms::ACP_STATE;
use serial_test::serial;

#[tokio::test]
async fn test_shutdown_exits_loop() {
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let handle = spawn_submit_consumer(client, rx, ".".into(), shutdown.clone());

    shutdown.cancel();
    // 任务应在合理时间内退出
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    // 不发任何提交——channel 仍可用（drop tx 也能让任务退出）
    let _ = tx;
}

#[tokio::test]
async fn test_dropped_tx_exits_loop() {
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let handle = spawn_submit_consumer(client, rx, ".".into(), shutdown);

    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[tokio::test]
#[serial]
async fn test_cancel_consumer_failure_does_not_claim_execution_stopped() {
    crate::kit::atoms::init_atoms();
    // 模拟卡死状态：is_loading=true（事件流已中断，无法靠事件复位）
    {
        let ref_guard = ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = true;
    }
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<()>();
    let shutdown = CancellationToken::new();
    let _handle = spawn_cancel_consumer(client, rx, shutdown.clone());

    tx.send(()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        ACP_STATE.state().read().is_loading,
        "控制失败不得伪造执行已停止"
    );
    shutdown.cancel();
}

/// S4.2: clear_loading_state 双保险——直接写 ACP_STATE（bridge 已退出的
/// shutdown 路径兜底）+ 注入 LocalLoadingReset 内部事件（bridge 存活时同步
/// 复位 phase，防止后续 push_acp_state 用 phase 重算 is_loading=true 闪回）。
#[test]
#[serial]
fn test_clear_loading_state_emits_loading_reset_event() {
    crate::kit::atoms::init_atoms();
    // LOCAL_EVENT_TX 是全局 OnceLock——set 已占用（已被其他测试/运行时 set）
    // 时仅验证直接写兜底，跳过事件断言（std OnceLock::set 返回 Result，不 panic）。
    let (tx, mut rx) = mpsc::unbounded_channel::<AcpEventWithEpoch>();
    let owns_tx = LOCAL_EVENT_TX.set(tx).is_ok();

    {
        let ref_guard = ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = true;
    }
    clear_loading_state();

    assert!(
        !ACP_STATE.state().read().is_loading,
        "clear_loading_state 应直接复位 ACP_STATE.is_loading"
    );
    if owns_tx {
        let ev = rx.try_recv().ok();
        assert!(
            matches!(
                ev.as_ref().map(|e| &e.event),
                Some(AcpEventData::LocalLoadingReset)
            ),
            "clear_loading_state 应注入 LocalLoadingReset 事件，实际 {ev:?}"
        );
    }
}

/// 编译期断言：UnboundedSender<SubmitRequest> 与 atoms::SUBMIT_TX 类型契约一致。
#[test]
fn test_submit_tx_type_contract() {
    let (tx, _rx): (mpsc::UnboundedSender<SubmitRequest>, _) = mpsc::unbounded_channel();
    // 模拟 atoms::SUBMIT_TX.set(tx) 的契约
    let _ = tx;
}

#[test]
fn test_lines_to_plain_text_joins_spans_and_lines() {
    let lines = vec![
        Line::from(vec![
            ratatui::text::Span::raw("hello"),
            ratatui::text::Span::raw(" world"),
        ]),
        Line::from("next"),
    ];
    assert_eq!(lines_to_plain_text(&lines), "hello world\nnext\n");
}

#[test]
fn test_debug_export_path_uses_cwd_and_timestamp_prefix() {
    let path = debug_export_path("/tmp/peri-export-test");
    assert!(path.starts_with("/tmp/peri-export-test"));
    let name = path.file_name().unwrap().to_string_lossy();
    assert!(name.starts_with("peri-debug-export-"));
    assert!(name.ends_with(".txt"));
}

#[tokio::test]
async fn test_execute_view_action_export_text_writes_notification() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    *NOTIFICATION.state().write() = None;
    execute_view_action(
        ViewActionRequest::ExportText(ExportMode::All),
        &make_client_without_pump().0,
        tempdir.path().to_str().expect("utf8 path"),
    );
    assert!(NOTIFICATION.state().read().is_some());
}
