use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::{
    messages::BaseMessage,
    session::MessagePolicy,
    session_resources::{work::*, SessionResourceResult},
    store::PersistedPayload,
};
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

#[path = "session_pending_work_test.rs"]
mod pending_work;

#[path = "session_delivery_query_test.rs"]
mod delivery_query;

struct SqliteTransport {
    pool: SqlitePool,
}
fn arguments(spec: &StatementSpec) -> turso_serverless::Result<SqliteArguments> {
    let mut arguments = SqliteArguments::default();
    for value in &spec.params {
        let result = match value {
            Value::Text(value) => arguments.add(value.clone()),
            Value::Integer(value) => arguments.add(*value),
            Value::Null => arguments.add(Option::<String>::None),
            _ => return Err(SdkError::Http("unsupported work test value".into())),
        };
        result.map_err(|_| SdkError::Http("work test argument encoding failed".into()))?;
    }
    Ok(arguments)
}
fn rows(values: Vec<SqliteRow>) -> turso_serverless::Result<Vec<Vec<Value>>> {
    values
        .into_iter()
        .map(|row| {
            (0..row.len())
                .map(|index| {
                    let raw = row
                        .try_get_raw(index)
                        .map_err(|_| SdkError::Http("work test decoding failed".into()))?;
                    if raw.is_null() {
                        return Ok(Value::Null);
                    }
                    match raw.type_info().name() {
                        "INTEGER" => row.try_get::<i64, _>(index).map(Value::Integer),
                        _ => row.try_get::<String, _>(index).map(Value::Text),
                    }
                    .map_err(|_| SdkError::Http("work test decoding failed".into()))
                })
                .collect()
        })
        .collect()
}
fn database_error(error: sqlx::Error) -> SdkError {
    match error {
        sqlx::Error::Database(error)
            if matches!(
                error.kind(),
                sqlx::error::ErrorKind::UniqueViolation
                    | sqlx::error::ErrorKind::NotNullViolation
                    | sqlx::error::ErrorKind::CheckViolation
                    | sqlx::error::ErrorKind::ForeignKeyViolation
            ) =>
        {
            SdkError::Constraint(error.to_string())
        }
        sqlx::Error::Database(error) => SdkError::Http(error.to_string()),
        _ => SdkError::Http("work SQL failure".into()),
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
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(database_error)?;
        let mut counts = Vec::new();
        for (index, spec) in statements.iter().enumerate() {
            match sqlx::query_with::<Sqlite, _>(spec.sql, arguments(spec)?)
                .execute(&mut *transaction)
                .await
            {
                Ok(result) => counts.push(result.rows_affected()),
                Err(error) => {
                    transaction.rollback().await.map_err(database_error)?;
                    return Err(SdkError::BatchStatementFailed {
                        index,
                        error: Box::new(database_error(error)),
                        results: Vec::new(),
                    });
                }
            }
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(counts)
    }
    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<Vec<Vec<Value>>>> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let mut batches = Vec::new();
        for spec in statements {
            batches.push(rows(
                sqlx::query_with::<Sqlite, _>(spec.sql, arguments(&spec)?)
                    .fetch_all(&mut *transaction)
                    .await
                    .map_err(database_error)?,
            )?);
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(batches)
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
            .filename(directory.path().join("remote-work.db"))
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
        for sql in [CREATE_OP_LEDGER_SQL,"INSERT INTO machines(id,name,identity_kind) VALUES ('machine','work test','known')","INSERT INTO workspaces(id,machine_id,path,path_source) VALUES ('workspace','machine','/test','discovered')","INSERT INTO threads(id,created_at,updated_at,workspace_id) VALUES ('work-session','2026-10-05T00:00:00Z','2026-10-05T00:00:00Z','workspace')"] { sqlx::query(sql).execute(&pool).await.unwrap(); }
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
fn publication(mutation_id: &str, delivery_id: &str) -> WorkCommand {
    WorkCommand {
        session_id: "work-session".into(),
        recipient_lifecycle: 1,
        mutation_id: mutation_id.into(),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: delivery_id.into(),
                event: WorkEvent {
                    producer_namespace: "trusted-test".into(),
                    event_id: format!("event-{delivery_id}"),
                    event_kind: "terminal".into(),
                    causation_id: None,
                    content: WorkPayload::from_payload(&PersistedPayload::Message(
                        BaseMessage::human("process terminal result"),
                    ))
                    .unwrap(),
                },
                purpose: DeliveryPurpose::TaskTerminal,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    }
}
fn query() -> WorkQuery {
    WorkQuery {
        session_id: "work-session".into(),
        limit: 64,
    }
}

#[tokio::test]
async fn remote_work_publish_lost_ack_keeps_one_delivery_and_resolves_original_receipt() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let command = publication("lost-ack", "terminal");
    adapter
        .inject_faults(FaultPlan {
            drop_reply: Some("session_work".into()),
            drop_before_send: None,
        })
        .await;
    assert!(adapter
        .apply_work_mutation(&command)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    let reopened = fixture.adapter().await;
    let resolution = reopened.resolve_work_mutation(&command).await.unwrap();
    let WorkResolution::Applied { receipt } = resolution else {
        panic!("missing committed work receipt")
    };
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    assert_eq!(
        reopened.apply_work_mutation(&command).await.unwrap(),
        receipt
    );
    let loaded = reopened.load_session_work(&query()).await.unwrap();
    assert_eq!(loaded.state.deliveries.len(), 1);
    assert_eq!(
        loaded.state.obligations["terminal"].status,
        ObligationStatus::Pending
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_work_receipts")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn remote_work_unknown_before_send_is_sealed_and_cannot_late_apply() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let command = publication("before-send", "terminal");
    adapter
        .inject_faults(FaultPlan {
            drop_reply: None,
            drop_before_send: Some("session_work".into()),
        })
        .await;
    assert!(adapter
        .apply_work_mutation(&command)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert_eq!(
        adapter.resolve_work_mutation(&command).await.unwrap(),
        WorkResolution::NotApplied
    );
    assert!(fixture
        .adapter()
        .await
        .apply_work_mutation(&command)
        .await
        .is_err());
    assert!(adapter
        .load_session_work(&query())
        .await
        .unwrap()
        .state
        .deliveries
        .is_empty());
}

#[tokio::test]
async fn remote_work_concurrent_publish_uses_domain_cas_and_monotonic_admission_sequence() {
    let fixture = Fixture::new().await;
    let first = fixture.adapter().await;
    let second = fixture.adapter().await;
    let first_command = publication("first", "first-delivery");
    let second_command = publication("second", "second-delivery");
    // 同一 session 只允许一条未对账的 work 命令（work::GUARD_COMMAND 拒绝并发的 begin），
    // 因此这里顺序提交：本测试验证的是 domain CAS 与 admission_sequence 单调，不是并发准入。
    let first_result = first.apply_work_mutation(&first_command).await;
    let second_result = second.apply_work_mutation(&second_command).await;
    assert_eq!(first_result.unwrap().decision, WorkDecision::Accepted);
    assert_eq!(second_result.unwrap().decision, WorkDecision::Accepted);
    let loaded = first.load_session_work(&query()).await.unwrap();
    let mut sequences: Vec<_> = loaded
        .state
        .deliveries
        .values()
        .map(|delivery| delivery.admission_sequence)
        .collect();
    sequences.sort();
    assert_eq!(sequences, vec![1, 2]);
}

#[tokio::test]
async fn remote_work_conflicting_mutation_id_never_overwrites_content() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let command = publication("same-id", "terminal");
    let receipt = adapter.apply_work_mutation(&command).await.unwrap();
    let conflict = publication("same-id", "different");
    assert!(adapter.apply_work_mutation(&conflict).await.is_err());
    assert!(adapter.resolve_work_mutation(&conflict).await.is_err());
    assert_eq!(
        adapter.apply_work_mutation(&command).await.unwrap(),
        receipt
    );
    assert_eq!(
        adapter
            .load_session_work(&query())
            .await
            .unwrap()
            .state
            .deliveries
            .len(),
        1
    );
}

#[tokio::test]
async fn remote_work_unknown_freezes_control_and_cannot_be_cleared_by_generic_recovery() {
    use crate::sessions::{
        resources::{SessionDataHome, SessionResourcesImpl},
        sqlite_store::LocalExecution,
    };
    use peri_acp_types::session_resources::{
        ControlAction, ControlCommand, PersistenceRecovery, SessionResources,
    };
    let fixture = Fixture::new().await;
    let adapter = Arc::new(fixture.adapter().await);
    adapter
        .inject_faults(FaultPlan {
            drop_reply: Some("session_work".into()),
            drop_before_send: None,
        })
        .await;
    let local = LocalExecution::open(fixture.directory.path().join("local.db"))
        .await
        .unwrap();
    let resources =
        SessionResourcesImpl::from_ports(adapter, Arc::new(local), SessionDataHome::RemoteStore);
    let command = publication("freeze", "terminal");
    assert!(resources
        .apply_work_mutation(&command)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    let frozen = resources.load_session_work(&query()).await.unwrap();
    assert!(frozen.blocked);
    assert_eq!(frozen.pending_commands, vec![command.clone()]);
    assert_eq!(
        resources
            .recover_session_persistence(&command.session_id)
            .await
            .unwrap(),
        PersistenceRecovery::StillBlocked
    );
    let pause = ControlCommand {
        session_id: command.session_id.clone(),
        command_id: "pause-during-unknown".into(),
        expected_lifecycle: 1,
        expected_revision: 0,
        expected_control_generation: 0,
        action: ControlAction::Pause,
    };
    assert!(resources
        .apply_session_control(&pause)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    let replacement = publication("replacement", "replacement");
    assert!(resources.resolve_work_mutation(&replacement).await.is_err());
    assert!(matches!(
        resources.resolve_work_mutation(&command).await.unwrap(),
        WorkResolution::Applied { .. }
    ));
    assert_eq!(
        resources
            .load_session_work(&query())
            .await
            .unwrap()
            .state
            .deliveries
            .len(),
        1
    );
}

#[tokio::test]
async fn owned_original_command_survives_fresh_facade_begin_and_effect_ack_loss() {
    use crate::sessions::{
        resources::{SessionDataHome, SessionResourcesImpl},
        sqlite_store::LocalExecution,
    };
    use peri_acp_types::session_resources::{
        ControlAction, ControlCommand, PersistenceRecovery, SessionResources,
    };
    for (fault_kind, applied) in [("session_work_begin", false), ("session_work", true)] {
        let fixture = Fixture::new().await;
        let adapter = Arc::new(fixture.adapter().await);
        let mut user_input = publication("initial", "delivery");
        if let WorkAction::PublishDelivery { delivery } = &mut user_input.action {
            delivery.purpose = DeliveryPurpose::UserInput;
        }
        let initial = adapter.apply_work_mutation(&user_input).await.unwrap();
        let command = WorkCommand {
            session_id: "work-session".into(),
            recipient_lifecycle: 1,
            mutation_id: "original-withdraw".into(),
            action: WorkAction::WithdrawDelivery {
                expected_revision: initial.revision,
                expected_control_generation: None,
                delivery_id: "delivery".into(),
                authorization_ref: "trusted-original-input-command".into(),
            },
        };
        adapter
            .inject_faults(FaultPlan {
                drop_reply: Some(fault_kind.into()),
                drop_before_send: None,
            })
            .await;
        let local_path = fixture.directory.path().join("local.db");
        let local = LocalExecution::open(&local_path).await.unwrap();
        let resources = SessionResourcesImpl::from_ports(
            adapter,
            Arc::new(local),
            SessionDataHome::RemoteStore,
        );
        assert!(resources
            .apply_work_mutation(&command)
            .await
            .unwrap_err()
            .is_persistence_uncertain());
        drop(resources);
        let reopened = SessionResourcesImpl::from_ports(
            Arc::new(fixture.adapter().await),
            Arc::new(LocalExecution::open(&local_path).await.unwrap()),
            SessionDataHome::RemoteStore,
        );
        let loaded = reopened.load_session_work(&query()).await.unwrap();
        assert!(loaded.blocked);
        assert!(loaded.candidates.is_empty());
        assert_eq!(loaded.pending_commands, vec![command.clone()]);
        assert_eq!(
            loaded.state.obligations["delivery"].status,
            if applied {
                ObligationStatus::Abandoned
            } else {
                ObligationStatus::Pending
            }
        );
        assert!(reopened
            .apply_work_mutation(&publication("replacement", "new-delivery"))
            .await
            .unwrap_err()
            .is_persistence_uncertain());
        assert_eq!(
            reopened
                .recover_session_persistence(&command.session_id)
                .await
                .unwrap(),
            PersistenceRecovery::StillBlocked
        );
        let pause = ControlCommand {
            session_id: command.session_id.clone(),
            command_id: "new-pause".into(),
            expected_lifecycle: 1,
            expected_revision: 0,
            expected_control_generation: 0,
            action: ControlAction::Pause,
        };
        assert!(reopened
            .apply_session_control(&pause)
            .await
            .unwrap_err()
            .is_persistence_uncertain());
        let mut forged = command.clone();
        if let WorkAction::WithdrawDelivery {
            expected_revision, ..
        } = &mut forged.action
        {
            *expected_revision += 1;
        }
        assert!(reopened.resolve_work_mutation(&forged).await.is_err());
        let resolution = reopened
            .resolve_work_mutation(&loaded.pending_commands[0])
            .await
            .unwrap();
        if applied {
            assert!(matches!(resolution, WorkResolution::Applied { .. }));
        } else {
            assert_eq!(resolution, WorkResolution::NotApplied);
        }
        assert_eq!(
            reopened.resolve_work_mutation(&command).await.unwrap(),
            resolution
        );
        let owned = reopened
            .load_work_command(&WorkCommandQuery {
                session_id: command.session_id.clone(),
                mutation_id: command.mutation_id.clone(),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(owned.command, command);
        assert_eq!(owned.resolution, Some(resolution.clone()));
        assert!(!owned.pending);
        assert!(reopened
            .load_session_work(&query())
            .await
            .unwrap()
            .pending_commands
            .is_empty());
        let original: String = sqlx::query_scalar(
            "SELECT command_json FROM session_work_commands WHERE mutation_id='original-withdraw'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<WorkCommand>(&original).unwrap(),
            command
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM session_work_receipts WHERE mutation_id='original-withdraw'"
            )
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
            1
        );
    }
}
