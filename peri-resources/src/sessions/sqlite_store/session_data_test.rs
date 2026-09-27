//! `SessionDataPort`（SQLite 数据面）行为测试。
//!
//! 断言以可观察结果为准：一次保存后的完整事实、碰撞是否失败、墓碑与执行行是否
//! 与数据删除同事务收敛；不复制实现细节。

use super::*;
use crate::sessions::data::SessionDataPort;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::{
    BindingState, ChildSnapshot, ForkSnapshot, FrozenSnapshotBytes, FrozenState, NewSession,
    NewSessionMeta, PersistenceRecovery, RewindBoundary, SessionMetaPatch,
};
use peri_acp_types::store::{CompactionChange, PersistedPayload, ThreadStore};
use peri_acp_types::workspace::{RecoveryRequiredDetails, ResolvedWorkspace};
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
    peri_acp_types::workspace::SessionBinding {
        schema_version: peri_acp_types::workspace::SESSION_BINDING_VERSION,
        revision: 1,
        project_id: workspace.project_id,
        workspace_id: workspace.workspace_id,
        cwd_relative_to_workspace: workspace.relative_cwd.clone(),
    }
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
async fn test_snapshot_read_is_consistent_and_meta_projection_skips_cache_blob() {
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
    // 直接写入派生缓存正文：轻量投影不得把它带出来。
    sqlx::query("UPDATE threads SET cached_context = ?1 WHERE id = 's-read'")
        .bind("x".repeat(4096))
        .execute(&store.database.pool)
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
    assert!(
        meta.cached_context.is_none(),
        "轻量 metadata 投影不加载派生缓存正文"
    );
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
async fn test_load_snapshot_reports_missing_rows_and_unregistered_bindings() {
    let (store, data, directory) = database().await;
    let error = data.load_snapshot(&"absent".to_owned()).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));

    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-orphan", &cwd, &workspace, frozen("orphan")))
        .await
        .unwrap();
    // 登记行被移除（曾有写入方在未强制外键时删掉登记）后，绑定不再能于本机验证：
    // 报告为「本机登记缺失」而不是损坏，也不是 legacy。
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(directory.path().join("threads.db")),
    )
    .await
    .unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workspaces WHERE id = ?1")
        .bind(workspace.workspace_id.to_string())
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let snapshot = data.load_snapshot(&"s-orphan".to_owned()).await.unwrap();
    assert_eq!(snapshot.binding, BindingState::ExternalOrUnregistered);
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
        .create_thread(ThreadMeta::new(cwd.as_str()))
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

#[tokio::test]
async fn test_adopt_legacy_session_refuses_to_bypass_dirty_execution() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let id = store
        .create_thread(ThreadMeta::new(cwd.as_str()))
        .await
        .unwrap();
    sqlx::query("INSERT INTO execution_runs (thread_id, generation, clean) VALUES (?1, 1, 0)")
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();

    let error = data
        .adopt_legacy_session(&id, &cwd, &workspace, &frozen("legacy"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Workspace(
                peri_acp_types::workspace::WorkspaceError::InvalidBinding
            )
        ),
        "{error}"
    );
    let snapshot = data.load_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.binding, BindingState::Missing);
    assert_eq!(snapshot.frozen, FrozenState::LegacyAbsent);
}

// ─── 投影、compaction、rewind ─────────────────────────────────────────────────

#[tokio::test]
async fn test_projection_and_compaction_maintain_derived_views_in_the_same_write() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-compact", &cwd, &workspace, frozen("compact")))
        .await
        .unwrap();
    let history = payloads(3);
    data.append_history(&"s-compact".to_owned(), &history)
        .await
        .unwrap();
    let epoch_before: (i64,) =
        sqlx::query_as("SELECT context_cache_epoch FROM threads WHERE id = ?")
            .bind("s-compact")
            .fetch_one(&store.database.pool)
            .await
            .unwrap();

    // 一次性投影变更集：flags 与派生视图同事务生效。
    data.apply_message_projections(
        &"s-compact".to_owned(),
        &[(
            history[0].id(),
            peri_acp_types::store::MessageFlags {
                truncated: true,
                excluded: false,
                projection: None,
            },
        )],
    )
    .await
    .unwrap();
    let flags = data
        .load_snapshot(&"s-compact".to_owned())
        .await
        .unwrap()
        .flags;
    assert!(flags[&history[0].id()].truncated);
    let epoch_after: (i64,) =
        sqlx::query_as("SELECT context_cache_epoch FROM threads WHERE id = ?")
            .bind("s-compact")
            .fetch_one(&store.database.pool)
            .await
            .unwrap();
    assert_eq!(epoch_after.0, epoch_before.0 + 1);

    // 指向别会话/不存在的条目：整体失败，不留部分写入。
    let error = data
        .apply_message_projections(
            &"s-compact".to_owned(),
            &[
                (
                    history[1].id(),
                    peri_acp_types::store::MessageFlags {
                        truncated: true,
                        excluded: true,
                        projection: None,
                    },
                ),
                (
                    peri_acp_types::messages::MessageId::new(),
                    peri_acp_types::store::MessageFlags::default(),
                ),
            ],
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
    ));
    let flags = data
        .load_snapshot(&"s-compact".to_owned())
        .await
        .unwrap()
        .flags;
    assert!(
        !flags
            .get(&history[1].id())
            .is_some_and(|flags| flags.truncated),
        "失败批次不得留下部分 flags"
    );

    // compaction：追加摘要消息与 flags 一次生效。
    let summary = BaseMessage::ai("summary");
    let summary_id = summary.id();
    data.apply_compaction(
        &"s-compact".to_owned(),
        &CompactionChange {
            flag_updates: vec![(
                history[2].id(),
                peri_acp_types::store::MessageFlags {
                    truncated: false,
                    excluded: true,
                    projection: None,
                },
            )],
            appended_messages: vec![summary],
        },
    )
    .await
    .unwrap();
    let snapshot = data.load_snapshot(&"s-compact".to_owned()).await.unwrap();
    assert_eq!(snapshot.payloads.len(), 4);
    assert_eq!(snapshot.payloads[3].id(), summary_id);
    assert!(snapshot.flags[&history[2].id()].excluded);
    assert_eq!(snapshot.meta.message_count, 4);

    // 归属校验：别会话的条目不能在本次 compaction 里被改掉。
    let error = data
        .apply_compaction(
            &"s-compact".to_owned(),
            &CompactionChange {
                flag_updates: vec![(
                    peri_acp_types::messages::MessageId::new(),
                    peri_acp_types::store::MessageFlags::default(),
                )],
                appended_messages: vec![],
            },
        )
        .await
        .unwrap_err();
    assert!(!matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));
}

#[tokio::test]
async fn test_rewind_boundaries_are_distinct_and_unknown_cutoffs_change_nothing() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-rewind", &cwd, &workspace, frozen("rewind")))
        .await
        .unwrap();
    let history = payloads(4);
    data.append_history(&"s-rewind".to_owned(), &history)
        .await
        .unwrap();

    // 未知截止点：无变更（保留现有 rewind 语义）。
    data.rewind_history(
        &"s-rewind".to_owned(),
        RewindBoundary::RemoveFrom(peri_acp_types::messages::MessageId::new()),
    )
    .await
    .unwrap();
    assert_eq!(
        data.load_snapshot(&"s-rewind".to_owned())
            .await
            .unwrap()
            .payloads
            .len(),
        4
    );

    // 保留到目标：目标本身保留。
    data.rewind_history(
        &"s-rewind".to_owned(),
        RewindBoundary::KeepThrough(history[1].id()),
    )
    .await
    .unwrap();
    let snapshot = data.load_snapshot(&"s-rewind".to_owned()).await.unwrap();
    assert_eq!(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![history[0].id(), history[1].id()]
    );
    assert_eq!(snapshot.meta.message_count, 2);

    // 从目标开始移除：目标及之后的条目都不再存在。
    data.rewind_history(
        &"s-rewind".to_owned(),
        RewindBoundary::RemoveFrom(history[1].id()),
    )
    .await
    .unwrap();
    let snapshot = data.load_snapshot(&"s-rewind".to_owned()).await.unwrap();
    assert_eq!(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![history[0].id()]
    );
    assert_eq!(snapshot.meta.message_count, 1);
}

#[tokio::test]
async fn test_remove_history_entries_is_exact_and_idempotent() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-remove", &cwd, &workspace, frozen("remove")))
        .await
        .unwrap();
    data.save_new_session(&session("s-other", &cwd, &workspace, frozen("other")))
        .await
        .unwrap();
    let history = payloads(2);
    let other = payloads(1);
    data.append_history(&"s-remove".to_owned(), &history)
        .await
        .unwrap();
    data.append_history(&"s-other".to_owned(), &other)
        .await
        .unwrap();

    // 别会话的条目不得被本次移除命中。
    let error = data
        .remove_history_entries(&"s-remove".to_owned(), &[other[0].id()])
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
    ));
    assert_eq!(
        data.load_snapshot(&"s-other".to_owned())
            .await
            .unwrap()
            .payloads
            .len(),
        1
    );

    data.remove_history_entries(&"s-remove".to_owned(), &[history[0].id()])
        .await
        .unwrap();
    assert_eq!(
        data.load_snapshot(&"s-remove".to_owned())
            .await
            .unwrap()
            .payloads
            .len(),
        1
    );
    // 已经不存在的条目：幂等，不报错。
    data.remove_history_entries(&"s-remove".to_owned(), &[history[0].id()])
        .await
        .unwrap();
}

// ─── metadata、resume、登记 ───────────────────────────────────────────────────

#[tokio::test]
async fn test_update_meta_applies_only_requested_fields() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-meta", &cwd, &workspace, frozen("meta")))
        .await
        .unwrap();
    let before = data.load_meta(&"s-meta".to_owned()).await.unwrap();

    // 空 patch：不改任何字段，也不刷新时间戳。
    data.update_meta(&"s-meta".to_owned(), &SessionMetaPatch::default())
        .await
        .unwrap();
    let after = data.load_meta(&"s-meta".to_owned()).await.unwrap();
    assert_eq!(after.updated_at, before.updated_at);

    data.update_meta(
        &"s-meta".to_owned(),
        &SessionMetaPatch {
            title: Some(Some("renamed".to_owned())),
            status: Some(AgentStatus::Done),
            cancel_policy: Some(peri_acp_types::thread::CancelPolicy::Independent),
            config: Some(Some("{\"k\":1}".to_owned())),
        },
    )
    .await
    .unwrap();
    let updated = data.load_meta(&"s-meta".to_owned()).await.unwrap();
    assert_eq!(updated.title.as_deref(), Some("renamed"));
    assert_eq!(updated.agent_status, AgentStatus::Done);
    assert_eq!(
        updated.cancel_policy,
        peri_acp_types::thread::CancelPolicy::Independent
    );
    assert_eq!(updated.config.as_deref(), Some("{\"k\":1}"));
    // cwd/parent/计数不是定向更新能改的字段。
    assert_eq!(updated.cwd, before.cwd);
    assert_eq!(updated.parent_thread_id, before.parent_thread_id);
    assert_eq!(updated.created_at, before.created_at);

    data.update_meta(
        &"s-meta".to_owned(),
        &SessionMetaPatch {
            title: Some(None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(data
        .load_meta(&"s-meta".to_owned())
        .await
        .unwrap()
        .title
        .is_none());

    let error = data
        .update_meta(
            &"absent".to_owned(),
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));
}

#[tokio::test]
async fn test_child_resume_record_round_trip() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-resume", &cwd, &workspace, frozen("resume")))
        .await
        .unwrap();
    let mut child = session("s-resume-child", &cwd, &workspace, frozen("resume"));
    child.meta.parent_thread_id = Some("s-resume".to_owned());
    data.save_child(&ChildSnapshot {
        target: child,
        parent_id: "s-resume".to_owned(),
        root_id: "s-resume".to_owned(),
        inherited: Default::default(),
    })
    .await
    .unwrap();

    let record = data
        .load_child_resume_record(&"s-resume-child".to_owned())
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Active);
    assert!(record.claimed, "active 的 child 视为已被认领");

    data.store_child_resume_record(
        &"s-resume-child".to_owned(),
        &crate::sessions::data::ChildResumeRecord {
            status: AgentStatus::Done,
            claimed: false,
        },
    )
    .await
    .unwrap();
    let record = data
        .load_child_resume_record(&"s-resume-child".to_owned())
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Done);
    assert!(!record.claimed);
}

// ─── 只读与关闭 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_read_only_store_serves_reads_and_refuses_every_mutation() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-ro", &cwd, &workspace, frozen("ro")))
        .await
        .unwrap();
    let path = directory.path().join("threads.db");
    store.close().await;
    let reader = SqliteThreadStore::open_existing_read_only(&path)
        .await
        .unwrap();
    let read_only = SqliteSessionData::new(Arc::clone(&reader.database));

    assert!(read_only.load_snapshot(&"s-ro".to_owned()).await.is_ok());
    let error = read_only
        .save_new_session(&session("s-ro-2", &cwd, &workspace, frozen("ro")))
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::ReadOnlyStore
    ));
    let error = read_only
        .append_history(&"s-ro".to_owned(), &payloads(1))
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::ReadOnlyStore
    ));
    let error = read_only.delete_tree(&"s-ro".to_owned()).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::ReadOnlyStore
    ));
    // 收敛检查在只读下不写库：没有未决锚点时报告可重载。
    assert_eq!(
        read_only
            .recover_persistence(&"s-ro".to_owned())
            .await
            .unwrap(),
        PersistenceRecovery::Recovered
    );
}

#[tokio::test]
async fn test_close_stops_writes_and_keeps_history_readable() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-close", &cwd, &workspace, frozen("close")))
        .await
        .unwrap();

    data.close().await.unwrap();
    let error = data
        .append_history(&"s-close".to_owned(), &payloads(1))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Unavailable { .. }
        ),
        "{error}"
    );
    assert!(data.load_snapshot(&"s-close".to_owned()).await.is_ok());
}

#[tokio::test]
async fn test_dirty_execution_generation_survives_a_port_restart() {
    let (_store, data, directory) = database().await;
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-dirty", &cwd, &workspace, frozen("dirty")))
        .await
        .unwrap();
    store
        .acquire_execution_lease(&"s-dirty".to_owned())
        .await
        .unwrap();
    drop(store);
    assert!(data.load_snapshot(&"s-dirty".to_owned()).await.is_ok());

    // 未 clean 的代际跨实例保留：精确代际解除仍然可用（执行面语义，数据侧只提供事实）。
    let reopened = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let generation: (i64, bool) =
        sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = 's-dirty'")
            .fetch_one(&reopened.database.pool)
            .await
            .unwrap();
    assert!(!generation.1, "Drop 不代表 clean");
    reopened
        .reset_dirty_execution(&RecoveryRequiredDetails {
            thread_id: "s-dirty".to_owned(),
            generation: generation.0,
        })
        .await
        .unwrap();
}
