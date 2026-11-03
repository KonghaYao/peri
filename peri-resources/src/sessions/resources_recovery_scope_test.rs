//! 恢复范围读取：根解析、`working` 过滤、嵌套归属与旧代际只读。
//!
//! 夹具一律是临时文件 + 真实 SQLite；断言以读回的事实为准，不解析真实库路径。

use peri_acp_types::thread::WorkState;

use super::*;

/// 在指定父之下建 child（frozen 逐字节取自根的已保存快照）。
async fn save_child(
    facade: &SessionResourcesImpl,
    workspace: &ResolvedWorkspace,
    child: &str,
    parent: &str,
    root: &str,
    at: &str,
) {
    let root_frozen = match facade
        .load_session_snapshot(&root.to_owned())
        .await
        .unwrap()
        .frozen
    {
        FrozenState::Present(bytes) => bytes,
        other => panic!("root frozen snapshot is missing: {other:?}"),
    };
    let snapshot = ChildSnapshot {
        target: NewSession {
            thread_id: child.to_owned(),
            created_at: at.to_owned(),
            meta: NewSessionMeta {
                title: Some(format!("child {child}")),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: Some(parent.to_owned()),
                hidden: true,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(workspace),
            frozen: root_frozen,
        },
        parent_id: parent.to_owned(),
        root_id: root.to_owned(),
        inherited: peri_acp_types::store::InheritedContext {
            payloads: Vec::new(),
            flags: Default::default(),
        },
    };
    facade.save_child(&snapshot).await.unwrap();
}

async fn create_root(facade: &SessionResourcesImpl, workspace: &ResolvedWorkspace, id: &str) {
    let input = NewSession {
        thread_id: id.to_owned(),
        created_at: "2026-09-26T00:00:00Z".to_owned(),
        meta: NewSessionMeta {
            title: Some(format!("session {id}")),
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(workspace),
        frozen: FrozenSnapshotBytes::new(format!(r#"{{"v":1,"id":"{id}"}}"#)),
    };
    facade.create_session(&input).await.unwrap();
}

async fn set_work(facade: &SessionResourcesImpl, id: &str, state: WorkState) {
    facade.set_work_state(&id.to_owned(), state).await.unwrap();
}

/// A(root) → B → D，A → C：只有 `working` 的 thread 进候选，子树不变。
#[tokio::test]
async fn scope_reads_the_working_subtree_from_any_entry_thread() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    create_root(&fixture.facade, &workspace, "A").await;
    save_child(
        &fixture.facade,
        &workspace,
        "B",
        "A",
        "A",
        "2026-09-26T00:00:01Z",
    )
    .await;
    save_child(
        &fixture.facade,
        &workspace,
        "C",
        "A",
        "A",
        "2026-09-26T00:00:02Z",
    )
    .await;
    save_child(
        &fixture.facade,
        &workspace,
        "D",
        "B",
        "A",
        "2026-09-26T00:00:03Z",
    )
    .await;
    set_work(&fixture.facade, "B", WorkState::Working).await;
    set_work(&fixture.facade, "D", WorkState::Working).await;
    set_work(&fixture.facade, "C", WorkState::Stopped).await;

    for entered in ["A", "B", "D", "C"] {
        let scope = fixture
            .facade
            .load_recovery_scope(&entered.to_owned())
            .await
            .unwrap();
        assert_eq!(scope.root_thread_id, "A", "进入 {entered} 必须解析到同一根");
        assert_eq!(
            scope
                .candidates
                .iter()
                .map(|candidate| candidate.thread_id.as_str())
                .collect::<Vec<_>>(),
            ["B", "D"],
            "只有 working 的 thread 是候选，顺序按创建时间"
        );
        assert_eq!(scope.candidates[0].parent_thread_id.as_deref(), Some("A"));
        assert_eq!(scope.candidates[1].parent_thread_id.as_deref(), Some("B"));
    }
}

/// 根自身为 `working` 时也是候选；上下文占用区分「未知」与「已知」。
#[tokio::test]
async fn working_root_is_a_candidate_with_its_context_usage() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    create_root(&fixture.facade, &workspace, "A").await;
    set_work(&fixture.facade, "A", WorkState::Working).await;
    let scope = fixture
        .facade
        .load_recovery_scope(&"A".to_owned())
        .await
        .unwrap();
    assert_eq!(scope.candidates.len(), 1);
    assert_eq!(scope.candidates[0].thread_id, "A");
    assert_eq!(scope.candidates[0].context_usage_tokens, None);

    fixture
        .facade
        .set_context_usage(&"A".to_owned(), Some(4096))
        .await
        .unwrap();
    let scope = fixture
        .facade
        .load_recovery_scope(&"A".to_owned())
        .await
        .unwrap();
    assert_eq!(scope.candidates[0].context_usage_tokens, Some(4096));
}

/// 父链缺行：范围不可解释，必须显式失败，不能退回自身把范围悄悄变窄。
#[tokio::test]
async fn missing_ancestor_is_reported_as_corrupt() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    create_root(&fixture.facade, &workspace, "A").await;
    save_child(
        &fixture.facade,
        &workspace,
        "B",
        "A",
        "A",
        "2026-09-26T00:00:01Z",
    )
    .await;
    sqlx::query("UPDATE threads SET parent_thread_id = 'ghost' WHERE id = 'B'")
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();

    let error = fixture
        .facade
        .load_recovery_scope(&"B".to_owned())
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            SessionResourceErrorKind::Corrupt { detail } if detail == "thread ancestry is missing"
        ),
        "实际: {error}"
    );
}

/// 父链成环：同样是 `Corrupt`，不是「没有候选」。
#[tokio::test]
async fn cyclic_ancestry_is_reported_as_corrupt() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    create_root(&fixture.facade, &workspace, "A").await;
    save_child(
        &fixture.facade,
        &workspace,
        "B",
        "A",
        "A",
        "2026-09-26T00:00:01Z",
    )
    .await;
    sqlx::query("UPDATE threads SET parent_thread_id = 'B' WHERE id = 'A'")
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();

    let error = fixture
        .facade
        .load_recovery_scope(&"B".to_owned())
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            SessionResourceErrorKind::Corrupt { detail } if detail == "cyclic thread ancestry"
        ),
        "实际: {error}"
    );
}

/// 旧代际只读库（schema 13）：读不到工作意图 ⇒ 零候选，不报错、不写入。
#[tokio::test]
async fn legacy_read_only_database_yields_zero_candidates_without_writing() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().to_path_buf();
    std::fs::create_dir_all(workspace.join(".peri")).unwrap();
    let path = directory.path().join("threads.db");
    let before = {
        let facade = SessionResourcesImpl::open(&path).await.unwrap();
        let workspace = facade.resolve_workspace(&workspace).await.unwrap();
        create_root(&facade, &workspace, "A").await;
        save_child(&facade, &workspace, "B", "A", "A", "2026-09-26T00:00:01Z").await;
        // 旧代际没有这两列：先写入再删列，模拟 v13 库里的历史行。
        set_work(&facade, "A", WorkState::Working).await;
        set_work(&facade, "B", WorkState::Working).await;
        let rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM threads WHERE id IN ('A','B')")
            .fetch_one(facade.local_pool())
            .await
            .unwrap();
        sqlx::raw_sql(
            "ALTER TABLE threads DROP COLUMN context_usage_tokens;
             ALTER TABLE threads DROP COLUMN work_state;
             PRAGMA user_version = 13;",
        )
        .execute(facade.local_pool())
        .await
        .unwrap();
        rows.0
    };

    let read_only = SessionResourcesImpl::open_existing_read_only(&path)
        .await
        .unwrap();
    for entered in ["A", "B"] {
        let scope = read_only
            .load_recovery_scope(&entered.to_owned())
            .await
            .unwrap_or_else(|error| panic!("旧库读取必须零候选而不是失败：{error}"));
        assert_eq!(scope.root_thread_id, "A");
        assert!(
            scope.candidates.is_empty(),
            "读不到工作意图时不得猜测候选：{:?}",
            scope.candidates
        );
    }
    // 只读句柄不能写：范围读取本身也不提写。
    assert!(
        read_only
            .set_work_state(&"A".to_owned(), WorkState::Working)
            .await
            .is_err(),
        "只读库上的写必须失败"
    );
    assert_eq!(
        read_only
            .load_session_meta(&"A".to_owned())
            .await
            .unwrap()
            .work_state,
        WorkState::Idle
    );
    let after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM threads WHERE id IN ('A','B')")
        .fetch_one(read_only.local_pool())
        .await
        .unwrap();
    assert_eq!(after.0, before, "范围读取不得增删行");
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(read_only.local_pool())
        .await
        .unwrap();
    assert_eq!(version, 13, "只读打开不得升级 schema");
}
