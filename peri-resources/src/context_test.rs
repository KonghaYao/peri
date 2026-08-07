//! context.rs 单元测试：`Resources::open_with` 显式路径语义。

use tempfile::tempdir;

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

/// [P1] `open_with(None)` 与既有 `open()` 行为一致：默认路径或临时 fallback 均成功。
#[tokio::test]
async fn test_open_with_none_default_ok() {
    let resources = Resources::open_with(None).await.unwrap();
    let threads = resources.thread_store().list_threads().await;
    assert!(threads.is_ok(), "默认存储应可查询: {:?}", threads.err());
}
