//! 恢复的 wire 接线：请求 seam、错误码 `-32011` 与「响应先于恢复通知」。
//!
//! 走真实 `ServerLoop`：模式分流、生命周期锁与响应装配都是分发层的职责，只测
//! `HostSessionRecovery` 会漏掉「恢复是否在响应之后放行」这一层。

use super::*;

/// 真实 server loop；transport 的收件箱由用例提前填充。
pub(super) async fn run_server_loop(harness: &RecoveryHarness) {
    let connection = Arc::new(tokio::sync::Mutex::new(
        crate::host::connection::ConnectionContext::new(false),
    ));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let recovery = harness.recovery();
    crate::host::server_loop::ServerLoop {
        transport: &harness.transport,
        cfg: &harness.cfg,
        sessions: &harness.sessions,
        prompt_locks: &harness.locks,
        cont_tx: &harness.sender,
        connection: &connection,
        connection_cancellation: &cancellation,
        recovery: &recovery,
    }
    .run()
    .await;
}

pub(super) fn push_request(harness: &RecoveryHarness, id: i64, method: &str, params: Value) {
    harness
        .recorded
        .incoming
        .lock()
        .unwrap()
        .push_back(IncomingMessage::Request {
            id: RequestId::Number(id),
            method: method.to_owned(),
            params,
        });
}

/// 10. `session/new` 不触发恢复：零候选、零提醒、零工作意图写入。
#[tokio::test]
async fn session_new_does_not_trigger_recovery() {
    let harness = RecoveryHarness::new().await;
    push_request(&harness, 7, "session/new", json!({"cwd": harness.cwd}));
    run_server_loop(&harness).await;

    let responses = harness.recorded.responses();
    let created = responses
        .iter()
        .find(|(id, _)| *id == RequestId::Number(7))
        .and_then(|(_, result)| result.as_ref().ok())
        .expect("session/new 必须成功");
    let session_id = created["sessionId"].as_str().unwrap().to_owned();
    assert!(
        harness.recovery_rows(&session_id).await.is_empty(),
        "新建会话不得注入恢复提醒"
    );
    assert_eq!(
        harness
            .cfg
            .session_resources
            .load_session_meta(&session_id)
            .await
            .unwrap()
            .work_state,
        WorkState::Idle,
        "进入不得写工作意图"
    );
}

/// 11. `session/load` 的响应先于恢复 turn 的任何通知：启动门在响应之后。
#[tokio::test]
async fn load_response_precedes_the_recovery_turn() {
    let mut harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    harness.spawn_continuation_scheduler();
    push_request(
        &harness,
        1,
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {"_meta": {}}}),
    );
    push_request(&harness, 2, "session/load", json!({"sessionId": thread_id}));
    run_server_loop(&harness).await;

    let responses = harness.recorded.responses();
    assert!(
        responses
            .iter()
            .any(|(id, result)| *id == RequestId::Number(2) && result.is_ok()),
        "load 必须成功返回：{responses:?}"
    );
    wait_for("恢复 turn 调用模型", || async {
        harness._mock.matched()
    })
    .await;
    harness._mock.assert_async().await;

    let order = harness.order.lock().unwrap().clone();
    assert_eq!(
        &order[..3],
        ["response", "response", "continuation"],
        "恢复启动必须排在 load 响应之后：{order:?}"
    );
    assert!(
        harness.recovery_rows(&thread_id).await.is_empty(),
        "提醒应已由 Receive 移交"
    );
}

/// 8. 缺父：`enter` 返回 `Corrupt`，响应是 `-32011`；历史推送已经发出。
#[tokio::test]
async fn corrupt_scope_returns_recovery_failure_code_after_history_replay() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "orphan-child", &root).await;
    harness
        .cfg
        .session_resources
        .append_history(
            &"orphan-child".to_owned(),
            &[peri_acp_types::store::PersistedPayload::Message(
                peri_acp_types::messages::BaseMessage::human("kept history"),
            )],
        )
        .await
        .unwrap();
    // 父行缺失只能由存储事实制造：公开门面不允许写出断裂的父链。
    use sqlx::Connection as _;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&harness.db_path),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE threads SET parent_thread_id = 'ghost-parent' WHERE id = 'orphan-child'")
        .execute(&mut connection)
        .await
        .unwrap();
    drop(connection);

    push_request(
        &harness,
        2,
        "session/load",
        json!({"sessionId": "orphan-child"}),
    );
    run_server_loop(&harness).await;

    let responses = harness.recorded.responses();
    let (_, result) = responses
        .iter()
        .find(|(id, _)| *id == RequestId::Number(2))
        .expect("load 必须有响应");
    let error = result.as_ref().expect_err("缺父必须显式失败，不能放行执行");
    assert!(error.contains("-32011"), "必须是恢复失败错误码：{error}");
    assert!(error.contains("corrupt"), "失败分类必须可定位：{error}");
    let notifications = harness.recorded.notifications();
    assert!(
        notifications.iter().any(|(method, params)| {
            method == "session/update" && params.to_string().contains("kept history")
        }),
        "历史回放必须先于错误响应：{notifications:?}"
    );
    assert!(
        harness.recovery_rows("orphan-child").await.is_empty(),
        "失败进入不得注入提醒"
    );
}

/// 12. `session/load` 载荷带已保存占用与当前有效 window；NULL 如实为 null。
///
/// 客户端按「缺失/null 即未知」展示，服务端不把未知写成 0。
#[tokio::test]
async fn load_response_carries_saved_context_usage() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    let window = u64::from(harness.cfg.provider.read().effective_context_window());

    load_and_check_usage(&harness, 1, &thread_id, Value::Null, window).await;

    harness
        .cfg
        .session_resources
        .set_context_usage(&thread_id, Some(4096))
        .await
        .unwrap();
    load_and_check_usage(&harness, 2, &thread_id, json!(4096), window).await;
}

async fn load_and_check_usage(
    harness: &RecoveryHarness,
    id: i64,
    thread_id: &str,
    expected: Value,
    window: u64,
) {
    push_request(harness, id, "session/load", json!({"sessionId": thread_id}));
    run_server_loop(harness).await;
    let responses = harness.recorded.responses();
    let (_, result) = responses
        .iter()
        .find(|(request_id, _)| *request_id == RequestId::Number(id))
        .expect("load 必须有响应");
    let response = result.as_ref().expect("load 必须成功");
    assert_eq!(
        response["_meta"]["peri.contextUsage"]["tokens"], expected,
        "载荷必须给出已保存占用（NULL 保持未知）：{response}"
    );
    assert_eq!(
        response["_meta"]["peri.contextUsage"]["window"],
        json!(window),
        "百分比按当前有效 window 计算：{response}"
    );
}
