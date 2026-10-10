use super::*;

#[tokio::test]
async fn test_delete_cascades_canonical_data_and_allows_identity_reuse() {
    let fixture = Fixture::new().await;
    fixture.create("s-del-root").await;
    let _child = fixture.child("s-del-child", "s-del-root").await;
    fixture
        .facade
        .delete_session_tree(&"s-del-root".to_owned())
        .await
        .unwrap();
    for id in ["s-del-root", "s-del-child"] {
        assert_eq!(fixture.count_threads(id).await, 0);
        assert_eq!(fixture.count_bindings(id).await, 0);
        assert_eq!(fixture.count_messages(id).await, 0);
    }
    let error = fixture
        .facade
        .recover_session_persistence(&"s-del-root".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::NotFound
    ));
    assert!(fixture
        .facade
        .append_history(
            &"s-del-root".to_owned(),
            &[payload("deleted session write")]
        )
        .await
        .is_err());
    let workspace = fixture.workspace().await;
    let input = fixture.session("s-del-root", &workspace, r#"{"v":1}"#);
    fixture.facade.create_session(&input).await.unwrap();
    fixture
        .facade
        .append_history(
            &"s-del-root".to_owned(),
            &[payload("recreated session write")],
        )
        .await
        .unwrap();
    assert_eq!(fixture.count_messages("s-del-root").await, 1);
    assert_eq!(fixture.count_threads("s-del-child").await, 0);
    fixture.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_unbound_frozen_session_remains_readable_and_cannot_be_automatically_adopted() {
    let fixture = Fixture::new().await;
    let id = "s-unbound-frozen-history".to_owned();
    fixture.create(&id).await;
    fixture
        .facade
        .append_history(&id, &[payload("preserved unbound history")])
        .await
        .unwrap();
    let before = fixture.facade.load_session_snapshot(&id).await.unwrap();
    sqlx::query("DELETE FROM session_bindings WHERE thread_id = ?1")
        .bind(&id)
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();
    let reopened = fixture.second_host().await;
    let after = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(after.binding, BindingState::Missing);
    assert_eq!(after.frozen, before.frozen);
    assert_eq!(after.payloads.len(), 1);
    assert_eq!(
        peri_acp_types::store::serialize_persisted_payload(&after.payloads[0]).unwrap(),
        peri_acp_types::store::serialize_persisted_payload(&before.payloads[0]).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&after.meta).unwrap(),
        serde_json::to_value(&before.meta).unwrap()
    );
    let workspace = fixture.workspace().await;
    let error = reopened
        .adopt_legacy_session(
            &id,
            &after.meta.cwd,
            &workspace,
            &FrozenSnapshotBytes::new("replacement".to_owned()),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error_kind(&error),
            SessionResourceErrorKind::Workspace(WorkspaceError::InvalidBinding)
        ),
        "unclaimed recovered session must not be mutated: {error}"
    );
    let rejected = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(rejected.binding, BindingState::Missing);
    assert_eq!(rejected.frozen, before.frozen);
    assert_eq!(rejected.payloads.len(), 1);
}

#[tokio::test]
async fn test_close_stops_new_writes_and_reports_unknown_persistence() {
    let fixture = Fixture::new().await;
    fixture.create("s-close").await;
    let id = "s-close".to_owned();
    fixture.facade.drain_persistence(&id).await.unwrap();
    fixture.shutdown().await.unwrap();
    assert!(fixture
        .facade
        .append_history(&id, &[payload("late")])
        .await
        .is_err());
    fixture.shutdown().await.unwrap();
    let uncertain = Fixture::new().await;
    uncertain.create("s-close-uncertain").await;
    drop(
        uncertain
            .facade
            .gate
            .admit(&"s-close-uncertain".to_owned())
            .await
            .unwrap(),
    );
    assert!(uncertain
        .shutdown()
        .await
        .unwrap_err()
        .is_persistence_uncertain());
}

#[tokio::test]
async fn test_cancelled_close_does_not_become_success() {
    let fixture = Fixture::new().await;
    fixture.create("s-close-cancel").await;
    let id = "s-close-cancel".to_owned();
    let scope = fixture.facade.gate.admit(&id).await.unwrap();

    // 第一次关闭在等待中被取消（调用方超时放弃）。
    let cancelled = tokio::time::timeout(Duration::from_millis(50), fixture.shutdown()).await;
    assert!(
        cancelled.is_err(),
        "close must wait for the in-flight write"
    );
    // 取消不构成任何确认：下一次关闭仍要真实等待，不能直接成功。
    let again = tokio::time::timeout(Duration::from_millis(50), fixture.shutdown()).await;
    assert!(
        again.is_err(),
        "a cancelled close must not be recorded as closed"
    );

    // 在途写入结清之后关闭才成立；此时重复关闭才是幂等成功。
    scope.settle(&Ok::<(), SessionResourceError>(()));
    fixture.shutdown().await.unwrap();
    fixture.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_deleted_root_does_not_block_resource_shutdown() {
    let fixture = Fixture::new().await;
    fixture.create("s-delete-close").await;
    fixture
        .facade
        .delete_session_tree(&"s-delete-close".to_owned())
        .await
        .unwrap();
    fixture.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_recovery_after_failed_shutdown_allows_shutdown_to_finish() {
    let fixture = Fixture::new().await;
    let id = "s-close-recovery".to_owned();
    fixture.create(&id).await;
    drop(fixture.facade.gate.admit(&id).await.unwrap());
    assert!(fixture
        .shutdown()
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    fixture
        .facade
        .recover_session_persistence(&id)
        .await
        .unwrap();
    assert!(fixture
        .facade
        .append_history(&id, &[payload("closed to new writes")])
        .await
        .is_err());
    fixture.shutdown().await.unwrap();
}
