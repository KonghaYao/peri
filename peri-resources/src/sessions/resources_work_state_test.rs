//! durable 工作意图与上下文占用：门面写入的真实库行为。
//!
//! 两列都是「最后提交的快照」：只写自己，不碰 `updated_at`——否则每一轮 turn 都会
//! 改写会话排序时间。断言以读回的可观察事实为准。

use super::*;

/// 固定会话的 `updated_at`：若写入顺手更新了它，哨兵值就会被当前时间替换。
async fn freeze_updated_at(fixture: &Fixture, id: &ThreadId) -> peri_acp_types::thread::ThreadMeta {
    sqlx::query("UPDATE threads SET updated_at = '2000-01-01T00:00:00Z' WHERE id = ?1")
        .bind(id.as_str())
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();
    fixture.facade.load_session_meta(id).await.unwrap()
}

#[tokio::test]
async fn work_state_writes_round_trip_without_touching_updated_at() {
    let fixture = Fixture::new().await;
    fixture.create("session").await;
    let id = ThreadId::from("session".to_owned());
    let before = freeze_updated_at(&fixture, &id).await;
    assert_eq!(before.work_state, WorkState::Idle);

    fixture
        .facade
        .set_work_state(&id, WorkState::Working)
        .await
        .unwrap();
    let working = fixture.facade.load_session_meta(&id).await.unwrap();
    assert_eq!(working.work_state, WorkState::Working);
    assert_eq!(
        working.updated_at, before.updated_at,
        "工作意图不是会话活动，不得改写排序时间"
    );

    fixture
        .facade
        .set_work_state(&id, WorkState::Stopped)
        .await
        .unwrap();
    let stopped = fixture.facade.load_session_meta(&id).await.unwrap();
    assert_eq!(stopped.work_state, WorkState::Stopped);
    assert_eq!(stopped.updated_at, before.updated_at);
}

#[tokio::test]
async fn context_usage_writes_round_trip_without_touching_updated_at() {
    let fixture = Fixture::new().await;
    fixture.create("session").await;
    let id = ThreadId::from("session".to_owned());
    let before = freeze_updated_at(&fixture, &id).await;
    assert_eq!(before.context_usage_tokens, None);

    fixture
        .facade
        .set_context_usage(&id, Some(4096))
        .await
        .unwrap();
    let known = fixture.facade.load_session_meta(&id).await.unwrap();
    assert_eq!(known.context_usage_tokens, Some(4096));
    assert_eq!(
        known.updated_at, before.updated_at,
        "占用快照不是会话活动，不得改写排序时间"
    );

    fixture.facade.set_context_usage(&id, None).await.unwrap();
    let unknown = fixture.facade.load_session_meta(&id).await.unwrap();
    assert_eq!(unknown.context_usage_tokens, None, "未知占用必须写回 NULL");
    assert_eq!(unknown.updated_at, before.updated_at);
}

/// 目标 thread 不存在时单列写入必须失败：0 行不是「已提交」。
#[tokio::test]
async fn work_state_write_to_missing_thread_is_not_found() {
    let fixture = Fixture::new().await;
    let missing = ThreadId::from("missing".to_owned());
    let error = fixture
        .facade
        .set_work_state(&missing, WorkState::Working)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
        ),
        "缺失 thread 必须是 NotFound：{error:?}"
    );

    let error = fixture
        .facade
        .set_context_usage(&missing, Some(1))
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));
}
