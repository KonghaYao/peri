use super::*;
use crate::sessions::sqlite_store::{schema::CURRENT_SCHEMA_VERSION, SqliteThreadStore};
use std::sync::Arc;
use tokio::{sync::Barrier, task::JoinSet};

async fn concurrent_stores(path: &Path, instances: usize) -> Vec<SqliteThreadStore> {
    let barrier = Arc::new(Barrier::new(instances));
    let mut opens = JoinSet::new();
    for _ in 0..instances {
        let path = path.to_path_buf();
        let barrier = barrier.clone();
        opens.spawn(async move {
            barrier.wait().await;
            SqliteThreadStore::new(path).await
        });
    }
    let mut stores = Vec::new();
    while let Some(opened) = opens.join_next().await {
        stores.push(opened.unwrap().expect("concurrent database open"));
    }
    stores
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_cold_opens_initialize_wal_and_schema_repeatedly() {
    let directory = tempfile::tempdir().unwrap();
    for iteration in 0..16 {
        let path = directory.path().join(format!("cold-{iteration}.db"));
        let stores = concurrent_stores(&path, 8).await;
        for store in &stores {
            let (mode,): (String,) = sqlx::query_as("PRAGMA journal_mode")
                .fetch_one(&store.database.pool)
                .await
                .unwrap();
            let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
                .fetch_one(&store.database.pool)
                .await
                .unwrap();
            assert_eq!(mode, "wal");
            assert_eq!(version, CURRENT_SCHEMA_VERSION);
        }
        for store in stores {
            store.close().await;
        }
        assert!(!path.with_extension("db-wal").exists());
        assert!(!path.with_extension("db-shm").exists());
        let reopened = SqliteThreadStore::new(&path).await.unwrap();
        reopened.close().await;
    }
}

async fn registration_upgrade_fixture(path: &Path) {
    let mut connection = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    for statement in crate::sessions::canonical::CREATE_TABLES {
        sqlx::raw_sql(AssertSqlSafe((*statement).to_owned()))
            .execute(&mut connection)
            .await
            .unwrap();
    }
    sqlx::raw_sql(
        "DROP TABLE workspaces;
         DROP TABLE projects;
         CREATE TABLE projects (
             id TEXT PRIMARY KEY, locator TEXT NOT NULL UNIQUE,
             object_identity TEXT NOT NULL UNIQUE
         );
         CREATE TABLE workspaces (
             id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
             root TEXT NOT NULL UNIQUE, root_identity TEXT NOT NULL UNIQUE,
             discovery TEXT NOT NULL, UNIQUE(id, project_id)
         );
         INSERT INTO projects VALUES ('project', '/original', 'project-identity');
         INSERT INTO workspaces VALUES (
             '11111111-1111-4111-8111-111111111111', 'project', '/original', 'workspace-identity', '{\"root\":\"/original\",\"root_identity\":{\"device\":1,\"inode\":1},\"common_dir\":null,\"common_identity\":null,\"private_dir\":null,\"private_identity\":null}'
         );
         PRAGMA user_version = 4;",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_migration_rebuilds_registration_tables_only_once() {
    let directory = tempfile::tempdir().unwrap();
    let control_path = directory.path().join("control.db");
    registration_upgrade_fixture(&control_path).await;
    let control = SqliteThreadStore::new(&control_path).await.unwrap();
    let (expected_cookie,): (i64,) = sqlx::query_as("PRAGMA schema_version")
        .fetch_one(&control.database.pool)
        .await
        .unwrap();
    control.close().await;
    let path = directory.path().join("concurrent-upgrade.db");
    registration_upgrade_fixture(&path).await;
    let stores = concurrent_stores(&path, 8).await;
    for store in stores {
        let (cookie,): (i64,) = sqlx::query_as("PRAGMA schema_version")
            .fetch_one(&store.database.pool)
            .await
            .unwrap();
        assert_eq!(cookie, expected_cookie, "DDL must run only once");
        let row: (String, String, String, String) = sqlx::query_as(
            "SELECT projects.id, projects.object_identity, w.id, w.discovery
             FROM projects JOIN workspaces w ON w.project_id = projects.id",
        )
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
        assert_eq!(
            row,
            (
                "project".into(),
                "project-identity".into(),
                "11111111-1111-4111-8111-111111111111".into(),
                "{\"root\":\"/original\",\"root_identity\":{\"device\":1,\"inode\":1},\"common_dir\":null,\"common_identity\":null,\"private_dir\":null,\"private_identity\":null}".into()
            )
        );
        store.close().await;
    }
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    let (cookie,): (i64,) = sqlx::query_as("PRAGMA schema_version")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(cookie, expected_cookie);
    reopened.close().await;
}

#[tokio::test]
async fn initialization_lock_has_a_budget_and_preserves_the_sidecar() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.db");
    let held = lock_schema_open(&path, Duration::from_secs(1))
        .await
        .unwrap();
    let lock_path = schema_lock_path(&path).await.unwrap();
    drop(held);
    tokio::fs::write(&lock_path, b"stable sidecar")
        .await
        .unwrap();
    let held = lock_schema_open(&path, Duration::ZERO).await.unwrap();
    let waiting = lock_schema_open(&path, Duration::from_millis(40));
    tokio::pin!(waiting);
    tokio::select! {
        result = &mut waiting => panic!("lock wait ended before the timer: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(5)) => {}
    }
    let error = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::TimedOut
    );
    drop(held);
    let retried = lock_schema_open(&path, Duration::from_secs(1))
        .await
        .unwrap();
    drop(retried);
    assert_eq!(
        tokio::fs::read(&lock_path).await.unwrap(),
        b"stable sidecar"
    );
    assert!(lock_path.exists());
}

#[tokio::test]
async fn cancelling_a_lock_holder_allows_another_database_open() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cancelled-holder.db");
    let (ready, acquired) = tokio::sync::oneshot::channel();
    let holder_path = path.clone();
    let holder = tokio::spawn(async move {
        let _held = lock_schema_open(&holder_path, Duration::from_secs(1))
            .await
            .unwrap();
        ready.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    acquired.await.unwrap();
    holder.abort();
    assert!(holder.await.unwrap_err().is_cancelled());
    let reacquired = lock_schema_open(&path, SCHEMA_OPEN_LOCK_TIMEOUT)
        .await
        .unwrap();
    drop(reacquired);
    let store = SqliteThreadStore::new(&path).await.unwrap();
    store.close().await;
    assert!(schema_lock_path(&path).await.unwrap().exists());
}

#[tokio::test]
async fn cancelling_a_waiter_does_not_acquire_an_orphan_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cancelled-waiter.db");
    let held = lock_schema_open(&path, Duration::from_secs(1))
        .await
        .unwrap();
    let waiter_path = path.clone();
    let waiter = tokio::spawn(async move { SqliteThreadStore::new(waiter_path).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!waiter.is_finished());
    waiter.abort();
    assert!(waiter.await.err().unwrap().is_cancelled());
    drop(held);
    let reacquired = lock_schema_open(&path, SCHEMA_OPEN_LOCK_TIMEOUT)
        .await
        .unwrap();
    drop(reacquired);
    let store = SqliteThreadStore::new(&path).await.unwrap();
    store.close().await;
}

#[tokio::test]
async fn cancelling_an_open_during_schema_initialization_releases_its_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cancelled-initialization.db");
    let initial = SqliteThreadStore::new(&path).await.unwrap();
    initial.close().await;
    let mut blocker =
        sqlx::SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut blocker)
        .await
        .unwrap();
    let opening_path = path.clone();
    let opening = tokio::spawn(async move { SqliteThreadStore::new(opening_path).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match lock_schema_open(&path, Duration::ZERO).await {
                Ok(held) => drop(held),
                Err(error) => {
                    assert_eq!(
                        error.downcast_ref::<std::io::Error>().unwrap().kind(),
                        std::io::ErrorKind::TimedOut
                    );
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!opening.is_finished());
    opening.abort();
    assert!(opening.await.err().unwrap().is_cancelled());
    // abort 不会中断已启动的 spawn_blocking 锁尝试；等待它归还文件句柄。
    let held = lock_schema_open(&path, Duration::from_secs(1))
        .await
        .unwrap();
    drop(held);
    sqlx::query("ROLLBACK").execute(&mut blocker).await.unwrap();
    blocker.close().await.unwrap();
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    reopened.close().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while tokio::fs::try_exists(path.with_extension("db-wal"))
            .await
            .unwrap()
            || tokio::fs::try_exists(path.with_extension("db-shm"))
                .await
                .unwrap()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!path.with_extension("db-wal").exists());
    assert!(!path.with_extension("db-shm").exists());
}

#[tokio::test]
async fn failed_migration_closes_connections_and_releases_initialization_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failed.db");
    let mut connection = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("fixtures/legacy_with_goals.sql"))
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("CREATE VIEW session_bindings AS SELECT id FROM threads")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(error.to_string().contains("session_bindings"));
    assert!(!path.with_extension("db-wal").exists());
    assert!(!path.with_extension("db-shm").exists());
    let held = lock_schema_open(&path, Duration::ZERO).await.unwrap();
    drop(held);
    let mut connection =
        sqlx::SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 0);
    sqlx::query("DROP VIEW session_bindings")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let store = SqliteThreadStore::new(&path).await.unwrap();
    store.close().await;
}

#[tokio::test]
async fn incompatible_schema_is_rejected_without_wal_or_database_changes() {
    let directory = tempfile::tempdir().unwrap();
    for version in [0, CURRENT_SCHEMA_VERSION + 1] {
        let path = directory.path().join(format!("unsupported-{version}.db"));
        let mut connection = sqlx::SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::raw_sql(AssertSqlSafe(format!(
            "CREATE TABLE unrelated (value TEXT); PRAGMA user_version = {version};"
        )))
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
        let before = tokio::fs::read(&path).await.unwrap();
        let error = SqliteThreadStore::new(&path).await.err().unwrap();
        match error.downcast_ref::<WorkspaceError>().unwrap() {
            WorkspaceError::UnsupportedDatabaseSchema => assert_eq!(version, 0),
            WorkspaceError::UnsupportedSchemaVersion { found, supported } => {
                assert_eq!(*found, version);
                assert_eq!(*supported, CURRENT_SCHEMA_VERSION);
            }
            other => panic!("unexpected error classification: {other:?}"),
        }
        assert_eq!(tokio::fs::read(&path).await.unwrap(), before);
        assert!(!path.with_extension("db-wal").exists());
        assert!(!path.with_extension("db-shm").exists());
        let held = lock_schema_open(&path, Duration::ZERO).await.unwrap();
        drop(held);
    }
}

#[tokio::test]
async fn read_only_open_never_creates_an_initialization_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("read-only.db");
    let mut connection = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    for statement in crate::sessions::canonical::CREATE_TABLES {
        sqlx::raw_sql(AssertSqlSafe((*statement).to_owned()))
            .execute(&mut connection)
            .await
            .unwrap();
    }
    connection.close().await.unwrap();
    let before = tokio::fs::read(&path).await.unwrap();
    let store = SqliteThreadStore::open_existing_read_only(&path)
        .await
        .unwrap();
    store.close().await;
    assert_eq!(tokio::fs::read(&path).await.unwrap(), before);
    assert!(!schema_lock_path(&path).await.unwrap().exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_database_and_symlink_aliases_share_one_lock_inode() {
    use std::os::unix::fs::{symlink, MetadataExt};

    let directory = tempfile::tempdir().unwrap();
    let aliases = tempfile::tempdir().unwrap();
    symlink(directory.path(), aliases.path().join("directory")).unwrap();
    let path = directory.path().join("aliased.db");
    let cold_alias = aliases.path().join("directory/aliased.db");
    let held = lock_schema_open(&path, Duration::ZERO).await.unwrap();
    let lock_path = schema_lock_path(&path).await.unwrap();
    let inode = std::fs::metadata(&lock_path).unwrap().ino();
    assert!(lock_schema_open(&cold_alias, Duration::ZERO).await.is_err());
    drop(held);
    let (first, second) = tokio::join!(
        SqliteThreadStore::new(&path),
        SqliteThreadStore::new(&cold_alias)
    );
    first.unwrap().close().await;
    second.unwrap().close().await;
    let existing_alias = aliases.path().join("existing.db");
    symlink(&path, &existing_alias).unwrap();
    assert_eq!(schema_lock_path(&existing_alias).await.unwrap(), lock_path);
    let held = lock_schema_open(&existing_alias, Duration::ZERO)
        .await
        .unwrap();
    assert!(lock_schema_open(&path, Duration::ZERO).await.is_err());
    drop(held);
    assert_eq!(std::fs::metadata(&lock_path).unwrap().ino(), inode);
    assert!(!aliases.path().join("existing.db.schema-lock").exists());
}

#[test]
fn initialization_lock_child_process() {
    let Some(lock_path) = std::env::var_os("PERI_INITIALIZATION_LOCK_CHILD") else {
        return;
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    file.try_lock().unwrap();
    std::fs::write(
        std::env::var_os("PERI_INITIALIZATION_LOCK_READY").unwrap(),
        b"ready",
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(30));
    drop(file);
}

#[tokio::test]
async fn exited_process_releases_initialization_lock_for_retry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("exited-holder.db");
    let held = lock_schema_open(&path, Duration::ZERO).await.unwrap();
    drop(held);
    let lock_path = schema_lock_path(&path).await.unwrap();
    let ready_path = directory.path().join("child.ready");
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::sqlite_store::connection::open_tests::initialization_lock_child_process",
        ])
        .env("PERI_INITIALIZATION_LOCK_CHILD", &lock_path)
        .env("PERI_INITIALIZATION_LOCK_READY", &ready_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !tokio::fs::try_exists(&ready_path).await.unwrap() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let error = lock_schema_open(&path, Duration::from_millis(40))
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::TimedOut
    );
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let store = tokio::time::timeout(Duration::from_secs(1), SqliteThreadStore::new(&path))
        .await
        .unwrap()
        .unwrap();
    store.close().await;
    assert!(lock_path.exists());
}
