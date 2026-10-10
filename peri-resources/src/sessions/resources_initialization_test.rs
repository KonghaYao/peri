use super::*;

// ─── 撤销未发布创建 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_abandon_initialization_revokes_data_and_allows_a_fresh_retry() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-abandon").await;
    let id = "s-abandon".to_owned();
    initialization.clone().abandon().await.unwrap();
    assert_eq!(fixture.count_threads(&id).await, 0);
    assert_eq!(fixture.count_bindings(&id).await, 0);
    let workspace = fixture.workspace().await;
    let input = fixture.session(&id, &workspace, r#"{"v":1,"id":"s-abandon"}"#);
    fixture.facade.create_session(&input).await.unwrap();
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.count_bindings(&id).await, 1);
}

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
    assert_eq!(
        second.load_session_snapshot(&id).await.unwrap().frozen,
        FrozenState::LegacyAbsent
    );
}

#[tokio::test]
async fn test_unknown_draft_persistence_blocks_commit_abandon_and_discard() {
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
#[tokio::test]
async fn test_crash_after_commit_preserves_snapshot_and_refuses_draft_cleanup() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-post").await;
    let frozen = FrozenSnapshotBytes::new(r#"{"v":1,"id":"s-post"}"#);
    initialization.commit_frozen(&frozen).await.unwrap();
    let id = "s-post".to_owned();
    drop(initialization);
    let reopened = fixture.second_host().await;
    let snapshot = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.frozen, FrozenState::Present(frozen.clone()));
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
            NULL, NULL, 0, 'cascade', NULL, NULL, 'active', ?2)",
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
    assert!(
        matches!(
            error_kind(&error),
            SessionResourceErrorKind::Conflict { .. }
        ),
        "unexpected refusal: {error:?}"
    );
    assert_eq!(fixture.count_threads("s-legacy").await, 1);
}
