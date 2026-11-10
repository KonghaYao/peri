//! durable 工作意图与上下文占用：门面写入的真实库行为。
//!
//! 两列都是「最后提交的快照」：只写自己，不碰 `updated_at`——否则每一轮 turn 都会
//! 改写会话排序时间。断言以读回的可观察事实为准。

use super::*;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::{MessageKind, MessagePolicy, MessageSource, MqInput};
use peri_acp_types::store::PersistedPayload;

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

fn pending_input(text: &str, policy: MessagePolicy) -> MqInput {
    let id = peri_acp_types::messages::MessageId::new();
    MqInput {
        id,
        kind: MessageKind::Prompt,
        source: MessageSource::UserInput,
        payload: PersistedPayload::Message(BaseMessage::Human {
            id,
            content: text.into(),
        }),
        policy,
        original_draft: None,
    }
}

/// 仍有待处理的 `EnsureProcessing` 消息时收尾保持 `working`：不能覆盖刚到的消息。
#[tokio::test]
async fn settle_work_state_keeps_working_while_processing_is_pending() {
    let fixture = Fixture::new().await;
    fixture.create("settle-pending").await;
    let id = ThreadId::from("settle-pending".to_owned());
    fixture
        .facade
        .set_work_state(&id, WorkState::Working)
        .await
        .unwrap();
    fixture
        .facade
        .mq_enqueue(
            &id,
            pending_input("still queued", MessagePolicy::ensure_processing()),
        )
        .await
        .unwrap();

    let written = fixture
        .facade
        .settle_work_state_after_turn(&id)
        .await
        .unwrap();
    assert!(!written, "有待处理消息时收尾不得写入 idle");
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&id)
            .await
            .unwrap()
            .work_state,
        WorkState::Working
    );
}

/// 没有待处理工作时收尾把 `working` 收敛为 `idle`；Passive 消息不构成待处理工作。
#[tokio::test]
async fn settle_work_state_returns_working_to_idle_without_pending_work() {
    let fixture = Fixture::new().await;
    fixture.create("settle-idle").await;
    let id = ThreadId::from("settle-idle".to_owned());
    fixture
        .facade
        .set_work_state(&id, WorkState::Working)
        .await
        .unwrap();
    fixture
        .facade
        .mq_enqueue(&id, pending_input("info only", MessagePolicy::passive()))
        .await
        .unwrap();

    assert!(
        fixture
            .facade
            .settle_work_state_after_turn(&id)
            .await
            .unwrap(),
        "没有待处理工作应写入 idle"
    );
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&id)
            .await
            .unwrap()
            .work_state,
        WorkState::Idle
    );
    assert!(
        !fixture
            .facade
            .settle_work_state_after_turn(&id)
            .await
            .unwrap(),
        "已 idle 的 thread 收尾不产生写入"
    );
}

/// 收尾不解除显式停止：`stopped` 保持到下一次执行准入。
#[tokio::test]
async fn settle_work_state_does_not_lift_stopped() {
    let fixture = Fixture::new().await;
    fixture.create("settle-stopped").await;
    let id = ThreadId::from("settle-stopped".to_owned());
    fixture
        .facade
        .set_work_state(&id, WorkState::Stopped)
        .await
        .unwrap();

    assert!(
        !fixture
            .facade
            .settle_work_state_after_turn(&id)
            .await
            .unwrap(),
        "stopped 不在收尾的写入条件里"
    );
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&id)
            .await
            .unwrap()
            .work_state,
        WorkState::Stopped
    );
}

/// 默认 idle 结算（mock / 其他 adapter 的兜底）只把 `working` 收敛为 `idle`。
///
/// 两个真实 adapter 都覆盖了该方法；这里直接调用默认实现，锁定不覆盖显式停止
/// 的条件写语义。
#[tokio::test]
async fn default_settle_work_state_only_rewrites_working() {
    let fixture = Fixture::new().await;
    fixture.create("default-settle").await;
    let id = ThreadId::from("default-settle".to_owned());
    let port = fixture.facade.gate.data().clone();

    for state in [WorkState::Stopped, WorkState::Idle] {
        port.set_work_state(&id, state).await.unwrap();
        assert!(
            !crate::sessions::data::settle_work_state_after_turn_default(port.as_ref(), &id)
                .await
                .unwrap(),
            "只有 working 才允许默认收尾写入"
        );
        assert_eq!(
            fixture
                .facade
                .load_session_meta(&id)
                .await
                .unwrap()
                .work_state,
            state
        );
    }

    port.set_work_state(&id, WorkState::Working).await.unwrap();
    assert!(
        crate::sessions::data::settle_work_state_after_turn_default(port.as_ref(), &id)
            .await
            .unwrap(),
        "working 必须收敛为 idle"
    );
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&id)
            .await
            .unwrap()
            .work_state,
        WorkState::Idle
    );
}
