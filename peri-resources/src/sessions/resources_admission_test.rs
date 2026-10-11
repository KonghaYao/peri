use super::*;

fn canonical_history_bytes(payloads: &[PersistedPayload]) -> Vec<String> {
    payloads
        .iter()
        .map(|payload| peri_acp_types::store::serialize_persisted_payload(payload).unwrap())
        .collect()
}

#[tokio::test]
async fn test_create_session_saves_complete_data_in_one_step() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    fixture.create("s-new").await;

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
    assert_eq!(
        fixture
            .facade
            .inspect_availability(Some(&"s-new".to_owned()))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::Available)
    );
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
async fn test_create_session_rejects_invalid_binding_without_partial_data() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
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
    assert_eq!(fixture.count_bindings("s-fail").await, 0);
}

#[tokio::test]
async fn test_create_session_rejects_a_reused_identity_with_different_frozen() {
    let fixture = Fixture::new().await;
    fixture.create("s-dup").await;
    let workspace = fixture.workspace().await;
    let input = fixture.session("s-dup", &workspace, r#"{"v":1}"#);
    let error = match fixture.facade.create_session(&input).await {
        Ok(_) => panic!("expected create to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
}

#[tokio::test]
async fn test_create_session_converges_when_data_was_saved_without_admission() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    fixture
        .save_without_admission("s-converge", &workspace)
        .await;
    let input = fixture.session("s-converge", &workspace, r#"{"v":1,"id":"s-converge"}"#);
    fixture.facade.create_session(&input).await.unwrap();
    assert_eq!(fixture.count_threads("s-converge").await, 1);
    assert_eq!(fixture.count_bindings("s-converge").await, 1);
}

#[tokio::test]
async fn test_create_session_rejects_conflicting_binding_without_overwriting_data() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    fixture
        .save_without_admission("s-premise", &workspace)
        .await;
    let mut input = fixture.session("s-premise", &workspace, r#"{"v":1,"id":"s-premise"}"#);
    input.binding.workspace_id = peri_acp_types::workspace::WorkspaceId::new();

    let error = match fixture.facade.create_session(&input).await {
        Ok(_) => panic!("expected create to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(
        error.effect(),
        peri_acp_types::session_resources::MutationOutcome::NotApplied
    );
    assert_eq!(fixture.count_threads("s-premise").await, 1);
}

#[tokio::test]
async fn test_same_id_creation_retry_preserves_saved_data_after_reopen() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "s-retry".to_owned();
    let input = fixture.session(&id, &workspace, r#"{"v":1,"id":"s-retry"}"#);
    fixture.facade.create_session(&input).await.unwrap();
    fixture
        .facade
        .append_history(&id, &[payload("persisted")])
        .await
        .unwrap();
    fixture
        .facade
        .update_session_meta(
            &id,
            &SessionMetaPatch {
                title: Some(Some("edited title".to_owned())),
                status: Some(AgentStatus::Cancelled),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let before = fixture.facade.load_session_snapshot(&id).await.unwrap();
    fixture.facade.create_session(&input).await.unwrap();
    fixture.shutdown().await.unwrap();
    assert!(fixture.facade.create_session(&input).await.is_err());
    let reopened = fixture.second_host().await;
    reopened.create_session(&input).await.unwrap();
    let after = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(
        serde_json::to_value(&before.meta).unwrap(),
        serde_json::to_value(&after.meta).unwrap()
    );
    assert_eq!(
        canonical_history_bytes(&before.payloads),
        canonical_history_bytes(&after.payloads)
    );
    assert_eq!(before.binding, after.binding);
    assert_eq!(before.frozen, after.frozen);
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.count_bindings(&id).await, 1);
}

#[tokio::test]
async fn test_creation_retry_rejects_all_immutable_conflicts_without_overwriting() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "s-immutable".to_owned();
    let input = fixture.session(&id, &workspace, r#"{"v":1,"id":"s-immutable"}"#);
    fixture.facade.create_session(&input).await.unwrap();
    fixture
        .facade
        .append_history(&id, &[payload("original")])
        .await
        .unwrap();
    let before = fixture.facade.load_session_snapshot(&id).await.unwrap();
    let mut conflicts = vec![input.clone(); 6];
    conflicts[0].binding.workspace_id = peri_acp_types::workspace::WorkspaceId::new();
    conflicts[1].frozen = FrozenSnapshotBytes::new(r#"{"different":true}"#);
    conflicts[2].meta.parent_thread_id = Some("different-parent".to_owned());
    conflicts[3].meta.snapshot_at_message_id = Some(MessageId::new());
    conflicts[4].created_at = "2026-09-27T00:00:00Z".to_owned();
    conflicts[5].meta.cwd.push_str("/different");
    for conflict in conflicts {
        let error = match fixture.facade.create_session(&conflict).await {
            Ok(_) => panic!("immutable conflict was admitted"),
            Err(error) => error,
        };
        assert!(matches!(
            error_kind(&error),
            SessionResourceErrorKind::Conflict { .. }
        ));
        let after = fixture.facade.load_session_snapshot(&id).await.unwrap();
        assert_eq!(
            serde_json::to_value(&before.meta).unwrap(),
            serde_json::to_value(&after.meta).unwrap()
        );
        assert_eq!(
            canonical_history_bytes(&before.payloads),
            canonical_history_bytes(&after.payloads)
        );
        assert_eq!(before.binding, after.binding);
        assert_eq!(before.frozen, after.frozen);
    }
}

#[tokio::test]
async fn test_same_id_fork_retry_preserves_history_and_checks_target_identity() {
    let fixture = Fixture::new().await;
    fixture.create("s-fork-source").await;
    let workspace = fixture.workspace().await;
    let id = "s-fork-retry".to_owned();
    let mut fork = ForkSnapshot {
        source_id: "s-fork-source".to_owned(),
        target: fixture.session(&id, &workspace, r#"{"v":1,"id":"s-fork-retry"}"#),
        payloads: vec![payload("forked history")],
        flags: Default::default(),
    };
    fork.target.meta.snapshot_at_message_id = Some(MessageId::new());
    fixture.facade.save_fork(&fork).await.unwrap();
    fixture
        .facade
        .append_history(&id, &[payload("continued")])
        .await
        .unwrap();
    fork.payloads = vec![payload("must not replace history")];
    fixture.facade.save_fork(&fork).await.unwrap();
    assert_eq!(fixture.count_messages(&id).await, 2);
    fork.target.meta.snapshot_at_message_id = Some(MessageId::new());
    let error = match fixture.facade.save_fork(&fork).await {
        Ok(_) => panic!("conflicting fork target was admitted"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(fixture.count_messages(&id).await, 2);
}

#[tokio::test]
async fn test_same_id_creation_retry_cannot_bypass_unknown_persistence() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "s-retry-unknown".to_owned();
    let input = fixture.session(&id, &workspace, r#"{"v":1,"id":"s-retry-unknown"}"#);
    fixture.facade.create_session(&input).await.unwrap();
    drop(fixture.facade.gate.admit(&id).await.unwrap());
    fixture.facade.create_session(&input).await.unwrap();
    assert!(fixture
        .facade
        .append_history(&id, &[payload("blocked")])
        .await
        .is_err());
    assert!(fixture
        .facade
        .drain_persistence(&id)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert_eq!(fixture.count_messages(&id).await, 0);
}

#[tokio::test]
async fn test_read_only_store_refuses_registration_and_session_writes() {
    let fixture = Fixture::new().await;
    fixture.create("s-readonly").await;
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
    let error = read_only
        .append_history(&"s-readonly".to_owned(), &[payload("late")])
        .await
        .unwrap_err();
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

#[tokio::test]
async fn test_unknown_write_blocks_local_mutations_until_recovery() {
    let fixture = Fixture::new().await;
    fixture.create("s-unsure").await;
    let id = "s-unsure".to_owned();
    fixture
        .facade
        .append_history(&id, &[payload("first turn")])
        .await
        .unwrap();
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
    assert!(error.is_persistence_uncertain());
    let error = fixture.facade.delete_session_tree(&id).await.unwrap_err();
    assert!(error.is_persistence_uncertain());
    // 未结清不能被伪装成已排空，也不能被当成干净收尾。
    let error = fixture.facade.drain_persistence(&id).await.unwrap_err();
    assert!(error.is_persistence_uncertain());
    // 被拒的写入没有留下效果：历史仍是第一轮那一条。
    assert_eq!(fixture.count_messages(&id).await, 1);
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

    let reopened = fixture.second_host().await;
    reopened
        .append_history(&id, &[payload("after restart")])
        .await
        .unwrap();
    assert_eq!(fixture.count_messages(&id).await, 2);
    assert!(fixture
        .facade
        .append_history(&id, &[payload("still unknown")])
        .await
        .is_err());
    fixture
        .facade
        .recover_session_persistence(&id)
        .await
        .unwrap();
    fixture
        .facade
        .append_history(&id, &[payload("after recovery")])
        .await
        .unwrap();
    assert_eq!(fixture.count_messages(&id).await, 3);
}

// ─── guard：只有效果确定才结清 ─────────────────────────────────────────────────

#[tokio::test]
async fn test_write_scope_settles_only_on_determinate_effect() {
    let fixture = Fixture::new().await;
    fixture.create("s-settle").await;
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
    assert!(error.is_persistence_uncertain());
}

/// 提交阶段的失败必须被报成 Unknown：`NotApplied` 会让 `settle` 释放写入范围，
/// 而「提交是否落盘」在这一刻无从证明。
#[tokio::test]
async fn test_commit_stage_failure_blocks_writes_and_drain() {
    let fixture = Fixture::new().await;
    fixture.create("s-commit").await;
    let id = "s-commit".to_owned();

    let error = write_failure(anyhow::Error::new(commit_failure(Some(id.clone()))));
    assert!(error.is_persistence_uncertain());
    let scope = fixture.facade.gate.admit(&id).await.unwrap();
    scope.settle(&Err::<(), _>(error));

    let blocked = fixture
        .facade
        .append_history(&id, &[payload("after commit failure")])
        .await
        .unwrap_err();
    assert!(blocked.is_persistence_uncertain());
}

#[tokio::test]
async fn test_cancelled_mutation_blocks_writes_and_drain() {
    let fixture = Fixture::new().await;
    fixture.create("s-cancel").await;
    let id = "s-cancel".to_owned();
    {
        let _scope = fixture.facade.gate.admit(&id).await.unwrap();
    }
    let error = fixture
        .facade
        .append_history(&id, &[payload("after cancel")])
        .await
        .unwrap_err();
    assert!(error.is_persistence_uncertain());
    // 排空同样不得把未决写入当成已结清。
    let error = fixture.facade.drain_persistence(&id).await.unwrap_err();
    assert!(error.is_persistence_uncertain());
}

#[tokio::test]
async fn test_aborted_mutation_requires_recovery_before_writes_resume() {
    let fixture = Fixture::new().await;
    let id = "s-aborted-write".to_owned();
    fixture.create(&id).await;
    let facade = fixture.facade.clone();
    let writing_id = id.clone();
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let writing = tokio::spawn(async move {
        facade
            .gate
            .with_mutation(&writing_id, || async move {
                started_sender.send(()).unwrap();
                std::future::pending::<SessionResourceResult<()>>().await
            })
            .await
    });
    started_receiver.await.unwrap();
    writing.abort();
    assert!(writing.await.unwrap_err().is_cancelled());
    assert!(fixture
        .facade
        .append_history(&id, &[payload("blocked")])
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert!(fixture
        .facade
        .drain_persistence(&id)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert_eq!(fixture.count_messages(&id).await, 0);
    fixture
        .facade
        .recover_session_persistence(&id)
        .await
        .unwrap();
    fixture
        .facade
        .append_history(&id, &[payload("recovered")])
        .await
        .unwrap();
    assert_eq!(fixture.count_messages(&id).await, 1);
}
