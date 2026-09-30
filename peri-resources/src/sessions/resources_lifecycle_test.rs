use super::*;

// ─── 删除与恢复证据 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_ends_data_execution_facts_and_ownership() {
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
    // 删除即删除：整棵树的数据、绑定、执行代际行都不在，也没有第二份「被删过」的痕迹
    // （v10 之后本机不为终止状态留锚点——删除的对象是数据，不是身份）。
    assert_eq!(fixture.count_threads("s-del-root").await, 0);
    assert_eq!(fixture.count_threads("s-del-child").await, 0);
    assert_eq!(fixture.count_bindings("s-del-root").await, 0);
    assert_eq!(fixture.count_execution_runs("s-del-root").await, 0);
    assert_eq!(fixture.count_execution_runs("s-del-child").await, 0);
    // 删除同时结束本次所有权：owner 不再接收写入（下面按「没有活 owner」被拒），锁也已
    // 释放——这一点由本测试末尾用同一 identity 重新创建证明：重建要重新取得同一把
    // sidecar 锁，锁没释放就会是 `ExecutionBusy`。
    root_lease.mark_clean().await.unwrap();
    assert_eq!(fixture.count_execution_runs("s-del-root").await, 0);
    // 收敛读取没有对象：会话不存在，就没有「可重载」这回事。
    let error = fixture
        .facade
        .recover_session_persistence(&"s-del-root".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::NotFound
    ));
    // 收尾中的 owner 仍在册：此刻的写入按「没有活 owner」被拒绝——所有权事实优先于
    // 数据事实，重试收尾不会被悄悄放行。
    let error = fixture
        .facade
        .delete_session_tree(&"s-del-root".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    drop(root_lease);
    // owner 释放后，剩下的结论才是数据事实：这条会话已不存在。
    let error = fixture
        .facade
        .delete_session_tree(&"s-del-root".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::NotFound
    ));
    // 同一 identity 可以重新创建：删除的对象是这条会话的数据与执行事实，不是这个名字。
    // 没有 durable 痕迹时不留「不许再用」的封印——那需要一张跨进程存活的表，而本机
    // 不再有那样的表（见 schema v10 的删除清单）。
    let workspace = fixture.workspace().await;
    let input = fixture.session("s-del-root", &workspace, r#"{"v":1}"#);
    let fresh = fixture.facade.create_session(&input).await.unwrap();
    assert_eq!(fresh.thread_id().as_str(), "s-del-root");
    assert_eq!(
        fixture.execution_row("s-del-root").await,
        Some((1, false)),
        "重新创建是全新的一条会话，代际从 1 开始且未结清"
    );
}

// ─── 排空与关闭 ───────────────────────────────────────────────────────────────

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
    // 收尾仍由 owner 完成：关闭不等于替 owner 写完 clean。
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

// ─── 跨进程：他处所有权、崩溃后的 dirty 与排空 ─────────────────────────────────

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
        "clean" => {
            let lease = facade.acquire_execution(&id, &workspace).await.unwrap();
            lease.mark_clean().await.unwrap();
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
    facade_process(&db, repo, &id, "clean");
    assert_eq!(fixture.execution_row(&id).await, Some((2, true)));
    lease.mark_clean().await.unwrap();
    assert_eq!(fixture.execution_row(&id).await, Some((2, true)));
    drop(lease);
    facade_process(&db, repo, &id, "crash");
    assert_eq!(fixture.execution_row(&id).await, Some((3, false)));
    fixture.facade.drain_persistence(&id).await.unwrap();
    let error = fixture
        .facade
        .reset_dirty_execution(&ResetDirtyRequest {
            target: RecoveryRequiredDetails {
                thread_id: id.clone(),
                generation: 3,
            },
            accept_risk: false,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
    let next = fixture
        .facade
        .acquire_execution(&id, &fixture.workspace().await)
        .await
        .unwrap();
    assert_eq!(next.thread_id(), &id);
    assert_eq!(fixture.execution_row(&id).await, Some((4, false)));
    next.mark_clean().await.unwrap();
    assert_eq!(fixture.execution_row(&id).await, Some((4, true)));
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
