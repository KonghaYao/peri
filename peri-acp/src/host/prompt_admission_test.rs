//! 工作意图准入：写入失败必须在模型调用之前拒绝本轮。
//!
//! 用 SQLite trigger 让 `work_state` 更新失败，验证 fail-closed 与内存回滚。

use super::*;
use crate::host::{dispatch_prompt_turn, PromptOrigin};

#[tokio::test]
async fn work_state_commit_failure_rejects_prompt_before_model_call() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    use sqlx::Connection as _;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&harness.db_path),
    )
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_work_state BEFORE UPDATE OF work_state ON threads
         BEGIN SELECT RAISE(ABORT, 'work_state blocked'); END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    drop(connection);

    let error = dispatch_prompt_turn(
        json!({
            "sessionId": thread_id,
            "prompt": [{"type": "text", "text": "hello"}],
        }),
        PromptOrigin::User,
        None,
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .expect_err("工作意图写入失败必须拒绝 prompt");

    assert_eq!(error.code, -32603, "{error:?}");
    assert_eq!(
        error.data.as_ref().unwrap()["reason"],
        "work_intent_commit_failed"
    );
    assert!(!harness._mock.matched(), "拒绝必须发生在模型调用之前");
    assert_eq!(
        harness
            .cfg
            .session_resources
            .load_session_meta(&thread_id)
            .await
            .unwrap()
            .work_state,
        WorkState::Idle,
        "写入失败不得留下 working"
    );
    assert!(
        harness
            .sessions
            .lock()
            .await
            .get(&thread_id)
            .is_some_and(|state| state.cancel_token.is_none()),
        "准入失败必须清除本次发布的令牌"
    );
}

/// 准入回滚只撤销本次准入发布的令牌：后续轮次已替换的令牌不得被误清（#196 P2 复审）。
///
/// 在准入闸门处人为固定「后续轮次已替换令牌」的时序，再让工作意图写入失败，
/// 验证错误提前返回的清理按令牌身份判断。
#[tokio::test]
async fn admission_rollback_only_clears_its_own_token() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    use sqlx::Connection as _;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&harness.db_path),
    )
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_work_state BEFORE UPDATE OF work_state ON threads
         BEGIN SELECT RAISE(ABORT, 'work_state blocked'); END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    drop(connection);

    let gate =
        crate::host::prompt_dispatch::admission_gate::install(&format!("{thread_id}:admitted"));
    let prompt = {
        let sessions = Arc::clone(&harness.sessions);
        let locks = Arc::clone(&harness.locks);
        let transport = Arc::clone(&harness.transport);
        let cfg = Arc::clone(&harness.cfg);
        let sender = Arc::clone(&harness.sender);
        let thread_id = thread_id.clone();
        tokio::spawn(async move {
            dispatch_prompt_turn(
                json!({
                    "sessionId": thread_id,
                    "prompt": [{"type": "text", "text": "hello"}],
                }),
                PromptOrigin::User,
                None,
                &sessions,
                &locks,
                &transport,
                &cfg,
                &sender,
            )
            .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.entered.notified())
        .await
        .expect("prompt 必须到达准入后的暂停点");

    // 模拟后续轮次已经发布自己的令牌：本次准入只能撤销自己持有的那一个。
    let later = tokio_util::sync::CancellationToken::new();
    harness
        .sessions
        .lock()
        .await
        .get_mut(&thread_id)
        .expect("会话存在")
        .cancel_token = Some(later.clone());
    gate.release.notify_one();

    let error = prompt
        .await
        .expect("prompt 任务不得 panic")
        .expect_err("工作意图写入失败必须拒绝 prompt");
    assert_eq!(error.code, -32603, "{error:?}");
    let states = harness.sessions.lock().await;
    assert_eq!(
        states[&thread_id].cancel_token.as_ref(),
        Some(&later),
        "回滚不得清除后续轮次发布的令牌"
    );
}

/// Stop 落在最终准入复查之后、工作意图提交之前：落库的 `stopped` 不得被本轮
/// 的 `working` 覆盖，也不得发起模型调用。
#[tokio::test]
async fn stop_before_work_intent_keeps_stopped_without_model_call() {
    assert_stop_around_work_intent("before_work_intent").await;
}

/// Stop 落在工作意图提交之后、进入执行器之前：同样回写 `stopped` 并跳过模型调用。
#[tokio::test]
async fn stop_after_work_intent_keeps_stopped_without_model_call() {
    assert_stop_around_work_intent("after_work_intent").await;
}

async fn assert_stop_around_work_intent(pause_point: &str) {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{thread_id}:{pause_point}"
    ));
    let prompt = {
        let sessions = Arc::clone(&harness.sessions);
        let locks = Arc::clone(&harness.locks);
        let transport = Arc::clone(&harness.transport);
        let cfg = Arc::clone(&harness.cfg);
        let sender = Arc::clone(&harness.sender);
        let thread_id = thread_id.clone();
        tokio::spawn(async move {
            dispatch_prompt_turn(
                json!({
                    "sessionId": thread_id,
                    "prompt": [{"type": "text", "text": "hello"}],
                }),
                PromptOrigin::User,
                None,
                &sessions,
                &locks,
                &transport,
                &cfg,
                &sender,
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .expect("prompt 必须到达工作意图暂停点");

    // 与生产 Stop 同路径：取消本轮令牌，并把停止意图落库。
    let outcome = {
        let mut sessions = harness.sessions.lock().await;
        crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": thread_id}),
            &mut sessions,
            &harness.cfg,
        )
        .await
    };
    crate::host::notify::persist_notification_effects(
        "session/cancel",
        &harness.cfg,
        outcome.stop_targets,
        outcome.context_usage_invalidations,
    )
    .await;
    gate.release.notify_one();

    let result = tokio::time::timeout(Duration::from_secs(10), prompt)
        .await
        .expect("prompt 任务不得卡死")
        .expect("prompt 任务不得 panic")
        .expect("Stop 后的空轮次是成功空操作");
    assert_eq!(result, Value::Null);
    assert!(!harness._mock.matched(), "Stop 生效后不得发起模型调用");
    assert_eq!(
        harness
            .cfg
            .session_resources
            .load_session_meta(&thread_id)
            .await
            .unwrap()
            .work_state,
        WorkState::Stopped,
        "最终 durable 工作意图必须是 stopped"
    );
    assert!(
        harness.sessions.lock().await[&thread_id]
            .cancel_token
            .is_none(),
        "准入回滚必须清除本次发布的令牌"
    );
}
