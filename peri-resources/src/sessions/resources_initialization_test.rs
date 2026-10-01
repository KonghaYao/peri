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

    // 别的 owner 不能替它承担补偿：撤销会删执行行，必须由持有它的同一所有权发起。
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
    // 数据与执行代际行一起撤销，本机不留第二份痕迹：撤销判定只依据现有数据事实
    // （v10 删掉了「初始化被放弃」的终态锚点表）。
    assert_eq!(fixture.count_threads(&id).await, 0);
    assert_eq!(fixture.count_bindings(&id).await, 0);
    assert_eq!(fixture.count_execution_runs(&id).await, 0);
    // 补偿走的是放弃所有权，不是 clean：这里不会写出一条假的 clean 记录。
    lease.mark_clean().await.unwrap();
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&id)
            .await
            .unwrap(),
        None
    );
    // 同一 identity 可以重来：这条创建从没发布过，客户端按同一个 id 重试是正常动作
    // （删除则更彻底——它删的是已发布会话的数据，见 `test_delete_ends_data_execution_facts_and_ownership`）。
    let workspace = fixture.workspace().await;
    let input = fixture.session(&id, &workspace, r#"{"v":1,"id":"s-abandon"}"#);
    let fresh = fixture.facade.create_session(&input).await.unwrap();
    assert_eq!(fresh.thread_id().as_str(), id);
    assert_eq!(
        fixture.execution_row(&id).await,
        Some((1, false)),
        "重试建出的是全新会话：代际从 1 开始且未结清"
    );
    drop(other);
}

// ─── J2 两阶段：草稿 → commit_frozen / abandon ────────────────────────────────

/// 第一阶段：草稿行成立（frozen 暂空、代际未结清）、不出现在列表、同 identity 只有一个 owner。
#[tokio::test]
async fn test_begin_initialization_writes_draft_without_frozen() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-draft").await;
    let id = "s-draft".to_owned();

    assert_eq!(initialization.thread_id(), &id);
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.count_bindings(&id).await, 1);
    assert_eq!(fixture.execution_row(&id).await, Some((1, false)));
    assert_eq!(fixture.frozen_of(&id).await, None, "草稿不得带 frozen");
    assert!(
        !fixture.visible_ids().await.contains(&id),
        "未发布草稿不得出现在会话列表"
    );

    let second = fixture.second_host().await;
    let workspace = fixture.workspace().await;
    let second_run = second.acquire_execution(&id, &workspace).await.unwrap();
    assert_eq!(second_run.thread_id(), &id);
    assert_eq!(fixture.execution_row(&id).await, Some((2, false)));
    initialization.execution_lease().mark_clean().await.unwrap();
    assert_eq!(fixture.execution_row(&id).await, Some((2, false)));
    second_run.mark_clean().await.unwrap();
    assert_eq!(fixture.execution_row(&id).await, Some((2, true)));
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
    assert_eq!(fixture.execution_row(&id).await, Some((1, false)));

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
    assert_eq!(fixture.count_execution_runs(&id).await, 1);
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
    assert_eq!(fixture.count_execution_runs(&id).await, 0);
}

/// 崩溃矩阵（未提交）：重开之后草稿仍不可见、cold load 得到「bound 但无 frozen」，
/// 清理判据成立时删除全部行；已提交的会话不走清理（交给 dirty 恢复）。
#[tokio::test]
async fn test_crash_before_commit_leaves_removable_draft() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-crash").await;
    let id = "s-crash".to_owned();
    // 崩溃等价：进程内的 owner 随句柄一起消失（sidecar 锁由内核释放）。
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
    assert_eq!(fixture.count_execution_runs(&id).await, 0);
}

/// 崩溃矩阵（已提交）：cold load 可读，下一次准入是精确代际的 dirty，显式接受风险后可用。
#[tokio::test]
async fn test_crash_after_commit_preserves_snapshot_and_allows_id_recovery() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-post").await;
    let frozen = FrozenSnapshotBytes::new(r#"{"v":1,"id":"s-post"}"#);
    initialization.commit_frozen(&frozen).await.unwrap();
    let id = "s-post".to_owned();
    drop(initialization);
    assert_eq!(fixture.execution_row(&id).await, Some((1, false)));
    let reopened = fixture.second_host().await;
    let snapshot = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.frozen, FrozenState::Present(frozen.clone()));
    let workspace = fixture.workspace().await;
    let lease = reopened.acquire_execution(&id, &workspace).await.unwrap();
    assert_eq!(lease.thread_id(), &id);
    assert_eq!(fixture.execution_row(&id).await, Some((2, false)));
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
    assert_eq!(fixture.execution_row(&id).await, Some((2, true)));
}

/// 清理判据与 legacy 互斥：无绑定行的会话不被清理（它不是半写草稿）。
#[tokio::test]
async fn test_discard_refuses_a_session_without_binding() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    // legacy 形态：有行、有 cwd、无绑定、无执行代际。
    sqlx::query(
        "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
            parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config,
            frozen_context, agent_status)
         VALUES ('s-legacy', NULL, ?1, '2026-09-26T00:00:00Z', '2026-09-26T00:00:00Z', 0,
            NULL, NULL, 0, '{}', NULL, NULL, 'active')",
    )
    .bind(workspace.cwd.to_string_lossy().into_owned())
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
