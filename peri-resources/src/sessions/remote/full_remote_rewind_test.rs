//! 远端批 rewind 有效视图合同：两次 Full 后的边界、前缀排除与外来 clear_id。
//!
//! 由 `full_remote_test.rs` 挂载（`include!` 装载，故用 `#[path]` 显式指向）。

use super::*;

// ─── rewind 有效视图重建（远端批，与本机同一套语义） ──────────────────────────

/// 两次 Full 之后的远端历史状态（与本机 A19 矩阵同一形状）：
/// `A B C D | S1 | E F | S2 | G H`，其中 `S1` 由第二次 Full 一并排除。
struct RemoteTwoFull {
    covered_by_first: Vec<MessageId>,
    first_summary: MessageId,
    between_fulls: Vec<MessageId>,
    second_summary: MessageId,
    after_second: Vec<MessageId>,
}

/// 远端 rewind 夹具：虚拟 workspace 上的一个会话 + 两次 Full 的历史。
async fn remote_two_full_fixture(
    directory: &tempfile::TempDir,
) -> (
    Arc<SqliteTransport>,
    Arc<crate::sessions::SessionResourcesImpl>,
    String,
    RemoteTwoFull,
) {
    use crate::sessions::remote::environment::RemoteWorkspaceEnvironment;
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::session_resources::{
        FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
    };
    use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
    use peri_acp_types::thread::CancelPolicy;
    use peri_acp_types::workspace::SessionBinding;

    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("rewind-view-wire.db"))
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .unwrap();
    for statement in crate::sessions::remote::session_schema::initialization_plan() {
        sqlx::query(statement.sql).execute(&pool).await.unwrap();
    }
    sqlx::query(schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(crate::sessions::remote::ledger::CREATE_OP_LEDGER_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, ?1, 'rewind-view-test', ?2, 'now')")
        .bind(schema::REMOTE_SCHEMA_VERSION)
        .bind(schema::STORE_CONTRACT)
        .execute(&pool)
        .await
        .unwrap();
    let transport = Arc::new(SqliteTransport {
        pool: pool.clone(),
        writes: AtomicUsize::new(0),
        read_batches: AtomicUsize::new(0),
        fail_column_drop: AtomicBool::new(false),
        fail_recovery_drop: AtomicBool::new(false),
        change_schema: AtomicBool::new(false),
        before_batch: Mutex::new(None),
        drop_reply: AtomicBool::new(false),
        truncate_reply: AtomicBool::new(false),
    });
    let machine = uuid::Uuid::new_v4().to_string();
    let workspace_dir = directory.path().join("workspace");
    std::fs::create_dir(&workspace_dir).unwrap();
    let environment =
        RemoteWorkspaceEnvironment::virtual_workspace(&machine, workspace_dir.clone()).unwrap();
    let facade = virtual_remote_facade(transport.clone(), environment);
    let workspace = facade.resolve_workspace(&workspace_dir).await.unwrap();
    let session_id = "remote-rewind-view".to_owned();
    facade
        .create_session(&NewSession {
            thread_id: session_id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace_dir.to_str().unwrap().to_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: CancelPolicy::Cascade,
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new("{}"),
        })
        .await
        .unwrap();

    let excluded_updates = |payloads: &[PersistedPayload]| {
        payloads
            .iter()
            .map(|payload| {
                (
                    payload.id(),
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>()
    };
    let covered_by_first = vec![
        PersistedPayload::Message(BaseMessage::human("first 0")),
        PersistedPayload::Message(BaseMessage::human("first 1")),
    ];
    facade
        .append_history(&session_id, &covered_by_first)
        .await
        .unwrap();
    let first_summary = BaseMessage::human("summary one");
    facade
        .apply_compaction(
            &session_id,
            &CompactionChange {
                flag_updates: excluded_updates(&covered_by_first),
                appended_messages: vec![first_summary.clone()],
            },
        )
        .await
        .unwrap();
    let between_fulls = vec![PersistedPayload::Message(BaseMessage::human("between"))];
    facade
        .append_history(&session_id, &between_fulls)
        .await
        .unwrap();
    let second_summary = BaseMessage::human("summary two");
    let mut second_batch = between_fulls.clone();
    second_batch.push(PersistedPayload::Message(first_summary.clone()));
    facade
        .apply_compaction(
            &session_id,
            &CompactionChange {
                flag_updates: excluded_updates(&second_batch),
                appended_messages: vec![second_summary.clone()],
            },
        )
        .await
        .unwrap();
    let after_second = vec![PersistedPayload::Message(BaseMessage::human("after"))];
    facade
        .append_history(&session_id, &after_second)
        .await
        .unwrap();

    let state = RemoteTwoFull {
        covered_by_first: covered_by_first.iter().map(PersistedPayload::id).collect(),
        first_summary: first_summary.id(),
        between_fulls: between_fulls.iter().map(PersistedPayload::id).collect(),
        second_summary: second_summary.id(),
        after_second: after_second.iter().map(PersistedPayload::id).collect(),
    };
    (transport, facade, session_id, state)
}

/// ACP 侧的调用形状（与本机侧同一算法）：读全量历史 → 算保留长度与清零集合 → 一次原子提交。
async fn remote_rewind_plan(
    facade: &Arc<crate::sessions::SessionResourcesImpl>,
    session_id: &str,
    boundary: RewindBoundary,
) -> Vec<MessageId> {
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::session_resources::SessionResources;
    use peri_acp_types::store::history::{
        rewind_clear_excluded_ids, rewind_keep_len, RewindFlagEntry,
    };

    let session_id = session_id.to_owned();
    let snapshot = facade.load_session_snapshot(&session_id).await.unwrap();
    let entries: Vec<RewindFlagEntry> = snapshot
        .payloads
        .iter()
        .map(|payload| {
            (
                payload.id(),
                snapshot
                    .flags
                    .get(&payload.id())
                    .cloned()
                    .unwrap_or_default(),
                matches!(payload.as_message(), Some(BaseMessage::System { .. })),
            )
        })
        .collect();
    let ids: Vec<MessageId> = entries.iter().map(|(id, _, _)| *id).collect();
    let keep_len = rewind_keep_len(&ids, boundary).unwrap();
    rewind_clear_excluded_ids(&entries, keep_len)
}

async fn remote_stored_ids(
    facade: &Arc<crate::sessions::SessionResourcesImpl>,
    session_id: &str,
) -> Vec<MessageId> {
    use peri_acp_types::session_resources::SessionResources;
    let session_id = session_id.to_owned();
    facade
        .load_session_snapshot(&session_id)
        .await
        .unwrap()
        .payloads
        .iter()
        .map(PersistedPayload::id)
        .collect()
}

async fn remote_effective_ids(
    facade: &Arc<crate::sessions::SessionResourcesImpl>,
    session_id: &str,
) -> Vec<MessageId> {
    use peri_acp_types::session_resources::SessionResources;
    let session_id = session_id.to_owned();
    facade
        .load_active_snapshot(&session_id)
        .await
        .unwrap()
        .payloads
        .iter()
        .map(PersistedPayload::id)
        .collect()
}

#[tokio::test]
async fn remote_rewind_between_two_fulls_restores_every_retained_body() {
    use peri_acp_types::session_resources::SessionResources;

    let directory = tempfile::tempdir().unwrap();
    let (_transport, facade, session_id, state) = remote_two_full_fixture(&directory).await;
    let boundary = RewindBoundary::KeepThrough(state.between_fulls[0]);
    let clear = remote_rewind_plan(&facade, &session_id, boundary).await;
    facade
        .rewind_history(&session_id, boundary, &clear)
        .await
        .unwrap();

    let retained = state
        .covered_by_first
        .iter()
        .copied()
        .chain([state.first_summary, state.between_fulls[0]])
        .collect::<Vec<_>>();
    assert_eq!(remote_stored_ids(&facade, &session_id).await, retained);
    assert_eq!(
        remote_effective_ids(&facade, &session_id).await,
        retained,
        "失去依据的 excluded 必须在同一批里清零"
    );
}

#[tokio::test]
async fn remote_rewind_after_the_second_full_keeps_the_prefix_excluded() {
    use peri_acp_types::session_resources::SessionResources;

    let directory = tempfile::tempdir().unwrap();
    let (_transport, facade, session_id, state) = remote_two_full_fixture(&directory).await;
    let boundary = RewindBoundary::KeepThrough(state.after_second[0]);
    let clear = remote_rewind_plan(&facade, &session_id, boundary).await;
    assert!(clear.is_empty(), "存活摘要覆盖的前缀不清零");
    facade
        .rewind_history(&session_id, boundary, &clear)
        .await
        .unwrap();

    assert_eq!(
        remote_effective_ids(&facade, &session_id).await,
        vec![state.second_summary, state.after_second[0]]
    );
}

#[tokio::test]
async fn remote_rewind_with_a_foreign_clear_id_changes_nothing() {
    use peri_acp_types::session_resources::{SessionResourceErrorKind, SessionResources};

    let directory = tempfile::tempdir().unwrap();
    let (_transport, facade, session_id, state) = remote_two_full_fixture(&directory).await;
    let before = remote_stored_ids(&facade, &session_id).await;
    let boundary = RewindBoundary::KeepThrough(state.covered_by_first[0]);
    let mut clear = remote_rewind_plan(&facade, &session_id, boundary).await;
    clear.push(MessageId::new());
    let error = facade
        .rewind_history(&session_id, boundary, &clear)
        .await
        .expect_err("清零集合必须全部落在本会话里");
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. }),
        "与本机同一归类：{error:?}"
    );
    assert_eq!(remote_stored_ids(&facade, &session_id).await, before);
}
