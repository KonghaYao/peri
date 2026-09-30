use super::*;

// ─── 创建：完整数据 + owner 一次成立 ───────────────────────────────────────────

#[tokio::test]
async fn test_create_session_saves_complete_data_and_owner_in_one_step() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    let lease = fixture.create("s-new").await;

    // 完整数据：metadata、binding、frozen 都已可读。
    let snapshot = fixture
        .facade
        .load_session_snapshot(&"s-new".to_owned())
        .await
        .unwrap();
    assert_eq!(snapshot.meta.cwd, workspace.cwd.to_string_lossy());
    assert_eq!(
        snapshot.binding,
        BindingState::Bound(Fixture::binding(&workspace))
    );
    assert_eq!(
        snapshot.frozen,
        FrozenState::Present(FrozenSnapshotBytes::new(r#"{"v":1,"id":"s-new"}"#))
    );

    // 执行代际：一次提交里就带着 owner 事实（未结清）。
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&"s-new".to_owned())
            .await
            .unwrap(),
        Some((1, false))
    );
    assert_eq!(lease.thread_id(), &"s-new".to_owned());
    // 活 owner 在册：本次不能再次取得执行权（「有主」不是「需要恢复」）。
    assert_eq!(
        fixture
            .facade
            .inspect_availability(Some(&"s-new".to_owned()))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::Available)
    );
    // owner 消失（崩溃等价）后剩下的才是代际事实：精确代际的普通 dirty。
    drop(lease);
    assert_eq!(
        fixture
            .facade
            .inspect_availability(Some(&"s-new".to_owned()))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::Available)
    );
}

#[tokio::test]
async fn test_create_session_collapses_data_and_generation_into_one_commit() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    // 用一条会失败的输入（binding 指向未登记工作区）证明失败时什么都不留：
    // 事务整体回滚，不会留下没有执行代际的会话行。
    let mut input = fixture.session("s-fail", &workspace, r#"{"v":1}"#);
    input.binding.workspace_id = peri_acp_types::workspace::WorkspaceId::new();
    let error = match fixture.facade.create_session(&input).await {
        Ok(_) => panic!("expected create to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::InvalidBinding)
    ));
    assert_eq!(fixture.count_threads("s-fail").await, 0);
    assert_eq!(fixture.count_execution_runs("s-fail").await, 0);
}

#[tokio::test]
async fn test_create_session_rejects_a_reused_identity() {
    let fixture = Fixture::new().await;
    let _lease = fixture.create("s-dup").await;
    let workspace = fixture.workspace().await;
    let input = fixture.session("s-dup", &workspace, r#"{"v":1}"#);
    let error = match fixture.facade.create_session(&input).await {
        Ok(_) => panic!("expected create to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
}

#[tokio::test]
async fn test_create_session_converges_when_data_was_saved_without_admission() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    // 崩在「数据已保存、执行代际未写」之间：下次同一个 ThreadId 的创建收敛准入，
    // 不重造 binding/frozen，也不报「已存在」。
    fixture
        .save_without_admission("s-converge", &workspace)
        .await;
    let input = fixture.session("s-converge", &workspace, r#"{"v":1,"id":"s-converge"}"#);
    let lease = fixture.facade.create_session(&input).await.unwrap();
    assert_eq!(lease.thread_id(), &"s-converge".to_owned());
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&"s-converge".to_owned())
            .await
            .unwrap(),
        Some((1, false))
    );
    assert_eq!(fixture.count_threads("s-converge").await, 1);
    assert_eq!(fixture.count_bindings("s-converge").await, 1);
    drop(lease);
}

#[tokio::test]
async fn test_create_session_reports_saved_but_not_admitted_when_premise_changed() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    fixture
        .save_without_admission("s-premise", &workspace)
        .await;
    // 同 identity 但换了一份绑定：数据已保存这一事实不变，准入前提不再成立。
    let mut input = fixture.session("s-premise", &workspace, r#"{"v":1}"#);
    input.binding.workspace_id = peri_acp_types::workspace::WorkspaceId::new();

    let error = match fixture.facade.create_session(&input).await {
        Ok(_) => panic!("expected create to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::SavedButNotAdmitted { .. }
    ));
    // 效果是「已生效」：数据仍在，不得被调用方据此删除。
    assert_eq!(
        error.effect(),
        peri_acp_types::session_resources::MutationOutcome::Applied
    );
    assert_eq!(fixture.count_threads("s-premise").await, 1);
}

// ─── 统一写入准入 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_read_only_store_refuses_registration_and_session_writes() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-readonly").await;
    lease.mark_clean().await.unwrap();
    drop(lease);
    let db_path = fixture._db.path().join("threads.db");

    let before: Vec<std::ffi::OsString> = std::fs::read_dir(fixture._db.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    let read_only = SessionResourcesImpl::open_existing_read_only(&db_path)
        .await
        .unwrap();
    // 登记新身份：连会话都还没有，没有可降级的对象。
    let error = match read_only.resolve_workspace(fixture.repo.path()).await {
        Ok(_) => panic!("expected resolve_workspace to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ReadOnlyStore)
    ));
    // 已有会话上的写入：历史可读、执行权不可得。
    let error = read_only
        .append_history(&"s-readonly".to_owned(), &[payload("late")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    let error = match read_only
        .acquire_execution(
            &"s-readonly".to_owned(),
            &read_only_workspace(&fixture).await,
        )
        .await
    {
        Ok(_) => panic!("expected acquire_execution to fail on a read-only store"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    // 只读路径不创建任何文件：目录内容与只读打开之前逐项一致（包括不建锁文件）。
    let after: Vec<std::ffi::OsString> = std::fs::read_dir(fixture._db.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
    assert!(!db_path
        .with_file_name("threads.db.execution-locks")
        .join("new.lock")
        .exists());
    // 读取仍然成立。
    let meta = read_only
        .load_session_meta(&"s-readonly".to_owned())
        .await
        .unwrap();
    assert_eq!(meta.title.as_deref(), Some("session s-readonly"));
}

async fn read_only_workspace(fixture: &Fixture) -> ResolvedWorkspace {
    fixture.workspace().await
}

#[tokio::test]
async fn test_mutation_without_live_run_does_not_require_ownership_claim() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "s-owner".to_owned();
    fixture.save_without_admission(&id, &workspace).await;
    fixture
        .facade
        .append_history(&id, &[payload("without live run")])
        .await
        .unwrap();
    assert_eq!(fixture.count_messages(&id).await, 1);
    assert_eq!(fixture.execution_row(&id).await, None);
    assert_eq!(
        fixture
            .facade
            .inspect_availability(Some(&id))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::Available)
    );
}

/// 一次「已准入、效果无法证明」的写入：阻塞面与唯一的出路。
///
/// 未决证据只在**进程内的租约**上（v10 删除了本机 durable 锚点：不做跨安装的终态判定）。
/// 因此这里用真实的准入 + 未知效果驱动同一条 `Drop` 语义，再验证后续写入/删除/排空/clean
/// 被拒绝、读取面不受影响、被拒的写入没有留下效果；owner 消失之后，durable 事实只剩
/// `execution_runs` 的未结清代际，由显式风险接受收敛，下一代 owner 从头开始。
#[tokio::test]
async fn test_unresolved_write_blocks_active_run_but_allows_restart() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-unsure").await;
    let id = "s-unsure".to_owned();
    fixture
        .facade
        .append_history(&id, &[payload("first turn")])
        .await
        .unwrap();

    // 提交阶段的失败与取消走同一条路径：准入范围按 Unknown 结清，未决证据留在租约上。
    let scope = fixture.facade.gate.admit(&id).await.unwrap();
    scope.settle(&Err::<(), _>(SessionResourceError::persistence_uncertain(
        Some(id.clone()),
    )));

    // 后续写入与删除都被拒绝：不能在一个结果未知的写入之后继续写。
    let error = fixture
        .facade
        .append_history(&id, &[payload("blocked")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Unavailable { .. }
    ));
    let error = fixture.facade.delete_session_tree(&id).await.unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Unavailable { .. }
    ));
    // 未结清不能被伪装成已排空，也不能被当成干净收尾。
    let error = fixture.facade.drain_persistence(&id).await.unwrap_err();
    assert!(error.is_persistence_uncertain());
    assert!(lease.mark_clean().await.is_err());
    // 被拒的写入没有留下效果：历史仍是第一轮那一条。
    assert_eq!(fixture.count_messages(&id).await, 1);
    // 读取面不受未决写影响：历史可读性与执行资格分开表达。
    assert!(fixture.facade.load_session_meta(&id).await.is_ok());
    let page = fixture
        .facade
        .list_sessions(&peri_acp_types::workspace::ScopedThreadQuery {
            scope: peri_acp_types::workspace::ThreadScope::All,
            cursor: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(page.entries.iter().any(|entry| entry.thread.id == id));

    drop(lease);
    assert_eq!(fixture.execution_row(&id).await, Some((1, false)));
    let next = fixture
        .facade
        .acquire_execution(&id, &fixture.workspace().await)
        .await
        .unwrap();
    assert_eq!(next.thread_id(), &id);
    assert_eq!(fixture.execution_row(&id).await, Some((2, false)));
    fixture
        .facade
        .append_history(&id, &[payload("after restart")])
        .await
        .unwrap();
    assert_eq!(fixture.count_messages(&id).await, 2);
    next.mark_clean().await.unwrap();
    assert_eq!(fixture.execution_row(&id).await, Some((2, true)));
}

// ─── guard：只有效果确定才结清 ─────────────────────────────────────────────────

#[tokio::test]
async fn test_write_scope_settles_only_on_determinate_effect() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-settle").await;
    let id = "s-settle".to_owned();

    // 已证明未生效（NotApplied）：结清，后续写入仍然可以进入。
    let scope = fixture.facade.gate.admit(&id).await.unwrap();
    let not_applied: SessionResourceResult<()> = Err(SessionResourceError::new(
        SessionResourceErrorKind::InvalidInput {
            detail: "rejected before side effects".to_owned(),
        },
    ));
    scope.settle(&not_applied);
    fixture
        .facade
        .append_history(&id, &[payload("after not applied")])
        .await
        .unwrap();

    // 无法证明终态（Unknown）：不结清，同根后续写入与 clean 都被拒绝。
    let scope = fixture.facade.gate.admit(&id).await.unwrap();
    let unknown: SessionResourceResult<()> = Err(SessionResourceError::persistence_uncertain(
        Some(id.clone()),
    ));
    scope.settle(&unknown);
    let error = fixture
        .facade
        .append_history(&id, &[payload("after unknown")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Unavailable { .. }
    ));
    assert!(lease.mark_clean().await.is_err());
    // dirty 代际保持在库内：未结清的写入不会被当成干净收尾。
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&id)
            .await
            .unwrap(),
        Some((1, false))
    );
    drop(lease);
}

/// 提交阶段的失败必须被报成 Unknown：`NotApplied` 会让 `settle` 释放写入范围，
/// 而「提交是否落盘」在这一刻无从证明。这里用数据面提交映射经 `anyhow` 传播后的
/// 真实产物驱动准入（compaction 事务与本机执行面正是这条链路）。
#[tokio::test]
async fn test_commit_stage_failure_blocks_writes_and_clean() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-commit").await;
    let id = "s-commit".to_owned();

    let error = write_failure(anyhow::Error::new(commit_failure(Some(id.clone()))));
    assert!(error.is_persistence_uncertain());
    let scope = fixture.facade.gate.admit(&id).await.unwrap();
    scope.settle(&Err::<(), _>(error));

    // Unknown 不结清范围：同根后续写入与 clean 都被拒绝，库内代际仍是 dirty。
    let blocked = fixture
        .facade
        .append_history(&id, &[payload("after commit failure")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&blocked),
        SessionResourceErrorKind::Unavailable { .. }
    ));
    assert!(lease.mark_clean().await.is_err());
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&id)
            .await
            .unwrap(),
        Some((1, false))
    );
    drop(lease);
}

#[tokio::test]
async fn test_cancelled_mutation_leaves_uncertain_lease_and_blocks_clean() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-cancel").await;
    let id = "s-cancel".to_owned();
    {
        // 丢弃准入范围而不结清，等价于写入 future 在提交边界被取消。
        let _scope = fixture.facade.gate.admit(&id).await.unwrap();
    }
    assert!(lease.mark_clean().await.is_err());
    let error = fixture
        .facade
        .append_history(&id, &[payload("after cancel")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Unavailable { .. }
    ));
    // 排空同样不得把未决写入当成已结清。
    let error = fixture.facade.drain_persistence(&id).await.unwrap_err();
    assert!(error.is_persistence_uncertain());
    drop(lease);
}
