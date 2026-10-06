use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::session_resources::{
    ControlAction, ControlAttempt, ControlCommand, ControlDecision, ControlRejection,
    ControlResolution, ControlState, ControlStatus, SessionResourceResult,
};
use peri_acp_types::{identity::AttemptId, session::TurnId};
use sqlx::{
    sqlite::{SqliteArguments, SqlitePoolOptions, SqliteRow},
    Arguments, Row, Sqlite, SqlitePool, TypeInfo, ValueRef,
};
use turso_serverless::{Error as SdkError, Value};

use super::super::{
    connection::RemoteTransport,
    generation::{ConnectionFactory, ConnectionGate},
    ledger::CREATE_OP_LEDGER_SQL,
    mutation::{FaultPlan, RemoteStore, StoreAccess},
    schema::StoreId,
    session_data::RemoteSessionData,
    sql::StatementSpec,
};
use crate::sessions::data::SessionDataPort;

struct SqliteTransport {
    pool: SqlitePool,
}

fn arguments(spec: &StatementSpec) -> turso_serverless::Result<SqliteArguments> {
    let mut arguments = SqliteArguments::default();
    for value in &spec.params {
        let result = match value {
            Value::Text(text) => arguments.add(text.clone()),
            Value::Integer(integer) => arguments.add(*integer),
            Value::Null => arguments.add(Option::<String>::None),
            _ => return Err(SdkError::Http("unsupported test value".into())),
        };
        result.map_err(|_| SdkError::Http("test argument encoding failed".into()))?;
    }
    Ok(arguments)
}

fn rows(values: Vec<SqliteRow>) -> turso_serverless::Result<Vec<Vec<Value>>> {
    values
        .into_iter()
        .map(|row| {
            row.columns()
                .iter()
                .enumerate()
                .map(|(index, _column)| {
                    let raw = row
                        .try_get_raw(index)
                        .map_err(|_| SdkError::Http("test row decoding failed".into()))?;
                    if raw.is_null() {
                        return Ok(Value::Null);
                    }
                    match raw.type_info().name() {
                        "INTEGER" => row
                            .try_get::<Option<i64>, _>(index)
                            .map(|value| value.map(Value::Integer).unwrap_or(Value::Null)),
                        _ => row
                            .try_get::<Option<String>, _>(index)
                            .map(|value| value.map(Value::Text).unwrap_or(Value::Null)),
                    }
                    .map_err(|_| SdkError::Http("test row decoding failed".into()))
                })
                .collect()
        })
        .collect()
}

fn database_error(error: sqlx::Error) -> SdkError {
    match error {
        sqlx::Error::Database(error)
            if error.is_unique_violation()
                || error.is_check_violation()
                || error.kind() == sqlx::error::ErrorKind::NotNullViolation
                || error.is_foreign_key_violation() =>
        {
            SdkError::Constraint(error.to_string())
        }
        sqlx::Error::Database(error) => SdkError::Http(error.to_string()),
        _ => SdkError::Http("SQL transaction failure".into()),
    }
}

#[async_trait]
impl RemoteTransport for SqliteTransport {
    async fn sql_values(&self, spec: &StatementSpec) -> turso_serverless::Result<Vec<Vec<Value>>> {
        rows(
            sqlx::query_with::<Sqlite, _>(spec.sql, arguments(spec)?)
                .fetch_all(&self.pool)
                .await
                .map_err(database_error)?,
        )
    }

    async fn managed_batch(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<u64>> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(database_error)?;
        let mut counts = Vec::new();
        for (index, spec) in statements.iter().enumerate() {
            match sqlx::query_with::<Sqlite, _>(spec.sql, arguments(spec)?)
                .execute(&mut *tx)
                .await
            {
                Ok(result) => counts.push(result.rows_affected()),
                Err(error) => {
                    tx.rollback().await.map_err(database_error)?;
                    return Err(SdkError::BatchStatementFailed {
                        index,
                        error: Box::new(database_error(error)),
                        results: Vec::new(),
                    });
                }
            }
        }
        tx.commit().await.map_err(database_error)?;
        Ok(counts)
    }

    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<Vec<Vec<Value>>>> {
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        let mut results = Vec::new();
        for spec in statements {
            results.push(rows(
                sqlx::query_with::<Sqlite, _>(spec.sql, arguments(&spec)?)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(database_error)?,
            )?);
        }
        tx.commit().await.map_err(database_error)?;
        Ok(results)
    }

    fn is_autocommit(&self) -> turso_serverless::Result<bool> {
        Ok(true)
    }
    async fn close(&self) -> turso_serverless::Result<()> {
        Ok(())
    }
}

struct Factory {
    pool: SqlitePool,
    gate: Arc<ConnectionGate>,
}

#[async_trait]
impl ConnectionFactory for Factory {
    async fn connect(&self) -> SessionResourceResult<RemoteStore> {
        Ok(RemoteStore::new(
            Arc::new(SqliteTransport {
                pool: self.pool.clone(),
            }),
            StoreAccess::ReadWrite,
            self.gate.mint(),
            self.gate.clone(),
        ))
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    pool: SqlitePool,
    id: StoreId,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("remote.db"))
            .create_if_missing(true)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        for spec in super::super::session_schema::initialization_plan() {
            sqlx::query(spec.sql).execute(&pool).await.unwrap();
        }
        for sql in [CREATE_OP_LEDGER_SQL,
            "INSERT INTO machines(id,name,identity_kind) VALUES ('machine','remote test','known')",
            "INSERT INTO workspaces(id,machine_id,path,path_source) VALUES ('workspace','machine','/test','discovered')",
            "INSERT INTO threads(id,created_at,updated_at,workspace_id) VALUES ('remote-control','2026-10-05T00:00:00Z','2026-10-05T00:00:00Z','workspace')"] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        Self {
            directory,
            pool,
            id: StoreId::mint(),
        }
    }

    async fn adapter(&self) -> RemoteSessionData {
        let gate = Arc::new(ConnectionGate::default());
        let factory = Arc::new(Factory {
            pool: self.pool.clone(),
            gate: gate.clone(),
        });
        let store = factory.connect().await.unwrap();
        RemoteSessionData::with_connection_for_test(self.id.clone(), store, factory, gate)
    }
}

fn command(command_id: &str, state: &ControlState, action: ControlAction) -> ControlCommand {
    ControlCommand {
        session_id: "remote-control".into(),
        command_id: command_id.into(),
        expected_lifecycle: state.lifecycle,
        expected_revision: state.revision,
        expected_control_generation: state.control_generation,
        action,
    }
}

#[tokio::test]
async fn remote_control_replays_original_business_receipt_on_new_adapter() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let pause = command("pause", &ControlState::default(), ControlAction::Pause);
    let original = adapter.apply_session_control(&pause).await.unwrap();
    let resume = command("resume", &original.state, ControlAction::Resume);
    adapter.apply_session_control(&resume).await.unwrap();
    let other = fixture.adapter().await;
    assert_eq!(other.apply_session_control(&pause).await.unwrap(), original);
    assert_eq!(
        other
            .load_session_control(&pause.session_id)
            .await
            .unwrap()
            .status,
        ControlStatus::Active
    );
    assert!(fixture.directory.path().join("remote.db").exists());
}

#[tokio::test]
async fn remote_control_lost_commit_ack_resolves_original_id_without_reapplication() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    adapter
        .inject_faults(FaultPlan {
            drop_reply: Some("session_control".into()),
            drop_before_send: None,
        })
        .await;
    let close = command("lost-ack", &ControlState::default(), ControlAction::Close);
    assert!(adapter
        .apply_session_control(&close)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    let other = fixture.adapter().await;
    let ControlResolution::Applied { receipt } =
        other.resolve_session_control(&close).await.unwrap()
    else {
        panic!("missing committed control receipt")
    };
    assert_eq!(receipt.state.status, ControlStatus::Closing);
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_close_intents")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(other.apply_session_control(&close).await.unwrap(), receipt);
    assert_eq!(
        other
            .load_session_control(&close.session_id)
            .await
            .unwrap()
            .revision,
        1
    );
}

#[tokio::test]
async fn remote_control_pre_send_unknown_is_finalized_and_late_original_is_rejected() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    adapter
        .inject_faults(FaultPlan {
            drop_reply: None,
            drop_before_send: Some("session_control".into()),
        })
        .await;
    let pause = command(
        "before-send",
        &ControlState::default(),
        ControlAction::Pause,
    );
    assert!(adapter
        .apply_session_control(&pause)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert_eq!(
        adapter.resolve_session_control(&pause).await.unwrap(),
        ControlResolution::NotApplied
    );
    assert!(fixture
        .adapter()
        .await
        .apply_session_control(&pause)
        .await
        .is_err());
    assert_eq!(
        adapter
            .load_session_control(&pause.session_id)
            .await
            .unwrap(),
        ControlState::default()
    );
}

#[tokio::test]
async fn remote_control_concurrent_domain_cas_records_one_acceptance_and_one_rejection() {
    let fixture = Fixture::new().await;
    let first = fixture.adapter().await;
    let second = fixture.adapter().await;
    let pause = command("race-pause", &ControlState::default(), ControlAction::Pause);
    let close = command("race-close", &ControlState::default(), ControlAction::Close);
    let (first, second) = tokio::join!(
        first.apply_session_control(&pause),
        second.apply_session_control(&close)
    );
    let receipts = [first.unwrap(), second.unwrap()];
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.decision == ControlDecision::Accepted)
            .count(),
        1
    );
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.decision
                == ControlDecision::Rejected {
                    reason: ControlRejection::StaleRevision
                })
            .count(),
        1
    );
}

#[tokio::test]
async fn remote_control_conflicting_identity_cannot_replace_receipt() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let pause = command("same", &ControlState::default(), ControlAction::Pause);
    let receipt = adapter.apply_session_control(&pause).await.unwrap();
    let conflicting = command("same", &receipt.state, ControlAction::Resume);
    assert!(adapter.apply_session_control(&conflicting).await.is_err());
    assert!(adapter.resolve_session_control(&conflicting).await.is_err());
    assert_eq!(
        adapter
            .load_session_control(&pause.session_id)
            .await
            .unwrap(),
        receipt.state
    );
}

#[tokio::test]
async fn remote_control_finish_retains_tombstone_after_session_delete() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let close = command("close", &ControlState::default(), ControlAction::Close);
    let closing = adapter.apply_session_control(&close).await.unwrap();
    sqlx::query("DELETE FROM threads")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let finish = command("finish", &closing.state, ControlAction::FinishClose);
    let closed = adapter.apply_session_control(&finish).await.unwrap();
    assert_eq!(closed.state.status, ControlStatus::Closed);
    assert_eq!(
        fixture
            .adapter()
            .await
            .load_session_control(&close.session_id)
            .await
            .unwrap(),
        closed.state
    );
}

#[tokio::test]
async fn remote_control_unknown_freezes_facade_until_original_id_is_resolved() {
    use crate::sessions::{
        resources::{SessionDataHome, SessionResourcesImpl},
        sqlite_store::LocalExecution,
    };
    use peri_acp_types::session_resources::{PersistenceRecovery, SessionResources};

    let fixture = Fixture::new().await;
    let adapter = Arc::new(fixture.adapter().await);
    adapter
        .inject_faults(FaultPlan {
            drop_reply: Some("session_control".into()),
            drop_before_send: None,
        })
        .await;
    let local = LocalExecution::open(fixture.directory.path().join("local.db"))
        .await
        .unwrap();
    let facade =
        SessionResourcesImpl::from_ports(adapter, Arc::new(local), SessionDataHome::RemoteStore);
    let pause = command("pending", &ControlState::default(), ControlAction::Pause);
    assert!(facade
        .apply_session_control(&pause)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert!(facade
        .load_session_control(&pause.session_id)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert_eq!(
        facade
            .recover_session_persistence(&pause.session_id)
            .await
            .unwrap(),
        PersistenceRecovery::StillBlocked
    );
    let wrong = command(
        "replacement",
        &ControlState::default(),
        ControlAction::Resume,
    );
    assert!(facade
        .apply_session_control(&wrong)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert!(facade.resolve_session_control(&wrong).await.is_err());
    assert!(matches!(
        facade.resolve_session_control(&pause).await.unwrap(),
        ControlResolution::Applied { .. }
    ));
    assert_eq!(
        facade
            .load_session_control(&pause.session_id)
            .await
            .unwrap()
            .status,
        ControlStatus::Paused
    );
}

#[tokio::test]
async fn remote_control_observed_attempt_cannot_be_replaced_and_stop_is_exact() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let target = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let observe = command(
        "observe",
        &ControlState::default(),
        ControlAction::ObserveAttempt {
            target: Some(target.clone()),
        },
    );
    let observed = adapter.apply_session_control(&observe).await.unwrap();
    assert_eq!(observed.state.control_generation, 0);
    let replacement = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let replace = command(
        "replace",
        &observed.state,
        ControlAction::ObserveAttempt {
            target: Some(replacement),
        },
    );
    let rejected = adapter.apply_session_control(&replace).await.unwrap();
    assert_eq!(
        rejected.decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleAttempt
        }
    );
    assert_eq!(rejected.state, observed.state);
    let wrong_stop = command(
        "wrong-stop",
        &observed.state,
        ControlAction::Stop {
            target: ControlAttempt {
                turn_id: TurnId::new(),
                attempt_id: target.attempt_id.clone(),
            },
        },
    );
    assert_eq!(
        adapter
            .apply_session_control(&wrong_stop)
            .await
            .unwrap()
            .decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleAttempt
        }
    );
    let stop = command(
        "exact-stop",
        &observed.state,
        ControlAction::Stop { target },
    );
    let stopped = adapter.apply_session_control(&stop).await.unwrap();
    assert_eq!(stopped.decision, ControlDecision::Accepted);
    assert_eq!(stopped.state.status, ControlStatus::Paused);
    let stale = command("stale-generation", &stopped.state, ControlAction::Resume);
    let stale = ControlCommand {
        expected_control_generation: 0,
        ..stale
    };
    assert_eq!(
        adapter
            .apply_session_control(&stale)
            .await
            .unwrap()
            .decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleControlGeneration
        }
    );
}

#[tokio::test]
async fn remote_control_close_and_reopen_project_existing_close_flags_atomically() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let close = command(
        "close-flags",
        &ControlState::default(),
        ControlAction::Close,
    );
    let closing = adapter.apply_session_control(&close).await.unwrap();
    assert_eq!(closing.state.status, ControlStatus::Closing);
    let flags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_close_intents")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(flags, 1);
    let finish = command("finish-flags", &closing.state, ControlAction::FinishClose);
    let closed = adapter.apply_session_control(&finish).await.unwrap();
    sqlx::query("INSERT INTO session_close_intents(thread_id,requested_at) VALUES ('remote-control','legacy')").execute(&fixture.pool).await.unwrap();
    let reopen = command("reopen-flags", &closed.state, ControlAction::Reopen);
    let reopened = adapter.apply_session_control(&reopen).await.unwrap();
    assert_eq!(reopened.state.status, ControlStatus::Active);
    assert_eq!(reopened.state.lifecycle, closed.state.lifecycle + 1);
    let flags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_close_intents")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(flags, 0);
    assert_eq!(
        adapter.apply_session_control(&close).await.unwrap(),
        closing
    );
}

#[tokio::test]
async fn remote_control_v14_refusal_preserves_existing_close_intent_and_store_identity() {
    use super::super::schema::{StoreSnapshot, STORE_CONTRACT};
    let directory = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("legacy14.db"))
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE IF NOT EXISTS machines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    identity_kind TEXT NOT NULL CHECK(identity_kind IN ('known', 'legacy_unknown'))
);
CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    machine_id TEXT NOT NULL REFERENCES machines(id),
    path TEXT NOT NULL,
    path_source TEXT NOT NULL CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
    UNIQUE(machine_id, path)
);
CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))
);
CREATE TABLE IF NOT EXISTS messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT
);
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
    UNIQUE(locator, object_identity)
);
CREATE TABLE IF NOT EXISTS legacy_execution_registrations (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
    UNIQUE(root, root_identity), UNIQUE(id, project_id)
);
CREATE TABLE IF NOT EXISTS session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
    discovery_snapshot TEXT,
    evidence_origin TEXT NOT NULL,
    FOREIGN KEY(workspace_id, project_id) REFERENCES legacy_execution_registrations(id, project_id)
);
CREATE TABLE IF NOT EXISTS mcp_oauth_credentials (
    principal_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    server_key TEXT NOT NULL,
    credentials_blob TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(principal_id, workspace_id, server_key)
);
CREATE TABLE IF NOT EXISTS session_close_intents (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    requested_at TEXT NOT NULL
);"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(super::super::schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO machines VALUES ('machine','legacy','known'); INSERT INTO workspaces VALUES ('workspace','machine','/old','unverified'); INSERT INTO threads(id,created_at,updated_at,workspace_id) VALUES ('remote-control','now','now','workspace'); INSERT INTO session_close_intents VALUES ('remote-control','legacy')").execute(&pool).await.unwrap();
    let id = StoreId::mint();
    sqlx::query("INSERT INTO peri_store_meta VALUES (0,14,?1,?2,'original timestamp')")
        .bind(id.as_str())
        .bind(STORE_CONTRACT)
        .execute(&pool)
        .await
        .unwrap();
    let gate = Arc::new(ConnectionGate::default());
    let factory = Factory {
        pool: pool.clone(),
        gate,
    };
    let store = factory.connect().await.unwrap();
    let snapshot = StoreSnapshot {
        store_id: id.clone(),
        schema_version: 14,
        contract: STORE_CONTRACT.into(),
    };
    let error = super::super::schema_v14_upgrade::upgrade(&store, &snapshot)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::Unsupported
    ));
    let row: (i64, String, String) = sqlx::query_as(
        "SELECT schema_version,store_id,requested_at FROM peri_store_meta,session_close_intents",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (14, id.as_str().into(), "legacy".into()));
    let tables: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('session_control_state','session_control_receipts','session_work_head')").fetch_one(&pool).await.unwrap();
    assert_eq!(tables, 0);
}
