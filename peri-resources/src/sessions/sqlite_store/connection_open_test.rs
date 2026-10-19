use super::*;
use crate::sessions::sqlite_store::{schema::CURRENT_SCHEMA_VERSION, SqliteThreadStore};
use std::sync::Arc;
use tokio::{sync::Barrier, task::JoinSet};

/// 「前一个持有者已释放，锁应当可以再获取」这类释放探测的等待预算。
///
/// 不能是零：`flock` 的锁属于 open file description，本套件会自我 spawn 子进程
/// （`current_exe()`），`fork` 会把同进程其它用例持有的 `.schema-lock` fd 复制给
/// 子进程，继承者存活期间该锁仍被持有、且持有者在本进程之外。零预算对这类瞬时
/// 占用零容忍，会把测试二进制内部的进程耦合误报成
/// `session database initialization is busy`。真实泄漏会持续到持有进程退出，远长
/// 于本预算，仍会被检出。
const LOCK_RELEASE_PROBE_BUDGET: Duration = Duration::from_millis(250);

/// 释放探测的统一入口：各用例 `drop` 掉自己的持有者后，用它确认锁已归还（同进程之外
/// 可能还有继承来的持有者，见 `LOCK_RELEASE_PROBE_BUDGET`）。
///
/// 四个真实调用点与回归用例 `release_probe_waits_out_an_external_holder` 共用这一处，
/// 等待预算只有这一个来源——它被退回零等待，回归用例即红。
///
/// 新增「持有者 `drop` 后锁应已归还」这类探测一律经本入口；调用点自己内联零等待探测
/// **不在本守卫覆盖内**（已在 `.recon/S13-lock-test.md` §4.7 实测登记：那种写法绕过
/// 本入口后回归仍绿），内联零等待会把 fork 继承窗口报成假红。
async fn probe_initialization_lock_release(path: &Path) -> Result<std::fs::File> {
    lock_schema_open(path, LOCK_RELEASE_PROBE_BUDGET).await
}

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
    for statement in crate::sessions::canonical::V10_CREATE_TABLES {
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

/// 索引集随后续构建演进，而形状判定只看表与列：当前形状（版本已是最新）的库缺了新索引时，
/// 写打开必须按 canonical 清单幂等补齐。否则上一版构建建出的库永远拿不到新索引，
/// 有效范围读取会退化成逐行过滤（A05）。
#[tokio::test]
async fn current_shape_open_backfills_a_missing_canonical_index() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let store = SqliteThreadStore::new(&path).await.unwrap();
    // 模拟上一版构建建出的库：删掉新索引后，版本与形状都不变，打开路径仍判 Current。
    sqlx::raw_sql("DROP INDEX idx_messages_active")
        .execute(&store.database.pool)
        .await
        .unwrap();
    store.close().await;

    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    let (present,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name = 'idx_messages_active'",
    )
    .fetch_one(&reopened.database.pool)
    .await
    .unwrap();
    assert_eq!(present, 1, "当前形状的库在写打开时必须补回 canonical 索引");
    reopen_close_keeps_version(&reopened).await;
}

/// 补齐是幂等的：再打开一次不改变版本，也不影响既有对象。
async fn reopen_close_keeps_version(store: &SqliteThreadStore) {
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
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
    let held = probe_initialization_lock_release(&path).await.unwrap();
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
    let held = probe_initialization_lock_release(&path).await.unwrap();
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
    // 0 是「形状读不懂」的未知库；12..=19 是本次压缩掉的开发期代数（从未正式发布）。
    // 两类都拒绝，且拒绝**不动库**：版本、数据行、库文件字节与侧车文件都保持原样。
    for version in [0, 12, 13, 14, 15, 16, 17, 18, 19] {
        let path = directory.path().join(format!("unsupported-{version}.db"));
        let mut connection = sqlx::SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::raw_sql(AssertSqlSafe(format!(
            "CREATE TABLE unrelated (value TEXT);
             INSERT INTO unrelated VALUES ('preserved row');
             PRAGMA user_version = {version};"
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
        let mut probe = sqlx::SqliteConnection::connect_with(
            &SqliteConnectOptions::new().filename(&path).read_only(true),
        )
        .await
        .unwrap();
        let (value,): (String,) = sqlx::query_as("SELECT value FROM unrelated")
            .fetch_one(&mut probe)
            .await
            .unwrap();
        assert_eq!(value, "preserved row");
        probe.close().await.unwrap();
        let held = probe_initialization_lock_release(&path).await.unwrap();
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
    for statement in crate::sessions::canonical::V10_CREATE_TABLES {
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
    let held = probe_initialization_lock_release(&existing_alias)
        .await
        .unwrap();
    assert!(lock_schema_open(&path, Duration::ZERO).await.is_err());
    drop(held);
    assert_eq!(std::fs::metadata(&lock_path).unwrap().ino(), inode);
    assert!(!aliases.path().join("existing.db.schema-lock").exists());
}

/// 子进程侧的锁获取：与父进程的释放探测同理，不能对瞬时占用零容忍——父进程在交给
/// 子进程前自己也持有过同一路径的锁，`fork` 继承者会让它在子进程之外多存活一段窗口
/// （见 `LOCK_RELEASE_PROBE_BUDGET`）。持有语义不变，只是不再要求瞬时可得。
fn acquire_initialization_lock_within(
    lock_path: impl AsRef<Path>,
    budget: Duration,
) -> std::fs::File {
    let deadline = std::time::Instant::now() + budget;
    loop {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path.as_ref())
            .unwrap();
        match file.try_lock() {
            Ok(()) => return file,
            Err(std::fs::TryLockError::WouldBlock) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "initialization lock was not acquirable within {budget:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(std::fs::TryLockError::Error(error)) => panic!("{error:?}"),
        }
    }
}

#[test]
fn initialization_lock_child_process() {
    let Some(lock_path) = std::env::var_os("PERI_INITIALIZATION_LOCK_CHILD") else {
        return;
    };
    let file = acquire_initialization_lock_within(&lock_path, LOCK_RELEASE_PROBE_BUDGET);
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
    // 外层超时必须宽于生产锁预算 SCHEMA_OPEN_LOCK_TIMEOUT，让真实缺陷以 TimedOut 暴露。
    // 曾用 1s，CI 负载下外层先到期误报（run 38145652592）。
    let store = tokio::time::timeout(Duration::from_secs(10), SqliteThreadStore::new(&path))
        .await
        .unwrap()
        .unwrap();
    store.close().await;
    assert!(lock_path.exists());
}

#[test]
fn initialization_lock_holder_child_process() {
    let Some(lock_path) = std::env::var_os("PERI_INITIALIZATION_LOCK_HOLDER") else {
        return;
    };
    let ready_path = std::env::var_os("PERI_INITIALIZATION_LOCK_HOLDER_READY").unwrap();
    let release_path = std::env::var_os("PERI_INITIALIZATION_LOCK_HOLDER_RELEASE").unwrap();
    let file = acquire_initialization_lock_within(&lock_path, LOCK_RELEASE_PROBE_BUDGET);
    std::fs::write(&ready_path, b"ready").unwrap();
    // 持锁直到父进程写出释放文件，不依赖 sleep 时长，负载下也确定。
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !std::path::Path::new(&release_path).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "holder release file never appeared"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(file);
}

/// 断言释放探测的等待时长时容许的测量/调度偏差。
///
/// 与被测语义无关：只把「等待不得短于下界」写成下界断言，避免定时器粒度造成的假红。
const PROBE_WAIT_MEASUREMENT_SLACK: Duration = Duration::from_millis(50);

/// 判别断言的独立下界：**不引用**被测的 [`LOCK_RELEASE_PROBE_BUDGET`]。
///
/// 参照值与阈值同源时，改小预算常量即可让断言恒真——判别力被静默取消（实测：预算
/// 调到 20 ms 时本用例仍绿）。下界与预算是两个独立事实：断言变红要么说明等待没有真正
/// 发生，要么说明预算已被调到低于下界，两种都必须有人来看一眼。
const PROBE_WAIT_FLOOR: Duration = Duration::from_millis(200);

/// 断言探测因锁仍被持有而放弃（`session database initialization is busy`）。
fn assert_initialization_lock_busy(result: Result<std::fs::File>) {
    let error = result.expect_err("lock probe unexpectedly acquired a held initialization lock");
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::TimedOut
    );
}

/// 持有者在进程之外仍持锁时，释放探测不得停在首次 `WouldBlock` 上。
///
/// 顺序由显式握手固定，不靠时长猜：
/// 1. 子进程取得 `.schema-lock` 后写 `holder.ready`，并受契约约束——读到 `holder.release`
///    才释放。父进程读到 `holder.ready` 之后发起探测，必定遇到持锁。
/// 2. 受控首次 `WouldBlock`：零预算探测在这里只能 `TimedOut`。
/// 3. 放行文件此时尚未写出，因此共享入口 `probe_initialization_lock_release` 的整段调用
///    （含首次 `WouldBlock`）都落在持锁窗口内。持锁期间它只能以 `TimedOut` 结束，而重试
///    循环只在取得锁或越过 deadline 时才退出——等待时长不得短于预算。把该入口退回原调用
///    点的零等待写法，它会在首次 `WouldBlock` 后立即返回，下面的断言即红。
/// 4. 调用返回后再次零预算探测仍是 `TimedOut`：证明步骤 3 整段调用期间锁一直被持有。
/// 5. 写放行文件后，同一入口必须取得锁。
#[tokio::test]
async fn release_probe_waits_out_an_external_holder() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("external-holder.db");
    // 侧车文件先由本进程建立；随后的持有者在进程之外。
    let held = lock_schema_open(&path, Duration::from_secs(1))
        .await
        .unwrap();
    let lock_path = schema_lock_path(&path).await.unwrap();
    drop(held);
    let ready_path = directory.path().join("holder.ready");
    let release_path = directory.path().join("holder.release");
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::sqlite_store::connection::open_tests::initialization_lock_holder_child_process",
        ])
        .env("PERI_INITIALIZATION_LOCK_HOLDER", &lock_path)
        .env("PERI_INITIALIZATION_LOCK_HOLDER_READY", &ready_path)
        .env("PERI_INITIALIZATION_LOCK_HOLDER_RELEASE", &release_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !tokio::fs::try_exists(&ready_path).await.unwrap() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("holder child never acquired the initialization lock");

    // 步骤 2：受控首次 WouldBlock——持有者未收到放行信号。
    assert_initialization_lock_busy(lock_schema_open(&path, Duration::ZERO).await);

    // 步骤 3：判别断言。持锁期间探测只能在预算耗尽后放弃；下界是独立常量
    // （`PROBE_WAIT_FLOOR`），把预算调小不会让这条断言变成恒真。
    let started = peri_time::monotonic_now();
    let error = probe_initialization_lock_release(&path).await.unwrap_err();
    let waited = peri_time::elapsed_since(started);
    assert_initialization_lock_busy(Err(error));
    assert!(
        waited + PROBE_WAIT_MEASUREMENT_SLACK >= PROBE_WAIT_FLOOR,
        "release probe gave up before its budget while the holder still held the lock: \
         waited {waited:?} < {PROBE_WAIT_FLOOR:?} (budget {LOCK_RELEASE_PROBE_BUDGET:?})"
    );

    // 步骤 4：锁整段调用都被持有。
    assert_initialization_lock_busy(lock_schema_open(&path, Duration::ZERO).await);

    // 步骤 5：放行后同一入口必须取得锁。
    tokio::fs::write(&release_path, b"release").await.unwrap();
    let reacquired = probe_initialization_lock_release(&path)
        .await
        .unwrap_or_else(|error| {
            panic!("release probe did not acquire after the holder released: {error:?}")
        });
    drop(reacquired);
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("holder child did not exit after release")
        .unwrap();
    assert!(status.success(), "holder child exited with {status}");
    assert!(lock_path.exists());
}
