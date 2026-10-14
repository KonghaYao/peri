use super::*;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::RewindBoundary;
use peri_acp_types::store::PersistedPayload;

#[tokio::test]
async fn remote_close_intent_finishes_without_execution_ownership() {
    use crate::sessions::data::SessionDataPort;
    let fixture = Fixture::new().await;
    let store = fixture.store(StoreAccess::ReadWrite);
    schema_upgrade::upgrade(&store, &fixture.snapshot)
        .await
        .unwrap();
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(
        fixture.transport.clone(),
        StoreAccess::ReadWrite,
        gate.mint(),
        gate.clone(),
    );
    let data = super::super::session_data::RemoteSessionData::with_connection_for_test(
        fixture.snapshot.store_id.clone(),
        store,
        Arc::new(FullRemoteFactory {
            transport: fixture.transport.clone(),
            gate: gate.clone(),
        }),
        gate,
    );
    let id = "session".to_owned();
    assert_eq!(
        data.close_settlement(&id).await.unwrap(),
        CloseSettlement::Finished
    );
    data.mark_session_closing(&id).await.unwrap();
    assert_eq!(
        data.close_settlement(&id).await.unwrap(),
        CloseSettlement::Pending
    );
    data.finish_close(&id).await.unwrap();
    assert!(data.finish_close(&id).await.is_err());
    assert_eq!(
        data.close_settlement(&id).await.unwrap(),
        CloseSettlement::Finished
    );
    data.mark_session_closing(&id).await.unwrap();
    let read_only_gate = Arc::new(ConnectionGate::default());
    let read_only = super::super::session_data::RemoteSessionData::with_connection_for_test(
        fixture.snapshot.store_id.clone(),
        RemoteStore::new(
            fixture.transport.clone(),
            StoreAccess::ReadOnly,
            read_only_gate.mint(),
            read_only_gate.clone(),
        ),
        Arc::new(FullRemoteFactory {
            transport: fixture.transport.clone(),
            gate: read_only_gate.clone(),
        }),
        read_only_gate,
    );
    assert!(matches!(
        read_only.finish_close(&id).await.unwrap_err().kind(),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    assert_eq!(
        read_only.close_settlement(&id).await.unwrap(),
        CloseSettlement::Pending
    );
    data.inject_faults(super::super::mutation::FaultPlan {
        drop_reply: Some("finish_close".to_owned()),
        drop_before_send: None,
    })
    .await;
    assert!(data
        .finish_close(&id)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert_eq!(
        data.close_settlement(&id).await.unwrap(),
        CloseSettlement::Finished
    );
    assert_eq!(
        data.close_settlement(&"missing".to_owned()).await.unwrap(),
        CloseSettlement::Unknown
    );
    assert!(matches!(
        data.finish_close(&"missing".to_owned())
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));
}

// A SQLite-backed transport stands in for Turso's SQL wire protocol. The
// application composition below has no local SQLite execution database.
struct FullRemoteFactory {
    transport: Arc<SqliteTransport>,
    gate: Arc<ConnectionGate>,
}

#[async_trait]
impl super::super::generation::ConnectionFactory for FullRemoteFactory {
    async fn connect(
        &self,
    ) -> peri_acp_types::session_resources::SessionResourceResult<RemoteStore> {
        Ok(RemoteStore::new(
            self.transport.clone(),
            StoreAccess::ReadWrite,
            self.gate.mint(),
            self.gate.clone(),
        ))
    }
}

fn full_remote_facade(
    transport: Arc<SqliteTransport>,
) -> Arc<crate::sessions::SessionResourcesImpl> {
    use crate::sessions::data::SessionDataPort;
    use crate::sessions::local_port::LocalExecutionPort;
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(
        transport.clone(),
        StoreAccess::ReadWrite,
        gate.mint(),
        gate.clone(),
    );
    let data = Arc::new(
        super::super::session_data::RemoteSessionData::with_connection_for_test(
            schema::StoreId::mint(),
            store,
            Arc::new(FullRemoteFactory {
                transport,
                gate: gate.clone(),
            }),
            gate,
        ),
    );
    let local: Arc<dyn LocalExecutionPort> = Arc::new(
        super::super::execution::RemoteExecution::new(data.clone(), false),
    );
    let data: Arc<dyn SessionDataPort> = data;
    Arc::new(crate::sessions::SessionResourcesImpl::from_ports(
        data,
        local,
        crate::sessions::resources::SessionDataHome::RemoteStore,
    ))
}

fn virtual_remote_facade(
    transport: Arc<SqliteTransport>,
    environment: super::super::environment::RemoteWorkspaceEnvironment,
) -> Arc<crate::sessions::SessionResourcesImpl> {
    use crate::sessions::data::SessionDataPort;
    use crate::sessions::local_port::LocalExecutionPort;
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(
        transport.clone(),
        StoreAccess::ReadWrite,
        gate.mint(),
        gate.clone(),
    );
    let mut data = super::super::session_data::RemoteSessionData::with_connection_for_test(
        schema::StoreId::mint(),
        store,
        Arc::new(FullRemoteFactory {
            transport,
            gate: gate.clone(),
        }),
        gate,
    );
    data.machine_id = environment.machine_id().unwrap();
    let data = Arc::new(data);
    let local: Arc<dyn LocalExecutionPort> = Arc::new(
        super::super::execution::RemoteExecution::new_in_environment(
            data.clone(),
            false,
            environment,
        ),
    );
    let data: Arc<dyn SessionDataPort> = data;
    Arc::new(crate::sessions::SessionResourcesImpl::from_ports(
        data,
        local,
        crate::sessions::resources::SessionDataHome::RemoteStore,
    ))
}

#[tokio::test]
async fn virtual_remote_cold_recovery_and_read_only_fallbacks() {
    use super::super::environment::RemoteWorkspaceEnvironment;
    use peri_acp_types::session_resources::{
        BindingRecheck, ExecutionAvailability, FrozenSnapshotBytes, NewSession, NewSessionMeta,
        SessionResources,
    };
    use peri_acp_types::thread::CancelPolicy;
    use peri_acp_types::workspace::SessionBinding;

    let directory = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("virtual-wire.db"))
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .unwrap();
    for statement in super::super::session_schema::initialization_plan() {
        sqlx::query(statement.sql).execute(&pool).await.unwrap();
    }
    sqlx::query(schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(super::super::ledger::CREATE_OP_LEDGER_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, ?1, 'virtual-test', ?2, 'now')")
        .bind(schema::REMOTE_SCHEMA_VERSION).bind(schema::STORE_CONTRACT).execute(&pool).await.unwrap();
    let transport = Arc::new(SqliteTransport {
        pool: pool.clone(),
        writes: AtomicUsize::new(0),
        fail_column_drop: AtomicBool::new(false),
        fail_recovery_drop: AtomicBool::new(false),
        change_schema: AtomicBool::new(false),
        before_batch: Mutex::new(None),
        drop_reply: AtomicBool::new(false),
        truncate_reply: AtomicBool::new(false),
    });
    let machine = uuid::Uuid::new_v4().to_string();
    let root = std::env::temp_dir().join("virtual-peri-workspace");
    let environment =
        RemoteWorkspaceEnvironment::virtual_workspace(&machine, root.clone()).unwrap();
    let first = virtual_remote_facade(transport.clone(), environment.clone());
    let workspace = first.resolve_workspace(&root).await.unwrap();
    let session_id = "virtual-cold-recovery".to_owned();
    let input = NewSession {
        thread_id: session_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meta: NewSessionMeta {
            title: None,
            cwd: root.to_str().unwrap().to_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: CancelPolicy::Cascade,
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(&workspace),
        frozen: FrozenSnapshotBytes::new("{}"),
    };
    first.create_session(&input).await.unwrap();
    drop(first);

    let cold = virtual_remote_facade(transport.clone(), environment.clone());
    let restored = cold
        .validate_bound_workspace(&session_id, BindingRecheck::Full)
        .await
        .unwrap();
    assert_eq!(restored.workspace_id, workspace.workspace_id);
    drop(cold);

    for changed in [
        RemoteWorkspaceEnvironment::virtual_workspace(
            &uuid::Uuid::new_v4().to_string(),
            root.clone(),
        )
        .unwrap(),
        RemoteWorkspaceEnvironment::virtual_workspace(&machine, root.join("other")).unwrap(),
    ] {
        let host = virtual_remote_facade(transport.clone(), changed);
        assert!(host.load_session_history(&session_id).await.is_ok());
        assert_eq!(
            host.inspect_availability(Some(&session_id))
                .await
                .unwrap()
                .execution,
            Some(ExecutionAvailability::WorkspaceUnavailable)
        );
    }

    // The former WASM adapter persisted an unversioned {root} snapshot. It
    // remains readable, but cannot acquire execution under the shared format.
    sqlx::query("UPDATE session_bindings SET discovery_snapshot = ?1 WHERE thread_id = ?2")
        .bind(serde_json::json!({ "root": root }).to_string())
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();
    let legacy = virtual_remote_facade(transport.clone(), environment.clone());
    assert!(legacy.load_session_history(&session_id).await.is_ok());
    assert_eq!(
        legacy
            .inspect_availability(Some(&session_id))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::WorkspaceUnavailable)
    );

    sqlx::query("UPDATE session_bindings SET discovery_snapshot = NULL WHERE thread_id = ?1")
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();
    let no_snapshot = virtual_remote_facade(transport, environment);
    assert!(no_snapshot.load_session_history(&session_id).await.is_ok());
    assert_eq!(
        no_snapshot
            .inspect_availability(Some(&session_id))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::WorkspaceUnavailable)
    );
}

#[tokio::test]
async fn remote_only_cold_recovery_uses_saved_evidence_and_old_distinct_registration() {
    use peri_acp_types::session_resources::{
        BindingRecheck, ExecutionAvailability, FrozenSnapshotBytes, NewSession, NewSessionMeta,
        SessionResources,
    };
    use peri_acp_types::thread::CancelPolicy;
    use peri_acp_types::workspace::SessionBinding;

    crate::sessions::machine::initialize().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let remote_file = directory.path().join("wire-server.db");
    let poisoned_local_file = directory.path().join("threads.db");
    std::fs::write(&poisoned_local_file, b"broken local SQLite").unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&remote_file)
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .unwrap();
    for statement in super::super::session_schema::initialization_plan() {
        sqlx::query(statement.sql).execute(&pool).await.unwrap();
    }
    sqlx::query(schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(super::super::ledger::CREATE_OP_LEDGER_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, ?1, 'remote-only-test', ?2, 'now')")
        .bind(schema::REMOTE_SCHEMA_VERSION).bind(schema::STORE_CONTRACT).execute(&pool).await.unwrap();
    let transport = Arc::new(SqliteTransport {
        pool: pool.clone(),
        writes: AtomicUsize::new(0),
        fail_column_drop: AtomicBool::new(false),
        fail_recovery_drop: AtomicBool::new(false),
        change_schema: AtomicBool::new(false),
        before_batch: Mutex::new(None),
        drop_reply: AtomicBool::new(false),
        truncate_reply: AtomicBool::new(false),
    });
    let workspace_dir = directory.path().join("workspace");
    std::fs::create_dir(&workspace_dir).unwrap();
    let first = full_remote_facade(transport.clone());
    let second = full_remote_facade(transport.clone());
    let (workspace, competing) = tokio::join!(
        first.resolve_workspace(&workspace_dir),
        second.resolve_workspace(&workspace_dir),
    );
    let workspace = workspace.unwrap();
    assert_eq!(workspace.workspace_id, competing.unwrap().workspace_id);
    drop(second);
    let session_id = "remote-only-old-registration".to_owned();
    let input = NewSession {
        thread_id: session_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meta: NewSessionMeta {
            title: None,
            cwd: workspace.cwd.to_str().unwrap().to_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: CancelPolicy::Cascade,
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(&workspace),
        frozen: FrozenSnapshotBytes::new("{}"),
    };
    first.create_session(&input).await.unwrap();
    assert_eq!(
        first
            .resolve_workspace(&workspace_dir)
            .await
            .unwrap()
            .workspace_id,
        workspace.workspace_id
    );
    assert_eq!(
        std::fs::read(&poisoned_local_file).unwrap(),
        b"broken local SQLite"
    );
    drop(first);

    // Old v2 remote rows pointed the binding at a separate execution registration UUID.
    // v19 起绑定行的 workspace_id 必须就是会话归属行的 id：不一致的行再也无法靠
    // 「另一张表里的旧登记」被追认，只能失败关闭（与本机组合同一结论）。
    let old_registration = uuid::Uuid::new_v4().to_string();
    sqlx::query("UPDATE session_bindings SET workspace_id=?1 WHERE thread_id=?2")
        .bind(&old_registration)
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();
    let cold = full_remote_facade(transport.clone());
    let error = cold
        .validate_bound_workspace(&session_id, BindingRecheck::Full)
        .await
        .expect_err("a binding outside the session owner row must fail closed");
    assert!(
        matches!(
            error.kind(),
            SessionResourceErrorKind::Workspace(
                peri_acp_types::workspace::WorkspaceError::ExecutionBindingMismatch
            )
        ),
        "不是归属行的绑定必须判为绑定不匹配：{error:?}"
    );
    // 元数据写入同样要过绑定复核：不一致的绑定行连标题都不许改。
    let rejected_meta = cold
        .update_session_meta(
            &session_id,
            &peri_acp_types::session_resources::SessionMetaPatch {
                title: Some(Some("must stay rejected".to_owned())),
                ..Default::default()
            },
        )
        .await
        .expect_err("an inconsistent binding must block metadata writes");
    assert!(
        matches!(
            rejected_meta.kind(),
            SessionResourceErrorKind::Workspace(
                peri_acp_types::workspace::WorkspaceError::ExecutionBindingMismatch
            )
        ),
        "不是归属行的绑定必须挡下元数据写入：{rejected_meta:?}"
    );
    assert_eq!(
        std::fs::read(&poisoned_local_file).unwrap(),
        b"broken local SQLite"
    );
    drop(cold);
    // 把这一行改回归属行：后面的阶段各自验证自己的条件，不再被这条带偏。
    sqlx::query("UPDATE session_bindings SET workspace_id=?1 WHERE thread_id=?2")
        .bind(workspace.workspace_id.to_string())
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();

    // Saved binding evidence authorizes metadata edits without runtime ownership.
    let unleased = full_remote_facade(transport.clone());
    unleased
        .update_session_meta(
            &session_id,
            &peri_acp_types::session_resources::SessionMetaPatch {
                title: Some(Some("verified owner".to_owned())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    drop(unleased);

    let displaced = directory.path().join("original-workspace");
    std::fs::rename(&workspace_dir, &displaced).unwrap();
    std::fs::create_dir(&workspace_dir).unwrap();
    let changed = full_remote_facade(transport.clone());
    assert!(changed.load_session_history(&session_id).await.is_ok());
    assert_eq!(
        changed
            .inspect_availability(Some(&session_id))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::WorkspaceUnavailable)
    );
    drop(changed);
    std::fs::remove_dir(&workspace_dir).unwrap();
    std::fs::rename(&displaced, &workspace_dir).unwrap();

    let original_machine = crate::sessions::machine::current().unwrap();
    let other_machine = uuid::Uuid::new_v4().to_string();
    sqlx::query("UPDATE workspaces SET machine_id=?1 WHERE id=?2")
        .bind(&other_machine)
        .bind(workspace.workspace_id.to_string())
        .execute(&pool)
        .await
        .unwrap();
    let foreign = full_remote_facade(transport.clone());
    assert!(foreign.load_session_history(&session_id).await.is_ok());
    assert_eq!(
        foreign
            .inspect_availability(Some(&session_id))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::WorkspaceUnavailable)
    );
    drop(foreign);
    sqlx::query("UPDATE workspaces SET machine_id=?1 WHERE id=?2")
        .bind(original_machine)
        .bind(workspace.workspace_id.to_string())
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("UPDATE session_bindings SET discovery_snapshot=NULL WHERE thread_id=?1")
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();
    let missing = full_remote_facade(transport);
    assert!(missing.load_session_history(&session_id).await.is_ok());
    assert_eq!(
        missing
            .inspect_availability(Some(&session_id))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::WorkspaceUnavailable)
    );
    let projection = missing
        .validate_bound_workspace(&session_id, BindingRecheck::Full)
        .await
        .unwrap();
    assert_eq!(projection.workspace_id, workspace.workspace_id);
    let rejected_append = missing.append_history(&session_id, &[]).await;
    assert!(
        rejected_append.is_err(),
        "missing evidence must block history writes"
    );
    let rejected_meta = missing
        .update_session_meta(
            &session_id,
            &peri_acp_types::session_resources::SessionMetaPatch {
                title: Some(Some("should stay read only".to_owned())),
                ..Default::default()
            },
        )
        .await;
    assert!(
        rejected_meta.is_err(),
        "missing evidence must block metadata writes"
    );
    let rejected_adoption = missing
        .adopt_legacy_session(
            &session_id,
            workspace.cwd.to_str().unwrap(),
            &workspace,
            &FrozenSnapshotBytes::new("{}"),
        )
        .await;
    assert!(matches!(
        rejected_adoption.unwrap_err().kind(),
        SessionResourceErrorKind::Unsupported
    ));
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM threads WHERE id=?1")
        .bind(&session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(title.as_deref(), Some("verified owner"));
    let snapshot: Option<String> =
        sqlx::query_scalar("SELECT discovery_snapshot FROM session_bindings WHERE thread_id=?1")
            .bind(&session_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(snapshot, None);
    assert_eq!(
        std::fs::read(&poisoned_local_file).unwrap(),
        b"broken local SQLite"
    );
}

/// 有效视图与有界 flags 在本机/远端必须是同一套语义，且过滤发生在存储层：
/// 本用例用 SQLite 扮演的远端服务端真跑一遍生产语句。
#[tokio::test]
async fn remote_active_snapshot_and_bounded_flags_filter_at_the_storage_layer() {
    use super::super::environment::RemoteWorkspaceEnvironment;
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::session_resources::{
        FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
    };
    use peri_acp_types::store::{MessageFlags, PersistedPayload};
    use peri_acp_types::thread::CancelPolicy;
    use peri_acp_types::workspace::SessionBinding;

    let directory = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("filter-wire.db"))
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .unwrap();
    for statement in super::super::session_schema::initialization_plan() {
        sqlx::query(statement.sql).execute(&pool).await.unwrap();
    }
    sqlx::query(schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(super::super::ledger::CREATE_OP_LEDGER_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, ?1, 'filter-test', ?2, 'now')")
        .bind(schema::REMOTE_SCHEMA_VERSION)
        .bind(schema::STORE_CONTRACT)
        .execute(&pool)
        .await
        .unwrap();
    let transport = Arc::new(SqliteTransport {
        pool: pool.clone(),
        writes: AtomicUsize::new(0),
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
    let facade = virtual_remote_facade(transport, environment);
    let workspace = facade.resolve_workspace(&workspace_dir).await.unwrap();
    let session_id = "remote-filter".to_owned();
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
    let history = vec![
        PersistedPayload::Message(BaseMessage::human("message 0")),
        PersistedPayload::Message(BaseMessage::human("message 1")),
        PersistedPayload::Message(BaseMessage::human("message 2")),
    ];
    facade.append_history(&session_id, &history).await.unwrap();
    facade
        .apply_message_projections(
            &session_id,
            &[
                (
                    history[1].id(),
                    MessageFlags {
                        truncated: false,
                        excluded: true,
                        projection: None,
                    },
                ),
                (
                    history[2].id(),
                    MessageFlags {
                        truncated: true,
                        excluded: false,
                        projection: None,
                    },
                ),
            ],
        )
        .await
        .unwrap();

    // 有效视图：已排除正文与标记都不返回，截断条目照常返回。
    let active = facade.load_active_snapshot(&session_id).await.unwrap();
    assert_eq!(
        active
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![history[0].id(), history[2].id()]
    );
    assert!(!active.flags.contains_key(&history[1].id()));
    assert!(active.flags[&history[2].id()].truncated);

    // 完整读取仍然给出归档区：排除只影响有效视图。
    let full = facade.load_session_snapshot(&session_id).await.unwrap();
    assert_eq!(full.payloads.len(), 3);
    assert!(full.flags[&history[1].id()].excluded);

    // 有界 flags：只回答给定的 id；空集合不查库；未标记条目不在派生视图里。
    let flags = facade
        .load_message_flags(&session_id, &[history[1].id()])
        .await
        .unwrap();
    assert_eq!(flags.len(), 1);
    assert!(flags[&history[1].id()].excluded);
    assert!(facade
        .load_message_flags(&session_id, &[])
        .await
        .unwrap()
        .is_empty());
    assert!(facade
        .load_message_flags(&session_id, &[history[0].id()])
        .await
        .unwrap()
        .is_empty());
}

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
    use super::super::environment::RemoteWorkspaceEnvironment;
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
    for statement in super::super::session_schema::initialization_plan() {
        sqlx::query(statement.sql).execute(&pool).await.unwrap();
    }
    sqlx::query(schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(super::super::ledger::CREATE_OP_LEDGER_SQL)
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
