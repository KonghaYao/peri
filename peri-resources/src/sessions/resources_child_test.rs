use super::*;

/// 旧补偿延迟到新任务 `Active + Working` 之后到达时，不得覆盖新任务的工作意图。
///
/// 新任务的正常中间态与旧认领写入的值完全相同，按记录值判断归属会把新任务回滚成
/// `Done + Stopped`；归属由进程内认领世代区分。
#[tokio::test]
async fn late_resume_rollback_preserves_new_working() {
    use peri_acp_types::thread::WorkState;
    let fixture = Fixture::new().await;
    fixture.create("s-late-rollback-root").await;
    let child = fixture
        .child("s-late-rollback-child", "s-late-rollback-root")
        .await;
    let child_id = child.target.thread_id;
    let root_id = child.root_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Stopped)
        .await
        .unwrap();

    // 旧认领：旧 run 已工作并写入终态，工作意图回滚尚未执行。
    let old = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    old.hand_off_to_background().await.unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Working)
        .await
        .unwrap();
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // 新任务在旧补偿到达前完成认领并提交 Working。
    let new = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    new.hand_off_to_background().await.unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Working)
        .await
        .unwrap();
    let before = fixture.facade.load_session_meta(&child_id).await.unwrap();
    assert_eq!(before.agent_status, AgentStatus::Active);
    assert_eq!(before.work_state, WorkState::Working);

    // 迟到的旧补偿：当前值看起来与自己的中间态一致，但世代已被新认领覆盖。
    old.mark_failed().await.unwrap();

    let after = fixture.facade.load_session_meta(&child_id).await.unwrap();
    assert_eq!(after.agent_status, AgentStatus::Active);
    assert_eq!(
        after.work_state,
        WorkState::Working,
        "a stale resume rollback must not clear the newer run's durable working intent"
    );
}

/// 状态列已被改写时，不得只回滚工作意图列。
#[tokio::test]
async fn rollback_skips_both_columns_when_status_changed() {
    use peri_acp_types::thread::WorkState;
    let fixture = Fixture::new().await;
    fixture.create("s-partial-rollback-root").await;
    let child = fixture
        .child("s-partial-rollback-child", "s-partial-rollback-root")
        .await;
    let child_id = child.target.thread_id;
    let root_id = child.root_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Stopped)
        .await
        .unwrap();

    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    claim.hand_off_to_background().await.unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Working)
        .await
        .unwrap();
    // 新任务只改写了状态列。
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    claim.mark_failed().await.unwrap();
    let after = fixture.facade.load_session_meta(&child_id).await.unwrap();
    assert_eq!(after.agent_status, AgentStatus::Done);
    assert_eq!(
        after.work_state,
        WorkState::Working,
        "compensation must not restore only the work-state column"
    );
}

/// 工作意图列已被改写时，不得只回滚状态列。
#[tokio::test]
async fn rollback_skips_both_columns_when_work_state_changed() {
    use peri_acp_types::thread::WorkState;
    let fixture = Fixture::new().await;
    fixture.create("s-partial-work-root").await;
    let child = fixture
        .child("s-partial-work-child", "s-partial-work-root")
        .await;
    let child_id = child.target.thread_id;
    let root_id = child.root_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Stopped)
        .await
        .unwrap();

    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    claim.hand_off_to_background().await.unwrap();
    // 新任务只改写了工作意图列。
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Idle)
        .await
        .unwrap();
    claim.mark_failed().await.unwrap();
    let after = fixture.facade.load_session_meta(&child_id).await.unwrap();
    assert_eq!(
        after.agent_status,
        AgentStatus::Active,
        "compensation must not restore only the status column"
    );
    assert_eq!(after.work_state, WorkState::Idle);
}

#[tokio::test]
async fn test_save_child_preserves_parent_snapshot_and_tree() {
    let fixture = Fixture::new().await;
    fixture.create("s-child-root").await;
    let child = fixture.child("s-child", "s-child-root").await;
    let snapshot = fixture
        .facade
        .load_session_snapshot(&child.target.thread_id)
        .await
        .unwrap();
    assert_eq!(
        snapshot.meta.parent_thread_id.as_deref(),
        Some("s-child-root")
    );
    assert_eq!(snapshot.frozen, FrozenState::Present(child.target.frozen));
    assert_eq!(
        fixture
            .facade
            .list_session_tree(&"s-child-root".to_owned())
            .await
            .unwrap()
            .len(),
        2
    );
    fixture
        .facade
        .append_history(&"s-child".to_owned(), &[payload("child turn")])
        .await
        .unwrap();
    assert_eq!(fixture.count_messages("s-child").await, 1);
}

#[tokio::test]
async fn test_save_child_write_waits_for_the_root_gate() {
    let fixture = Fixture::new().await;
    fixture.create("s-gate-root").await;
    let snapshot = fixture.child_snapshot("s-gate-child", "s-gate-root").await;
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
    let facade = fixture.facade.clone();
    let held = tokio::spawn(async move {
        facade
            .gate
            .with_exclusive(&"s-gate-root".to_owned(), || async move {
                started_sender.send(()).unwrap();
                release_receiver.await.unwrap();
                Ok(())
            })
            .await
    });
    started_receiver.await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            fixture.facade.save_child(&snapshot),
        )
        .await
        .is_err(),
        "child save must wait for the root's write gate"
    );
    assert_eq!(
        fixture.count_threads("s-gate-child").await,
        0,
        "no child data may be written while the root gate is held"
    );
    release_sender.send(()).unwrap();
    held.await.unwrap().unwrap();

    // 门禁释放后同一次保存成立，且没有把 root 标成未决（结清只认确定性）。
    fixture.facade.save_child(&snapshot).await.unwrap();
    assert_eq!(fixture.count_threads("s-gate-child").await, 1);
}

/// 父子关系在 child 快照里出现两次：`parent_id`（声明）与 `target.meta.parent_thread_id`
/// （落库用的那一份）。只校验前者、照后者落库，会把「声明了合法父/根」的 child 写成一条
#[tokio::test]
async fn test_save_child_refuses_a_snapshot_that_disagrees_with_its_parent_relation() {
    let fixture = Fixture::new().await;
    fixture.create("s-rel-root").await;
    let legal = fixture.child_snapshot("s-rel-child", "s-rel-root").await;

    // 目标 meta 里没有父；父与声明不同；自指父关系；把自己当根。
    let mut without_parent = legal.clone();
    without_parent.target.meta.parent_thread_id = None;
    let mut other_parent = legal.clone();
    other_parent.target.meta.parent_thread_id = Some("s-rel-other".to_owned());
    let mut self_parent = legal.clone();
    self_parent.parent_id = "s-rel-child".to_owned();
    self_parent.target.meta.parent_thread_id = Some("s-rel-child".to_owned());
    let mut own_root = legal.clone();
    own_root.root_id = "s-rel-child".to_owned();

    for (label, snapshot) in [
        ("target without a parent", &without_parent),
        ("target under another parent", &other_parent),
        ("self parent", &self_parent),
        ("own root", &own_root),
    ] {
        let error = fixture.facade.save_child(snapshot).await.unwrap_err();
        assert!(
            matches!(
                error_kind(&error),
                SessionResourceErrorKind::InvalidInput { .. }
            ),
            "{label}: expected InvalidInput, got {error:?}"
        );
        assert_eq!(fixture.count_threads("s-rel-child").await, 0, "{label}");
        assert_eq!(fixture.count_bindings("s-rel-child").await, 0, "{label}");
    }
    let root_meta = fixture
        .facade
        .load_session_meta(&"s-rel-root".to_owned())
        .await
        .unwrap();
    assert_eq!(root_meta.parent_thread_id, None);
    assert_eq!(
        fixture
            .facade
            .list_session_tree(&"s-rel-root".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(fixture
        .facade
        .list_children(&"s-rel-root".to_owned())
        .await
        .unwrap()
        .is_empty());
    fixture
        .facade
        .append_history(&"s-rel-root".to_owned(), &[payload("root turn")])
        .await
        .unwrap();

    fixture.facade.save_child(&legal).await.unwrap();
    assert_eq!(fixture.count_threads("s-rel-child").await, 1);
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&"s-rel-child".to_owned())
            .await
            .unwrap()
            .parent_thread_id
            .as_deref(),
        Some("s-rel-root")
    );
    assert_eq!(
        fixture
            .facade
            .list_children(&"s-rel-root".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn test_claim_child_resume_serializes_and_restores_previous_state() {
    let fixture = Fixture::new().await;
    fixture.create("s-claim-root").await;
    let child = fixture.child("s-claim-child", "s-claim-root").await;
    let child_id = child.target.thread_id.clone();
    let root_id = child.root_id.clone();
    // 认领前的状态：Done（非 active），用于观察恢复是否真的发生了。
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    let record = fixture
        .facade
        .gate
        .data()
        .load_child_resume_record(&child_id)
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Active);
    assert!(record.claimed);
    // 仍在 active：并发/重复认领被拒绝，不会有两个执行者。
    let error = match fixture.facade.claim_child_resume(&child_id, &root_id).await {
        Ok(_) => panic!("expected the active child to refuse a second claim"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
    // 准备失败：恢复到认领前，不留 active 残留。
    claim.mark_failed().await.unwrap();
    let record = fixture
        .facade
        .gate
        .data()
        .load_child_resume_record(&child_id)
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Done);
    assert!(!record.claimed);

    // 移交后台后，前台的终止声明不能覆盖后台持有的终态。
    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    claim.hand_off_to_background().await.unwrap();
    let error = claim.mark_terminated().await.unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
}

#[tokio::test]
async fn test_background_handoff_preparation_failure_restores_previous_claim_record() {
    let fixture = Fixture::new().await;
    fixture.create("s-handoff-root").await;
    let child = fixture.child("s-handoff-child", "s-handoff-root").await;
    let child_id = child.target.thread_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &"s-handoff-root".to_owned())
        .await
        .unwrap();
    claim.hand_off_to_background().await.unwrap();
    claim.mark_failed().await.unwrap();
    claim.mark_failed().await.unwrap();
    let record = fixture
        .facade
        .gate
        .data()
        .load_child_resume_record(&child_id)
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Done);
    assert!(!record.claimed);
    fixture
        .facade
        .claim_child_resume(&child_id, &"s-handoff-root".to_owned())
        .await
        .unwrap();
}

/// [回归] 补偿写入失败必须保留认领：状态与工作意图原子落库，同一 handle 恢复后
/// 仍能完成补偿。旧实现在状态回滚已提交、工作意图写入失败后把重试判成 NotOwned，
/// 留下 `Cancelled + Working` 并释放认领。
#[tokio::test]
async fn claim_rollback_failure_keeps_claim_for_same_handle_retry() {
    use peri_acp_types::session_resources::MutationOutcome;
    use peri_acp_types::thread::WorkState;
    let fixture = Fixture::new().await;
    fixture.create("s-rollback-fail-root").await;
    let child = fixture
        .child("s-rollback-fail-child", "s-rollback-fail-root")
        .await;
    let child_id = child.target.thread_id;
    let root_id = child.root_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Cancelled),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Stopped)
        .await
        .unwrap();

    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Working)
        .await
        .unwrap();
    // 只拒绝工作意图写回：状态列与工作意图必须同生共死，不能先提交状态。
    sqlx::raw_sql(
        "CREATE TRIGGER reject_work_state_rollback BEFORE UPDATE OF work_state ON threads
         WHEN NEW.work_state = 'stopped'
         BEGIN SELECT RAISE(ABORT, 'work state rollback blocked'); END;",
    )
    .execute(fixture.facade.local_pool())
    .await
    .unwrap();

    let error = claim.mark_failed().await.unwrap_err();
    assert_eq!(error.effect(), MutationOutcome::NotApplied);
    assert!(
        fixture.facade.gate.claim_is_tracked(&child_id),
        "失败且未结算的认领必须保留世代"
    );
    let record = fixture
        .facade
        .gate
        .data()
        .load_child_resume_record(&child_id)
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Active);
    assert!(record.claimed, "失败后必须保留认领态供同一 handle 重试");
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&child_id)
            .await
            .unwrap()
            .work_state,
        WorkState::Working,
        "失败不得留下已回滚的状态列"
    );

    // 故障移除后同一 handle 完成整次补偿；旧实现会在这里因 NotOwned 而静默失败。
    sqlx::raw_sql("DROP TRIGGER reject_work_state_rollback")
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();
    claim.mark_failed().await.unwrap();
    let record = fixture
        .facade
        .gate
        .data()
        .load_child_resume_record(&child_id)
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Cancelled);
    assert!(!record.claimed);
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&child_id)
            .await
            .unwrap()
            .work_state,
        WorkState::Stopped
    );
}

/// [回归] 终态写入失败同样不能丢掉重试责任：恢复与终态在同一提交里完成。
#[tokio::test]
async fn terminal_write_failure_keeps_claim_for_same_handle_retry() {
    use peri_acp_types::session_resources::MutationOutcome;
    use peri_acp_types::thread::WorkState;
    let fixture = Fixture::new().await;
    fixture.create("s-terminal-fail-root").await;
    let child = fixture
        .child("s-terminal-fail-child", "s-terminal-fail-root")
        .await;
    let child_id = child.target.thread_id;
    let root_id = child.root_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Cancelled),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Stopped)
        .await
        .unwrap();

    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Working)
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TRIGGER reject_terminal_write BEFORE UPDATE OF agent_status ON threads
         WHEN NEW.agent_status = 'done'
         BEGIN SELECT RAISE(ABORT, 'terminal write blocked'); END;",
    )
    .execute(fixture.facade.local_pool())
    .await
    .unwrap();

    let error = claim
        .finish_with_status(AgentStatus::Done)
        .await
        .unwrap_err();
    assert_eq!(error.effect(), MutationOutcome::NotApplied);
    let record = fixture
        .facade
        .gate
        .data()
        .load_child_resume_record(&child_id)
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Active);
    assert!(record.claimed, "终态未写入时认领仍属于本次 handle");
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&child_id)
            .await
            .unwrap()
            .work_state,
        WorkState::Working,
        "终态失败不得留下已回滚的工作意图"
    );

    sqlx::raw_sql("DROP TRIGGER reject_terminal_write")
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();
    assert!(
        claim.finish_with_status(AgentStatus::Done).await.unwrap(),
        "故障移除后同一 handle 必须能补写终态"
    );
    let record = fixture
        .facade
        .gate
        .data()
        .load_child_resume_record(&child_id)
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Done);
    assert!(!record.claimed);
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&child_id)
            .await
            .unwrap()
            .work_state,
        WorkState::Stopped
    );
}

/// 记录回滚结果未知时不追加任何写入：working 与不确定屏障都必须保留。
#[tokio::test]
async fn claim_rollback_unknown_outcome_keeps_working_and_barrier() {
    use peri_acp_types::thread::WorkState;
    let fixture = Fixture::new().await;
    fixture.create("s-rollback-unknown-root").await;
    let child = fixture
        .child("s-rollback-unknown-child", "s-rollback-unknown-root")
        .await;
    let child_id = child.target.thread_id;
    let root_id = child.root_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Stopped)
        .await
        .unwrap();

    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Working)
        .await
        .unwrap();
    // 延迟外键在 COMMIT 阶段失败：回滚是否落盘无法证明，效果必须是 Unknown。
    sqlx::raw_sql(
        "CREATE TABLE rollback_guard (
             id TEXT PRIMARY KEY,
             thread_id TEXT REFERENCES threads(id) DEFERRABLE INITIALLY DEFERRED
         )",
    )
    .execute(fixture.facade.local_pool())
    .await
    .unwrap();
    sqlx::raw_sql(
        "CREATE TRIGGER reject_record_rollback AFTER UPDATE OF agent_status ON threads
         WHEN NEW.agent_status = 'done'
         BEGIN
             INSERT INTO rollback_guard (id, thread_id)
             VALUES ('rollback', 'missing-thread');
         END;",
    )
    .execute(fixture.facade.local_pool())
    .await
    .unwrap();

    let error = claim.mark_failed().await.unwrap_err();
    assert!(error.is_persistence_uncertain(), "{error:?}");
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&child_id)
            .await
            .unwrap()
            .work_state,
        WorkState::Working,
        "未知结果不得追加写入工作意图"
    );
    let blocked = fixture
        .facade
        .set_work_state(&child_id, WorkState::Stopped)
        .await
        .unwrap_err();
    assert!(
        blocked.is_persistence_uncertain(),
        "未知结果必须保留不确定屏障"
    );
    assert!(
        fixture.facade.gate.claim_is_tracked(&child_id),
        "失败且未结算的认领必须保留世代"
    );
}

/// 结算回收认领世代；迟到结算不得移除新认领的世代，新认领仍能正常建立。
#[tokio::test]
async fn settled_claim_releases_ledger_without_dropping_newer_generation() {
    let fixture = Fixture::new().await;
    fixture.create("s-ledger-root").await;
    let child = fixture.child("s-ledger-child", "s-ledger-root").await;
    let child_id = child.target.thread_id;
    let root_id = child.root_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let first = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    assert!(fixture.facade.gate.claim_is_tracked(&child_id));
    first.mark_failed().await.unwrap();
    assert!(
        !fixture.facade.gate.claim_is_tracked(&child_id),
        "结算后必须回收认领世代"
    );

    // 新认领覆盖世代后，旧 handle 的迟到结算不得移除新条目。
    let old = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    old.hand_off_to_background().await.unwrap();
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let new = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    old.mark_failed().await.unwrap();
    assert!(
        fixture.facade.gate.claim_is_tracked(&child_id),
        "旧 handle 不得移除新认领的世代"
    );
    new.mark_failed().await.unwrap();
    assert!(!fixture.facade.gate.claim_is_tracked(&child_id));
}

/// 删除会话子树后，随树消失的认领世代一并回收。
#[tokio::test]
async fn deleting_a_child_reclaims_its_claim_generation() {
    let fixture = Fixture::new().await;
    fixture.create("s-delete-claim-root").await;
    let child = fixture
        .child("s-delete-claim-child", "s-delete-claim-root")
        .await;
    let child_id = child.target.thread_id;
    let root_id = child.root_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    assert!(fixture.facade.gate.claim_is_tracked(&child_id));

    fixture.facade.delete_session_tree(&child_id).await.unwrap();
    assert!(!fixture.facade.gate.claim_is_tracked(&child_id));
    drop(claim);
}

/// 正常 Finish：终态与「恢复到认领前」在同一归属校验内提交。
#[tokio::test]
async fn finish_with_status_writes_terminal_status_for_owned_claim() {
    use peri_acp_types::thread::WorkState;
    let fixture = Fixture::new().await;
    fixture.create("s-finish-root").await;
    let child = fixture.child("s-finish-child", "s-finish-root").await;
    let child_id = child.target.thread_id;
    let root_id = child.root_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Stopped)
        .await
        .unwrap();

    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    fixture
        .facade
        .set_work_state(&child_id, WorkState::Working)
        .await
        .unwrap();

    assert!(
        claim.finish_with_status(AgentStatus::Error).await.unwrap(),
        "仍有归属时必须写入终态"
    );
    let meta = fixture.facade.load_session_meta(&child_id).await.unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(
        meta.work_state,
        WorkState::Stopped,
        "终态写入仍须恢复认领前的工作意图"
    );
}

/// [回归] 旧认领结清后新认领接管；迟到的终态写入不得覆盖新认领的 `Active`。
#[tokio::test]
async fn stale_claim_finish_with_status_does_not_override_new_claim() {
    let fixture = Fixture::new().await;
    fixture.create("s-stale-finish-root").await;
    let child = fixture
        .child("s-stale-finish-child", "s-stale-finish-root")
        .await;
    let child_id = child.target.thread_id;
    let root_id = child.root_id;
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let old = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    // 旧认领结清后新 run 重新认领；旧 handle 之后才到的终态写入必须被拒绝。
    old.mark_terminated().await.unwrap();
    let new = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&child_id)
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Active
    );

    assert!(
        !old.finish_with_status(AgentStatus::Cancelled)
            .await
            .unwrap(),
        "归属丢失后不得再写终态"
    );
    let record = fixture
        .facade
        .gate
        .data()
        .load_child_resume_record(&child_id)
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Active);
    assert!(record.claimed);
    new.mark_failed().await.unwrap();
}
