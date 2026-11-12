//! Tests for thread_load_consumer

#[cfg(test)]
use super::*;
use peri_acp::transport::{
    AcpTransport,
    mpsc::{MpscClientTransport, MpscServerTransport, mpsc_transport_pair},
    types::IncomingMessage,
};
use serial_test::serial;
use std::time::Duration;

/// Global atoms can leak across parallel lib tests; thread switch must not see
/// stale background tasks (consumer would open confirm and skip session/load).
fn clear_thread_load_isolation_atoms() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::BG_TASKS.state().write().clear();
    *crate::kit::atoms::CONFIRM_PAYLOAD.state().write() = None;
}

async fn create_current_session(client: &AcpTuiClient, server: &MpscServerTransport) {
    let opener = client.clone();
    let opened = tokio::spawn(async move { opener.new_session("/tmp", None).await });
    let message = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingMessage::Request { id, method, .. } = message else {
        panic!("expected session/new")
    };
    assert_eq!(method, "session/new");
    server
        .send_response(id, Ok(serde_json::json!({"sessionId": "current"})))
        .await
        .unwrap();
    assert_eq!(opened.await.unwrap().unwrap(), "current");
}

fn running_task() -> atoms::BgTaskEntry {
    atoms::BgTaskEntry {
        task_id: "running-shell".into(),
        kind: "shell".into(),
        summary: "long command".into(),
        started_at: String::new(),
        pid: None,
        revision: None,
        status: None,
    }
}

async fn wait_for_switch_confirmation() {
    tokio::time::timeout(Duration::from_secs(5), async {
        while atoms::CONFIRM_PAYLOAD.state().read().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("后台任务切换应请求确认");
}

#[tokio::test]
#[serial]
async fn test_confirmed_history_switch_loads_once_with_running_tasks() {
    clear_thread_load_isolation_atoms();
    let (client, server) = make_client_without_pump();
    create_current_session(&client, &server).await;
    atoms::BG_TASKS.set(vec![running_task()]);
    let (tx, rx) = mpsc::unbounded_channel();
    let dispatcher = ThreadLoadDispatcher::new(tx, client.clone());
    let shutdown = CancellationToken::new();
    let consumer = spawn_thread_load_consumer(client.clone(), rx, "/tmp".into(), shutdown.clone());
    dispatcher.send("target".into()).unwrap();
    wait_for_switch_confirmation().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server.recv())
            .await
            .is_err(),
        "未确认不得关闭原会话"
    );
    let action = atoms::CONFIRM_PAYLOAD
        .state()
        .read()
        .as_ref()
        .unwrap()
        .pending_action
        .clone();
    let atoms::ConfirmAction::ThreadSwitch {
        target_id,
        source_identity,
    } = action
    else {
        panic!("expected thread switch")
    };
    atoms::CONFIRM_PAYLOAD.set(None);
    dispatcher
        .send_confirmed(target_id, source_identity)
        .unwrap();
    let request = recv_session_load(&server, Duration::from_secs(5)).await;
    let IncomingMessage::Request { id, params, .. } = request else {
        panic!("expected session/load")
    };
    assert_eq!(params["sessionId"], "target");
    server
        .send_response(id, Ok(serde_json::json!({})))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.pending_session_load_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(client.current_session_id().as_deref(), Some("target"));
    assert!(
        atoms::CONFIRM_PAYLOAD.state().read().is_none(),
        "确认后不得再次弹相同确认"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server.recv())
            .await
            .is_err()
    );
    shutdown.cancel();
    consumer.await.unwrap();
    client.close();
    clear_thread_load_isolation_atoms();
}

#[tokio::test]
#[serial]
async fn test_confirmation_for_previous_session_does_not_authorize_new_session() {
    clear_thread_load_isolation_atoms();
    let (client, server) = make_client_without_pump();
    create_current_session(&client, &server).await;
    let (tx, rx) = mpsc::unbounded_channel();
    let dispatcher = ThreadLoadDispatcher::new(tx, client.clone());
    let seen_source = client.stable_session_identity();
    let switcher = client.clone();
    let switched =
        tokio::spawn(async move { switcher.load_session("another-session", "/tmp", None).await });
    let request = recv_session_load(&server, Duration::from_secs(5)).await;
    let IncomingMessage::Request { id, .. } = request else {
        panic!("expected session/load")
    };
    server
        .send_response(id, Ok(serde_json::json!({})))
        .await
        .unwrap();
    switched.await.unwrap().unwrap();
    // 旧弹窗被点击时已经是新会话：不能把点击时的身份当作用户已确认的来源。
    dispatcher
        .send_confirmed("target".into(), seen_source)
        .unwrap();
    atoms::BG_TASKS.set(vec![running_task()]);
    let shutdown = CancellationToken::new();
    let consumer = spawn_thread_load_consumer(client.clone(), rx, "/tmp".into(), shutdown.clone());
    wait_for_switch_confirmation().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server.recv())
            .await
            .is_err()
    );
    assert_eq!(
        client.current_session_id().as_deref(),
        Some("another-session")
    );
    shutdown.cancel();
    consumer.await.unwrap();
    client.close();
    clear_thread_load_isolation_atoms();
}

fn session_boundary_reset_observed() -> bool {
    crate::kit::atoms::POPUP_KIND.state().read().is_none()
        && crate::kit::atoms::HITL_PENDING.state().read().is_none()
        && crate::kit::atoms::TODO_ITEMS.state().read().is_empty()
        && crate::kit::atoms::INPUT_HISTORY_INDEX
            .state()
            .read()
            .is_none()
        && crate::kit::atoms::DRAFT.state().read().is_none()
}

async fn wait_for_session_boundary_reset(timeout: Duration) {
    tokio::time::timeout(timeout, async {
        while !session_boundary_reset_observed() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session boundary projection should clear popup/todo/history atoms");
}

async fn recv_session_load(server: &MpscServerTransport, timeout: Duration) -> IncomingMessage {
    tokio::time::timeout(timeout, async {
        loop {
            let Some(message) = server.recv().await else {
                panic!("server transport closed before session/load");
            };
            match &message {
                IncomingMessage::Request { method, .. } if method == "session/load" => {
                    return message;
                }
                IncomingMessage::Request { id, method, .. } if method == "session/close" => {
                    let _ = server
                        .send_response(id.clone(), Ok(serde_json::json!({})))
                        .await;
                }
                _ => return message,
            }
        }
    })
    .await
    .expect("session/load should become in-flight")
}

fn make_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
    let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
        mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    (client, server_transport)
}

fn make_interactive_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
    let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
        mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) =
        AcpTuiClient::new_interactive(client_transport);
    (client, server_transport)
}

#[tokio::test]
async fn test_empty_thread_id_skipped() {
    let (client, _server) = make_client_without_pump();
    let reservation = client.reserve_session_load();
    let result = handle_load(&client, ".", "   ".to_string(), &reservation).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_shutdown_exits_loop() {
    let (client, _server) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<ThreadLoadRequest>();
    let shutdown = CancellationToken::new();
    let handle = spawn_thread_load_consumer(client, rx, ".".into(), shutdown.clone());
    shutdown.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    let _ = tx;
}

#[tokio::test]
async fn test_dropped_tx_exits_loop() {
    let (client, _server) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<ThreadLoadRequest>();
    let shutdown = CancellationToken::new();
    let handle = spawn_thread_load_consumer(client, rx, ".".into(), shutdown);
    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[tokio::test]
#[serial]
async fn test_shutdown_cancels_inflight_load_and_releases_reservation() {
    clear_thread_load_isolation_atoms();
    let (client, server) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<ThreadLoadRequest>();
    let dispatcher = ThreadLoadDispatcher::new(tx, client.clone());
    let shutdown = CancellationToken::new();
    let handle = spawn_thread_load_consumer(client.clone(), rx, ".".into(), shutdown.clone());
    tokio::task::yield_now().await;
    dispatcher.send("target".into()).unwrap();

    let request = recv_session_load(&server, Duration::from_secs(5)).await;
    assert!(matches!(
        request,
        IncomingMessage::Request { ref method, .. } if method == "session/load"
    ));
    assert_eq!(client.pending_session_load_count(), 1);

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("consumer must stop while session/load is in-flight")
        .unwrap();
    assert_eq!(
        client.pending_session_load_count(),
        0,
        "shutdown must drop the in-flight reservation"
    );
}

/// 验证 handle_load 在调用 load_session 前已同步重置弹窗 / Todo / 历史指针。
///
/// 覆盖 H1/M3/L5/L10，与 submit_consumer::test_handle_clear_submit_*
/// 对称。load_session 无 server 会 hang；在后台 spawn handle_load 并轮询 atom，
/// 直到 client 在 load_session 内完成 project_session_boundary（发生在首个
/// 阻塞 RPC 之前），避免 CI 上 100ms 固定超时在 gate 排队时过早断言。
#[tokio::test]
#[serial]
async fn test_handle_load_resets_popup_todo_history_atoms() {
    clear_thread_load_isolation_atoms();
    // Arrange：把 4 类 atom 填充为"旧 session 残留"。
    *crate::kit::atoms::POPUP_KIND.state().write() = Some(crate::kit::atoms::PopupKind::Hitl);
    *crate::kit::atoms::HITL_PENDING.state().write() =
        Some(crate::kit::acp_types::PendingInteraction {
            owner: Default::default(),
            request_id_json: "\"old\"".into(),
            payload: peri_acp_types::event_data::HitlPending {
                tool_name: "old".into(),
                tool_input: serde_json::Value::Null,
                batch: None,
            },
        });
    *crate::kit::atoms::TODO_ITEMS.state().write() = vec![crate::kit::message_area::TodoItem {
        content: "stale".into(),
        status: crate::kit::message_area::TodoStatus::Pending,
    }];
    *crate::kit::atoms::INPUT_HISTORY_INDEX.state().write() = Some(3);
    *crate::kit::atoms::DRAFT.state().write() = Some("stale draft".to_string());

    let (client, _server) = make_interactive_client_without_pump();
    let cwd = ".".to_string();

    // Act：load_session 会在 session/load 上挂起；轮询直到 boundary 投影完成。
    let reservation = client.reserve_session_load();
    let load_task = {
        let client = client.clone();
        let cwd = cwd.clone();
        tokio::spawn(async move {
            handle_load(&client, &cwd, "thread-xyz".to_string(), &reservation).await
        })
    };
    wait_for_session_boundary_reset(Duration::from_secs(5)).await;
    load_task.abort();
    let _ = load_task.await;

    // Assert：4 类残留已清空。
    assert!(
        crate::kit::atoms::POPUP_KIND.state().read().is_none(),
        "POPUP_KIND should be None after thread switch"
    );
    assert!(
        crate::kit::atoms::HITL_PENDING.state().read().is_none(),
        "HITL_PENDING should be cleared after thread switch"
    );
    assert!(
        crate::kit::atoms::TODO_ITEMS.state().read().is_empty(),
        "TODO_ITEMS should be empty after thread switch"
    );
    assert!(
        crate::kit::atoms::INPUT_HISTORY_INDEX
            .state()
            .read()
            .is_none(),
        "INPUT_HISTORY_INDEX should be None after thread switch"
    );
    assert!(
        crate::kit::atoms::DRAFT.state().read().is_none(),
        "DRAFT should be None after thread switch"
    );
}
