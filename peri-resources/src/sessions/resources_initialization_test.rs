use super::*;

// ─── 撤销未发布创建 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_abandon_initialization_revokes_data_and_allows_a_fresh_retry() {
    let fixture = Fixture::new().await;
    // 撤销的对象是**未提交 frozen** 的草稿（两阶段的第一阶段）；已提交的会话不是
    // 「未发布创建」，见 `test_abandon_refuses_a_draft_whose_frozen_was_committed`。
    let initialization = fixture.begin("s-abandon").await;
    let lease = initialization.execution_lease();
    let other = fixture.create("s-other").await;
    let id = "s-abandon".to_owned();

    let error = fixture
        .facade
        .abandon_initialization(&id, &other)
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    assert_eq!(fixture.count_threads(&id).await, 1);

    initialization.clone().abandon().await.unwrap();
    assert_eq!(fixture.count_threads(&id).await, 0);
    assert_eq!(fixture.count_bindings(&id).await, 0);
    // 补偿走的是放弃所有权，不是 clean：这里不会写出一条假的 clean 记录。
    lease.mark_clean().await.unwrap();
    // 同一 identity 可以重来：这条创建从没发布过，客户端按同一个 id 重试是正常动作
    // （删除则更彻底——它删的是已发布会话的数据，见 `test_delete_ends_data_execution_facts_and_ownership`）。
    let workspace = fixture.workspace().await;
    let input = fixture.session(&id, &workspace, r#"{"v":1,"id":"s-abandon"}"#);
    let fresh = fixture.facade.create_session(&input).await.unwrap();
    assert_eq!(fresh.thread_id().as_str(), id);
    assert!(!Arc::ptr_eq(&lease, &fresh));
    drop(other);
}

// ─── J2 两阶段：草稿 → commit_frozen / abandon ────────────────────────────────

/// 第一阶段：草稿行成立、不可见，其他实例运行句柄不改变本实例的初始化资格。
#[tokio::test]
async fn test_begin_initialization_writes_draft_without_frozen() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-draft").await;
    let id = "s-draft".to_owned();

    assert_eq!(initialization.thread_id(), &id);
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.count_bindings(&id).await, 1);
    assert_eq!(fixture.frozen_of(&id).await, None, "草稿不得带 frozen");
    assert!(
        !fixture.visible_ids().await.contains(&id),
        "未发布草稿不得出现在会话列表"
    );

    let second = fixture.second_host().await;
    let workspace = fixture.workspace().await;
    assert!(second.acquire_execution(&id, &workspace).await.is_err());
    let first = initialization.execution_lease();
    let token = first.owner_token().unwrap();
    first.mark_clean().await.unwrap();
    fixture.facade.release_execution_owner(&token).await.unwrap();
    let second_run = second.acquire_execution(&id, &workspace).await.unwrap();
    assert_eq!(second_run.thread_id(), &id);
    second_run.mark_clean().await.unwrap();
    second.release_execution_owner(&second_run.owner_token().unwrap()).await.unwrap();
    assert_eq!(fixture.frozen_of(&id).await, None);
}

#[tokio::test]
async fn test_draft_frozen_commit_requires_the_exact_live_owner_arc() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-draft-owner").await;
    let id = initialization.thread_id().clone();
    let frozen = FrozenSnapshotBytes::new(r#"{"v":1}"#);
    let foreign: Arc<dyn SessionExecutionLease> =
        Arc::new(crate::sessions::execution::ExecutionLease::new(id.clone()));
    let error = fixture
        .facade
        .gate
        .local()
        .commit_frozen(&id, &foreign, &frozen)
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    assert_eq!(fixture.frozen_of(&id).await, None);
    let error = fixture
        .facade
        .discard_incomplete_initialization(&id)
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    initialization.commit_frozen(&frozen).await.unwrap();
    assert_eq!(
        fixture.frozen_of(&id).await.as_deref(),
        Some(frozen.as_str())
    );
    initialization.execution_lease().mark_clean().await.unwrap();
}

#[tokio::test]
async fn test_closed_draft_owner_cannot_commit_or_abandon_a_replacement_run() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-draft-closed").await;
    let id = initialization.thread_id().clone();
    let stale = initialization.execution_lease();
    stale.mark_clean().await.unwrap();
    let current = fixture
        .facade
        .acquire_execution(&id, &fixture.workspace().await)
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&stale, &current));
    assert!(initialization
        .commit_frozen(&FrozenSnapshotBytes::new(r#"{"v":1}"#))
        .await
        .is_err());
    assert!(initialization.clone().abandon().await.is_err());
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.frozen_of(&id).await, None);
    current.mark_clean().await.unwrap();
}

#[tokio::test]
async fn test_closed_draft_owner_only_allows_abandon_after_canonical_deletion() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-draft-stopped").await;
    let id = initialization.thread_id().clone();
    initialization.execution_lease().mark_clean().await.unwrap();
    let error = initialization.clone().abandon().await.unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.count_bindings(&id).await, 1);
    fixture
        .facade
        .discard_incomplete_initialization(&id)
        .await
        .unwrap();
    assert_eq!(fixture.count_threads(&id).await, 0);
    initialization.clone().abandon().await.unwrap();
}

#[tokio::test]
async fn test_closed_complete_owner_cannot_revoke_saved_canonical_data() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-complete-stopped").await;
    let id = lease.thread_id().clone();
    let before = fixture.facade.load_session_snapshot(&id).await.unwrap();
    lease.mark_clean().await.unwrap();
    let error = fixture
        .facade
        .abandon_initialization(&id, &lease)
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    let after = fixture.facade.load_session_snapshot(&id).await.unwrap();
    assert_eq!(before.binding, after.binding);
    assert_eq!(before.frozen, after.frozen);
}

#[tokio::test]
async fn test_uncertain_draft_owner_cannot_commit_abandon_or_discard() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-draft-uncertain").await;
    let id = initialization.thread_id().clone();
    drop(fixture.facade.gate.admit(&id).await.unwrap());
    assert!(initialization
        .commit_frozen(&FrozenSnapshotBytes::new(r#"{"v":1}"#))
        .await
        .is_err());
    assert!(initialization.clone().abandon().await.is_err());
    drop(initialization);
    assert!(fixture
        .facade
        .discard_incomplete_initialization(&id)
        .await
        .is_err());
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.count_bindings(&id).await, 1);
    assert_eq!(fixture.frozen_of(&id).await, None);
}

#[tokio::test]
async fn test_cancelled_abandon_keeps_unknown_effect_and_blocks_reacquire_commit_and_drain() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-abandon-cancelled").await;
    let id = initialization.thread_id().clone();
    let lease = initialization.execution_lease();
    let local = fixture.facade.gate.local().clone();
    let abandoning_id = id.clone();
    let abandoning_lease = lease.clone();
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let abandoning = tokio::spawn(async move {
        local
            .abandon_initialization(
                &abandoning_id,
                &abandoning_lease,
                Box::pin(async move {
                    started_sender.send(()).unwrap();
                    std::future::pending::<SessionResourceResult<()>>().await
                }),
            )
            .await
    });
    started_receiver.await.unwrap();
    abandoning.abort();
    assert!(abandoning.await.unwrap_err().is_cancelled());
    assert!(lease.mark_clean().await.is_err());
    assert!(fixture
        .facade
        .acquire_execution(&id, &fixture.workspace().await)
        .await
        .is_err());
    assert!(initialization
        .commit_frozen(&FrozenSnapshotBytes::new(r#"{"v":1}"#))
        .await
        .is_err());
    assert!(fixture
        .facade
        .drain_persistence(&id)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert!(initialization.clone().abandon().await.is_err());
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.count_bindings(&id).await, 1);
    assert_eq!(fixture.frozen_of(&id).await, None);
}

/// 第二阶段：commit 后 frozen 可读；重复 commit / 已提交后 abandon 都是 typed 冲突且不覆盖。
#[tokio::test]
async fn test_commit_frozen_is_write_once_and_blocks_abandon() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-commit").await;
    let id = "s-commit".to_owned();
    let frozen = FrozenSnapshotBytes::new(r#"{"v":1,"id":"s-commit"}"#);

    initialization.commit_frozen(&frozen).await.unwrap();
    assert_eq!(
        fixture.frozen_of(&id).await.as_deref(),
        Some(frozen.as_str())
    );
    let snapshot = fixture.facade.load_session_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.frozen, FrozenState::Present(frozen.clone()));

    // 重复提交：typed 冲突，且不覆盖已提交字节。
    let other = FrozenSnapshotBytes::new(r#"{"v":1,"id":"second-write"}"#);
    let error = initialization.commit_frozen(&other).await.unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(
        fixture.frozen_of(&id).await.as_deref(),
        Some(frozen.as_str())
    );

    // 已提交的草稿不是「未发布创建」：撤销必须拒绝，且一条行都不删。
    let error = initialization.clone().abandon().await.unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.count_bindings(&id).await, 1);
}

/// abandon 幂等：先成功后重复调用仍成功（行已不在，目标已达成）。
#[tokio::test]
async fn test_abandon_is_idempotent_after_success() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-idem").await;
    let id = "s-idem".to_owned();

    initialization.clone().abandon().await.unwrap();
    assert_eq!(fixture.count_threads(&id).await, 0);
    initialization.clone().abandon().await.unwrap();
    assert_eq!(fixture.count_threads(&id).await, 0);
}

/// 崩溃矩阵（未提交）：重开之后草稿仍不可见、cold load 得到「bound 但无 frozen」，
/// 清理判据成立时删除 canonical 草稿；已提交会话不走清理。
#[tokio::test]
async fn test_crash_before_commit_leaves_removable_draft() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-crash").await;
    let id = "s-crash".to_owned();
    drop(initialization);

    let reopened = fixture.second_host().await;
    assert!(
        !fixture.visible_ids().await.contains(&id),
        "半写草稿不得出现在会话列表"
    );
    let snapshot = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(
        snapshot.binding,
        BindingState::Bound(Fixture::binding(&fixture.workspace().await)),
        "绑定已成立"
    );
    assert_eq!(
        snapshot.frozen,
        FrozenState::LegacyAbsent,
        "bound 且无 frozen 是半写判据（ACP 侧按 typed 错误 fail-closed）"
    );

    reopened
        .discard_incomplete_initialization(&id)
        .await
        .unwrap();
    assert_eq!(fixture.count_threads(&id).await, 0);
    assert_eq!(fixture.count_bindings(&id).await, 0);
}

/// 崩溃矩阵（已提交）：cold load 可读，下一实例按 ID 准入，不覆盖 frozen。
#[tokio::test]
async fn test_crash_after_commit_preserves_snapshot_and_allows_id_recovery() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-post").await;
    let frozen = FrozenSnapshotBytes::new(r#"{"v":1,"id":"s-post"}"#);
    initialization.commit_frozen(&frozen).await.unwrap();
    let id = "s-post".to_owned();
    drop(initialization);
    let reopened = fixture.second_host().await;
    let snapshot = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.frozen, FrozenState::Present(frozen.clone()));
    let workspace = fixture.workspace().await;
    assert!(reopened.acquire_execution(&id, &workspace).await.is_err());
    expire_owner(fixture.facade.local_pool(), &id).await;
    let lease = reopened.acquire_execution(&id, &workspace).await.unwrap();
    assert_eq!(lease.thread_id(), &id);
    let error = reopened
        .discard_incomplete_initialization(&id)
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(
        reopened.load_session_snapshot(&id).await.unwrap().frozen,
        FrozenState::Present(frozen)
    );
    lease.mark_clean().await.unwrap();
}

/// 清理判据与 legacy 互斥：无绑定行的会话不被清理（它不是半写草稿）。
#[tokio::test]
async fn test_discard_refuses_a_session_without_binding() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    sqlx::query(
        "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
            parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config,
            frozen_context, agent_status, workspace_id)
         VALUES ('s-legacy', NULL, ?1, '2026-09-26T00:00:00Z', '2026-09-26T00:00:00Z', 0,
            NULL, NULL, 0, '{}', NULL, NULL, 'active', ?2)",
    )
    .bind(workspace.cwd.to_string_lossy().into_owned())
    .bind(workspace.workspace_id.to_string())
    .execute(fixture.facade.local_pool())
    .await
    .unwrap();

    let error = fixture
        .facade
        .discard_incomplete_initialization(&"s-legacy".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(fixture.count_threads("s-legacy").await, 1);
}
