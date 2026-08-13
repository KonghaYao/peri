//! WorkspaceRegistry 单测：cwd 校验 / create / list / get / remove / rebuild。

use std::sync::Arc;

use crate::control::StoreSink;
use crate::persist::{PersistConfig, Store};
use crate::state::doc_manager::{BatchConfig, DocManager};

use super::*;

/// 真实 RegistryState（经 DocManager 全局 registry 写者，§5.2 单写）。
async fn test_registry() -> (RegistryState, Arc<DocManager>) {
    let tmp = tempfile::tempdir().unwrap();
    let persist_cfg = PersistConfig {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(Store::open(&persist_cfg).unwrap());
    store.recover().await;
    let sink = Arc::new(StoreSink::new(store.clone()).await.unwrap());
    let doc = Arc::new(DocManager::new(BatchConfig::default(), sink.clone()));
    (doc.registry(), doc)
}

#[tokio::test]
async fn create_validates_cwd() {
    let (reg, _doc) = test_registry().await;
    let ws = WorkspaceRegistry::new(reg);
    // 相对路径 → CwdInvalid。
    let e = ws.create("w", "relative/path").await.unwrap_err();
    assert!(matches!(e, WorkspaceError::CwdInvalid(_)), "{e:?}");
    // 目录不存在 → CwdMissing。
    let e = ws.create("w", "/no/such/dir-xyz-123").await.unwrap_err();
    assert!(matches!(e, WorkspaceError::CwdMissing(_)), "{e:?}");
    // 空名 + 非法 cwd：cwd 校验先行（不产生名称兜底副作用）。
    let e = ws.create("", "relative").await.unwrap_err();
    assert!(matches!(e, WorkspaceError::CwdInvalid(_)), "{e:?}");
}

#[tokio::test]
async fn create_list_get_roundtrip() {
    let (reg, _doc) = test_registry().await;
    let ws = WorkspaceRegistry::new(reg);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    let rec = ws.create("my-ws", &path).await.unwrap();
    assert_eq!(rec.name, "my-ws");
    assert_eq!(rec.cwd, path);
    // 名称缺省 → 目录名兜底。
    let rec2 = ws.create("", &path).await.unwrap();
    let dirname = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(rec2.name, dirname);
    // list 按 created_at 升序。
    let all = ws.list().await;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].id, rec.id);
    // get。
    assert_eq!(ws.get(&rec.id).await.unwrap().cwd, path);
    assert!(ws.get(&rec2.id).await.is_some());
    assert!(ws.get("no-such-id").await.is_none());
}

#[tokio::test]
async fn remove_and_get() {
    let (reg, _doc) = test_registry().await;
    let ws = WorkspaceRegistry::new(reg);
    let dir = tempfile::tempdir().unwrap();
    let rec = ws.create("w", dir.path().to_str().unwrap()).await.unwrap();
    ws.remove(&rec.id).await.unwrap();
    assert!(ws.get(&rec.id).await.is_none());
    assert!(ws.list().await.is_empty());
    // 重复/不存在 remove → NotFound。
    let e = ws.remove(&rec.id).await.unwrap_err();
    assert!(matches!(e, WorkspaceError::NotFound(_)), "{e:?}");
}

#[tokio::test]
async fn rebuild_restores_from_registry_doc() {
    let (reg, doc) = test_registry().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    let ws = WorkspaceRegistry::new(reg.clone());
    ws.create("w", &path).await.unwrap();
    // 模拟重启：新的内存表从 Registry Doc 重建（同一 doc 写者仍在）。
    let ws2 = WorkspaceRegistry::new(reg);
    ws2.rebuild().await;
    let all = ws2.list().await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "w");
    assert_eq!(all[0].cwd, path);
    // 重建后 get 可解析（chat/create 携带 workspace_id 的解析面）。
    assert!(ws2.get(&all[0].id).await.is_some());
    drop(doc);
}
