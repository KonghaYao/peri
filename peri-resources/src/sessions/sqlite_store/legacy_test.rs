use super::*;
use peri_acp_types::store::serialize_persisted_payload;
use peri_acp_types::workspace::{ScopedThreadQuery, ThreadScope};
use sqlx::{Connection, SqliteConnection};

/// 旧版会话保存的调用方路径文本。
///
/// macOS 上保持调用方原样：`/var/...` 与登记的 `/private/var/...` 由
/// `legacy_path_sql` 归一，这里不预先归一，那条规则才有覆盖。
///
/// Windows 上不能直接用 `dir.path()`：runner 的 `%TEMP%` 是 8.3 短名
/// （`C:\Users\RUNNER~1\...`），而登记 root 来自 canonicalize 的长名
/// （`\\?\C:\Users\runneradmin\...`）。把短名展开成长名只能走文件系统，而这条匹配是
/// SQL 层的显示关联（`legacy_path_sql` 只归一分隔符、verbatim 前缀与尾部分隔符）。
/// 真实用户保存的是普通长名路径（`current_dir` 既不带短名也不带 verbatim 前缀），
/// 用例按这个形状取文本。
fn legacy_saved_text(dir: &std::path::Path) -> std::path::PathBuf {
    #[cfg(not(windows))]
    {
        dir.to_owned()
    }
    #[cfg(windows)]
    {
        let canonical = std::fs::canonicalize(dir).unwrap();
        // 先取出**拥有所有权**的普通形式：借用在这里结束，不匹配时才能把 canonical
        // 原样返回（match 的 scrutinee 借用活到 match 结束，直接在里面移动会编译失败）。
        let ordinary = canonical
            .to_str()
            .and_then(|text| text.strip_prefix(r"\\?\"))
            .map(std::path::PathBuf::from);
        ordinary.unwrap_or(canonical)
    }
}

async fn legacy_database(path: &std::path::Path, cwd: &std::path::Path) -> String {
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("fixtures/legacy_with_goals.sql"))
        .execute(&mut connection)
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count) VALUES (?, 'old history', ?, '2026-09-01T00:00:00Z', '2026-09-02T00:00:00Z', 1)")
        .bind(&id).bind(cwd.to_str().unwrap()).execute(&mut connection).await.unwrap();
    let message = BaseMessage::human("history survives upgrade");
    sqlx::query(
        "INSERT INTO messages (message_id, thread_id, role, content) VALUES (?, ?, 'user', ?)",
    )
    .bind(message.id().as_uuid().to_string())
    .bind(&id)
    .bind(serialize_persisted_payload(&PersistedPayload::Message(message)).unwrap())
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    id
}

#[tokio::test]
async fn legacy_history_refused_without_upgrade_on_repeated_opens() {
    let dir = tempfile::tempdir().unwrap();
    // Old releases saved the caller's ordinary path, not canonical/verbatim Windows paths.
    let cwd = legacy_saved_text(dir.path());
    let path = dir.path().join("threads.db");
    legacy_database(&path, &cwd).await;
    for _ in 0..2 {
        assert_legacy_rejected(&path).await;
    }
}

#[tokio::test]
async fn legacy_history_in_missing_directory_is_preserved_on_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("removed-checkout");
    let path = dir.path().join("threads.db");
    legacy_database(&path, &missing).await;
    assert_legacy_rejected(&path).await;
}

#[tokio::test]
async fn legacy_adoption_commits_snapshot_once_across_concurrent_restorers() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let path = dir.path().join("threads.db");
    let id = unbound_history_database(&path, &cwd).await;
    let left = SqliteThreadStore::new(&path).await.unwrap();
    let right = SqliteThreadStore::new(&path).await.unwrap();
    let workspace = left.resolve_workspace(&cwd).await.unwrap();
    let (left_adoption, right_adoption) = tokio::join!(
        left.adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "first snapshot"),
        right.adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "second snapshot"),
    );
    left_adoption.unwrap();
    right_adoption.unwrap();
    let winner = left.load_frozen_snapshot(&id).await.unwrap().unwrap();
    assert!(matches!(
        winner.as_str(),
        "first snapshot" | "second snapshot"
    ));
    assert_eq!(left.validate_session_binding(&id).await.unwrap(), workspace);
    left.adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "must not overwrite")
        .await
        .unwrap();
    assert_eq!(
        right.load_frozen_snapshot(&id).await.unwrap().unwrap(),
        winner
    );
    left.append_message(&id, BaseMessage::human("continued by left"))
        .await
        .unwrap();
    right
        .append_message(&id, BaseMessage::human("continued by right"))
        .await
        .unwrap();
    let messages = right.load_messages(&id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(BaseMessage::content)
            .collect::<Vec<_>>(),
        [
            "history survives upgrade",
            "continued by left",
            "continued by right"
        ]
    );
    left.close().await;
    right.close().await;
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    assert_eq!(
        reopened.load_frozen_snapshot(&id).await.unwrap().unwrap(),
        winner
    );
    assert_eq!(
        reopened.validate_session_binding(&id).await.unwrap(),
        workspace
    );
    assert_eq!(
        serde_json::to_value(reopened.load_messages(&id).await.unwrap()).unwrap(),
        serde_json::to_value(messages).unwrap()
    );
    reopened.close().await;
}

#[tokio::test]
async fn legacy_adoption_failure_rolls_back_binding_and_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let path = dir.path().join("threads.db");
    let id = unbound_history_database(&path, &cwd).await;
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let workspace = store.resolve_workspace(&cwd).await.unwrap();
    sqlx::raw_sql("CREATE TRIGGER reject_binding BEFORE INSERT ON session_bindings BEGIN SELECT RAISE(FAIL, 'injected binding failure'); END;")
        .execute(&store.database.pool).await.unwrap();
    let error = store
        .adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("injected binding failure"));
    assert!(store.load_session_binding(&id).await.unwrap().is_none());
    assert!(store.load_frozen_snapshot(&id).await.unwrap().is_none());
    sqlx::query("DROP TRIGGER reject_binding")
        .execute(&store.database.pool)
        .await
        .unwrap();
    store
        .adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap();
    assert_eq!(
        store.load_frozen_snapshot(&id).await.unwrap().as_deref(),
        Some("snapshot")
    );
    store.close().await;
}

#[tokio::test]
async fn legacy_adoption_rejects_changed_cwd_and_child_without_losing_history() {
    use peri_acp_types::workspace::WorkspaceError;
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let store = SqliteThreadStore::new(dir.path().join("threads.db"))
        .await
        .unwrap();
    let workspace = store.resolve_workspace(&cwd).await.unwrap();
    let id = store
        .create_thread(ThreadMeta::new_at(
            cwd.to_str().unwrap(),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    let parent = store
        .create_thread(ThreadMeta::new_at(
            cwd.to_str().unwrap(),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    store
        .append_message(&id, BaseMessage::human("history remains readable"))
        .await
        .unwrap();
    let messages = serde_json::to_value(store.load_messages(&id).await.unwrap()).unwrap();
    let mut meta = store.load_meta(&id).await.unwrap();
    meta.cwd = cwd.join("changed").to_str().unwrap().to_owned();
    store.update_meta(&id, meta.clone()).await.unwrap();
    let error = store
        .adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ExecutionBindingMismatch)
    ));
    assert_eq!(store.load_meta(&id).await.unwrap().cwd, meta.cwd);
    assert_eq!(
        serde_json::to_value(store.load_messages(&id).await.unwrap()).unwrap(),
        messages
    );
    assert!(store.load_session_binding(&id).await.unwrap().is_none());
    assert!(store.load_frozen_snapshot(&id).await.unwrap().is_none());
    meta.cwd = cwd.to_str().unwrap().to_owned();
    meta.parent_thread_id = Some(parent.clone());
    store.update_meta(&id, meta.clone()).await.unwrap();
    let error = store
        .adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ExecutionBindingMismatch)
    ));
    assert_eq!(
        store.load_meta(&id).await.unwrap().parent_thread_id,
        Some(parent)
    );
    assert_eq!(
        serde_json::to_value(store.load_messages(&id).await.unwrap()).unwrap(),
        messages
    );
    assert!(store.load_session_binding(&id).await.unwrap().is_none());
    assert!(store.load_frozen_snapshot(&id).await.unwrap().is_none());
    meta.parent_thread_id = None;
    store.update_meta(&id, meta).await.unwrap();
    assert!(store
        .load_meta(&id)
        .await
        .unwrap()
        .parent_thread_id
        .is_none());
    store.close().await;
}

#[tokio::test]
async fn legacy_adoption_rejects_unbound_frozen_session_without_losing_canonical_data() {
    use peri_acp_types::workspace::WorkspaceError;
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let store = SqliteThreadStore::new(dir.path().join("threads.db"))
        .await
        .unwrap();
    let workspace = store.resolve_workspace(&cwd).await.unwrap();
    let id = store
        .create_bound_thread(
            ThreadMeta::new_at(cwd.to_str().unwrap(), peri_time::now_wall()),
            &workspace,
        )
        .await
        .unwrap();
    assert!(store
        .store_frozen_snapshot_if_absent(&id, "native snapshot")
        .await
        .unwrap());
    store
        .append_message(&id, BaseMessage::human("native history"))
        .await
        .unwrap();
    let meta = serde_json::to_value(store.load_meta(&id).await.unwrap()).unwrap();
    let messages = serde_json::to_value(store.load_messages(&id).await.unwrap()).unwrap();
    let deleted = sqlx::query("DELETE FROM session_bindings WHERE thread_id = ?")
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    assert_eq!(deleted.rows_affected(), 1);
    store.close().await;
    let store = SqliteThreadStore::new(dir.path().join("threads.db"))
        .await
        .unwrap();
    let error = store
        .adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::InvalidBinding)
    ));
    assert!(store.load_session_binding(&id).await.unwrap().is_none());
    assert_eq!(
        store.load_frozen_snapshot(&id).await.unwrap().as_deref(),
        Some("native snapshot")
    );
    assert_eq!(
        serde_json::to_value(store.load_meta(&id).await.unwrap()).unwrap(),
        meta
    );
    assert_eq!(
        serde_json::to_value(store.load_messages(&id).await.unwrap()).unwrap(),
        messages
    );
    store.close().await;
}

#[tokio::test]
async fn legacy_history_scopes_keep_path_boundaries_and_mixed_pagination() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap().join("project_%");
    std::fs::create_dir(&cwd).unwrap();
    let store = SqliteThreadStore::new(dir.path().join("threads.db"))
        .await
        .unwrap();
    let ws = store.resolve_workspace(&cwd).await.unwrap();
    let native = store
        .create_bound_thread(
            ThreadMeta::new_at(cwd.to_str().unwrap(), peri_time::now_wall()),
            &ws,
        )
        .await
        .unwrap();
    store
        .append_message(&native, BaseMessage::human("new"))
        .await
        .unwrap();
    let mut expected = vec![native];
    for path in [
        cwd.clone(),
        cwd.join("deleted-subdir"),
        cwd.with_file_name("project_%other"),
        cwd.with_file_name("project_AB"),
    ] {
        let id = store
            .create_thread(ThreadMeta::new_at(
                path.to_str().unwrap(),
                peri_time::now_wall(),
            ))
            .await
            .unwrap();
        store
            .append_message(&id, BaseMessage::human("old"))
            .await
            .unwrap();
        if path == cwd {
            expected.push(id);
        }
    }
    // Register an overlapping root: an EXISTS filter must not duplicate any row.
    store.resolve_workspace(dir.path()).await.unwrap();
    let mut cursor = None;
    let mut found = Vec::new();
    loop {
        let page = store
            .list_scoped_threads(&ScopedThreadQuery {
                scope: ThreadScope::Workspace(ws.workspace_id),
                cursor,
                limit: 1,
            })
            .await
            .unwrap();
        found.extend(page.entries.into_iter().map(|entry| entry.thread.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    found.sort();
    expected.sort();
    assert_eq!(found, expected);
    let exact = store
        .list_scoped_threads(&ScopedThreadQuery {
            scope: ThreadScope::ExactDirectory {
                workspace_id: ws.workspace_id,
                relative_cwd: "deleted-subdir".into(),
            },
            cursor: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(exact.entries.len(), 1);
    assert!(exact.entries[0].binding.is_none());
    store.close().await;
}

async fn assert_legacy_rejected(path: &std::path::Path) {
    let before = legacy_evidence(path).await;
    let backup = path.with_extension("verified-backup");
    std::fs::copy(path, &backup).unwrap();
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        std::fs::read(path).unwrap()
    );
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path).read_only(true))
            .await
            .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    if version != 17 {
        let approval = crate::sessions::StoppedWriterApproval {
            source_version: version,
            writers_stopped: true,
            backup_verified: true,
        };
        assert!(crate::sessions::migrate_stopped_work_store(path, &approval)
            .await
            .is_err());
    }
    let error = SqliteThreadStore::new(path)
        .await
        .err()
        .expect("legacy writer must refuse");
    assert!(
        error
            .to_string()
            .contains("explicit stopped-writer offline migration"),
        "{error:#}"
    );
    let error = SqliteThreadStore::open_existing_read_only(path)
        .await
        .err()
        .expect("inline history is not reference history");
    assert!(matches!(
        error.kind(),
        super::connection::ReadOnlyStoreErrorKind::SchemaIncompatible
    ));
    assert_eq!(
        legacy_evidence(path).await,
        before,
        "refusal must preserve every schema object and row"
    );
}

async fn unbound_history_database(path: &std::path::Path, cwd: &std::path::Path) -> String {
    let store = SqliteThreadStore::new(path).await.unwrap();
    let id = store
        .create_thread(ThreadMeta::new_at(
            cwd.to_str().unwrap(),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    store
        .append_message(&id, BaseMessage::human("history survives upgrade"))
        .await
        .unwrap();
    assert!(store.load_session_binding(&id).await.unwrap().is_none());
    store.close().await;
    id
}

async fn legacy_evidence(path: &std::path::Path) -> Vec<(String, Vec<String>)> {
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path).read_only(true))
            .await
            .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let schema: Vec<String> = sqlx::query_scalar("SELECT json_array(type,name,tbl_name,sql) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").fetch_all(&mut connection).await.unwrap();
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&mut connection).await.unwrap();
    let mut evidence = vec![
        ("version".into(), vec![version.to_string()]),
        ("schema".into(), schema),
    ];
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
                .bind(&table)
                .fetch_all(&mut connection)
                .await
                .unwrap();
        let expressions = columns
            .iter()
            .map(|column| format!("quote(\"{}\")", column.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" || ',' || ");
        let statement = format!(
            "SELECT quote(rowid) || ':' || {expressions} FROM \"{}\" ORDER BY rowid",
            table.replace('"', "\"\"")
        );
        let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
            .fetch_all(&mut connection)
            .await
            .unwrap();
        evidence.push((table, rows));
    }
    connection.close().await.unwrap();
    evidence
}
