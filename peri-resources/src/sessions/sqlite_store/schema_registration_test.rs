use super::*;

/// 现行 schema 4：身份载荷已规范化，登记表仍保留单列唯一约束。
async fn version4_database(path: &Path, root: &Path) -> SqliteConnection {
    let mut connection = version3_database(path, root).await;
    let (identity,): (String,) = sqlx::query_as("SELECT object_identity FROM projects")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let identity = crate::sessions::sqlite_store::discovery::normalize_identity_json(
        &serde_json::from_str(&identity).unwrap(),
    )
    .unwrap();
    sqlx::query("UPDATE projects SET object_identity = ?")
        .bind(identity)
        .execute(&mut connection)
        .await
        .unwrap();
    let (root_identity, discovery): (String, String) =
        sqlx::query_as("SELECT root_identity, discovery FROM workspaces")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    let root_identity = crate::sessions::sqlite_store::discovery::normalize_identity_json(
        &serde_json::from_str(&root_identity).unwrap(),
    )
    .unwrap();
    let discovery = crate::sessions::sqlite_store::discovery::normalize_discovery_json(
        &serde_json::from_str(&discovery).unwrap(),
    )
    .unwrap();
    sqlx::query("UPDATE workspaces SET root_identity = ?, discovery = ?")
        .bind(root_identity)
        .bind(discovery)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA user_version = 4")
        .execute(&mut connection)
        .await
        .unwrap();
    connection
}

/// [回归测试] schema 4 的单列唯一约束把「同一路径上的另一个文件对象」挡在登记之外，
/// 目录被替换或换位后该路径无法建立新会话。升级只把登记键放宽为组合键：行、绑定、
/// 外键、线程行与消息（含 frozen snapshot）都保持原样，同一定位 + 同一证据仍然唯一。
#[tokio::test]
async fn test_version4_upgrade_relaxes_registration_keys_and_preserves_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let status = std::process::Command::new("git")
        .args(["init", "-q", dir.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "Git fixture initialization failed");
    let mut connection = version4_database(&path, dir.path()).await;
    // 升级前：同一 root 上的第二个文件对象无法登记。
    let blocked = sqlx::query(
        "INSERT INTO workspaces (id, project_id, root, root_identity, discovery)
         SELECT '33333333-3333-4333-8333-333333333333', project_id, root, '{\"device\":9,\"inode\":9}', discovery
         FROM workspaces",
    )
    .execute(&mut connection)
    .await;
    assert!(blocked.is_err(), "schema 4 的单列唯一约束必须仍然存在");
    let before = identity_bytes(&mut connection).await;
    let before_history = history_bytes(&mut connection).await;
    connection.close().await.unwrap();

    let store = SqliteThreadStore::new(&path).await.unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);

    // 原有登记、绑定与历史原样可用。
    let workspace = store.resolve_workspace(dir.path()).await.unwrap();
    assert_eq!(
        workspace.project_id.to_string(),
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(
        workspace.workspace_id.to_string(),
        "22222222-2222-4222-8222-222222222222"
    );
    assert_eq!(
        store
            .validate_session_binding(&"old-session".to_owned())
            .await
            .unwrap(),
        workspace
    );
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
        Some("frozen-owner-state"),
        "迁移不得丢失 frozen snapshot"
    );

    // 同一路径上的另一个文件对象可以登记，同一 (locator, 证据) 组合仍然唯一。
    let mut probe = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let after = identity_bytes(&mut probe).await;
    let after_history = history_bytes(&mut probe).await;
    probe.close().await.unwrap();
    assert_eq!(before, after, "迁移不得改写登记、绑定或执行状态");
    assert_eq!(
        before_history, after_history,
        "迁移不得改写线程行与消息：frozen snapshot 与历史都在其中"
    );

    sqlx::query(
        "INSERT INTO legacy_execution_registrations (id, project_id, root, root_identity, discovery)
         SELECT '33333333-3333-4333-8333-333333333333', project_id, root, '{\"device\":9,\"inode\":9}', discovery
         FROM legacy_execution_registrations WHERE id = '22222222-2222-4222-8222-222222222222'",
    )
    .execute(&store.database.pool)
    .await
    .unwrap();
    let duplicate_workspace = sqlx::query(
        "INSERT INTO legacy_execution_registrations (id, project_id, root, root_identity, discovery)
         SELECT '44444444-4444-4444-8444-444444444444', project_id, root, root_identity, discovery
         FROM legacy_execution_registrations WHERE id = '22222222-2222-4222-8222-222222222222'",
    )
    .execute(&store.database.pool)
    .await;
    assert!(
        duplicate_workspace.is_err(),
        "同一 (root, root_identity) 不得重复登记"
    );
    // 同一 locator 上的另一个文件对象可以登记（同一路径重新克隆）。
    sqlx::query(
        "INSERT INTO projects (id, locator, object_identity)
         SELECT '55555555-5555-4555-8555-555555555555', locator, '{\"device\":10,\"inode\":10}' FROM projects",
    )
    .execute(&store.database.pool)
    .await
    .unwrap();
    let duplicate_project = sqlx::query(
        "INSERT INTO projects (id, locator, object_identity)
         SELECT '66666666-6666-4666-8666-666666666666', locator, object_identity FROM projects
         WHERE id = '11111111-1111-4111-8111-111111111111'",
    )
    .execute(&store.database.pool)
    .await;
    assert!(
        duplicate_project.is_err(),
        "同一 (locator, object_identity) 不得重复登记"
    );
    // 重建登记表不能丢外键：引用不存在项目的工作区仍被拒绝。
    let orphan = sqlx::query(
        "INSERT INTO legacy_execution_registrations (id, project_id, root, root_identity, discovery)
         SELECT '77777777-7777-4777-8777-777777777777', 'missing-project', root || '-orphan', root_identity, discovery
         FROM legacy_execution_registrations WHERE id = '22222222-2222-4222-8222-222222222222'",
    )
    .execute(&store.database.pool)
    .await;
    assert!(orphan.is_err(), "工作区必须仍受 projects 外键约束");
    store.close().await;
}

/// 不可访问的旧会话也必须能升级；迁移不得为了补登记约束发现旧目录。
async fn assert_registration_upgrade_allows_directory_changes(version: i64) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    if version >= 3 {
        sqlx::query("ALTER TABLE session_bindings DROP COLUMN revision")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("PRAGMA user_version = 3")
            .execute(&mut connection)
            .await
            .unwrap();
    }
    if version >= 4 {
        // 独立重现旧版 3→4 的输出；5 的缺陷形态正是只更新版本号、未迁移约束。
        sqlx::raw_sql(
            "UPDATE projects SET object_identity = json_remove(object_identity, '$.birth_seconds', '$.birth_nanos');
             UPDATE workspaces SET root_identity = json_remove(root_identity, '$.birth_seconds', '$.birth_nanos'),
                discovery = json_remove(discovery, '$.root_identity.birth_seconds', '$.root_identity.birth_nanos');"
        ).execute(&mut connection).await.unwrap();
        sqlx::query(if version == 4 {
            "PRAGMA user_version = 4"
        } else {
            "PRAGMA user_version = 5"
        })
        .execute(&mut connection)
        .await
        .unwrap();
    }
    let old_history = history_bytes(&mut connection).await;
    let old_identity = identity_bytes(&mut connection).await;
    connection.close().await.unwrap();
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let original = dir.path().join("original");
    std::fs::create_dir(&original).unwrap();
    let workspace = store.resolve_workspace(&original).await.unwrap();
    let thread = store
        .create_bound_thread(
            ThreadMeta::new_at(original.to_str().unwrap(), peri_time::now_wall()),
            &workspace,
        )
        .await
        .unwrap();
    let binding = store.load_session_binding(&thread).await.unwrap();
    store.close().await;
    // 跨越关闭/重开；移动保留旧文件对象，再在原位置创建新对象，不依赖 inode 复用时序。
    let moved = dir.path().join("moved");
    std::fs::rename(&original, &moved).unwrap();
    std::fs::create_dir(&original).unwrap();
    let store = SqliteThreadStore::new(&path).await.unwrap();
    for cwd in [&moved, &original] {
        let resolved = store.resolve_workspace(cwd).await.unwrap_or_else(|error| {
            panic!("schema {version} 升级后目录 {cwd:?} 必须可登记：{error}")
        });
        assert_eq!(
            resolved.workspace_id == workspace.workspace_id,
            cwd == &original
        );
        assert_ne!(
            resolved.execution_registration_id,
            workspace.execution_registration_id
        );
        assert_ne!(resolved.project_id, workspace.project_id);
        let new_thread = store
            .create_bound_thread(
                ThreadMeta::new_at(cwd.to_str().unwrap(), peri_time::now_wall()),
                &resolved,
            )
            .await
            .unwrap();
        let owner = store.acquire_execution_lease(&new_thread).await.unwrap();
        assert_eq!(
            store.validate_session_binding(&new_thread).await.unwrap(),
            resolved
        );
        owner.mark_clean().await.unwrap();
    }
    assert_eq!(store.load_session_binding(&thread).await.unwrap(), binding);
    let error = store.validate_session_binding(&thread).await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<WorkspaceError>(),
            Some(WorkspaceError::NeedsRelink)
        ),
        "原路径被替换后旧会话必须拒绝执行：{error}"
    );
    let mut connection = store.database.pool.acquire().await.unwrap();
    // 精确读取旧会话，不以新增线程的插入顺序推断目标。
    assert_eq!(history_bytes(&mut connection).await, old_history);
    let after = identity_bytes(&mut connection).await;
    assert!(
        old_identity[2..].iter().all(|row| after.contains(row)),
        "原 binding 不得被迁移清除或改写"
    );
    if version >= 4 {
        assert!(
            old_identity.iter().all(|row| after.contains(row)),
            "已规范化的登记证据必须原样保留"
        );
    }
    let (current,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(current, CURRENT_SCHEMA_VERSION);
    drop(connection);
    store.close().await;
}

#[tokio::test]
async fn test_registration_upgrade_from_v2_allows_moved_and_replaced_directories() {
    assert_registration_upgrade_allows_directory_changes(2).await;
}

#[tokio::test]
async fn test_registration_upgrade_from_v3_allows_moved_and_replaced_directories() {
    assert_registration_upgrade_allows_directory_changes(3).await;
}

#[tokio::test]
async fn test_registration_upgrade_from_v4_allows_moved_and_replaced_directories() {
    assert_registration_upgrade_allows_directory_changes(4).await;
}

/// [回归测试] 已被旧 writer 标记为 5 的漏迁移库，重启后也必须自动补齐约束。
#[tokio::test]
async fn test_registration_upgrade_repairs_incomplete_v5() {
    assert_registration_upgrade_allows_directory_changes(5).await;
}

/// 独立构造健康 schema 5 的登记表；不能由本轮迁移生成 fixture，否则会掩盖回归。
async fn healthy_version5_database(path: &Path) -> SqliteConnection {
    let mut connection = version2_database(path).await;
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF;
         ALTER TABLE session_bindings DROP COLUMN revision;
         CREATE TABLE projects_v5 (
             id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
             UNIQUE(locator, object_identity));
         INSERT INTO projects_v5 SELECT id, locator, json_remove(object_identity, '$.birth_seconds', '$.birth_nanos') FROM projects;
         DROP TABLE projects;
         ALTER TABLE projects_v5 RENAME TO projects;
         CREATE TABLE workspaces_v5 (
             id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
             root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
             UNIQUE(root, root_identity), UNIQUE(id, project_id));
         INSERT INTO workspaces_v5 SELECT id, project_id, root,
             json_remove(root_identity, '$.birth_seconds', '$.birth_nanos'),
             json_remove(discovery, '$.root_identity.birth_seconds', '$.root_identity.birth_nanos') FROM workspaces;
         DROP TABLE workspaces;
         ALTER TABLE workspaces_v5 RENAME TO workspaces;
         PRAGMA user_version = 5;
         PRAGMA foreign_keys = ON;"
    ).execute(&mut connection).await.unwrap();
    connection
}

/// [回归测试] 健康 5 已允许同路径多对象、同对象多路径，补迁移不能重新收紧或归并它们。
#[tokio::test]
async fn test_registration_upgrade_preserves_healthy_v5_composite_registrations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = healthy_version5_database(&path).await;
    sqlx::raw_sql(
        "INSERT INTO projects SELECT '33333333-3333-4333-8333-333333333333', locator || '-moved', object_identity FROM projects;
         INSERT INTO projects SELECT '44444444-4444-4444-8444-444444444444', locator, '{\"device\":9,\"inode\":9}'
             FROM projects WHERE id = '11111111-1111-4111-8111-111111111111';
         INSERT INTO workspaces SELECT '55555555-5555-4555-8555-555555555555', project_id, root || '-moved', root_identity, json_set(discovery, '$.root', root || '-moved') FROM workspaces;
         INSERT INTO workspaces SELECT '66666666-6666-4666-8666-666666666666', project_id, root, '{\"device\":9,\"inode\":9}', discovery
             FROM workspaces WHERE id = '22222222-2222-4222-8222-222222222222';"
    ).execute(&mut connection).await.unwrap();
    let before = identity_bytes(&mut connection).await;
    let history = history_bytes(&mut connection).await;
    connection.close().await.unwrap();
    // 首次开库升级，第二次开库保持同一结果；不访问 fixture 中不存在的旧目录。
    for _ in 0..2 {
        let store = SqliteThreadStore::new(&path).await.unwrap();
        let mut connection = store.database.pool.acquire().await.unwrap();
        assert_eq!(identity_bytes(&mut connection).await, before);
        assert_eq!(history_bytes(&mut connection).await, history);
        let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        drop(connection);
        store.close().await;
    }
}

/// [回归测试] 补迁移遇到损坏引用必须回滚，不能留下半张表、升级版本或清 dirty。
#[tokio::test]
async fn test_registration_upgrade_corrupt_v5_rolls_back_all_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = healthy_version5_database(&path).await;
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF;
         UPDATE session_bindings SET workspace_id = 'missing-workspace';
         PRAGMA foreign_keys = ON;",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let rows = identity_bytes(&mut connection).await;
    let history = history_bytes(&mut connection).await;
    let schema: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    connection.close().await.unwrap();
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(
        matches!(error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "registration rebuild broke references"),
        "悬空引用必须拒绝提交：{error}"
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
    assert_eq!(after_schema, schema);
    assert_eq!(identity_bytes(&mut connection).await, rows);
    assert_eq!(history_bytes(&mut connection).await, history);
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 5, "失败不得提交新版本号");
    connection.close().await.unwrap();
}
