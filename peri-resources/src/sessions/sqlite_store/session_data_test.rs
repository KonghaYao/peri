//! `SessionDataPort`（SQLite 数据面）行为测试。
//!
//! 断言以可观察结果为准：一次保存后的完整事实、冲突是否失败以及级联删除的后置条件。

use super::*;
use crate::sessions::data::SessionDataPort;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::{
    BindingState, ChildSnapshot, ForkSnapshot, FrozenSnapshotBytes, FrozenState, NewSession,
    NewSessionMeta, PersistenceRecovery, RewindBoundary, SessionMetaPatch,
};
use peri_acp_types::store::{
    serialize_persisted_payload, CompactionChange, PersistedPayload, ThreadStore,
};
use peri_acp_types::workspace::ResolvedWorkspace;
use sqlx::Connection;
use std::collections::HashMap;
use tempfile::TempDir;

async fn database() -> (SqliteThreadStore, SqliteSessionData, TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let data = SqliteSessionData::new(Arc::clone(&store.database));
    (store, data, directory)
}

fn frozen(marker: &str) -> FrozenSnapshotBytes {
    FrozenSnapshotBytes::new(format!(r#"{{"version":1,"marker":"{marker}"}}"#))
}

fn binding_of(workspace: &ResolvedWorkspace) -> peri_acp_types::workspace::SessionBinding {
    peri_acp_types::workspace::SessionBinding::from_workspace(workspace)
}

async fn workspace(store: &SqliteThreadStore, cwd: &std::path::Path) -> ResolvedWorkspace {
    store.resolve_workspace(cwd).await.unwrap()
}

fn session(
    id: &str,
    cwd: &str,
    workspace: &ResolvedWorkspace,
    frozen: FrozenSnapshotBytes,
) -> NewSession {
    NewSession {
        thread_id: id.to_owned(),
        created_at: "2026-09-26T00:00:00Z".to_owned(),
        meta: NewSessionMeta {
            title: Some(format!("session {id}")),
            cwd: cwd.to_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: binding_of(workspace),
        frozen,
    }
}

fn payloads(count: usize) -> Vec<PersistedPayload> {
    (0..count)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("message {index}"))))
        .collect()
}

fn payload_bytes(payloads: &[PersistedPayload]) -> Vec<String> {
    payloads
        .iter()
        .map(|payload| serialize_persisted_payload(payload).unwrap())
        .collect()
}

#[tokio::test]
async fn archive_hides_only_root_from_regular_list_and_preserves_history_time() {
    use peri_acp_types::workspace::{ScopedThreadQuery, ThreadScope};

    let (store, data, directory) = database().await;
    let owner = workspace(&store, directory.path()).await;
    let id = "archive-root".to_owned();
    data.save_new_session(&session(
        &id,
        owner.cwd.to_str().unwrap(),
        &owner,
        frozen("archive"),
    ))
    .await
    .unwrap();
    data.append_history(&id, &payloads(1)).await.unwrap();
    let before: (String,) = sqlx::query_as("SELECT updated_at FROM threads WHERE id = ?1")
        .bind(&id)
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    let query = ScopedThreadQuery {
        scope: ThreadScope::Workspace(owner.workspace_id),
        cursor: None,
        limit: 10,
    };
    assert_eq!(data.list_sessions(&query).await.unwrap().entries.len(), 1);
    data.set_session_archived(&id, true).await.unwrap();
    assert!(data.list_sessions(&query).await.unwrap().entries.is_empty());
    assert_eq!(
        data.list_archived_sessions(&query)
            .await
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_eq!(data.load_snapshot(&id).await.unwrap().payloads.len(), 1);
    let after: (i64, String) =
        sqlx::query_as("SELECT archived, updated_at FROM threads WHERE id = ?1")
            .bind(&id)
            .fetch_one(&store.database.pool)
            .await
            .unwrap();
    assert_eq!(after, (1, before.0));
    data.set_session_archived(&id, false).await.unwrap();
    assert_eq!(data.list_sessions(&query).await.unwrap().entries.len(), 1);
    assert!(data
        .list_archived_sessions(&query)
        .await
        .unwrap()
        .entries
        .is_empty());
}

#[tokio::test]
async fn current_machine_name_changes_without_changing_workspace_identity() {
    let (store, data, directory) = database().await;
    let owner = workspace(&store, directory.path()).await;
    let current = data
        .list_machines()
        .await
        .unwrap()
        .into_iter()
        .find(|machine| machine.is_current)
        .unwrap();
    let original_id = current.id.clone();
    data.rename_machine(&original_id, "开发机").await.unwrap();
    let renamed = data
        .list_machines()
        .await
        .unwrap()
        .into_iter()
        .find(|machine| machine.is_current)
        .unwrap();
    assert_eq!(renamed.id, original_id);
    assert_eq!(renamed.name, "开发机");
    let workspaces = data.list_workspaces(&original_id).await.unwrap();
    assert!(workspaces
        .iter()
        .any(|workspace| workspace.id == owner.workspace_id));
    assert!(data.rename_machine(&original_id, "  ").await.is_err());
}
// ─── 新建：完整快照一次保存 ────────────────────────────────────────────────────

#[tokio::test]
async fn test_save_new_session_persists_complete_snapshot_in_one_write() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let input = session("s-new", &cwd, &workspace, frozen("new"));

    data.save_new_session(&input).await.unwrap();

    let snapshot = data.load_snapshot(&"s-new".to_owned()).await.unwrap();
    assert_eq!(snapshot.meta.title.as_deref(), Some("session s-new"));
    assert_eq!(snapshot.meta.cwd, cwd);
    assert_eq!(snapshot.meta.message_count, 0);
    assert_eq!(snapshot.meta.agent_status, AgentStatus::Active);
    assert_eq!(snapshot.binding, BindingState::Bound(input.binding.clone()));
    assert_eq!(
        snapshot.frozen,
        FrozenState::Present(FrozenSnapshotBytes::new(r#"{"version":1,"marker":"new"}"#))
    );
    assert!(snapshot.payloads.is_empty());
    assert!(snapshot.flags.is_empty());

    // 同一 identity 再保存一次必须是明确失败，不能静默覆盖已发布会话。
    let error = data.save_new_session(&input).await.unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
        ),
        "{error}"
    );
}

#[tokio::test]
async fn test_save_new_session_requires_a_registered_workspace() {
    let (_store, data, directory) = database().await;
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let workspace = workspace(&store, directory.path()).await;
    let unknown = ResolvedWorkspace {
        project_id: peri_acp_types::workspace::ProjectId::new(),
        ..workspace
    };
    let input = session("s-unknown", "/tmp", &unknown, frozen("unknown"));

    let error = data.save_new_session(&input).await.unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Workspace(_)
        ),
        "{error}"
    );
    // 未注册的绑定不能留下半条会话行。
    assert!(data.load_snapshot(&"s-unknown".to_owned()).await.is_err());
}

// ─── 读取：一致快照与轻量投影 ──────────────────────────────────────────────────

#[tokio::test]
async fn test_snapshot_read_returns_canonical_history_and_metadata() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let mut input = session("s-read", &cwd, &workspace, frozen("read"));
    input.meta.title = None;
    data.save_new_session(&input).await.unwrap();
    let payloads = payloads(3);
    data.append_history(&"s-read".to_owned(), &payloads)
        .await
        .unwrap();
    let first = payloads[0].id();
    data.apply_message_projections(
        &"s-read".to_owned(),
        &[(
            first,
            peri_acp_types::store::MessageFlags {
                truncated: true,
                excluded: false,
                projection: None,
            },
        )],
    )
    .await
    .unwrap();
    let snapshot = data.load_snapshot(&"s-read".to_owned()).await.unwrap();
    assert_eq!(snapshot.payloads.len(), 3);
    assert_eq!(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
    );
    assert!(snapshot.flags[&first].truncated);
    assert_eq!(snapshot.meta.message_count, 3);
    // 自动标题：首条 human 消息。
    assert_eq!(snapshot.meta.title.as_deref(), Some("message 0"));

    let meta = data.load_meta(&"s-read".to_owned()).await.unwrap();
    assert_eq!(meta.message_count, 3);
    let page = data
        .list_sessions(&peri_acp_types::workspace::ScopedThreadQuery {
            scope: peri_acp_types::workspace::ThreadScope::All,
            cursor: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].thread.id, "s-read");
}

#[tokio::test]
async fn test_load_snapshot_reports_missing_rows_and_preserves_unregistered_binding_facts() {
    let (store, data, directory) = database().await;
    let error = data.load_snapshot(&"absent".to_owned()).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));
    let error = data.load_binding(&"absent".to_owned()).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));

    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let id = "s-orphan".to_owned();
    let input = session(&id, &cwd, &workspace, frozen("orphan"));
    data.save_new_session(&input).await.unwrap();
    data.update_meta(
        &id,
        &SessionMetaPatch {
            config: Some(Some(r#"{"model":"orphan"}"#.to_owned())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let history = payloads(2);
    data.append_history(&id, &history).await.unwrap();
    let before = data.load_snapshot(&id).await.unwrap();
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(directory.path().join("threads.db")),
    )
    .await
    .unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut connection)
        .await
        .unwrap();
    let deleted = sqlx::query("DELETE FROM workspaces WHERE id = ?1")
        .bind(workspace.workspace_id.to_string())
        .execute(&mut connection)
        .await
        .unwrap();
    assert_eq!(deleted.rows_affected(), 1);
    connection.close().await.unwrap();
    let snapshot = data.load_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.binding, BindingState::Bound(input.binding.clone()));
    assert_eq!(data.load_binding(&id).await.unwrap(), snapshot.binding);
    assert_eq!(snapshot.frozen, FrozenState::Present(input.frozen));
    assert_eq!(
        snapshot.meta.config.as_deref(),
        Some(r#"{"model":"orphan"}"#)
    );
    assert_eq!(
        serde_json::to_value(&snapshot.meta).unwrap(),
        serde_json::to_value(&before.meta).unwrap()
    );
    assert_eq!(payload_bytes(&snapshot.payloads), payload_bytes(&history));
    assert_eq!(snapshot.flags, before.flags);
    assert!(snapshot.inherited.payloads.is_empty());
    assert!(snapshot.inherited.flags.is_empty());
    assert_eq!(data.machine_id_of(&id).await.unwrap(), None);
}

#[tokio::test]
async fn test_load_snapshot_rejects_unsupported_and_corrupt_binding_records() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let id = "s-invalid-binding".to_owned();
    let input = session(&id, &cwd, &workspace, frozen("invalid-binding"));
    data.save_new_session(&input).await.unwrap();
    sqlx::query("UPDATE session_bindings SET schema_version = ?1 WHERE thread_id = ?2")
        .bind(i64::from(input.binding.schema_version) + 1)
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    let snapshot_error = data.load_snapshot(&id).await.unwrap_err();
    let binding_error = data.load_binding(&id).await.unwrap_err();
    for error in [snapshot_error, binding_error] {
        assert!(matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Unsupported
        ));
    }

    let mut connection = store.database.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE session_bindings SET schema_version = ?1, workspace_id = 'invalid-uuid' WHERE thread_id = ?2",
    )
    .bind(i64::from(input.binding.schema_version))
    .bind(&id)
    .execute(&mut *connection)
    .await
    .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);
    let snapshot_error = data.load_snapshot(&id).await.unwrap_err();
    let binding_error = data.load_binding(&id).await.unwrap_err();
    for error in [snapshot_error, binding_error] {
        assert!(matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Corrupt { detail }
                if detail == "session binding is not decodable"
        ));
    }
}

// ─── append：碰撞失败、派生事实一并维护 ────────────────────────────────────────

#[tokio::test]
async fn test_append_history_rejects_duplicate_ids_instead_of_ignoring_them() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-append", &cwd, &workspace, frozen("append")))
        .await
        .unwrap();
    let batch = payloads(2);
    data.append_history(&"s-append".to_owned(), &batch)
        .await
        .unwrap();

    // 已存在的 ID：无论内容是否相同都不能静默跳过。
    let error = data
        .append_history(&"s-append".to_owned(), &batch[..1])
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
        ),
        "{error}"
    );
    // 批次内重复同样失败。
    let repeated = vec![batch[0].clone(), batch[0].clone()];
    let error = data
        .append_history(&"s-append".to_owned(), &repeated)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
    ));

    let snapshot = data.load_snapshot(&"s-append".to_owned()).await.unwrap();
    assert_eq!(snapshot.payloads.len(), 2, "失败的追加不得留下部分写入");
    assert_eq!(snapshot.meta.message_count, 2);

    let error = data
        .append_history(&"absent".to_owned(), &payloads(1))
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));
}

// ─── fork：目标完整、来源不变 ──────────────────────────────────────────────────

#[tokio::test]
async fn test_save_fork_copies_complete_target_and_leaves_source_untouched() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-source", &cwd, &workspace, frozen("source")))
        .await
        .unwrap();
    let source_payloads = payloads(2);
    data.append_history(&"s-source".to_owned(), &source_payloads)
        .await
        .unwrap();
    let source_flags = HashMap::from([(
        source_payloads[1].id(),
        peri_acp_types::store::MessageFlags {
            truncated: false,
            excluded: true,
            projection: None,
        },
    )]);
    data.apply_message_projections(
        &"s-source".to_owned(),
        &source_flags
            .iter()
            .map(|(id, flags)| (*id, flags.clone()))
            .collect::<Vec<_>>(),
    )
    .await
    .unwrap();

    // fork 的 ID 重映射由调用方（纯规则）完成；数据面保存的已经是重映射后的快照，
    // 因此目标 ID 与来源不同。
    let forked = payloads(2);
    assert_ne!(forked[0].id(), source_payloads[0].id());
    let target = session("s-fork", &cwd, &workspace, frozen("source"));
    let fork = ForkSnapshot {
        target,
        source_id: "s-source".to_owned(),
        payloads: forked.clone(),
        flags: HashMap::from([(
            forked[1].id(),
            peri_acp_types::store::MessageFlags {
                truncated: false,
                excluded: true,
                projection: None,
            },
        )]),
    };
    data.save_fork(&fork).await.unwrap();

    let snapshot = data.load_snapshot(&"s-fork".to_owned()).await.unwrap();
    assert_eq!(snapshot.payloads.len(), 2);
    assert_eq!(snapshot.payloads[0].id(), forked[0].id());
    assert!(snapshot.flags[&forked[1].id()].excluded);
    assert_eq!(snapshot.meta.message_count, 2);
    assert!(matches!(snapshot.frozen, FrozenState::Present(_)));
    assert_eq!(
        snapshot.binding,
        BindingState::Bound(fork.target.binding.clone())
    );

    let source = data.load_snapshot(&"s-source".to_owned()).await.unwrap();
    assert_eq!(
        source
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        source_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
    );
    assert!(source.flags[&source_payloads[1].id()].excluded);

    // 来源不存在时不得凭空写入目标。
    let mut missing = fork.clone();
    missing.target.thread_id = "s-fork-missing".to_owned();
    missing.source_id = "absent".to_owned();
    let error = data.save_fork(&missing).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));
    assert!(data
        .load_snapshot(&"s-fork-missing".to_owned())
        .await
        .is_err());
}

// ─── child：继承 root 的原始 frozen 与绑定身份 ─────────────────────────────────

#[tokio::test]
async fn test_save_child_uses_root_frozen_bytes_and_enforces_relationships() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-root", &cwd, &workspace, frozen("root")))
        .await
        .unwrap();

    let mut child = session("s-child", &cwd, &workspace, frozen("root"));
    child.meta.parent_thread_id = Some("s-root".to_owned());
    let snapshot = ChildSnapshot {
        target: child,
        parent_id: "s-root".to_owned(),
        root_id: "s-root".to_owned(),
        inherited: peri_acp_types::store::InheritedContext {
            payloads: payloads(1),
            flags: HashMap::new(),
        },
    };
    data.save_child(&snapshot).await.unwrap();

    let loaded = data.load_snapshot(&"s-child".to_owned()).await.unwrap();
    assert_eq!(loaded.meta.parent_thread_id.as_deref(), Some("s-root"));
    assert_eq!(loaded.inherited.payloads.len(), 1);
    assert_eq!(
        loaded.frozen,
        FrozenState::Present(FrozenSnapshotBytes::new(r#"{"version":1,"marker":"root"}"#))
    );
    assert_eq!(
        loaded.binding,
        BindingState::Bound(snapshot.target.binding.clone())
    );
    assert_eq!(loaded.meta.message_count, 0);
    assert_eq!(
        data.list_children(&"s-root".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        data.list_session_tree(&"s-root".to_owned())
            .await
            .unwrap()
            .len(),
        2
    );

    // 与 root 已保存快照不一致的 frozen 不得落库：child 用的是 root 的原始字节。
    let mut drifted = snapshot.clone();
    drifted.target.thread_id = "s-child-drift".to_owned();
    drifted.target.frozen = frozen("rebuilt");
    let error = data.save_child(&drifted).await.unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
        ),
        "{error}"
    );
    assert!(data
        .load_snapshot(&"s-child-drift".to_owned())
        .await
        .is_err());

    // root 归属必须与 parent 链一致。
    let mut wrong_root = snapshot.clone();
    wrong_root.target.thread_id = "s-child-root".to_owned();
    wrong_root.root_id = "s-other".to_owned();
    let error = data.save_child(&wrong_root).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
    ));
}

/// 数据 adapter 不经门面也必须安全：落库写的是 `target.meta` 里的父关系，声明（`parent_id`）
/// 与它不一致时要在写入前拒绝——直接调用数据面的调用方不提供「门面已经校验过」的保证。
#[tokio::test]
async fn test_save_child_refuses_a_declared_parent_relation_it_will_not_persist() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-guard-root", &cwd, &workspace, frozen("root")))
        .await
        .unwrap();

    // 声明的父/根都合法，但目标 meta 里没有父：旧行为会写出一条没有父的独立 root。
    let mut child = session("s-guard-child", &cwd, &workspace, frozen("root"));
    child.meta.parent_thread_id = None;
    let mut snapshot = ChildSnapshot {
        target: child,
        parent_id: "s-guard-root".to_owned(),
        root_id: "s-guard-root".to_owned(),
        inherited: Default::default(),
    };
    let error = data.save_child(&snapshot).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
    ));
    assert!(data
        .load_snapshot(&"s-guard-child".to_owned())
        .await
        .is_err());
    assert!(data
        .list_children(&"s-guard-root".to_owned())
        .await
        .unwrap()
        .is_empty());

    // 声明与 meta 一致时同一次保存才成立，落库的父就是声明的那一个。
    snapshot.target.meta.parent_thread_id = Some("s-guard-root".to_owned());
    data.save_child(&snapshot).await.unwrap();
    let loaded = data
        .load_snapshot(&"s-guard-child".to_owned())
        .await
        .unwrap();
    assert_eq!(
        loaded.meta.parent_thread_id.as_deref(),
        Some("s-guard-root")
    );
    assert_eq!(
        data.list_session_tree(&"s-guard-root".to_owned())
            .await
            .unwrap()
            .len(),
        2
    );
}

// ─── legacy 接纳 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_adopt_legacy_session_publishes_binding_and_frozen_once() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    // legacy 会话：有历史但无绑定、无 frozen（由旧版本写入）。
    let id = store
        .create_thread(ThreadMeta::new_at(cwd.as_str(), peri_time::now_wall()))
        .await
        .unwrap();

    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("legacy"))
        .await
        .unwrap();
    let snapshot = data.load_snapshot(&id).await.unwrap();
    assert_eq!(
        snapshot.binding,
        BindingState::Bound(binding_of(&workspace))
    );
    assert!(matches!(snapshot.frozen, FrozenState::Present(_)));

    // 重复接纳：既有绑定与本次一致即幂等，不覆盖、不修复。
    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("legacy"))
        .await
        .unwrap();

    // 借接纳改绑 cwd 必须失败。
    let error = data
        .adopt_legacy_session(&id, "/elsewhere", &workspace, &frozen("legacy"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Workspace(_)
        ),
        "{error}"
    );
    let after = data.load_snapshot(&id).await.unwrap();
    assert_eq!(after.binding, BindingState::Bound(binding_of(&workspace)));
    assert_eq!(after.meta.cwd, cwd);
}

/// legacy 竞争（write-once）：先提交的候选是 winner；后提交者不带覆盖，读回仍是最先那份字节。
///
/// ACP 侧据此把「adopt 后重读的 winner」传给装配（§6.3），因此这里的读回值就是装配事实源。
#[tokio::test]
async fn test_adopt_legacy_session_keeps_the_first_winner_bytes() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let id = store
        .create_thread(ThreadMeta::new_at(cwd.as_str(), peri_time::now_wall()))
        .await
        .unwrap();
    // 第二个句柄：同一份库事实上的另一次接纳（两宿主共享同一个 store 的等价形态）。
    let second = SqliteSessionData::new(Arc::clone(&store.database));

    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("winner"))
        .await
        .unwrap();
    second
        .adopt_legacy_session(&id, &cwd, &workspace, &frozen("candidate"))
        .await
        .expect("绑定一致时接纳幂等：候选不写入，也不报冲突");

    let snapshot = second.load_snapshot(&id).await.unwrap();
    match snapshot.frozen {
        FrozenState::Present(bytes) => assert_eq!(bytes.as_str(), frozen("winner").as_str()),
        other => panic!("winner bytes must stay persisted: {other:?}"),
    }
}

#[tokio::test]
async fn test_adopt_legacy_session_preserves_canonical_metadata_and_history() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let mut meta = ThreadMeta::new_at(cwd.as_str(), peri_time::now_wall());
    meta.config = Some(r#"{"model":"legacy"}"#.to_owned());
    let id = store.create_thread(meta).await.unwrap();
    let history = payloads(2);
    data.append_history(&id, &history).await.unwrap();
    let before = data.load_snapshot(&id).await.unwrap();
    assert_eq!(before.binding, BindingState::Missing);
    assert_eq!(before.frozen, FrozenState::LegacyAbsent);
    assert_eq!(before.meta.message_count, 2);
    assert_eq!(before.meta.config.as_deref(), Some(r#"{"model":"legacy"}"#));
    let machine = data.machine_id_of(&id).await.unwrap().unwrap();

    let error = data
        .adopt_legacy_session(&id, "/elsewhere", &workspace, &frozen("legacy"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Workspace(
                peri_acp_types::workspace::WorkspaceError::ExecutionBindingMismatch
            )
        ),
        "{error}"
    );
    let rejected = data.load_snapshot(&id).await.unwrap();
    assert_eq!(rejected.binding, BindingState::Missing);
    assert_eq!(rejected.frozen, FrozenState::LegacyAbsent);
    assert_eq!(
        serde_json::to_value(&rejected.meta).unwrap(),
        serde_json::to_value(&before.meta).unwrap()
    );
    assert_eq!(payload_bytes(&rejected.payloads), payload_bytes(&history));

    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("legacy"))
        .await
        .unwrap();
    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("candidate"))
        .await
        .unwrap();
    let snapshot = data.load_snapshot(&id).await.unwrap();
    assert_eq!(
        snapshot.binding,
        BindingState::Bound(binding_of(&workspace))
    );
    assert_eq!(data.load_binding(&id).await.unwrap(), snapshot.binding);
    assert_eq!(snapshot.frozen, FrozenState::Present(frozen("legacy")));
    assert_eq!(
        serde_json::to_value(&snapshot.meta).unwrap(),
        serde_json::to_value(&before.meta).unwrap()
    );
    assert_eq!(payload_bytes(&snapshot.payloads), payload_bytes(&history));
    assert_eq!(snapshot.flags, before.flags);
    assert!(snapshot.inherited.payloads.is_empty());
    assert!(snapshot.inherited.flags.is_empty());
    assert_eq!(data.machine_id_of(&id).await.unwrap(), Some(machine));
}

#[tokio::test]
async fn test_adopt_legacy_session_rejects_unbound_frozen_canonical_data() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let id = "s-unbound-frozen".to_owned();
    data.save_new_session(&session(&id, &cwd, &workspace, frozen("canonical")))
        .await
        .unwrap();
    data.append_history(&id, &payloads(2)).await.unwrap();
    sqlx::query("DELETE FROM session_bindings WHERE thread_id = ?1")
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    let before = data.load_snapshot(&id).await.unwrap();
    assert_eq!(before.binding, BindingState::Missing);
    let error = data
        .adopt_legacy_session(&id, &cwd, &workspace, &frozen("replacement"))
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::Workspace(
            peri_acp_types::workspace::WorkspaceError::InvalidBinding
        )
    ));
    let after = data.load_snapshot(&id).await.unwrap();
    assert_eq!(after.binding, before.binding);
    assert_eq!(after.frozen, before.frozen);
    assert_eq!(
        serde_json::to_value(&after.meta).unwrap(),
        serde_json::to_value(&before.meta).unwrap()
    );
    assert_eq!(
        payload_bytes(&after.payloads),
        payload_bytes(&before.payloads)
    );
}

// ─── 投影、compaction、rewind ─────────────────────────────────────────────────

#[path = "session_history_test.rs"]
mod history_tests;

#[tokio::test]
async fn close_intent_survives_reopen_and_finishes_without_execution_owner() {
    use peri_acp_types::session_resources::CloseSettlement;

    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = "close-reopen".to_owned();
    data.save_new_session(&session(
        &id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen("close"),
    ))
    .await
    .unwrap();
    data.mark_session_closing(&id).await.unwrap();
    data.mark_session_closing(&id).await.unwrap();
    assert_eq!(
        data.close_settlement(&id).await.unwrap(),
        CloseSettlement::Pending
    );
    store.close().await;

    let reopened = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let successor = SqliteSessionData::new(Arc::clone(&reopened.database));
    assert_eq!(
        successor.close_settlement(&id).await.unwrap(),
        CloseSettlement::Pending
    );
    successor.finish_close(&id).await.unwrap();
    assert_eq!(
        successor.close_settlement(&id).await.unwrap(),
        CloseSettlement::Finished
    );
    assert!(!successor.is_session_closing(&id).await.unwrap());
    assert!(successor.finish_close(&id).await.is_err());
    assert_eq!(
        successor
            .close_settlement(&"missing".to_owned())
            .await
            .unwrap(),
        CloseSettlement::Unknown
    );
    assert!(successor.finish_close(&"missing".to_owned()).await.is_err());
    assert!(successor.load_snapshot(&id).await.is_ok());
}

#[tokio::test]
async fn frozen_commit_is_write_once_and_bound_draft_revocation_is_guarded() {
    use peri_acp_types::session_resources::NewSessionDraft;

    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let target = session(
        "bound-draft",
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen("ignored"),
    );
    let id = target.thread_id.clone();
    data.save_new_session_draft(&NewSessionDraft {
        thread_id: target.thread_id,
        created_at: target.created_at,
        meta: target.meta,
        binding: target.binding,
    })
    .await
    .unwrap();
    data.commit_frozen(&id, &frozen("winner")).await.unwrap();
    assert!(data
        .commit_frozen(&id, &frozen("replacement"))
        .await
        .is_err());
    assert!(data.revoke_unpublished_draft(&id).await.is_err());
    assert_eq!(
        data.load_snapshot(&id).await.unwrap().frozen,
        FrozenState::Present(frozen("winner"))
    );
    data.revoke_unpublished_session(&id).await.unwrap();
    assert!(!data.session_exists(&id).await.unwrap());
}
