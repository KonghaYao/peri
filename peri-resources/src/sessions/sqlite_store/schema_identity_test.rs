// Identity migration fixtures and rollback/preservation cases.

// 独立保留 v2 的 revision 非空且无默认值约束，避免新 schema 掩盖旧库 INSERT 失败。
async fn version2_database(path: &Path) -> SqliteConnection {
    let mut connection = legacy_database(path).await;
    sqlx::raw_sql(
        r#"ALTER TABLE threads ADD COLUMN parent_thread_id TEXT;
        ALTER TABLE threads ADD COLUMN snapshot_at_message_id TEXT;
        ALTER TABLE threads ADD COLUMN hidden BOOLEAN NOT NULL DEFAULT 0;
        ALTER TABLE threads ADD COLUMN cancel_policy TEXT NOT NULL DEFAULT 'cascade';
        ALTER TABLE threads ADD COLUMN config TEXT;
        ALTER TABLE threads ADD COLUMN cached_context TEXT;
        ALTER TABLE threads ADD COLUMN frozen_context TEXT;
        ALTER TABLE threads ADD COLUMN inherited_context TEXT;
        ALTER TABLE threads ADD COLUMN agent_status TEXT NOT NULL DEFAULT 'active';
        ALTER TABLE threads ADD COLUMN context_cache_epoch INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE messages ADD COLUMN truncated BOOLEAN NOT NULL DEFAULT 0;
        ALTER TABLE messages ADD COLUMN excluded BOOLEAN NOT NULL DEFAULT 0;
        ALTER TABLE messages ADD COLUMN projection TEXT;
        CREATE INDEX idx_messages_thread_id ON messages(thread_id);
        CREATE TABLE projects (
            id TEXT PRIMARY KEY, locator TEXT NOT NULL UNIQUE, object_identity TEXT NOT NULL UNIQUE
        );
        CREATE TABLE workspaces (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
            root TEXT NOT NULL UNIQUE, root_identity TEXT NOT NULL UNIQUE, discovery TEXT NOT NULL,
            UNIQUE(id, project_id)
        );
        CREATE TABLE session_bindings (
            thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
            schema_version INTEGER NOT NULL, revision INTEGER NOT NULL,
            project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
            FOREIGN KEY(workspace_id, project_id) REFERENCES workspaces(id, project_id)
        );
        CREATE INDEX idx_bindings_project ON session_bindings(project_id, thread_id);
        CREATE INDEX idx_bindings_workspace ON session_bindings(workspace_id, relative_cwd, thread_id);
        CREATE INDEX idx_threads_updated ON threads(updated_at DESC, id DESC) WHERE hidden = 0 AND message_count > 0;
        CREATE TABLE execution_runs (
            thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
            generation INTEGER NOT NULL, clean BOOLEAN NOT NULL
        );
        INSERT INTO projects VALUES ('11111111-1111-4111-8111-111111111111', '/old/project', '{"device":1,"inode":2,"birth_seconds":3,"birth_nanos":4}');
        INSERT INTO workspaces VALUES ('22222222-2222-4222-8222-222222222222',
            '11111111-1111-4111-8111-111111111111', '/old/worktree', '{"device":5,"inode":6,"birth_seconds":7,"birth_nanos":8}', '{"root":"/old/worktree","root_identity":{"device":5,"inode":6,"birth_seconds":7,"birth_nanos":8},"common_dir":null,"common_identity":null,"private_dir":null,"private_identity":null}');
        INSERT INTO session_bindings VALUES ('old-session', 1, 1,
            '11111111-1111-4111-8111-111111111111', '22222222-2222-4222-8222-222222222222', '');
        INSERT INTO execution_runs VALUES ('old-session', 7, 0);
        PRAGMA user_version = 2;"#,
    ).execute(&mut connection).await.unwrap();
    connection
}

// A real schema-3 registry: old identity payloads still carry birth fields,
// while all directory values point at the live temporary workspace.
async fn version3_database(path: &Path, root: &Path) -> SqliteConnection {
    let mut connection = version2_database(path).await;
    let (_, observed) = super::super::discovery::observe(root).await.unwrap();
    let discovery = observed.discovery;
    let identity_with_legacy_fields = |value: serde_json::Value| {
        let mut value = value;
        value["birth_seconds"] = serde_json::json!(123);
        value["birth_nanos"] = serde_json::json!(456);
        value
    };
    let mut discovery_json = serde_json::to_value(&discovery).unwrap();
    // 旧行（schema ≤14）的文件对象身份：`Discovery` 已不再有这些字段，因此夹具按旧形状
    // 手工补上——它测的正是「v14 及更早写下的字节仍能被识别并升级」。
    let legacy_identity = |device: u64| {
        identity_with_legacy_fields(serde_json::json!({ "device": device, "inode": device }))
    };
    discovery_json["root_identity"] = legacy_identity(1);
    if discovery.common_dir.is_some() {
        // `project_identity()` 取 common 优先、root 兜底：旧行里三者同源。
        discovery_json["common_identity"] = legacy_identity(2);
        discovery_json["private_identity"] = legacy_identity(2);
    } else {
        discovery_json["common_identity"] = serde_json::Value::Null;
        discovery_json["private_identity"] = serde_json::Value::Null;
    }
    let project_identity = if discovery_json["common_identity"].is_null() {
        discovery_json["root_identity"].clone()
    } else {
        discovery_json["common_identity"].clone()
    };
    let root_identity = discovery_json["root_identity"].clone();
    let root = discovery.root.to_str().unwrap();
    let locator = discovery.project_locator().to_str().unwrap();
    sqlx::query("UPDATE projects SET locator = ?, object_identity = ? WHERE id = ?")
        .bind(locator)
        .bind(serde_json::to_string(&project_identity).unwrap())
        .bind("11111111-1111-4111-8111-111111111111")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("UPDATE workspaces SET root = ?, root_identity = ?, discovery = ? WHERE id = ?")
        .bind(root)
        .bind(serde_json::to_string(&root_identity).unwrap())
        .bind(serde_json::to_string(&discovery_json).unwrap())
        .bind("22222222-2222-4222-8222-222222222222")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("UPDATE threads SET frozen_context = ? WHERE id = 'old-session'")
        .bind("frozen-owner-state")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE session_bindings DROP COLUMN revision")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA user_version = 3")
        .execute(&mut connection)
        .await
        .unwrap();
    connection
}

#[tokio::test]
async fn test_schema3_identity_migration_reuses_binding_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let status = std::process::Command::new("git")
        .args(["init", "-q", dir.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "Git fixture initialization failed");
    version3_database(&path, dir.path())
        .await
        .close()
        .await
        .unwrap();

    let store = SqliteThreadStore::try_new(&path).await.unwrap();
    let workspace = store.resolve_workspace(dir.path()).await.unwrap();
    assert_eq!(
        workspace.project_id.to_string(),
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(
        workspace.workspace_id.to_string(),
        "22222222-2222-4222-8222-222222222222"
    );
    let execution: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM sqlite_schema WHERE name = 'execution_runs'")
            .fetch_one(&store.database.pool)
            .await
            .unwrap();
    assert_eq!(execution, (0,));
    assert_eq!(
        store
            .load_messages(&"old-session".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .load_frozen_snapshot(&"old-session".to_owned())
            .await
            .unwrap()
            .as_deref(),
        Some("frozen-owner-state")
    );
    let binding = store
        .load_session_binding(&"old-session".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        binding.project_id.to_string(),
        workspace.project_id.to_string()
    );
    assert_eq!(
        store
            .validate_session_binding(&"old-session".to_owned())
            .await
            .unwrap(),
        workspace
    );
    store.close().await;

    let reopened = SqliteThreadStore::try_new(&path).await.unwrap();
    let again = reopened.resolve_workspace(dir.path()).await.unwrap();
    assert_eq!(again.project_id, workspace.project_id);
    assert_eq!(again.workspace_id, workspace.workspace_id);
    assert_eq!(
        reopened
            .load_session_binding(&"old-session".to_owned())
            .await
            .unwrap()
            .unwrap()
            .workspace_id,
        workspace.workspace_id
    );
    reopened.close().await;
}

/// 记录 v2 升级不能改动的身份数据：项目、登记空间与绑定三段的原样值。
///
/// 登记空间的**载体与列名**随版本变化：v12 之前是 `workspaces(root, root_identity)`，
/// v12..v18 是改名为 `legacy_execution_registrations` 的原表，v19 起并回
/// `workspaces(path, ...)`。文件对象身份列在 v15 退役，因此投影只取跨代稳定的事实
/// (id, project_id, 根路径, 发现快照)——根路径在搬运中逐字不变。
async fn identity_bytes(connection: &mut SqliteConnection) -> Vec<String> {
    let registrations = column_names(connection, "legacy_execution_registrations")
        .await
        .unwrap();
    let workspaces = column_names(connection, "workspaces").await.unwrap();
    let registration_query = match (registrations.is_empty(), workspaces.contains("root")) {
        (false, _) => {
            "SELECT json_array(id, project_id, root, discovery)
             FROM legacy_execution_registrations ORDER BY id"
        }
        (true, true) => {
            "SELECT json_array(id, project_id, root, discovery)
             FROM workspaces ORDER BY id"
        }
        (true, false) => {
            "SELECT json_array(id, project_id, path, discovery)
             FROM workspaces ORDER BY id"
        }
    };
    let mut values = Vec::new();
    for query in [
        "SELECT json_array(id, locator) FROM projects ORDER BY id",
        registration_query,
        "SELECT json_array(thread_id, schema_version, project_id, workspace_id, relative_cwd) FROM session_bindings ORDER BY thread_id",
    ] {
        let rows: Vec<(String,)> = sqlx::query_as(AssertSqlSafe(query))
            .fetch_all(&mut *connection)
            .await
            .unwrap();
        values.extend(rows.into_iter().map(|(value,)| value));
    }
    values
}

#[tokio::test]
async fn test_version2_upgrade_removes_required_revision_and_preserves_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    let revision: (i64, Option<String>) = sqlx::query_as(
        "SELECT \"notnull\", dflt_value FROM pragma_table_info('session_bindings') WHERE name = 'revision'",
    ).fetch_one(&mut connection).await.unwrap();
    assert_eq!(revision, (1, None));
    let before_history = history_bytes(&mut connection).await;
    connection.close().await.unwrap();

    let store = SqliteThreadStore::try_new(&path).await.unwrap();
    let mut connection = store.database.pool.acquire().await.unwrap();
    assert_eq!(history_bytes(&mut connection).await, before_history);
    // v15 起身份列整体退役：`projects` / `workspaces` / `session_bindings` 都不再有它们。
    // `workspaces.discovery` 的字节**不做清洗**（历史证据原样保留），因此这里断言的是列，
    // 不是快照内容。
    for (table, column) in [
        ("projects", "object_identity"),
        ("workspaces", "identity"),
        ("workspaces", "root_identity"),
        ("session_bindings", "evidence_origin"),
    ] {
        assert!(
            !column_names(&mut connection, table)
                .await
                .unwrap()
                .contains(column),
            "{table}.{column} 必须在 v15 退役"
        );
    }
    assert!(!column_names(&mut connection, "session_bindings")
        .await
        .unwrap()
        .contains("revision"));
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
    drop(connection);

    let old = store
        .load_session_binding(&"old-session".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.revision, 1);
    assert_eq!(
        old.project_id.to_string(),
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(
        old.workspace_id.to_string(),
        "22222222-2222-4222-8222-222222222222"
    );
    assert!(old.cwd_relative_to_workspace.as_os_str().is_empty());
    let workspace = store.resolve_workspace(dir.path()).await.unwrap();
    let id = store
        .create_bound_thread(
            ThreadMeta::new_at(dir.path().to_str().unwrap(), peri_time::now_wall()),
            &workspace,
        )
        .await
        .unwrap();
    assert_eq!(
        store.validate_session_binding(&id).await.unwrap(),
        workspace
    );
    assert_eq!(
        store
            .load_session_binding(&id)
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    store.close().await;
}

#[tokio::test]
async fn test_version2_failed_column_drop_preserves_schema_version_and_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    // 现有视图依赖 revision 时，SQLite 必须拒绝删除该列。
    sqlx::query("CREATE VIEW binding_revision_view AS SELECT revision FROM session_bindings")
        .execute(&mut connection)
        .await
        .unwrap();
    let before_schema: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    let before_data = identity_bytes(&mut connection).await;
    connection.close().await.unwrap();
    let error = SqliteThreadStore::try_new(&path).await.err().unwrap();
    let message = error.to_string();
    assert!(
        message.contains("binding_revision_view") && message.contains("no such column: revision"),
        "应因视图依赖 revision 而拒绝 DROP COLUMN，实际错误：{message}"
    );
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let after_schema: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(after_schema, before_schema);
    assert_eq!(identity_bytes(&mut connection).await, before_data);
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 2);
    connection.close().await.unwrap();
}

#[tokio::test]
async fn test_identity_migration_collision_rolls_back_schema_and_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    sqlx::query("INSERT INTO projects VALUES (?, ?, ?)")
        .bind("33333333-3333-4333-8333-333333333333")
        .bind("/another/project")
        .bind(r#"{"device":1,"inode":2,"birth_seconds":9,"birth_nanos":10}"#)
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    assert!(SqliteThreadStore::try_new(&path).await.is_err());
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 2, "冲突必须回滚版本号");
    let (identity,): (String,) = sqlx::query_as(
        "SELECT object_identity FROM projects WHERE id = '11111111-1111-4111-8111-111111111111'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert!(identity.contains("birth_seconds"), "回滚不得改写旧身份");
}

#[tokio::test]
async fn test_identity_migration_corrupt_discovery_rolls_back_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    sqlx::query("UPDATE workspaces SET discovery = 'corrupt'")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    assert!(SqliteThreadStore::try_new(&path).await.is_err());
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 2, "损坏 discovery 必须回滚版本号");
}
