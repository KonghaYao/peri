use super::*;

// ─── fork 的收敛与撤销边界 ───────────────────────────────────────────────────

#[tokio::test]
async fn test_save_fork_converges_when_the_target_was_saved_without_admission() {
    let fixture = Fixture::new().await;
    fixture.create("s-fork-source").await;
    let workspace = fixture.workspace().await;
    let fork = ForkSnapshot {
        target: fixture.session(
            "s-fork-target",
            &workspace,
            r#"{"v":1,"id":"s-fork-source"}"#,
        ),
        source_id: "s-fork-source".to_owned(),
        payloads: vec![payload("forked turn")],
        flags: std::collections::HashMap::new(),
    };
    fixture.facade.gate.data().save_fork(&fork).await.unwrap();
    fixture.facade.save_fork(&fork).await.unwrap();
    assert_eq!(fixture.count_messages("s-fork-target").await, 1);
    fixture
        .facade
        .append_history(&"s-fork-target".to_owned(), &[payload("continued fork")])
        .await
        .unwrap();
    let before = fixture
        .facade
        .load_session_snapshot(&"s-fork-target".to_owned())
        .await
        .unwrap();
    fixture.facade.save_fork(&fork).await.unwrap();
    let after = fixture
        .facade
        .load_session_snapshot(&"s-fork-target".to_owned())
        .await
        .unwrap();
    assert_eq!(after.binding, before.binding);
    assert_eq!(after.frozen, before.frozen);
    assert_eq!(after.payloads.len(), 2);
    assert_eq!(
        serde_json::to_value(&after.meta).unwrap(),
        serde_json::to_value(&before.meta).unwrap()
    );
    let mut changed = fork.clone();
    changed.target.binding.workspace_id = peri_acp_types::workspace::WorkspaceId::new();
    let error = match fixture.facade.save_fork(&changed).await {
        Ok(_) => panic!("expected the changed premise to refuse admission"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
}

/// fork 目标的失败补偿是 write-once 语义：目标创建即带 frozen，撤销仍必须把它整条删掉
/// （「已提交 frozen 拒绝删除」只约束两阶段草稿，见
/// `test_commit_frozen_is_write_once_and_blocks_abandon`）。
#[tokio::test]
async fn test_abandon_initialization_deletes_a_fork_target_with_frozen() {
    let fixture = Fixture::new().await;
    fixture.create("s-fork-source").await;
    let workspace = fixture.workspace().await;
    let fork = ForkSnapshot {
        target: fixture.session(
            "s-fork-target",
            &workspace,
            r#"{"v":1,"id":"s-fork-source"}"#,
        ),
        source_id: "s-fork-source".to_owned(),
        payloads: vec![payload("forked turn")],
        flags: std::collections::HashMap::new(),
    };
    fixture.facade.save_fork(&fork).await.unwrap();
    assert_eq!(fixture.count_messages("s-fork-target").await, 1);

    // 与 `handle_fork` 的失败补偿同一条调用：装配/身份失败后撤销未发布目标。
    fixture
        .facade
        .abandon_initialization(&"s-fork-target".to_owned())
        .await
        .unwrap();

    assert_eq!(fixture.count_threads("s-fork-target").await, 0);
    assert_eq!(fixture.count_bindings("s-fork-target").await, 0);
    assert_eq!(fixture.count_messages("s-fork-target").await, 0);
    // source 原样保留：撤销不是通用 rollback。
    assert_eq!(fixture.count_threads("s-fork-source").await, 1);
    fixture
        .facade
        .append_history(
            &"s-fork-source".to_owned(),
            &[payload("source remains active")],
        )
        .await
        .unwrap();
    assert_eq!(fixture.count_messages("s-fork-source").await, 1);
}

/// 数据面两条撤销入口的判据分档：write-once（不叠加 frozen 判据）与两阶段草稿（叠加）。
#[tokio::test]
async fn test_revoke_entry_points_differ_only_by_the_frozen_criterion() {
    let fixture = Fixture::new().await;
    fixture.create("s-write-once").await;
    let draft = fixture.begin("s-draft-only").await;

    // 两阶段草稿：已提交 ⇒ 拒绝；未提交 ⇒ 删除。
    let error = fixture
        .facade
        .gate
        .data()
        .revoke_unpublished_draft(&"s-write-once".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(fixture.count_threads("s-write-once").await, 1);
    fixture
        .facade
        .gate
        .data()
        .revoke_unpublished_draft(&"s-draft-only".to_owned())
        .await
        .unwrap();
    assert_eq!(fixture.count_threads("s-draft-only").await, 0);

    // write-once：同一条已提交会话走这条入口必须真的删除（fork 补偿语义）。
    fixture
        .facade
        .gate
        .data()
        .revoke_unpublished_session(&"s-write-once".to_owned())
        .await
        .unwrap();
    assert_eq!(fixture.count_threads("s-write-once").await, 0);
    drop(draft);
}

#[tokio::test]
async fn test_revoke_refuses_a_session_that_already_has_children() {
    let fixture = Fixture::new().await;
    fixture.create("s-revoke-root").await;
    let _child = fixture.child("s-revoke-child", "s-revoke-root").await;

    // 已派生过子会话的 identity 不能被补偿掉：否则子会话会指向不存在的父节点。
    let error = fixture
        .facade
        .abandon_initialization(&"s-revoke-root".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
    assert_eq!(fixture.count_threads("s-revoke-root").await, 1);
    assert_eq!(fixture.count_threads("s-revoke-child").await, 1);
    fixture
        .facade
        .append_history(&"s-revoke-child".to_owned(), &[payload("still usable")])
        .await
        .unwrap();
}
