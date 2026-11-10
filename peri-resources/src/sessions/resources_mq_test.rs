//! 门面的 Thread MQ 行为：写操作经 mutation 门禁、查询只读，语义与数据面一致。
//!
//! 这里只验证**接线**（门禁、转发、提交后才可见）；逐条事务语义由
//! `sqlite_store::session_mq_tests` 与 `remote::session_mq_tests` 覆盖。

use peri_acp_types::messages::MessageId;
use peri_acp_types::session::{
    ExecutionBinding, MessageKind, MessagePolicy, MessageSource, MqCancelOutcome,
    MqEnqueueDisposition, MqEntry, MqFilter, MqInput, MqReceiveDisposition, MqReceiveItem,
    MqReceiveOutcome, MqReceivePurpose, MqReceiveSelection, MqReceiveStatus, TurnId,
};
use peri_acp_types::store::PersistedPayload;

use super::*;

fn input(id: MessageId, text: &str) -> MqInput {
    with_policy(
        id,
        text,
        MessageKind::Prompt,
        MessagePolicy::ensure_processing(),
    )
}

fn with_policy(id: MessageId, text: &str, kind: MessageKind, policy: MessagePolicy) -> MqInput {
    MqInput {
        id,
        kind,
        source: MessageSource::UserInput,
        payload: PersistedPayload::Message(peri_acp_types::messages::BaseMessage::Human {
            id,
            content: text.into(),
        }),
        policy,
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

/// MQ 接纳维护目标工作意图：`EnsureProcessing` 把 `idle` 提升为 `working`（§5.2）。
#[tokio::test]
async fn mq_acceptance_promotes_idle_work_state_for_ensure_processing() {
    let fixture = Fixture::new().await;
    fixture.create("mq-work-state").await;
    let thread: ThreadId = "mq-work-state".to_owned();
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&thread)
            .await
            .unwrap()
            .work_state,
        WorkState::Idle
    );

    fixture
        .facade
        .mq_enqueue(&thread, input(MessageId::new(), "wake"))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&thread)
            .await
            .unwrap()
            .work_state,
        WorkState::Working,
        "需要推动目标继续工作的消息必须授予续跑意图"
    );
}

/// `stopped` 不被被动回调解除；Passive / Info / ContinueCurrentRun 不授予执行资格。
#[tokio::test]
async fn mq_acceptance_keeps_stopped_work_state_and_ignores_passive() {
    let fixture = Fixture::new().await;
    fixture.create("mq-stopped").await;
    fixture.create("mq-passive").await;
    let stopped: ThreadId = "mq-stopped".to_owned();
    let passive: ThreadId = "mq-passive".to_owned();
    fixture
        .facade
        .set_work_state(&stopped, WorkState::Stopped)
        .await
        .unwrap();

    fixture
        .facade
        .mq_enqueue(&stopped, input(MessageId::new(), "later"))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&stopped)
            .await
            .unwrap()
            .work_state,
        WorkState::Stopped,
        "Stopped 只由显式继续解除，不被消息接纳覆盖"
    );

    // Info 的默认策略就是 Passive；ContinueCurrentRun 也只在同一执行内生效。
    for policy in [
        MessagePolicy::passive(),
        MessagePolicy::continue_current_run(ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        }),
        MessagePolicy::stop_current_run(ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        }),
    ] {
        fixture
            .facade
            .mq_enqueue(
                &passive,
                with_policy(MessageId::new(), "not work", MessageKind::Info, policy),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&passive)
            .await
            .unwrap()
            .work_state,
        WorkState::Idle,
        "非 EnsureProcessing 策略不得自行授予执行资格"
    );
}

/// 同 ID 重复接纳走「既有事实」分支，同样维护工作意图。
#[tokio::test]
async fn duplicate_mq_acceptance_still_maintains_work_state() {
    let fixture = Fixture::new().await;
    fixture.create("mq-duplicate").await;
    let thread: ThreadId = "mq-duplicate".to_owned();
    let id = MessageId::new();
    fixture
        .facade
        .mq_enqueue(&thread, input(id, "wake"))
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&thread, WorkState::Idle)
        .await
        .unwrap();

    let again = fixture
        .facade
        .mq_enqueue(&thread, input(id, "wake"))
        .await
        .unwrap();
    assert_eq!(again.disposition, MqEnqueueDisposition::Existing);
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&thread)
            .await
            .unwrap()
            .work_state,
        WorkState::Working,
        "重复接纳仍是接纳，必须维护目标工作意图"
    );
}

/// 终态 thread 的迟到 `EnsureProcessing` 接纳不得重新授予续跑意图。
#[tokio::test]
async fn mq_acceptance_does_not_promote_terminal_thread() {
    use peri_acp_types::session_resources::SessionMetaPatch;
    use peri_acp_types::thread::AgentStatus;

    let fixture = Fixture::new().await;
    fixture.create("mq-terminal").await;
    let thread: ThreadId = "mq-terminal".to_owned();
    fixture
        .facade
        .update_session_meta(
            &thread,
            &SessionMetaPatch {
                status: Some(AgentStatus::Error),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    fixture
        .facade
        .mq_enqueue(&thread, input(MessageId::new(), "late wake"))
        .await
        .unwrap();
    let meta = fixture.facade.load_session_meta(&thread).await.unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(
        meta.work_state,
        WorkState::Idle,
        "终态 thread 不因消息接纳变成待续跑"
    );
}

/// 已移交历史的消息不再要求继续处理：重复接纳不能把它重新提升为 `working`。
#[tokio::test]
async fn reaccepting_handed_off_message_keeps_idle_work_state() {
    let fixture = Fixture::new().await;
    fixture.create("mq-handoff").await;
    let thread: ThreadId = "mq-handoff".to_owned();
    let id = MessageId::new();
    let enqueued = fixture
        .facade
        .mq_enqueue(&thread, input(id, "wake"))
        .await
        .unwrap();
    fixture
        .facade
        .mq_receive(
            &thread,
            selection(enqueued.entry.clone(), MqReceiveDisposition::Commit),
        )
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&thread, WorkState::Idle)
        .await
        .unwrap();

    let again = fixture
        .facade
        .mq_enqueue(&thread, input(id, "wake"))
        .await
        .unwrap();
    assert_eq!(again.disposition, MqEnqueueDisposition::AlreadyHandedOff);
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&thread)
            .await
            .unwrap()
            .work_state,
        WorkState::Idle,
        "已移交的消息不再构成继续工作的意图"
    );
}
