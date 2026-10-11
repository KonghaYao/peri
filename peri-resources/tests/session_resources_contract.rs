//! 会话资源门面的公共行为契约（本机 SQLite）。
//!
//! 这是 crate 外视角的验证：只经 `SessionResources` 的公共行为，不碰内部句柄。

use std::path::Path;
use std::sync::Arc;

use peri_acp_types::session_resources::SessionStoreShutdownPort;
use peri_acp_types::session_resources::{
    AccessMode, BindingState, CloseSettlement, DataCapabilities, ExecutionAvailability,
    FrozenSnapshotBytes, FrozenState, NewSession, NewSessionMeta, RewindBoundary,
    SessionResourceErrorKind, SessionResources,
};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadQuery, SessionBinding, ThreadScope,
};
use peri_resources::sessions::{ReadOnlyStoreErrorKind, SessionResourcesImpl};
use peri_resources::SessionStoreShutdownOwner;
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> TempDir {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init", "-q"]);
    git(
        directory.path(),
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    );
    directory
}

fn binding(workspace: &ResolvedWorkspace) -> SessionBinding {
    SessionBinding::from_workspace(workspace)
}

fn session(id: &str, workspace: &ResolvedWorkspace) -> NewSession {
    NewSession {
        thread_id: id.to_owned(),
        created_at: "2026-09-26T00:00:00Z".to_owned(),
        meta: NewSessionMeta {
            // 标题留空：首条用户消息应成为自动标题，这条规则也是契约的一部分。
            title: None,
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: binding(workspace),
        frozen: FrozenSnapshotBytes::new(format!(r#"{{"v":1,"id":"{id}"}}"#)),
    }
}

fn message(text: &str) -> PersistedPayload {
    PersistedPayload::Message(peri_acp_types::messages::BaseMessage::human(text))
}

/// 部署装配点做的事：`Resources` 交出**业务句柄 + 关闭权**（唯一）。
///
/// 业务句柄（`Arc<dyn SessionResources>`）没有关闭路径，交给 Agent/Controller；关闭权
/// 留在调用方，只有持有它的装配能在任务排空之后关闭存储。crate 外没有第二条取得关闭权
/// 的路径：按具体类型打开只服务 I/O，`close` 只在 Resources 层内可见。
async fn deployment(path: &Path) -> (Arc<dyn SessionResources>, SessionStoreShutdownOwner) {
    peri_resources::Resources::open_with(Some(path.to_path_buf()))
        .await
        .unwrap()
        .into_parts()
}

fn files(directory: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn test_contract_create_append_rewind_and_reopen() {
    let repo = repository();
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("threads.db");
    let (facade, shutdown) = deployment(&path).await;
    let workspace = facade.resolve_workspace(repo.path()).await.unwrap();
    let id = "c-lifecycle".to_owned();

    facade
        .create_session(&session(&id, &workspace))
        .await
        .unwrap();
    let first = message("first");
    let second = message("second");
    facade
        .append_history(&id, &[first.clone(), second.clone()])
        .await
        .unwrap();
    // 追加后 metadata 与历史一起更新，不需要调用方补做计数。
    let snapshot = facade.load_session_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.meta.message_count, 2);
    assert_eq!(snapshot.meta.title.as_deref(), Some("first"));
    assert_eq!(snapshot.binding, BindingState::Bound(binding(&workspace)));
    assert!(matches!(snapshot.frozen, FrozenState::Present(_)));

    // rewind 只改目标之后的历史，并同步派生计数。
    facade
        .rewind_history(&id, RewindBoundary::RemoveFrom(second.id()))
        .await
        .unwrap();
    let snapshot = facade.load_session_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.meta.message_count, 1);
    assert_eq!(snapshot.payloads.len(), 1);

    drop(facade);
    shutdown.shutdown().await.unwrap();

    let facade = SessionResourcesImpl::open(&path).await.unwrap();
    let snapshot = facade.load_session_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.meta.message_count, 1);
    assert_eq!(snapshot.payloads.len(), 1);
    assert_eq!(snapshot.binding, BindingState::Bound(binding(&workspace)));
    assert_eq!(
        facade
            .inspect_availability(Some(&id))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::Available)
    );
    // 列表不加载大快照，但仍能看到这条会话。
    let page = facade
        .list_sessions(&ScopedThreadQuery {
            scope: ThreadScope::All,
            cursor: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(page.entries.iter().any(|entry| entry.thread.id == id));
}

#[tokio::test]
async fn test_contract_read_only_open_reports_independent_facts_and_writes_nothing() {
    let repo = repository();
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("threads.db");
    let (facade, shutdown) = deployment(&path).await;
    let workspace = facade.resolve_workspace(repo.path()).await.unwrap();
    let id = "c-readonly".to_owned();
    facade
        .create_session(&session(&id, &workspace))
        .await
        .unwrap();
    drop(facade);
    shutdown.shutdown().await.unwrap();
    let before = files(db.path());

    let facade = SessionResourcesImpl::open_existing_read_only(&path)
        .await
        .unwrap();
    let availability = facade.inspect_availability(Some(&id)).await.unwrap();
    assert_eq!(availability.access, AccessMode::ReadOnly);
    assert_eq!(availability.capabilities, DataCapabilities::HistoryReadOnly);
    assert_eq!(
        availability.execution,
        Some(ExecutionAvailability::ReadOnlyStore)
    );
    // 历史仍可读。
    assert!(facade.load_session_meta(&id).await.is_ok());
    let error = facade.resolve_workspace(repo.path()).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Workspace(
            peri_acp_types::workspace::WorkspaceError::ReadOnlyStore
        )
    ));
    let error = match facade
        .create_session(&session("c-readonly-new", &workspace))
        .await
    {
        Ok(_) => panic!("expected create_session to fail on a read-only store"),
        Err(error) => error,
    };
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Workspace(
            peri_acp_types::workspace::WorkspaceError::ReadOnlyStore
        )
    ));
    let error = facade
        .append_history(&id, &[message("blocked")])
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    // 只读路径不创建、不改写任何文件（含锁文件与 schema）。
    assert_eq!(before, files(db.path()));
    drop(facade);
}

#[tokio::test]
async fn test_contract_read_only_open_of_missing_database_creates_nothing() {
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("missing.db");
    let error = SessionResourcesImpl::open_existing_read_only(&path)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), ReadOnlyStoreErrorKind::DatabaseNotFound);
    assert_eq!(files(db.path()), Vec::<std::ffi::OsString>::new());
}

#[tokio::test]
async fn test_contract_delete_removes_canonical_data_and_allows_identity_reuse() {
    let repo = repository();
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("threads.db");
    let (facade, shutdown) = deployment(&path).await;
    let workspace = facade.resolve_workspace(repo.path()).await.unwrap();
    let id = "c-delete".to_owned();
    facade
        .create_session(&session(&id, &workspace))
        .await
        .unwrap();
    facade
        .append_history(&id, &[message("before delete")])
        .await
        .unwrap();

    facade.delete_session_tree(&id).await.unwrap();
    assert!(facade.load_session_meta(&id).await.is_err());
    let error = facade
        .inspect_availability(Some(&id))
        .await
        .expect_err("删除之后这条 identity 连执行事实都不该剩下");
    assert!(matches!(error.kind(), SessionResourceErrorKind::NotFound));
    // 收敛读取没有对象：会话不存在，就没有「可重载」这回事。
    let error = facade.recover_session_persistence(&id).await.unwrap_err();
    assert!(matches!(error.kind(), SessionResourceErrorKind::NotFound));
    // 重复删除按「数据事实」回答：会话不在了。
    let error = facade.delete_session_tree(&id).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Workspace(_) | SessionResourceErrorKind::NotFound
    ));
    drop(facade);
    shutdown.shutdown().await.unwrap();

    // 重开仍读到同一事实：删除不被回滚，同一 identity 也可以重新创建（没有 durable 的
    let facade = SessionResourcesImpl::open(&path).await.unwrap();
    assert!(facade.load_session_meta(&id).await.is_err());
    facade
        .create_session(&session(&id, &workspace))
        .await
        .expect("删除之后同名 identity 应当可以重新创建");
}

#[tokio::test]
async fn test_contract_independent_instances_preserve_canonical_facts() {
    let repo = repository();
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("threads.db");
    let first = SessionResourcesImpl::open(&path).await.unwrap();
    let workspace = first.resolve_workspace(repo.path()).await.unwrap();
    let id = "c-independent".to_owned();
    first
        .create_session(&session(&id, &workspace))
        .await
        .unwrap();
    first
        .append_history(&id, &[message("first instance")])
        .await
        .unwrap();
    let before = first.load_session_snapshot(&id).await.unwrap();
    let second = SessionResourcesImpl::open(&path).await.unwrap();
    second
        .create_session(&session(&id, &workspace))
        .await
        .unwrap();
    second
        .append_history(&id, &[message("second instance")])
        .await
        .unwrap();
    first
        .append_history(&id, &[message("first instance remains usable")])
        .await
        .unwrap();
    let after = second.load_session_snapshot(&id).await.unwrap();
    assert_eq!(after.binding, before.binding);
    assert_eq!(after.frozen, before.frozen);
    assert_eq!(after.payloads.len(), 3);
    assert_eq!(after.meta.message_count, 3);
    assert_eq!(after.meta.cwd, before.meta.cwd);
    assert_eq!(after.meta.parent_thread_id, before.meta.parent_thread_id);
}

#[tokio::test]
async fn test_contract_close_intent_survives_reopen_and_finishes_by_id() {
    let repo = repository();
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("threads.db");
    let (facade, shutdown) = deployment(&path).await;
    let workspace = facade.resolve_workspace(repo.path()).await.unwrap();
    let id = "c-close".to_owned();
    facade
        .create_session(&session(&id, &workspace))
        .await
        .unwrap();
    facade.mark_session_closing(&id).await.unwrap();
    facade.mark_session_closing(&id).await.unwrap();
    assert_eq!(
        facade.close_settlement(&id).await.unwrap(),
        CloseSettlement::Pending
    );
    drop(facade);
    shutdown.shutdown().await.unwrap();
    let reopened = SessionResourcesImpl::open(&path).await.unwrap();
    assert!(reopened.is_session_closing(&id).await.unwrap());
    reopened.drain_persistence(&id).await.unwrap();
    reopened.finish_close(&id).await.unwrap();
    assert!(!reopened.is_session_closing(&id).await.unwrap());
    assert_eq!(
        reopened.close_settlement(&id).await.unwrap(),
        CloseSettlement::Finished
    );
    assert!(reopened.load_session_snapshot(&id).await.is_ok());
}
