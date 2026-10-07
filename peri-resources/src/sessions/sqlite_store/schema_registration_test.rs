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

#[tokio::test]
async fn test_version4_refusal_preserves_registration_keys_and_rows() {
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
    connection.close().await.unwrap();

    assert_legacy_rejected(&path).await;
}

/// 不可访问的旧会话也必须能升级；迁移不得为了补登记约束发现旧目录。
async fn assert_registration_refusal_preserves_moved_directory_evidence(version: i64) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let old_root = dir.path().join("original");
    let moved_root = dir.path().join("moved");
    std::fs::create_dir_all(&old_root).unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "-q", old_root.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());
    let mut connection = version3_database(&path, &old_root).await;
    if version == 2 {
        sqlx::raw_sql("CREATE TABLE bindings_v2(thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,schema_version INTEGER NOT NULL,revision INTEGER NOT NULL,project_id TEXT NOT NULL,workspace_id TEXT NOT NULL,relative_cwd TEXT NOT NULL,FOREIGN KEY(workspace_id,project_id) REFERENCES workspaces(id,project_id)); INSERT INTO bindings_v2 SELECT thread_id,schema_version,1,project_id,workspace_id,relative_cwd FROM session_bindings; DROP TABLE session_bindings; ALTER TABLE bindings_v2 RENAME TO session_bindings; PRAGMA user_version=2").execute(&mut connection).await.unwrap();
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
    connection.close().await.unwrap();
    std::fs::rename(&old_root, &moved_root).unwrap();
    std::fs::create_dir_all(&old_root).unwrap();
    assert_legacy_rejected(&path).await;
}

#[tokio::test]
async fn test_registration_refusal_from_v2_preserves_moved_and_replaced_directories() {
    assert_registration_refusal_preserves_moved_directory_evidence(2).await;
}

#[tokio::test]
async fn test_registration_refusal_from_v3_preserves_moved_and_replaced_directories() {
    assert_registration_refusal_preserves_moved_directory_evidence(3).await;
}

#[tokio::test]
async fn test_registration_refusal_from_v4_preserves_moved_and_replaced_directories() {
    assert_registration_refusal_preserves_moved_directory_evidence(4).await;
}

#[tokio::test]
async fn test_registration_refusal_preserves_incomplete_v5() {
    assert_registration_refusal_preserves_moved_directory_evidence(5).await;
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

#[tokio::test]
async fn test_registration_refusal_preserves_healthy_v5_composite_registrations() {
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
    connection.close().await.unwrap();
    for _ in 0..2 {
        assert_legacy_rejected(&path).await;
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
    assert!(error
        .to_string()
        .contains("explicit stopped-writer offline migration"));
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
