// Atomic terminal settlement and retained message queue cases.

// ─── 终态结算：终态 + 工作意图 + 结果接纳一次提交 ──────────────────────────────

/// 读 `threads` 行的 `(agent_status, work_state)`，用于断言「一条都没写」。
async fn terminal_facts(fixture: &Fixture, thread: &ThreadId) -> (String, Option<String>) {
    sqlx::query_as("SELECT agent_status, work_state FROM threads WHERE id = ?1")
        .bind(thread.as_str())
        .fetch_one(&fixture.store.database.pool)
        .await
        .unwrap()
}

fn reminder(body: &str) -> peri_acp_types::system_reminder::TrustedSystemReminder {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("task_manager".into()),
            kind: "terminal".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: body.into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap()
}

fn terminal_input(id: MessageId, body: &str) -> MqInput {
    MqInput {
        id,
        kind: MessageKind::Defer,
        source: MessageSource::SubAgentComplete,
        payload: PersistedPayload::SystemReminder {
            id,
            reminder: reminder(body),
        },
        policy: MessagePolicy::ensure_processing(),
        original_draft: None,
        plugin_selections: Vec::new(),
    }
}

/// 无结果终态：一次条件更新同时写终态与工作意图。
#[tokio::test]
async fn settle_terminal_without_result_commits_status_and_work_state() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    fixture
        .data
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();

    let outcome = fixture
        .data
        .settle_terminal_without_result(&child, AgentStatus::Done, WorkState::Idle)
        .await
        .unwrap();
    assert_eq!(outcome, TerminalSettlementOutcome::Settled);
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), Some("idle".to_owned()))
    );
}

/// 重复结算读取既有终态并返回 no-op，不改写工作意图。
#[tokio::test]
async fn settle_terminal_without_result_is_idempotent() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    fixture
        .data
        .settle_terminal_without_result(&child, AgentStatus::Done, WorkState::Idle)
        .await
        .unwrap();
    let replay = fixture
        .data
        .settle_terminal_without_result(&child, AgentStatus::Done, WorkState::Working)
        .await
        .unwrap();
    assert_eq!(
        replay,
        TerminalSettlementOutcome::AlreadyTerminal(AgentStatus::Done)
    );
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), Some("idle".to_owned())),
        "no-op 不得改写 work_state"
    );
}

/// child 不存在时按 NotFound 失败，不创建行。
#[tokio::test]
async fn settle_terminal_without_result_rejects_unknown_child() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = ThreadId::from("missing-child".to_owned());
    let error = fixture
        .data
        .settle_terminal_without_result(&child, AgentStatus::Error, WorkState::Idle)
        .await
        .unwrap_err();
    assert!(matches!(error.kind(), SessionResourceErrorKind::NotFound));
}

/// 后到的终态不能覆盖先提交的终态。
#[tokio::test]
async fn settle_terminal_without_result_does_not_overwrite_terminal() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    fixture
        .data
        .settle_terminal_without_result(&child, AgentStatus::Done, WorkState::Idle)
        .await
        .unwrap();
    let late = fixture
        .data
        .settle_terminal_without_result(&child, AgentStatus::Error, WorkState::Idle)
        .await
        .unwrap();
    assert_eq!(
        late,
        TerminalSettlementOutcome::AlreadyTerminal(AgentStatus::Done)
    );
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), Some("idle".to_owned()))
    );
}

/// 三步在同一提交里生效：重开后终态、工作意图与目标 MQ 行都能读到。
#[tokio::test]
async fn finalize_child_terminal_commits_status_work_state_and_result() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    let target = fixture.thread("parent").await;
    fixture
        .data
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();
    let id = MessageId::new();
    let outcome = fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Done,
            WorkState::Idle,
            &target,
            terminal_input(id, "child done"),
        )
        .await
        .unwrap();
    assert_eq!(outcome.disposition, MqEnqueueDisposition::Inserted);
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), Some("idle".to_owned()))
    );
    assert_eq!(mq_count(&fixture, &target).await, 1);
    assert_eq!(mq_count(&fixture, &child).await, 0);

    // 重试同一身份只复用原接纳结果，不产生第二行。
    let replay = fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Done,
            WorkState::Idle,
            &target,
            terminal_input(id, "child done"),
        )
        .await
        .unwrap();
    assert_eq!(replay.disposition, MqEnqueueDisposition::Existing);
    assert_eq!(mq_count(&fixture, &target).await, 1);
}

/// 结果接纳与接收方工作意图共用一次提交：EnsureProcessing 提升，passive 不授予。
#[tokio::test]
async fn finalize_child_terminal_maintains_receiver_work_intent() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    let target = fixture.thread("parent").await;
    fixture
        .data
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();

    fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Done,
            WorkState::Idle,
            &target,
            terminal_input(MessageId::new(), "child done"),
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.data.load_meta(&target).await.unwrap().work_state,
        WorkState::Working,
        "EnsureProcessing 结果必须在同一事务内提升接收方工作意图"
    );

    fixture
        .data
        .set_work_state(&target, WorkState::Idle)
        .await
        .unwrap();
    let mut passive = terminal_input(MessageId::new(), "passive result");
    passive.policy = MessagePolicy::passive();
    fixture
        .data
        .finalize_child_terminal(&child, AgentStatus::Done, WorkState::Idle, &target, passive)
        .await
        .unwrap();
    assert_eq!(
        fixture.data.load_meta(&target).await.unwrap().work_state,
        WorkState::Idle,
        "passive 结果不得授予执行资格"
    );
}

/// [回归] 结果接纳失败必须让整次结算回滚：终态已写、MQ 没收到结果正是原缺陷。
///
/// 用「同 ID 已属于别的 thread 历史」注入冲突——冲突只在最后一步出现，前两步若
/// 各自提交就会留在库里。
#[tokio::test]
async fn finalize_child_terminal_rolls_back_when_the_result_cannot_be_enqueued() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    let target = fixture.thread("parent").await;
    let foreign = fixture.thread("foreign").await;
    fixture
        .data
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();
    let id = MessageId::new();
    fixture
        .data
        .append_history(
            &foreign,
            &[PersistedPayload::Message(human(id, "already elsewhere"))],
        )
        .await
        .unwrap();

    let error = fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Done,
            WorkState::Idle,
            &target,
            terminal_input(id, "child done"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::Conflict { .. }),
        "{error:?}"
    );
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("active".to_owned(), Some("working".to_owned())),
        "终态与工作意图都不得越过失败的提交流传"
    );
    assert_eq!(mq_count(&fixture, &target).await, 0);
    assert_eq!(history_len(&fixture, &foreign).await, 1);
}

/// [回归] 同一 child 的终态是最终事实：panic 兜底以同一 task_id 但不同状态/正文
/// 重放结算时不得改写已写下的终态，也不得因 MQ 同 ID 不同正文而永久冲突。
#[tokio::test]
async fn terminal_settlement_is_final_for_a_child() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    let target = fixture.thread("parent").await;
    let id = MessageId::new();
    fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Done,
            WorkState::Idle,
            &target,
            terminal_input(id, "result A"),
        )
        .await
        .unwrap();

    // 旧实现：同 ID 不同正文在 `mq_enqueue_fact` 判 Conflict，整次结算回滚后无限重试。
    fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Error,
            WorkState::Idle,
            &target,
            terminal_input(id, "panic B"),
        )
        .await
        .expect("已写下的终态必须让重复结算成为成功 no-op");

    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), Some("idle".to_owned())),
        "第二次结算不得改写终态或工作意图"
    );
    let rows = fixture
        .data
        .mq_query(&target, MqFilter::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "不得新增结果行");
    match &rows[0].payload {
        PersistedPayload::SystemReminder { reminder, .. } => {
            assert_eq!(reminder.as_reminder().body, "result A");
        }
        other => panic!("unexpected payload {other:?}"),
    }
}

/// [回归] 同状态、不同正文的重复结算（panic 兜底）也必须被吞掉：判据是结果 ID
/// 已落地，而不是「状态是否相同」。
#[tokio::test]
async fn terminal_settlement_ignores_a_repeat_with_the_same_status() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    let target = fixture.thread("parent").await;
    let id = MessageId::new();
    fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Error,
            WorkState::Idle,
            &target,
            terminal_input(id, "result A"),
        )
        .await
        .unwrap();

    // 旧判据只看状态：同状态不早退，继续入队时同 ID 不同正文判 Conflict。
    fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Error,
            WorkState::Idle,
            &target,
            terminal_input(id, "panic B"),
        )
        .await
        .expect("结果 ID 已落地时重复结算必须成功 no-op");

    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("error".to_owned(), Some("idle".to_owned())),
        "重复结算不得改写终态或工作意图"
    );
    let rows = fixture
        .data
        .mq_query(&target, MqFilter::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "不得新增结果行");
    match &rows[0].payload {
        PersistedPayload::SystemReminder { reminder, .. } => {
            assert_eq!(reminder.as_reminder().body, "result A");
        }
        other => panic!("unexpected payload {other:?}"),
    }
}

/// 同一状态重放仍走原有幂等路径：两次都成功且只有一行。
#[tokio::test]
async fn terminal_settlement_stays_idempotent_for_the_same_status() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    let target = fixture.thread("parent").await;
    let id = MessageId::new();
    for _ in 0..2 {
        fixture
            .data
            .finalize_child_terminal(
                &child,
                AgentStatus::Done,
                WorkState::Idle,
                &target,
                terminal_input(id, "child done"),
            )
            .await
            .unwrap();
    }
    assert_eq!(mq_count(&fixture, &target).await, 1);
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), Some("idle".to_owned()))
    );
}

/// [回归] 关闭结算的第一步写成功、第二步失败时必须整批回滚。
///
/// 两条提醒里第二条身份冲突：旧实现「先写提醒、再删 MQ」会留下已写提醒 + 未删行。
#[tokio::test]
async fn settle_retained_mq_rolls_back_history_and_rows_together() {
    use peri_acp_types::session::RetainedReminder;

    let fixture = Fixture::new().await;
    let thread = fixture.thread("child").await;
    let entry = fixture
        .data
        .mq_enqueue(&thread, input(MessageId::new(), "retained"))
        .await
        .unwrap();
    let accepted = MessageId::new();
    fixture
        .data
        .append_history(
            &thread,
            &[PersistedPayload::Message(human(accepted, "already there"))],
        )
        .await
        .unwrap();
    let before = history_len(&fixture, &thread).await;

    let fresh = MessageId::new();
    let error = fixture
        .data
        .settle_retained_mq(
            &thread,
            &[
                RetainedReminder {
                    id: fresh,
                    reminder: reminder("new settlement"),
                },
                RetainedReminder {
                    id: accepted,
                    reminder: reminder("conflicting settlement"),
                },
            ],
            std::slice::from_ref(&entry.entry),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. }),
        "{error:?}"
    );
    assert_eq!(
        history_len(&fixture, &thread).await,
        before,
        "第一条提醒也不得写入"
    );
    assert_eq!(mq_count(&fixture, &thread).await, 1, "待发行原样保留");

    // 同样的输入去掉冲突项后整体生效：提醒写入且 MQ 行删除。
    fixture
        .data
        .settle_retained_mq(
            &thread,
            &[RetainedReminder {
                id: fresh,
                reminder: reminder("new settlement"),
            }],
            std::slice::from_ref(&entry.entry),
        )
        .await
        .unwrap();
    assert_eq!(history_len(&fixture, &thread).await, before + 1);
    assert_eq!(mq_count(&fixture, &thread).await, 0);
}
include!("session_mq_migration_test.rs");
