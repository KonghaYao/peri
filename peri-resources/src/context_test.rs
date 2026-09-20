//! context.rs 单元测试：`Resources::open_with` 显式路径语义。

use tempfile::tempdir;

use peri_acp_types::thread::ThreadMeta;
use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};

use super::*;

/// [P0] 显式路径打开成功：数据库文件被创建，且同路径二次打开幂等。
#[tokio::test]
async fn test_open_with_explicit_path_creates_db() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("custom").join("threads.db");
    let first = Resources::open_with(Some(db_path.clone())).await.unwrap();
    assert!(
        tokio::fs::metadata(&db_path).await.is_ok(),
        "数据库文件应已创建: {}",
        db_path.display()
    );
    let second = Resources::open_with(Some(db_path)).await;
    assert!(
        second.is_ok(),
        "同路径二次打开应幂等成功: {:?}",
        second.err()
    );
    drop(first);
}

/// [P0] 显式路径不可用（父级为普通文件）时直接报错，不 fallback 临时目录，错误携带路径。
#[tokio::test]
async fn test_open_with_explicit_path_errors_no_fallback() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("f");
    std::fs::write(&file, "not a directory").unwrap();
    let db_path = file.join("threads.db");
    let err = match Resources::open_with(Some(db_path)).await {
        Ok(_) => panic!("父级为普通文件时应返回错误"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains(&file.display().to_string()),
        "错误必须携带路径: {err}"
    );
}

/// [P0] 显式路径指向目录时直接报错（sqlite 无法以目录为库），错误携带路径。
#[tokio::test]
async fn test_open_with_explicit_path_is_directory_errs() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("adir");
    std::fs::create_dir(&db_path).unwrap();
    let err = match Resources::open_with(Some(db_path.clone())).await {
        Ok(_) => panic!("指向目录的路径应返回错误"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains(&db_path.display().to_string()),
        "错误必须携带路径: {err}"
    );
}

/// [P0] 库的 `user_version` 本构建不认识时拒绝打开，用户可见的报错要复述实际版本与
/// 本构建上限——这正是「进入时只看到不支持」的那条链路。
#[tokio::test]
async fn test_open_with_explicit_path_reports_unsupported_schema_version() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::query("PRAGMA user_version = 99")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let err = match Resources::open_with(Some(db_path.clone())).await {
        Ok(_) => panic!("不认识的 schema 版本必须拒绝打开"),
        Err(e) => e,
    };
    let message = err.to_string();
    assert!(
        message.contains(&db_path.display().to_string()),
        "错误必须携带路径: {message}"
    );
    assert!(
        message.contains("version 99"),
        "错误必须复述实际版本: {message}"
    );
    assert!(
        message.contains("newest supported:"),
        "错误必须给出本构建上限: {message}"
    );
}

/// [P1] `open_with(None)` 使用默认数据库且可正常查询。
#[tokio::test]
async fn test_open_with_none_uses_default_store() {
    // 复用生产路径选择逻辑，只注入默认存储位置，禁止测试迁移用户真实数据库。
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("default.db");
    let default = db_path.clone();
    let resources = Resources::open_with_default(None, move || Ok(default))
        .await
        .unwrap();
    assert!(db_path.is_file(), "None 分支必须打开注入的默认存储");
    let threads = resources.thread_store().list_threads().await;
    assert!(threads.is_ok(), "默认存储应可查询: {:?}", threads.err());
}

/// 会话库被占（schema 锁未释放）：写打开失败不再挡住进入，降级为只读打开——
/// 历史仍可列表读取，写入由只读 store 自己按只读失败。
#[tokio::test]
async fn test_open_with_busy_schema_lock_degrades_to_read_only() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let writable = SqliteThreadStore::new(db_path.clone()).await.unwrap();
    let thread = writable
        .create_thread(ThreadMeta::new("/tmp/read-only-degradation"))
        .await
        .unwrap();
    writable.close().await;

    // 持住 schema 锁：写打开按「初始化被占」失败，只读打开不受影响。
    let canonical = db_path.canonicalize().unwrap();
    let lock_path = canonical.with_file_name(format!(
        "{}.schema-lock",
        canonical.file_name().unwrap().to_string_lossy()
    ));
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    held.lock().unwrap();

    let resources = Resources::open_with(Some(db_path.clone())).await.unwrap();
    let store = resources.thread_store();
    let listed = store.list_threads().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, thread);
    assert!(
        store.delete_thread(&thread).await.is_err(),
        "只读降级不得假装可写：写入必须失败"
    );
    assert!(store.load_meta(&thread).await.is_ok(), "降级后历史仍可读");
    drop(held);
}
