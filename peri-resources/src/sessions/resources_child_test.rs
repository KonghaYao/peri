use super::*;

// ─── child：沿用 root owner ───────────────────────────────────────────────────

#[tokio::test]
async fn test_save_child_requires_the_root_owner_and_shares_its_gate() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-child-root").await;
    let foreign = fixture.create("s-child-foreign").await;
    let workspace = fixture.workspace().await;
    let root_frozen = match fixture
        .facade
        .load_session_snapshot(&"s-child-root".to_owned())
        .await
        .unwrap()
        .frozen
    {
        FrozenState::Present(frozen) => frozen,
        other => panic!("root frozen snapshot is missing: {other:?}"),
    };
    let snapshot = ChildSnapshot {
        target: NewSession {
            thread_id: "s-child".to_owned(),
            created_at: "2026-09-26T00:00:01Z".to_owned(),
            meta: NewSessionMeta {
                title: Some("child".to_owned()),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: Some("s-child-root".to_owned()),
                hidden: true,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: Fixture::binding(&workspace),
            frozen: root_frozen,
        },
        parent_id: "s-child-root".to_owned(),
        root_id: "s-child-root".to_owned(),
        inherited: peri_acp_types::store::InheritedContext {
            payloads: Vec::new(),
            flags: std::collections::HashMap::new(),
        },
    };
    // 别的 root 的 owner 不能借来写这条 child。
    let error = fixture
        .facade
        .save_child(&snapshot, &foreign)
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    assert_eq!(fixture.count_threads("s-child").await, 0);

    fixture
        .facade
        .save_child(&snapshot, &root_lease)
        .await
        .unwrap();
    // 子会话没有自己的执行代际：写入落在 root 的 owner 上。
    assert_eq!(fixture.count_execution_runs("s-child").await, 0);
    fixture
        .facade
        .append_history(&"s-child".to_owned(), &[payload("child turn")])
        .await
        .unwrap();
    // root owner 关闭后，child 的写入同样被拒绝。
    root_lease.mark_clean().await.unwrap();
    let error = fixture
        .facade
        .append_history(&"s-child".to_owned(), &[payload("after clean")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    drop(root_lease);
    fixture
        .facade
        .append_history(&"s-child".to_owned(), &[payload("after run disposal")])
        .await
        .unwrap();
    assert_eq!(fixture.count_messages("s-child").await, 2);
    drop(foreign);
}

/// child 的写入归属 root 执行域：它必须与 root 的写入共享同一条门禁，而不是因为
/// 「child 自己还没有 identity/owner」就免于门禁（B §4.1.3）。
#[tokio::test]
async fn test_save_child_write_waits_for_the_root_gate() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-gate-root").await;
    let snapshot = fixture.child_snapshot("s-gate-child", "s-gate-root").await;

    // 占住 root 的写侧门禁（等价于该 owner 上另一次 mutation 正在检查+写入之间）。
    let facts = facts_of(&fixture.facade, "s-gate-root").await;
    let held = fixture
        .facade
        .gate
        .local()
        .exclusive_guard(&"s-gate-root".to_owned(), &facts)
        .await
        .unwrap()
        .expect("the root owner is alive");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            fixture.facade.save_child(&snapshot, &root_lease),
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
    held.finish();

    // 门禁释放后同一次保存成立，且没有把 root 标成未决（结清只认确定性）。
    fixture
        .facade
        .save_child(&snapshot, &root_lease)
        .await
        .unwrap();
    assert_eq!(fixture.count_threads("s-gate-child").await, 1);
    root_lease.mark_clean().await.unwrap();
    drop(root_lease);
}

/// 父子关系在 child 快照里出现两次：`parent_id`（声明）与 `target.meta.parent_thread_id`
/// （落库用的那一份）。只校验前者、照后者落库，会把「声明了合法父/根」的 child 写成一条
/// **没有父的独立 root**——此后它还能自己取得执行权。门面必须在任何副作用之前拒绝，
/// 并且拒绝不留痕（零行、无 lease、root 原样）。
#[tokio::test]
async fn test_save_child_refuses_a_snapshot_that_disagrees_with_its_parent_relation() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-rel-root").await;
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
        let error = fixture
            .facade
            .save_child(snapshot, &root_lease)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error_kind(&error),
                SessionResourceErrorKind::InvalidInput { .. }
            ),
            "{label}: expected InvalidInput, got {error:?}"
        );
        // 零行：没有会话、没有绑定、没有执行代际。
        assert_eq!(fixture.count_threads("s-rel-child").await, 0, "{label}");
        assert_eq!(fixture.count_bindings("s-rel-child").await, 0, "{label}");
        assert_eq!(
            fixture.count_execution_runs("s-rel-child").await,
            0,
            "{label}"
        );
        // 无 lease：这条 identity 不存在，也就没有独立 root 可取得执行权。
        let facts = facts_of(&fixture.facade, "s-rel-child").await;
        assert!(
            fixture
                .facade
                .gate
                .local()
                .owner_lease(&"s-rel-child".to_owned(), &facts)
                .await
                .unwrap()
                .is_none(),
            "{label}"
        );
    }
    // 原 root 不变：仍是独立 root、树里只有自己、children 为空，且它的 owner 照常可写。
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

    // 合法的 child 仍然成立，且仍然挂在 root 之下（不是独立 root，也没有自己的执行代际）。
    fixture
        .facade
        .save_child(&legal, &root_lease)
        .await
        .unwrap();
    assert_eq!(fixture.count_threads("s-rel-child").await, 1);
    assert_eq!(fixture.count_execution_runs("s-rel-child").await, 0);
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
    // 子会话没有自己的执行代际：它解析到的是 root 的那条 owner，而不是自己当 root。
    let facts = facts_of(&fixture.facade, "s-rel-child").await;
    let owned = fixture
        .facade
        .gate
        .local()
        .owner_lease(&"s-rel-child".to_owned(), &facts)
        .await
        .unwrap()
        .expect("the child belongs to the root's execution domain");
    assert!(owned.is_active());
    assert_eq!(owned.thread_id(), &"s-rel-root".to_owned());
    root_lease.mark_clean().await.unwrap();
    drop(root_lease);
}

#[tokio::test]
async fn test_claim_child_resume_serializes_and_restores_previous_state() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-claim-root").await;
    let child = fixture
        .child("s-claim-child", "s-claim-root", &root_lease)
        .await;
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

    // root owner 不在本进程时不能认领。
    drop(root_lease);
    let error = match fixture.facade.claim_child_resume(&child_id, &root_id).await {
        Ok(_) => panic!("expected claim without a live root owner to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
}
