use super::*;

// ─── 入队与结算的批内原子性 ────────────────────────────────────────────────────

/// 让夹具回到「child 尚未结算」的状态，用于在同一 child 上制造重复/竞态结算。
async fn reset_child_active(fixture: &Fixture, child: &ThreadId) {
    sqlx::query("UPDATE threads SET agent_status = 'active', work_state = 'working' WHERE id = ?1")
        .bind(child.as_str())
        .execute(&fixture.pool)
        .await
        .unwrap();
}

/// 插入与提升同批：只让提升语句失败时，插入也必须一起回滚，不能留下 `idle + pending`。
#[tokio::test]
async fn remote_enqueue_inserted_promotes_in_the_same_batch() {
    let fixture = fixture().await;
    let thread = thread("mq-thread");
    fixture
        .data
        .set_work_state(&thread, WorkState::Idle)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_work_promote BEFORE UPDATE ON threads
         WHEN NEW.work_state = 'working'
         BEGIN SELECT RAISE(ABORT, 'injected promote failure'); END",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    let error = fixture
        .data
        .mq_enqueue(&thread, input(MessageId::new(), "wake"))
        .await
        .unwrap_err();
    // 触发器中止的 SDK 分类随驱动而定：这里只要求「确定未生效」，语义由回滚断言覆盖。
    assert!(
        matches!(
            error.kind(),
            SessionResourceErrorKind::InvalidInput { .. }
                | SessionResourceErrorKind::Unavailable { .. }
        ),
        "{error:?}"
    );
    assert_eq!(
        mq_rows(&fixture, &thread).await,
        0,
        "插入必须随提升失败一起回滚"
    );
    assert_eq!(work_state(&fixture, &thread).await, WorkState::Idle);
}

/// 整批在发出前丢失：不返回已接纳，也不留下任何行。
#[tokio::test]
async fn remote_enqueue_dropped_batch_is_not_accepted() {
    let fixture = fixture().await;
    let thread = thread("mq-thread");
    fixture
        .data
        .set_work_state(&thread, WorkState::Idle)
        .await
        .unwrap();
    fixture
        .data
        .inject_faults(FaultPlan {
            drop_before_send: Some("mq_enqueue".to_owned()),
            ..Default::default()
        })
        .await;

    let error = fixture
        .data
        .mq_enqueue(&thread, input(MessageId::new(), "wake"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            SessionResourceErrorKind::PersistenceUncertain { .. }
        ),
        "{error:?}"
    );
    assert_eq!(mq_rows(&fixture, &thread).await, 0);
    assert_eq!(work_state(&fixture, &thread).await, WorkState::Idle);
}

/// Existing 路径的提升失败必须 fail-closed：返回可重试错误，调用方用同一 ID 重试后变
/// `working`，不把「已接纳、意图未落地」伪装成成功。
#[tokio::test]
async fn remote_enqueue_existing_promotion_failure_returns_error_then_retry_promotes() {
    let fixture = fixture().await;
    let thread = thread("mq-thread");
    let id = MessageId::new();
    fixture
        .data
        .set_work_state(&thread, WorkState::Idle)
        .await
        .unwrap();
    fixture
        .data
        .mq_enqueue(&thread, input(id, "wake"))
        .await
        .unwrap();
    fixture
        .data
        .set_work_state(&thread, WorkState::Idle)
        .await
        .unwrap();
    fixture
        .data
        .inject_faults(FaultPlan {
            drop_before_send: Some("work_state_promote".to_owned()),
            ..Default::default()
        })
        .await;

    let error = fixture
        .data
        .mq_enqueue(&thread, input(id, "wake"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            SessionResourceErrorKind::PersistenceUncertain { .. }
        ),
        "提升失败必须是可重试错误，而不是接纳成功: {error:?}"
    );
    assert_eq!(mq_rows(&fixture, &thread).await, 1, "同 ID 不产生第二行");
    assert_eq!(work_state(&fixture, &thread).await, WorkState::Idle);

    let retry = fixture
        .data
        .mq_enqueue(&thread, input(id, "wake"))
        .await
        .unwrap();
    assert_eq!(retry.disposition, MqEnqueueDisposition::Existing);
    assert_eq!(work_state(&fixture, &thread).await, WorkState::Working);
}

/// 结算批在发出前丢失：终态、意图与结果都不落库。
#[tokio::test]
async fn remote_finalize_is_single_batch_before_send() {
    let fixture = fixture().await;
    let child = thread("other-thread");
    let target = thread("mq-thread");
    fixture
        .data
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();
    fixture
        .data
        .inject_faults(FaultPlan {
            drop_before_send: Some("finalize_child_terminal".to_owned()),
            ..Default::default()
        })
        .await;

    let error = fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Done,
            WorkState::Idle,
            &target,
            terminal_input(MessageId::new(), "child done"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            SessionResourceErrorKind::PersistenceUncertain { .. }
        ),
        "{error:?}"
    );
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("active".to_owned(), "working".to_owned())
    );
    assert_eq!(mq_rows(&fixture, &target).await, 0);
}

/// 结果插入失败时终态与工作意图必须一起回滚：不能留下「终态已写、结果未入队」。
#[tokio::test]
async fn remote_finalize_rolls_back_when_the_result_insert_fails() {
    let fixture = fixture().await;
    let child = thread("other-thread");
    let target = thread("mq-thread");
    let id = MessageId::new();
    fixture
        .data
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();
    // 语句文本只由本测试的固定前缀与 UUID 组成，不含外部输入。
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE TRIGGER fail_result_insert BEFORE INSERT ON thread_mq
         WHEN NEW.id = '{}'
         BEGIN SELECT RAISE(ABORT, 'injected result failure'); END",
        id.as_uuid()
    )))
    .execute(&fixture.pool)
    .await
    .unwrap();

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
        .unwrap_err();
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("active".to_owned(), "working".to_owned()),
        "终态与工作意图不得越过失败的批次"
    );
    assert_eq!(mq_rows(&fixture, &target).await, 0);
    assert_eq!(history_rows(&fixture, &target).await, 0);
}

/// 批次已提交但响应丢失：返回未决；同一 ID 重试读到已落地事实，不重复写结果。
#[tokio::test]
async fn remote_finalize_unknown_reply_is_retryable() {
    let fixture = fixture().await;
    let child = thread("other-thread");
    let target = thread("mq-thread");
    let id = MessageId::new();
    fixture
        .data
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();
    fixture
        .data
        .inject_faults(FaultPlan {
            drop_reply: Some("finalize_child_terminal".to_owned()),
            ..Default::default()
        })
        .await;

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
        matches!(
            error.kind(),
            SessionResourceErrorKind::PersistenceUncertain { .. }
        ),
        "{error:?}"
    );

    let retry = fixture
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
    assert_eq!(retry.disposition, MqEnqueueDisposition::Existing);
    assert_eq!(mq_rows(&fixture, &target).await, 1, "不得重复接纳结果");
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), "idle".to_owned())
    );
}

/// 结算维护接收方工作意图：EnsureProcessing 提升、passive 不提升、stopped 不解除。
#[tokio::test]
async fn remote_finalize_maintains_receiver_work_intent() {
    let fixture = fixture().await;
    let child = thread("other-thread");
    let target = thread("mq-thread");

    fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Done,
            WorkState::Idle,
            &target,
            terminal_input(MessageId::new(), "ensure"),
        )
        .await
        .unwrap();
    assert_eq!(work_state(&fixture, &target).await, WorkState::Working);

    fixture
        .data
        .set_work_state(&target, WorkState::Idle)
        .await
        .unwrap();
    reset_child_active(&fixture, &child).await;
    let mut passive = terminal_input(MessageId::new(), "passive");
    passive.policy = MessagePolicy::passive();
    fixture
        .data
        .finalize_child_terminal(&child, AgentStatus::Done, WorkState::Idle, &target, passive)
        .await
        .unwrap();
    assert_eq!(work_state(&fixture, &target).await, WorkState::Idle);

    fixture
        .data
        .set_work_state(&target, WorkState::Stopped)
        .await
        .unwrap();
    reset_child_active(&fixture, &child).await;
    fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Done,
            WorkState::Idle,
            &target,
            terminal_input(MessageId::new(), "stopped stays"),
        )
        .await
        .unwrap();
    assert_eq!(work_state(&fixture, &target).await, WorkState::Stopped);
}

/// 终态只写一次：同一 child 先 Done 再 Error（不同结果 ID）。后到的结算既不能覆盖终态，
/// 也不能再投递一条结果；结果未落地时按批后读取归类为冲突。
#[tokio::test]
async fn remote_finalize_later_terminal_does_not_overwrite_the_first() {
    let fixture = fixture().await;
    let child = thread("other-thread");
    let target = thread("mq-thread");
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
    fixture
        .data
        .set_work_state(&target, WorkState::Idle)
        .await
        .unwrap();

    let error = fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Error,
            WorkState::Working,
            &target,
            terminal_input(MessageId::new(), "child failed"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::Conflict { .. }),
        "先提交的终态已生效且新结果未落地: {error:?}"
    );
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), "idle".to_owned()),
        "后到的 Error 不得覆盖已提交的 Done"
    );
    assert_eq!(mq_rows(&fixture, &target).await, 1, "父 MQ 只能有一条结果");
    assert_eq!(
        work_state(&fixture, &target).await,
        WorkState::Idle,
        "后到的结算不得提升接收方意图"
    );
}

/// 批前读到 `active`、批间另一个结算提交：本批的结果插入与终态更新都因守卫失效，
/// 批后按读取归类为 `Existing`；终态与结果行都只保留先提交的那一份。
#[tokio::test]
async fn remote_finalize_does_not_overwrite_a_settlement_committed_after_the_read() {
    let fixture = fixture().await;
    let child = thread("other-thread");
    let target = thread("mq-thread");
    let id = MessageId::new();
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
    // 把夹具恢复到「先提交的结算尚未发生」：批前读取因此看到 active/working。
    reset_child_active(&fixture, &child).await;
    fixture
        .data
        .set_work_state(&target, WorkState::Idle)
        .await
        .unwrap();
    // 批前读取之后、本批提交之前，另一个结算落下同一 child 的终态。
    *fixture.transport.before_batch.lock().unwrap() = Some(format!(
        "UPDATE threads SET agent_status = 'done', work_state = 'idle' WHERE id = '{}'",
        child.as_str()
    ));

    let replay = fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Error,
            WorkState::Working,
            &target,
            terminal_input(id, "child done"),
        )
        .await
        .unwrap();
    assert_eq!(replay.disposition, MqEnqueueDisposition::Existing);
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), "idle".to_owned()),
        "批间提交的终态不得被后到批次覆盖"
    );
    assert_eq!(mq_rows(&fixture, &target).await, 1, "不得重复接纳结果");
    assert_eq!(work_state(&fixture, &target).await, WorkState::Idle);
}

/// 结果投递回 child 自己（恢复期）不构成新的接收方，不得把刚写的终态重新提升为 `working`。
#[tokio::test]
async fn remote_finalize_target_equals_child_keeps_work_state() {
    let fixture = fixture().await;
    let child = thread("other-thread");
    fixture
        .data
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();

    let outcome = fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Done,
            WorkState::Idle,
            &child,
            terminal_input(MessageId::new(), "self delivery"),
        )
        .await
        .unwrap();
    assert_eq!(outcome.disposition, MqEnqueueDisposition::Inserted);
    assert_eq!(mq_rows(&fixture, &child).await, 1);
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), "idle".to_owned())
    );
}

/// 同 ID 结果已落地：重复结算（甚至状态与正文不同）必须成功 no-op，不改写任何事实。
#[tokio::test]
async fn remote_finalize_is_idempotent_once_landed() {
    let fixture = fixture().await;
    let child = thread("other-thread");
    let target = thread("mq-thread");
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
    fixture
        .data
        .set_work_state(&target, WorkState::Idle)
        .await
        .unwrap();

    let replay = fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Error,
            WorkState::Working,
            &target,
            terminal_input(id, "panic B"),
        )
        .await
        .unwrap();
    assert_eq!(replay.disposition, MqEnqueueDisposition::Existing);
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), "idle".to_owned()),
        "已落地结算的重复调用不得改写终态或工作意图"
    );
    assert_eq!(work_state(&fixture, &target).await, WorkState::Idle);
    assert_eq!(mq_rows(&fixture, &target).await, 1);
}

/// 结果 ID 已属于别的 thread：冲突在批内被守卫拦下，终态与工作意图都不得提交。
#[tokio::test]
async fn remote_finalize_conflict_does_not_write_terminal_state() {
    let fixture = fixture().await;
    let child = thread("child-thread");
    let target = thread("mq-thread");
    let foreign = thread("other-thread");
    sqlx::query(
        "INSERT INTO threads (id, cwd, created_at, updated_at, workspace_id)
         VALUES ('child-thread', '/tmp', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'ws')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
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
        ("active".to_owned(), "working".to_owned())
    );
    assert_eq!(mq_rows(&fixture, &target).await, 0);
}

/// 批前读取与提交之间的冲突由批内守卫拦下：终态不得越过守卫提交。
#[tokio::test]
async fn remote_finalize_guards_a_conflict_that_appears_after_the_read() {
    let fixture = fixture().await;
    let child = thread("child-thread");
    let target = thread("mq-thread");
    let foreign = thread("other-thread");
    sqlx::query(
        "INSERT INTO threads (id, cwd, created_at, updated_at, workspace_id)
         VALUES ('child-thread', '/tmp', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'ws')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    fixture
        .data
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();
    let id = MessageId::new();
    *fixture.transport.before_batch.lock().unwrap() = Some(format!(
        "INSERT INTO messages (message_id, thread_id, role, content)
         VALUES ('{}', '{}', 'user', 'raced elsewhere')",
        id.as_uuid(),
        foreign.as_str()
    ));

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
        ("active".to_owned(), "working".to_owned())
    );
    assert_eq!(mq_rows(&fixture, &target).await, 0);
}

/// 无结果终态：单批条件更新同时写终态与工作意图。
#[tokio::test]
async fn remote_settle_terminal_without_result_commits_status_and_work_state() {
    use peri_acp_types::session_resources::TerminalSettlementOutcome;

    let fixture = fixture().await;
    let child = thread("other-thread");
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
        ("done".to_owned(), "idle".to_owned())
    );
}

/// 重复结算不产生第二次写入，也不覆盖既有终态。
#[tokio::test]
async fn remote_settle_terminal_without_result_duplicate_is_noop() {
    use peri_acp_types::session_resources::TerminalSettlementOutcome;

    let fixture = fixture().await;
    let child = thread("other-thread");
    fixture
        .data
        .settle_terminal_without_result(&child, AgentStatus::Done, WorkState::Idle)
        .await
        .unwrap();
    let replay = fixture
        .data
        .settle_terminal_without_result(&child, AgentStatus::Error, WorkState::Working)
        .await
        .unwrap();
    assert_eq!(
        replay,
        TerminalSettlementOutcome::AlreadyTerminal(AgentStatus::Done)
    );
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), "idle".to_owned())
    );
}

/// 批内会话守卫拒绝不存在的 child，并按 NotFound 分类。
#[tokio::test]
async fn remote_settle_terminal_without_result_rejects_missing_child() {
    let fixture = fixture().await;
    let child = thread("missing-child");
    let error = fixture
        .data
        .settle_terminal_without_result(&child, AgentStatus::Done, WorkState::Idle)
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::NotFound),
        "{error:?}"
    );
}
