use super::*;

#[tokio::test]
async fn test_delete_ends_canonical_data_and_runtime_ownership() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-del-root").await;
    let _child = fixture
        .child("s-del-child", "s-del-root", &root_lease)
        .await;
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
    root_lease.mark_clean().await.unwrap();
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
        .append_history(&"s-del-root".to_owned(), &[payload("deleted owner write")])
        .await
        .is_err());
    let workspace = fixture.workspace().await;
    let input = fixture.session("s-del-root", &workspace, r#"{"v":1}"#);
    let fresh = fixture.facade.create_session(&input).await.unwrap();
    assert_eq!(fresh.thread_id().as_str(), "s-del-root");
    root_lease.mark_clean().await.unwrap();
    fixture
        .facade
        .append_history(&"s-del-root".to_owned(), &[payload("fresh owner write")])
        .await
        .unwrap();
    assert_eq!(fixture.count_messages("s-del-root").await, 1);
    assert_eq!(fixture.count_threads("s-del-child").await, 0);
    fresh.mark_clean().await.unwrap();
}

#[tokio::test]
async fn test_unbound_frozen_session_remains_readable_and_cannot_be_automatically_adopted() {
    let fixture = Fixture::new().await;
    let id = "s-unbound-frozen-history".to_owned();
    let lease = fixture.create(&id).await;
    fixture
        .facade
        .append_history(&id, &[payload("preserved unbound history")])
        .await
        .unwrap();
    let before = fixture.facade.load_session_snapshot(&id).await.unwrap();
    lease.mark_clean().await.unwrap();
    fixture
        .facade
        .release_execution_owner(&lease.owner_token().unwrap())
        .await
        .unwrap();
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
            SessionResourceErrorKind::Conflict { .. }
        ),
        "unclaimed recovered session must not be mutated: {error}"
    );
    let rejected = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(rejected.binding, BindingState::Missing);
    assert_eq!(rejected.frozen, before.frozen);
    assert_eq!(rejected.payloads.len(), 1);
}

#[tokio::test]
async fn test_close_stops_new_writes_and_reports_unsettled_owners() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-close").await;
    let id = "s-close".to_owned();
    fixture.facade.drain_persistence(&id).await.unwrap();

    fixture.shutdown().await.unwrap();
    // 关闭后不再接受新写入；读取仍然可用（历史可解释性不受影响）。
    let error = fixture
        .facade
        .append_history(&id, &[payload("after close")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Unavailable { .. }
    ));
    assert!(fixture.facade.load_session_meta(&id).await.is_ok());
    // 重复关闭是幂等成功。
    fixture.shutdown().await.unwrap();
    lease.mark_clean().await.unwrap();
    drop(lease);

    // 未结清的写入存在时，关闭不宣告完成。
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-close-uncertain").await;
    {
        let _scope = fixture
            .facade
            .gate
            .admit(&"s-close-uncertain".to_owned())
            .await
            .unwrap();
    }
    let error = fixture.shutdown().await.unwrap_err();
    assert!(error.is_persistence_uncertain());
    drop(lease);
}

/// 子进程入口：按环境变量在**另一进程**里执行同一门面动作。
///
/// 没有环境变量时直接返回，因此它只在被父测试拉起时工作。
#[tokio::test]
async fn test_facade_child_process() {
    let Ok(db) = std::env::var("PERI_TEST_FACADE_DB") else {
        return;
    };
    let id = std::env::var("PERI_TEST_FACADE_ID").unwrap();
    let repo = std::env::var("PERI_TEST_FACADE_REPO").unwrap();
    let expected = std::env::var("PERI_TEST_FACADE_EXPECT").unwrap();
    let facade = SessionResourcesImpl::open(db).await.unwrap();
    let workspace = facade.resolve_workspace(Path::new(&repo)).await.unwrap();
    match expected.as_str() {
        "blocked" => {
            assert!(facade.acquire_execution(&id, &workspace).await.is_err());
        }
        "clean" => {
            let lease = facade.acquire_execution(&id, &workspace).await.unwrap();
            lease.mark_clean().await.unwrap();
            facade
                .release_execution_owner(&lease.owner_token().unwrap())
                .await
                .unwrap();
        }
        "crash" => {
            let _lease = facade.acquire_execution(&id, &workspace).await.unwrap();
            std::process::exit(0);
        }
        other => panic!("unknown expected child result: {other}"),
    }
}

fn facade_process(db: &Path, repo: &Path, id: &str, expected: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::resources::tests::lifecycle::test_facade_child_process",
            "--nocapture",
        ])
        .env("PERI_TEST_FACADE_DB", db)
        .env("PERI_TEST_FACADE_REPO", repo)
        .env("PERI_TEST_FACADE_ID", id)
        .env("PERI_TEST_FACADE_EXPECT", expected)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "facade child failed ({expected}): {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_facade_process_runs_overlap_and_crash_does_not_block_recovery() {
    let fixture = Fixture::new().await;
    let id = "s-xproc".to_owned();
    let lease = fixture.create(&id).await;
    let db = fixture._db.path().join("threads.db");
    let repo = fixture.repo.path();
    let before = fixture.facade.load_session_snapshot(&id).await.unwrap();
    facade_process(&db, repo, &id, "blocked");
    fixture
        .facade
        .append_history(&id, &[payload("original owner")])
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    fixture
        .facade
        .release_execution_owner(&lease.owner_token().unwrap())
        .await
        .unwrap();
    facade_process(&db, repo, &id, "clean");
    facade_process(&db, repo, &id, "crash");
    fixture.facade.drain_persistence(&id).await.unwrap();
    assert!(fixture
        .facade
        .acquire_execution(&id, &fixture.workspace().await)
        .await
        .is_err());
    expire_owner(fixture.facade.local_pool(), &id).await;
    let next = fixture
        .facade
        .acquire_execution(&id, &fixture.workspace().await)
        .await
        .unwrap();
    assert_eq!(next.thread_id(), &id);
    fixture
        .facade
        .append_history(&id, &[payload("after process crash")])
        .await
        .unwrap();
    let after = fixture.facade.load_session_snapshot(&id).await.unwrap();
    assert_eq!(after.binding, before.binding);
    assert_eq!(after.frozen, before.frozen);
    assert_eq!(after.payloads.len(), 2);
    next.mark_clean().await.unwrap();
    assert!(!fixture
        ._db
        .path()
        .join("threads.db.execution-locks")
        .exists());
}

#[tokio::test]
async fn test_cancelled_close_does_not_become_success() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-close-cancel").await;
    let id = "s-close-cancel".to_owned();
    // 一条已准入、未结清的写入：关闭必须先等它结束，而不是宣告完成。
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
    drop(lease);
}
