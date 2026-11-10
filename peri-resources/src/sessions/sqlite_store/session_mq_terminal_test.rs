//! 终态结算的守卫回归：同一 child 的终态只写一次，后到的结算不得覆盖或追加结果。
//!
//! 夹具与输入构造复用父模块 `session_mq_test`；这里只覆盖 `active` 守卫、不同结果 ID
//! 与跨 thread 冲突这些与本机单事务形状直接相关的路径。

use super::*;

/// 先 Done + 结果 A，再 Error + 结果 B（不同 ID）：终态与结果都保留先提交的那一份。
#[tokio::test]
async fn finalize_does_not_overwrite_terminal_with_a_different_result_id() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    let target = fixture.thread("parent").await;
    let first = MessageId::new();
    let second = MessageId::new();
    fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Done,
            WorkState::Idle,
            &target,
            terminal_input(first, "result A"),
        )
        .await
        .unwrap();

    let late = fixture
        .data
        .finalize_child_terminal(
            &child,
            AgentStatus::Error,
            WorkState::Working,
            &target,
            terminal_input(second, "result B"),
        )
        .await
        .unwrap();
    assert_eq!(
        late.disposition,
        MqEnqueueDisposition::Existing,
        "已终态的不同结果 ID 只能成功 no-op"
    );
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), Some("idle".to_owned())),
        "后到的结算不得改写终态或工作意图"
    );
    assert_eq!(mq_count(&fixture, &target).await, 1, "不得写第二条结果");

    let rows = fixture
        .data
        .mq_query(&target, MqFilter::default())
        .await
        .unwrap();
    match &rows[0].payload {
        PersistedPayload::SystemReminder { reminder, .. } => {
            assert_eq!(reminder.as_reminder().body, "result A");
        }
        other => panic!("unexpected payload {other:?}"),
    }
}

/// 同一结果 ID 重复结算仍是幂等 no-op（判据是 ID 已落地，不是状态相同）。
#[tokio::test]
async fn finalize_replaying_the_same_result_id_is_a_noop() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    let target = fixture.thread("parent").await;
    let id = MessageId::new();
    for (status, body) in [
        (AgentStatus::Done, "result A"),
        (AgentStatus::Error, "panic B"),
    ] {
        fixture
            .data
            .finalize_child_terminal(
                &child,
                status,
                WorkState::Idle,
                &target,
                terminal_input(id, body),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), Some("idle".to_owned()))
    );
    assert_eq!(mq_count(&fixture, &target).await, 1);
}

/// 结果投递回 child 自己（恢复期自投递）不是新的接收方，不得提升工作意图。
#[tokio::test]
async fn finalize_target_equals_child_does_not_promote_receiver() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    fixture
        .data
        .set_work_state(&child, WorkState::Idle)
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
    assert_eq!(mq_count(&fixture, &child).await, 1);
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("done".to_owned(), Some("idle".to_owned())),
        "终态 work_state 不得被接收方提升改写"
    );
}

/// 结果 ID 已在别的 thread 的 MQ：整次结算冲突回滚，终态不得越过守卫提交。
#[tokio::test]
async fn finalize_conflicts_when_the_result_id_is_pending_in_another_thread() {
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
        .mq_enqueue(&foreign, input(id, "already pending elsewhere"))
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
        "冲突不得留下终态或工作意图"
    );
    assert_eq!(mq_count(&fixture, &target).await, 0);
    assert_eq!(mq_count(&fixture, &foreign).await, 1);
}

/// 终态 + 残留 `working`：迟到结算只收敛工作意图，不覆盖终态、不新增结果行。
#[tokio::test]
async fn late_finalize_converges_terminal_child_residual_working() {
    use peri_acp_types::thread::{AgentStatus, WorkState};

    for (stored, expected) in [
        (AgentStatus::Done, WorkState::Idle),
        (AgentStatus::Error, WorkState::Idle),
        (AgentStatus::Cancelled, WorkState::Stopped),
    ] {
        let fixture = Fixture::new().await;
        let child = fixture.thread("child").await;
        let target = fixture.thread("parent").await;
        // 模拟旧路径只写 agent_status、留下 working 的终态。
        sqlx::query("UPDATE threads SET agent_status = ?1, work_state = 'working' WHERE id = ?2")
            .bind(stored.as_str())
            .bind(child.as_str())
            .execute(&fixture.store.database.pool)
            .await
            .unwrap();

        let outcome = fixture
            .data
            .finalize_child_terminal(
                &child,
                AgentStatus::Done,
                WorkState::Idle,
                &target,
                terminal_input(MessageId::new(), "late result"),
            )
            .await
            .unwrap();
        assert_eq!(outcome.disposition, MqEnqueueDisposition::Existing);
        assert_eq!(
            terminal_facts(&fixture, &child).await,
            (
                stored.as_str().to_owned(),
                Some(expected.as_str().to_owned())
            ),
            "{stored:?}: 终态保持不变，工作意图收敛到派生值"
        );
        assert_eq!(mq_count(&fixture, &target).await, 0, "不得新增结果行");
        assert_eq!(history_len(&fixture, &target).await, 0, "不得写历史");
    }
}

/// 无结果终态的重复结算同样收敛残留 `working`，不改写已提交终态。
#[tokio::test]
async fn late_settle_without_result_converges_terminal_child_residual_working() {
    use peri_acp_types::session_resources::TerminalSettlementOutcome;
    use peri_acp_types::thread::{AgentStatus, WorkState};

    let fixture = Fixture::new().await;
    let child = fixture.thread("child").await;
    sqlx::query("UPDATE threads SET agent_status = 'error', work_state = 'working' WHERE id = ?1")
        .bind(child.as_str())
        .execute(&fixture.store.database.pool)
        .await
        .unwrap();

    let outcome = fixture
        .data
        .settle_terminal_without_result(&child, AgentStatus::Done, WorkState::Idle)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        TerminalSettlementOutcome::AlreadyTerminal(AgentStatus::Error)
    );
    assert_eq!(
        terminal_facts(&fixture, &child).await,
        ("error".to_owned(), Some("idle".to_owned())),
        "已提交终态不被覆盖，working 收敛为 idle"
    );
}
