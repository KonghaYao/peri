use super::*;

#[tokio::test]
async fn remote_owner_descriptor_survives_takeover_and_unsupported_is_monotonic() {
    use crate::sessions::data::SessionDataPort;
    use peri_acp_types::workspace::WorkspaceExecutionDescriptor;

    let directory = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new().max_connections(2)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("owner.db")).create_if_missing(true).foreign_keys(false))
        .await.unwrap();
    sqlx::query("CREATE TABLE threads(id TEXT PRIMARY KEY, parent_thread_id TEXT)")
        .execute(&pool).await.unwrap();
    for sql in [
        crate::sessions::canonical::CREATE_SESSION_EXECUTION_OWNERS_TABLE_SQL,
        crate::sessions::canonical::CREATE_SESSION_EXECUTION_WORKSPACE_DESCRIPTORS_TABLE_SQL,
        crate::sessions::canonical::CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL,
        super::super::ledger::CREATE_OP_LEDGER_SQL,
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO threads(id) VALUES ('owner-root')").execute(&pool).await.unwrap();
    let transport = Arc::new(SqliteTransport { pool: pool.clone(),
        writes: AtomicUsize::new(0), fail_column_drop: AtomicBool::new(false),
        change_schema: AtomicBool::new(false), drop_reply: AtomicBool::new(false),
        truncate_reply: AtomicBool::new(false) });
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(transport.clone(), StoreAccess::ReadWrite, gate.mint(), gate.clone());
    let data = super::super::session_data::RemoteSessionData::with_connection_for_test(
        schema::StoreId::mint(), store,
        Arc::new(FullRemoteFactory { transport, gate: gate.clone() }), gate);
    let root = "owner-root".to_owned();
    let first = data.claim_execution_owner(&root, false, None).await.unwrap();
    assert!(first.prior_unreleased.is_none());
    let first_descriptor = WorkspaceExecutionDescriptor {
        endpoint: "https://workspace.example/mcp".into(), owner_identity: "a".repeat(64),
        agent_generation_id: "agent-one".into(), unsupported_async_owners: false,
    };
    data.bind_execution_workspace_owner(&first.token, &first_descriptor).await.unwrap();
    data.mark_unsupported_async_owner(&first.token).await.unwrap();
    sqlx::query("UPDATE session_execution_owners SET expires_at_unix = 0 WHERE root_id = ?1")
        .bind(&root).execute(&pool).await.unwrap();

    let second = data.claim_execution_owner(&root, false, None).await.unwrap();
    assert_eq!(second.prior_unreleased.unwrap().agent_generation_id.as_deref(), Some("agent-one"));
    let old = data.read_execution_workspace_owner(&root).await.unwrap().unwrap();
    assert_eq!(old.current_epoch, second.token.epoch);
    assert_eq!(old.descriptor_epoch, first.token.epoch);
    assert!(old.descriptor.unsupported_async_owners);
    assert!(data.bind_execution_workspace_owner(&first.token, &first_descriptor).await.is_err());
    let second_descriptor = WorkspaceExecutionDescriptor {
        agent_generation_id: "agent-two".into(), unsupported_async_owners: false,
        ..first_descriptor.clone()
    };
    data.bind_execution_workspace_owner(&second.token, &second_descriptor).await.unwrap();
    let current = data.read_execution_workspace_owner(&root).await.unwrap().unwrap();
    assert_eq!(current.descriptor_epoch, second.token.epoch);
    assert_eq!(current.descriptor.agent_generation_id, "agent-two");
    assert!(current.descriptor.unsupported_async_owners);

    sqlx::query("INSERT INTO session_close_intents VALUES (?1, 'now')")
        .bind(&root).execute(&pool).await.unwrap();
    sqlx::query("UPDATE session_execution_owners SET expires_at_unix = 0 WHERE root_id = ?1")
        .bind(&root).execute(&pool).await.unwrap();
    assert!(data.claim_execution_owner(&root, true, Some(first.token.epoch)).await.is_err());
    let closing = data.claim_execution_owner(&root, true, Some(second.token.epoch)).await.unwrap();
    assert_eq!(closing.prior_unreleased.unwrap().agent_generation_id.as_deref(), Some("agent-two"));
    assert_eq!(data.read_execution_workspace_owner(&root).await.unwrap().unwrap().descriptor_epoch,
        second.token.epoch);
    data.finish_close(&closing.token).await.unwrap();
    assert!(data.read_execution_workspace_owner(&root).await.unwrap().is_none());
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
    let local: Arc<dyn LocalExecutionPort> =
        Arc::new(super::super::execution::RemoteExecution::new(data.clone(), false));
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
    for statement in crate::sessions::canonical::CREATE_V2_TABLES
        .iter()
        .chain(crate::sessions::canonical::CREATE_V2_INDEXES)
    {
        sqlx::query(*statement).execute(&pool).await.unwrap();
    }
    sqlx::query(schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(super::super::ledger::CREATE_OP_LEDGER_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, 13, 'virtual-test', 'peri.session.store/v3', 'now')")
        .execute(&pool).await.unwrap();
    let transport = Arc::new(SqliteTransport {
        pool: pool.clone(),
        writes: AtomicUsize::new(0),
        fail_column_drop: AtomicBool::new(false),
        change_schema: AtomicBool::new(false),
        drop_reply: AtomicBool::new(false),
        truncate_reply: AtomicBool::new(false),
    });
    let machine = uuid::Uuid::new_v4().to_string();
    let root = std::path::PathBuf::from("/virtual/peri-workspace");
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
    let lease = first.create_session(&input).await.unwrap();
    let token = lease.owner_token().unwrap();
    lease.mark_clean().await.unwrap();
    first.release_execution_owner(&token).await.unwrap();
    drop(first);

    let cold = virtual_remote_facade(transport.clone(), environment.clone());
    let restored = cold
        .validate_bound_workspace(&session_id, BindingRecheck::Full)
        .await
        .unwrap();
    assert_eq!(restored.workspace_id, workspace.workspace_id);
    let lease = cold
        .acquire_execution(&session_id, &restored)
        .await
        .unwrap();
    let token = lease.owner_token().unwrap();
    lease.mark_clean().await.unwrap();
    cold.release_execution_owner(&token).await.unwrap();
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
    for statement in crate::sessions::canonical::CREATE_V2_TABLES
        .iter()
        .chain(crate::sessions::canonical::CREATE_V2_INDEXES)
    {
        sqlx::query(*statement).execute(&pool).await.unwrap();
    }
    sqlx::query(schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(super::super::ledger::CREATE_OP_LEDGER_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, 13, 'remote-only-test', 'peri.session.store/v3', 'now')")
        .execute(&pool).await.unwrap();
    let transport = Arc::new(SqliteTransport {
        pool: pool.clone(),
        writes: AtomicUsize::new(0),
        fail_column_drop: AtomicBool::new(false),
        change_schema: AtomicBool::new(false),
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
    let first_lease = first.create_session(&input).await.unwrap();
    let first_token = first_lease.owner_token().unwrap();
    first_lease.mark_clean().await.unwrap();
    first.release_execution_owner(&first_token).await.unwrap();
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

    // Old v2 remote rows used a separate execution registration UUID. The
    // logical owner remains the remote workspace row and its saved snapshot.
    let old_registration = uuid::Uuid::new_v4().to_string();
    sqlx::query("UPDATE session_bindings SET workspace_id=?1 WHERE thread_id=?2")
        .bind(&old_registration)
        .bind(&session_id)
        .execute(&pool)
        .await
        .unwrap();
    let cold = full_remote_facade(transport.clone());
    let restored = cold
        .validate_bound_workspace(&session_id, BindingRecheck::Full)
        .await
        .unwrap();
    assert_eq!(restored.workspace_id, workspace.workspace_id);
    assert_eq!(
        restored.execution_registration_id.to_string(),
        old_registration
    );
    let cold_lease = cold.acquire_execution(&session_id, &restored).await.unwrap();
    let cold_token = cold_lease.owner_token().unwrap();
    cold_lease.mark_clean().await.unwrap();
    cold.release_execution_owner(&cold_token).await.unwrap();
    assert_eq!(
        std::fs::read(&poisoned_local_file).unwrap(),
        b"broken local SQLite"
    );
    drop(cold);

    // Existing metadata edits require a new Store owner generation.
    let unleased = full_remote_facade(transport.clone());
    let editing_lease = unleased.acquire_execution(&session_id, &restored).await.unwrap();
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
    let editing_token = editing_lease.owner_token().unwrap();
    editing_lease.mark_clean().await.unwrap();
    unleased.release_execution_owner(&editing_token).await.unwrap();
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
    assert!(missing
        .acquire_execution(&session_id, &projection)
        .await
        .is_err());
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
