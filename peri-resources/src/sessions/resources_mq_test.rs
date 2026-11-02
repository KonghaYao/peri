//! 门面的 Thread MQ 行为：写操作经 mutation 门禁、查询只读，语义与数据面一致。
//!
//! 这里只验证**接线**（门禁、转发、提交后才可见）；逐条事务语义由
//! `sqlite_store::session_mq_tests` 与 `remote::session_mq_tests` 覆盖。

use peri_acp_types::messages::MessageId;
use peri_acp_types::session::{
    MessageKind, MessagePolicy, MessageSource, MqCancelOutcome, MqEnqueueDisposition, MqEntry,
    MqFilter, MqInput, MqReceiveDisposition, MqReceiveItem, MqReceiveOutcome, MqReceivePurpose,
    MqReceiveSelection, MqReceiveStatus, TurnId,
};
use peri_acp_types::store::PersistedPayload;

use super::*;

fn input(id: MessageId, text: &str) -> MqInput {
    MqInput {
        id,
        kind: MessageKind::Prompt,
        source: MessageSource::UserInput,
        payload: PersistedPayload::Message(peri_acp_types::messages::BaseMessage::Human {
            id,
            content: text.into(),
        }),
        policy: MessagePolicy::ensure_processing(),
        original_draft: Some(text.to_owned()),
    }
}

fn selection(entry: MqEntry, disposition: MqReceiveDisposition) -> MqReceiveSelection {
    MqReceiveSelection {
        items: vec![MqReceiveItem { entry, disposition }],
        purpose: MqReceivePurpose::ModelLoop,
        execution: peri_acp_types::session::ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        },
    }
}

/// 入队、查询、取消、移交都经门面完成；查询是只读投影，不消费。
#[tokio::test]
async fn facade_exposes_the_thread_mq_surface() {
    let fixture = Fixture::new().await;
    fixture.create("mq-facade").await;
    let thread: ThreadId = "mq-facade".to_owned();
    let id = MessageId::new();
    let enqueued = fixture
        .facade
        .mq_enqueue(&thread, input(id, "hello"))
        .await
        .unwrap();
    assert_eq!(enqueued.disposition, MqEnqueueDisposition::Inserted);

    let listed = fixture
        .facade
        .mq_query(
            &thread,
            MqFilter {
                user_input_only: true,
                ..MqFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);

    let again = fixture
        .facade
        .mq_enqueue(&thread, input(id, "hello"))
        .await
        .unwrap();
    assert_eq!(again.disposition, MqEnqueueDisposition::Existing);

    let received: MqReceiveOutcome = fixture
        .facade
        .mq_receive(
            &thread,
            selection(enqueued.entry.clone(), MqReceiveDisposition::Commit),
        )
        .await
        .unwrap();
    assert_eq!(received.items[0].status, MqReceiveStatus::Committed);
    assert!(fixture
        .facade
        .mq_query(&thread, MqFilter::default())
        .await
        .unwrap()
        .is_empty());

    // 移交后的历史事实成立：计数已维护；已有标题不被派生标题覆盖（派生只在标题为空时生效，
    // 逐条规则见 `sqlite_store::session_mq_tests::receive_maintains_count_and_derived_title`）。
    let meta = fixture.facade.load_session_meta(&thread).await.unwrap();
    assert_eq!(meta.message_count, 1);
    assert_eq!(meta.title.as_deref(), Some("session mq-facade"));
    assert!(matches!(
        fixture.facade.mq_cancel(&thread, id).await.unwrap(),
        MqCancelOutcome::AlreadyHandedOff
    ));
}

/// 取消返回整行原稿；清理只作用于本 thread。
#[tokio::test]
async fn facade_cancel_and_clear_follow_the_thread_scope() {
    let fixture = Fixture::new().await;
    fixture.create("mq-cancel").await;
    fixture.create("mq-other").await;
    let thread: ThreadId = "mq-cancel".to_owned();
    let other: ThreadId = "mq-other".to_owned();
    let id = MessageId::new();
    fixture
        .facade
        .mq_enqueue(&thread, input(id, "draft"))
        .await
        .unwrap();
    fixture
        .facade
        .mq_enqueue(&other, input(MessageId::new(), "other"))
        .await
        .unwrap();

    let cancelled = fixture.facade.mq_cancel(&thread, id).await.unwrap();
    let MqCancelOutcome::Cancelled(entry) = cancelled else {
        panic!("expected a cancelled row");
    };
    assert_eq!(entry.original_draft.as_deref(), Some("draft"));
    assert_eq!(
        fixture
            .facade
            .mq_query(&other, MqFilter::default())
            .await
            .unwrap()
            .len(),
        1,
        "别的 thread 的待发项不受影响"
    );

    fixture.facade.mq_clear(&thread).await.unwrap();
    assert!(fixture
        .facade
        .mq_query(&thread, MqFilter::default())
        .await
        .unwrap()
        .is_empty());
}
