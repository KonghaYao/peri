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

/// 8. 缺父：会话装配与历史回放成功，恢复失败以 `_meta.peri.recovery` 报告；
///    执行被阻断，历史通知仍然存在，MQ 不注入提醒。
#[tokio::test]
async fn corrupt_scope_returns_success_with_failed_recovery_meta_after_history_replay() {
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
    let response = result.as_ref().expect("会话装配成功时 load 必须返回 Ok");
    let recovery = &response["_meta"]["peri.recovery"];
    assert_eq!(recovery["status"], "failed", "{response}");
    assert_eq!(recovery["error"]["kind"], "corrupt", "{response}");
    assert_eq!(recovery["error"]["executionBlocked"], true, "{response}");
    assert_eq!(recovery["executionBlocked"], true, "{response}");
    let notifications = harness.recorded.notifications();
    assert!(
        notifications.iter().any(|(method, params)| {
            method == "session/update" && params.to_string().contains("kept history")
        }),
        "历史回放必须先于恢复结果：{notifications:?}"
    );
    assert!(
        harness.recovery_rows("orphan-child").await.is_empty(),
        "失败进入不得注入提醒"
    );
    let session_id = "orphan-child";
    assert!(
        harness
            .cfg
            .session_manager
            .get_session(session_id)
            .expect("会话 runtime 必须已登记")
            .recovery_execution_block()
            .is_some(),
        "不可执行分类必须留下执行阻断"
    );
}

/// 8b. 被阻断的会话拒绝 prompt：`-32017` + data.reason，模型零调用。
#[tokio::test]
async fn blocked_session_prompt_returns_recovery_execution_blocked() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "orphan-child", &root).await;
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
    push_request(
        &harness,
        3,
        "session/prompt",
        json!({
            "sessionId": "orphan-child",
            "prompt": [{"type": "text", "text": "continue"}],
        }),
    );
    run_server_loop(&harness).await;

    wait_for("阻断的 prompt 返回错误", || async {
        harness.recorded.error_for(RequestId::Number(3)).is_some()
    })
    .await;
    let error = harness
        .recorded
        .error_for(RequestId::Number(3))
        .expect("prompt 必须有错误响应");
    assert_eq!(error.code, -32017, "{error:?}");
    assert_eq!(
        error.data.as_ref().unwrap()["reason"],
        "recovery_execution_blocked"
    );
    assert_eq!(error.data.as_ref().unwrap()["kind"], "corrupt");
    assert!(!harness._mock.matched(), "阻断必须在模型调用之前");
}

/// 8c. 普通 idle load 不产生恢复字段：只有实际恢复或失败才附加。
#[tokio::test]
async fn idle_load_has_no_recovery_meta() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    push_request(&harness, 2, "session/load", json!({"sessionId": thread_id}));
    run_server_loop(&harness).await;

    let responses = harness.recorded.responses();
    let (_, result) = responses
        .iter()
        .find(|(id, _)| *id == RequestId::Number(2))
        .expect("load 必须有响应");
    let response = result.as_ref().expect("idle load 必须成功");
    assert!(
        response["_meta"].get("peri.recovery").is_none(),
        "idle load 不应有恢复字段：{response}"
    );
}

/// 8d. working load：status ok，recovered 列表给出实际注入提醒的 thread。
#[tokio::test]
async fn working_load_meta_reports_recovered_threads() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    push_request(&harness, 2, "session/load", json!({"sessionId": thread_id}));
    run_server_loop(&harness).await;

    let responses = harness.recorded.responses();
    let (_, result) = responses
        .iter()
        .find(|(id, _)| *id == RequestId::Number(2))
        .expect("load 必须有响应");
    let response = result.as_ref().expect("working load 必须成功");
    let recovery = &response["_meta"]["peri.recovery"];
    assert_eq!(recovery["status"], "ok", "{response}");
    assert_eq!(recovery["executionBlocked"], false, "{response}");
    assert_eq!(recovery["recovered"], json!([thread_id]), "{response}");
    assert_eq!(recovery["enteredThreadId"], thread_id, "{response}");
}

/// 15. root idle + child working：load root 后，冷 child 必须取得 TaskManager 绑定
///     与当前连接 endpoint；父级不启动模型。
#[tokio::test]
async fn cold_child_recovery_binds_task_manager_and_registers_endpoint() {
    let harness = RecoveryHarness::new_with_workspace(true).await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    push_request(&harness, 2, "session/load", json!({"sessionId": root}));
    run_server_loop(&harness).await;

    let responses = harness.recorded.responses();
    let (_, result) = responses
        .iter()
        .find(|(id, _)| *id == RequestId::Number(2))
        .expect("load 必须有响应");
    let response = result.as_ref().expect("root idle load 必须成功");
    assert_eq!(
        response["_meta"]["peri.recovery"]["recovered"],
        json!(["child-b"]),
        "{response}"
    );

    let session_id = harness
        .cfg
        .session_manager
        .session_id_for_thread(&"child-b".to_owned())
        .expect("冷 child 必须取得运行实例");
    let (task_events_started, environment, published) = {
        let session = harness
            .cfg
            .session_manager
            .get_session(&session_id)
            .expect("会话 runtime 必须已登记");
        let task_events_started = session
            .task_events_started
            .load(std::sync::atomic::Ordering::Acquire);
        drop(session);
        let states = harness.sessions.lock().await;
        (
            task_events_started,
            states
                .get(&session_id)
                .and_then(|state| state.environment.clone()),
            states
                .iter()
                .map(|(key, state)| (key.clone(), state.environment.is_some()))
                .collect::<Vec<_>>(),
        )
    };
    assert!(task_events_started, "恢复后必须绑定 TaskManager 事件");
    let Some(environment) = environment else {
        panic!("冷 child 环境必须已发布；session_id={session_id} published={published:?}");
    };
    assert_eq!(
        environment.interaction_router().online_endpoint_count(),
        1,
        "当前连接必须登记为冷 child 的在线 endpoint"
    );
    assert!(!harness._mock.matched(), "父级不因 child 恢复而启动模型");
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
