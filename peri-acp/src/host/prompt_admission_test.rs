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
}
