//! FIX-04 wire 回归：真实 `ServerLoop` + 可注入后端屏障。
//!
//! 覆盖慢读期间 `session/cancel` 仍被处理、窗口 close 先于迟到页结果返回，以及
//! 在途配额饱和时立即拒绝而不阻塞接收循环。用例名以 `server_loop_history_` 开头。

use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::history_v2::{
    DecimalU64, HistoryErrorData, HistoryErrorReason, HistoryLimits, HistoryRecovery,
    HistoryServerCapability, HistoryV2Backend, HistoryWindowPage, HISTORY_CHANGED_NOTIFICATION,
    HISTORY_METHOD,
};
use peri_acp_types::PeriCaps;

use super::*;
use crate::host::history_service::backend_test::{
    body_params, close_params, opaque_page, open_params, page_data, page_params, snap, tail_ref,
    FakeBackend,
};
use crate::host::history_service::HISTORY_MAX_IN_FLIGHT;

fn history_v2_caps() -> PeriCaps {
    PeriCaps {
        history_v2: Some(HistoryServerCapability {
            version: 2,
            selected_profile: "windowed".to_owned(),
            method: HISTORY_METHOD.to_owned(),
            notification: HISTORY_CHANGED_NOTIFICATION.to_owned(),
            limits: HistoryLimits::default(),
        }),
        ..Default::default()
    }
}

/// 起一个真实 `ServerLoop`，并把连接级历史后端替换为可控 fake。
///
/// 同时返回连接取消令牌与服务句柄：断连用例需要主动取消并检查窗口/配额。
async fn spawn_loop_with_backend(
    cfg: Arc<AcpServerConfig>,
    sessions: crate::host::SharedSessions,
    backend: Arc<dyn HistoryV2Backend>,
) -> (
    Arc<crate::transport::mpsc::MpscClientTransport>,
    CancellationToken,
    Arc<tokio::sync::Mutex<crate::host::connection::ConnectionContext>>,
    tokio::task::JoinHandle<()>,
) {
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let server: Arc<dyn crate::transport::AcpTransport> = Arc::new(server);
    let client = Arc::new(client);
    let cancellation = CancellationToken::new();
    let exposed_cancellation = cancellation.clone();
    let mut context = crate::host::connection::ConnectionContext::new(false);
    context.commit_initialize(history_v2_caps());
    context.set_history_v2_backend_override(backend);
    let connection = Arc::new(tokio::sync::Mutex::new(context));
    let exposed_connection = Arc::clone(&connection);
    let task = tokio::spawn(async move {
        let locks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
        let cont_tx = Arc::new(cont_tx);
        crate::host::server_loop::ServerLoop {
            transport: &server,
            cfg: &cfg,
            sessions: &sessions,
            prompt_locks: &locks,
            cont_tx: &cont_tx,
            connection: &connection,
            connection_cancellation: &cancellation,
        }
        .run()
        .await;
    });
    (client, exposed_cancellation, exposed_connection, task)
}

async fn open_window(
    client: &crate::transport::mpsc::MpscClientTransport,
    sid: &str,
    fake: &FakeBackend,
    key: u64,
) -> HistoryWindowPage {
    fake.set_open_result(Ok(snap_with_opaque()));
    let value = client
        .send_request(HISTORY_METHOD, open_params(key, sid, 50, 0))
        .await
        .expect("open must succeed");
    serde_json::from_value(value).expect("window page")
}

fn snap_with_opaque() -> peri_acp_types::history_v2::HistoryOpenedSnapshot {
    let mut opened = snap(7);
    opened.page = opaque_page();
    opened.page.history_revision = DecimalU64::new(7);
    opened.page.committed_tail = Some(tail_ref());
    opened.page.projection_version = 3;
    opened
}

fn error_data(error: &AcpError) -> HistoryErrorData {
    serde_json::from_value(error.data.clone().expect("domain data")).expect("history error data")
}

async fn wait_for_cancel_epoch(sessions: &crate::host::SharedSessions, sid: &str, baseline: u64) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = sessions
                .lock()
                .await
                .get(sid)
                .map(|state| state.continuation_epoch)
                .unwrap_or_default();
            if current > baseline {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("session/cancel 必须在慢读期间被接收循环处理");
}

async fn cancel_epoch(sessions: &crate::host::SharedSessions, sid: &str) -> u64 {
    sessions
        .lock()
        .await
        .get(sid)
        .map(|state| state.continuation_epoch)
        .unwrap_or_default()
}

#[tokio::test]
#[serial]
async fn server_loop_history_cancel_is_processed_while_body_blocked() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let fake = Arc::new(FakeBackend::new(snap_with_opaque()));
    let (client, _cancellation, _connection, task) = spawn_loop_with_backend(
        Arc::clone(&fixture.cfg),
        fixture.sessions.clone(),
        fake.clone(),
    )
    .await;
    let page = open_window(&client, &sid, &fake, 1).await;
    let baseline = cancel_epoch(&fixture.sessions, &sid).await;

    let gate = fake.gate_next_body();
    let body_client = Arc::clone(&client);
    let body_sid = sid.clone();
    let body_page = page.clone();
    let body = tokio::spawn(async move {
        body_client
            .send_request(
                HISTORY_METHOD,
                body_params(
                    2,
                    &body_sid,
                    &body_page.window_id,
                    &body_page.snapshot_token,
                    &body_page.read_ticket,
                ),
            )
            .await
    });
    fake.body_entered.notified().await;

    client
        .send_notification("session/cancel", json!({ "sessionId": sid }))
        .await
        .expect("cancel notification");
    wait_for_cancel_epoch(&fixture.sessions, &sid, baseline).await;
    assert!(!body.is_finished(), "cancel 只表示处理到达，不得取消历史读");

    gate.notify_one();
    let value = body
        .await
        .expect("body task")
        .expect("释放屏障后正文必须可读");
    assert_eq!(value["bodyRef"], "body-1");

    task.abort();
}

#[tokio::test]
#[serial]
async fn server_loop_history_close_returns_before_blocked_page_result() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let fake = Arc::new(FakeBackend::new(snap_with_opaque()));
    let (client, _cancellation, _connection, task) = spawn_loop_with_backend(
        Arc::clone(&fixture.cfg),
        fixture.sessions.clone(),
        fake.clone(),
    )
    .await;
    let page = open_window(&client, &sid, &fake, 1).await;

    let gate = fake.gate_next_page();
    let page_client = Arc::clone(&client);
    let page_sid = sid.clone();
    let page_copy = page.clone();
    let blocked = tokio::spawn(async move {
        page_client
            .send_request(
                HISTORY_METHOD,
                page_params(
                    2,
                    &page_sid,
                    &page_copy.window_id,
                    &page_copy.snapshot_token,
                    &page_copy.read_ticket,
                    page_copy.before_cursor.as_deref(),
                    "before",
                    50,
                ),
            )
            .await
    });
    fake.page_entered.notified().await;
    assert!(!blocked.is_finished());

    // close 走快速路径：无需等待阻塞中的 page。
    let closed = tokio::time::timeout(
        Duration::from_secs(2),
        client.send_request(
            HISTORY_METHOD,
            close_params(
                3,
                &sid,
                &page.window_id,
                &page.snapshot_token,
                &page.read_ticket,
            ),
        ),
    )
    .await
    .expect("close 必须及时返回")
    .expect("close succeeds");
    assert_eq!(closed["kind"], "closed");

    gate.notify_one();
    let error = blocked
        .await
        .expect("page task")
        .expect_err("关闭后的迟到页面不得发布成功");
    assert_eq!(error.code, -32000);
    assert_eq!(error_data(&error).reason, HistoryErrorReason::WindowClosed);

    task.abort();
}

#[tokio::test]
#[serial]
async fn server_loop_history_capacity_saturation_rejects_without_waiting() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let fake = Arc::new(FakeBackend::new(snap_with_opaque()));
    let (client, _cancellation, _connection, task) = spawn_loop_with_backend(
        Arc::clone(&fixture.cfg),
        fixture.sessions.clone(),
        fake.clone(),
    )
    .await;
    let page = open_window(&client, &sid, &fake, 1).await;
    fake.set_page_result(Ok(page_data(7, Some("b1"), Some("a1"))));

    let gates: Vec<_> = (0..HISTORY_MAX_IN_FLIGHT)
        .map(|_| fake.gate_next_page())
        .collect();
    let mut in_flight = Vec::new();
    for key in 2..(2 + HISTORY_MAX_IN_FLIGHT as u64) {
        let request_client = Arc::clone(&client);
        let request_sid = sid.clone();
        let request_page = page.clone();
        in_flight.push(tokio::spawn(async move {
            request_client
                .send_request(
                    HISTORY_METHOD,
                    page_params(
                        key,
                        &request_sid,
                        &request_page.window_id,
                        &request_page.snapshot_token,
                        &request_page.read_ticket,
                        request_page.before_cursor.as_deref(),
                        "before",
                        50,
                    ),
                )
                .await
        }));
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if fake.page_calls.lock().len() == HISTORY_MAX_IN_FLIGHT {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("在途读必须全部进入后端");

    let saturated = tokio::time::timeout(
        Duration::from_secs(2),
        client.send_request(
            HISTORY_METHOD,
            page_params(
                20,
                &sid,
                &page.window_id,
                &page.snapshot_token,
                &page.read_ticket,
                page.before_cursor.as_deref(),
                "before",
                50,
            ),
        ),
    )
    .await
    .expect("配额饱和必须立即返回而不是排队")
    .expect_err("超配额必须失败");
    let data = error_data(&saturated);
    assert_eq!(data.reason, HistoryErrorReason::CapacityExceeded);
    assert_eq!(data.recovery, HistoryRecovery::Retry);
    assert_eq!(
        fake.page_calls.lock().len(),
        HISTORY_MAX_IN_FLIGHT,
        "被拒绝的请求不得进入后端"
    );

    // 饱和期间取消仍被处理。
    let baseline = cancel_epoch(&fixture.sessions, &sid).await;
    client
        .send_notification("session/cancel", json!({ "sessionId": sid }))
        .await
        .expect("cancel notification");
    wait_for_cancel_epoch(&fixture.sessions, &sid, baseline).await;

    // 饱和期间 close 仍可处理（不占在途配额）。
    let closed = tokio::time::timeout(
        Duration::from_secs(2),
        client.send_request(
            HISTORY_METHOD,
            close_params(
                21,
                &sid,
                &page.window_id,
                &page.snapshot_token,
                &page.read_ticket,
            ),
        ),
    )
    .await
    .expect("close 不得被配额阻塞")
    .expect("close succeeds");
    assert_eq!(closed["kind"], "closed");

    for gate in gates {
        gate.notify_one();
    }
    for request in in_flight {
        let error = request
            .await
            .expect("page task")
            .expect_err("窗口关闭后的在途页不得成功");
        assert_eq!(error_data(&error).reason, HistoryErrorReason::WindowClosed);
    }

    task.abort();
}

/// 等待执行任务释放全部在途配额（断连回收的可观察证据）。
async fn wait_for_available_permits(
    connection: &Arc<tokio::sync::Mutex<crate::host::connection::ConnectionContext>>,
    expected: usize,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let available = { connection.lock().await.history_v2.available_in_flight() };
            if available == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("断连后执行任务必须释放配额");
}

/// FIX-04：连接断连时，卡在屏障上的 page 不得发布成功响应；配额与窗口不泄漏。
#[tokio::test]
#[serial]
async fn server_loop_history_disconnect_drops_blocked_page_without_publishing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let fake = Arc::new(FakeBackend::new(snap_with_opaque()));
    let (client, cancellation, connection, task) = spawn_loop_with_backend(
        Arc::clone(&fixture.cfg),
        fixture.sessions.clone(),
        fake.clone(),
    )
    .await;
    let page = open_window(&client, &sid, &fake, 1).await;

    let gate = fake.gate_next_page();
    let page_client = Arc::clone(&client);
    let page_sid = sid.clone();
    let page_copy = page.clone();
    let mut blocked = tokio::spawn(async move {
        page_client
            .send_request(
                HISTORY_METHOD,
                page_params(
                    2,
                    &page_sid,
                    &page_copy.window_id,
                    &page_copy.snapshot_token,
                    &page_copy.read_ticket,
                    page_copy.before_cursor.as_deref(),
                    "before",
                    50,
                ),
            )
            .await
    });
    fake.page_entered.notified().await;

    cancellation.cancel();
    wait_for_available_permits(&connection, HISTORY_MAX_IN_FLIGHT).await;
    {
        let connection = connection.lock().await;
        assert_eq!(connection.history_v2.window_count(), 1, "窗口不得泄漏");
        assert_eq!(
            connection.history_v2.opening_window_count(),
            0,
            "stale ticket 不得残留"
        );
    }

    gate.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut blocked)
            .await
            .is_err(),
        "断连后不得向客户端发送成功响应"
    );
    blocked.abort();
    task.abort();
}

/// FIX-04：同上，覆盖 body 读取。
#[tokio::test]
#[serial]
async fn server_loop_history_disconnect_drops_blocked_body_without_publishing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let fake = Arc::new(FakeBackend::new(snap_with_opaque()));
    let (client, cancellation, connection, task) = spawn_loop_with_backend(
        Arc::clone(&fixture.cfg),
        fixture.sessions.clone(),
        fake.clone(),
    )
    .await;
    let page = open_window(&client, &sid, &fake, 1).await;

    let gate = fake.gate_next_body();
    let body_client = Arc::clone(&client);
    let body_sid = sid.clone();
    let body_page = page.clone();
    let mut blocked = tokio::spawn(async move {
        body_client
            .send_request(
                HISTORY_METHOD,
                body_params(
                    2,
                    &body_sid,
                    &body_page.window_id,
                    &body_page.snapshot_token,
                    &body_page.read_ticket,
                ),
            )
            .await
    });
    fake.body_entered.notified().await;

    cancellation.cancel();
    wait_for_available_permits(&connection, HISTORY_MAX_IN_FLIGHT).await;
    {
        let connection = connection.lock().await;
        assert_eq!(connection.history_v2.window_count(), 1);
        assert_eq!(connection.history_v2.opening_window_count(), 0);
    }

    gate.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut blocked)
            .await
            .is_err(),
        "断连后不得向客户端发送正文响应"
    );
    blocked.abort();
    task.abort();
}
