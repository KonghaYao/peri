use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use peri_acp_types::session_resources::SessionResourceErrorKind;
use sqlx::{
    sqlite::{SqliteArguments, SqlitePoolOptions, SqliteRow},
    Connection, Row, Sqlite, SqlitePool, TypeInfo, ValueRef,
};
use turso_serverless::{Error as SdkError, Value};

use super::connection::RemoteTransport;
use super::generation::ConnectionGate;
use super::mutation::{RemoteStore, StoreAccess};
use super::schema::{self, StoreIdentityRead, StoreSnapshot};
use super::schema_upgrade;
use super::session_data::{open_step, OpenStep};
use super::sql::StatementSpec;
use crate::sessions::{
    canonical::CREATE_TABLES,
    schema_cleanup::{LEGACY_EXECUTION_SQL, LEGACY_GOALS_SQL},
};

struct SqliteTransport {
    pool: SqlitePool,
    writes: AtomicUsize,
    fail_column_drop: AtomicBool,
    change_schema: AtomicBool,
    drop_reply: AtomicBool,
    truncate_reply: AtomicBool,
}

fn query(spec: &StatementSpec) -> sqlx::query::Query<'static, Sqlite, SqliteArguments> {
    let mut query = sqlx::query(spec.sql);
    for value in &spec.params {
        query = match value {
            Value::Null => query.bind(None::<String>),
            Value::Integer(number) => query.bind(*number),
            Value::Real(number) => query.bind(*number),
            Value::Text(text) => query.bind(text.clone()),
            Value::Blob(bytes) => query.bind(bytes.clone()),
        };
    }
    query
}

fn values(row: &SqliteRow) -> Vec<Value> {
    (0..row.len())
        .map(|index| {
            let raw = row.try_get_raw(index).unwrap();
            if raw.is_null() {
                return Value::Null;
            }
            match raw.type_info().name() {
                "INTEGER" => Value::Integer(row.get(index)),
                "REAL" => Value::Real(row.get(index)),
                "BLOB" => Value::Blob(row.get(index)),
                _ => Value::Text(row.get(index)),
            }
        })
        .collect()
}

fn error_of(error: sqlx::Error) -> SdkError {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation() || error.is_foreign_key_violation())
    {
        SdkError::Constraint("local fixture rejected statement".into())
    } else {
        SdkError::Error("local fixture rejected statement".into())
    }
}

#[async_trait]
impl RemoteTransport for SqliteTransport {
    async fn sql_values(&self, spec: &StatementSpec) -> turso_serverless::Result<Vec<Vec<Value>>> {
        Ok(query(spec)
            .fetch_all(&self.pool)
            .await
            .map_err(error_of)?
            .iter()
            .map(values)
            .collect())
    }

    async fn managed_batch(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<u64>> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.change_schema.swap(false, Ordering::SeqCst) {
            sqlx::query("CREATE TABLE late_extension (id TEXT REFERENCES thread_goals(thread_id))")
                .execute(&self.pool)
                .await
                .map_err(error_of)?;
        }
        let mut connection = self.pool.acquire().await.map_err(error_of)?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(error_of)?;
        let mut counts = Vec::new();
        for (index, statement) in statements.iter().enumerate() {
            let result = if statement.sql == "ALTER TABLE threads DROP COLUMN cached_context"
                && self.fail_column_drop.swap(false, Ordering::SeqCst)
            {
                sqlx::query("ALTER TABLE threads DROP COLUMN absent_column")
                    .execute(&mut *transaction)
                    .await
            } else {
                query(statement).execute(&mut *transaction).await
            };
            match result {
                Ok(result) => counts.push(result.rows_affected()),
                Err(error) => {
                    transaction.rollback().await.map_err(error_of)?;
                    return Err(SdkError::BatchStatementFailed {
                        index,
                        error: Box::new(error_of(error)),
                        results: Vec::new(),
                    });
                }
            }
        }
        transaction.commit().await.map_err(error_of)?;
        if self.drop_reply.swap(false, Ordering::SeqCst) {
            return Err(SdkError::Http("simulated lost upgrade response".into()));
        }
        if self.truncate_reply.swap(false, Ordering::SeqCst) {
            counts.pop();
        }
        Ok(counts)
    }

    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<Vec<Vec<Value>>>> {
        let mut connection = self.pool.acquire().await.map_err(error_of)?;
        let mut transaction = connection
            .begin_with("BEGIN DEFERRED")
            .await
            .map_err(error_of)?;
        let mut results = Vec::new();
        for statement in statements {
            let rows = query(&statement)
                .fetch_all(&mut *transaction)
                .await
                .map_err(error_of)?;
            results.push(rows.iter().map(values).collect());
        }
        transaction.commit().await.map_err(error_of)?;
        Ok(results)
    }

    fn is_autocommit(&self) -> turso_serverless::Result<bool> {
        Ok(true)
    }
    async fn close(&self) -> turso_serverless::Result<()> {
        Ok(())
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    transport: Arc<SqliteTransport>,
    snapshot: StoreSnapshot,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("remote.db"))
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        for statement in CREATE_TABLES {
            sqlx::query(*statement).execute(&pool).await.unwrap();
        }
        sqlx::query(schema::CREATE_STORE_META_SQL)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(super::ledger::CREATE_OP_LEDGER_SQL)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(LEGACY_GOALS_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql("ALTER TABLE threads ADD COLUMN cached_context TEXT;
            ALTER TABLE threads ADD COLUMN context_cache_epoch INTEGER NOT NULL DEFAULT 0;
            INSERT INTO peri_store_meta VALUES (0, 10, 'existing-store', 'peri.session.store/v2', 'original timestamp');
            INSERT INTO peri_op_ledger VALUES ('operation', 'append', 'digest', 'applied', 'receipt', 'original timestamp');
            INSERT INTO threads (id, cwd, created_at, updated_at, config, cached_context, context_cache_epoch) VALUES ('session', '/tmp', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', '{\"model\":\"keep\"}', 'old cache', 5);
            INSERT INTO thread_goals VALUES ('session', 'goal', 'old goal', 'active', NULL, 0, 0, 1, 1);
            INSERT INTO messages (rowid, message_id, thread_id, role, content, excluded, projection) VALUES (7, 'message', 'session', 'user', 'original content', 1, 'original projection')")
            .execute(&pool).await.unwrap();
        sqlx::raw_sql("INSERT INTO projects VALUES ('project', 'locator', 'identity');
            INSERT INTO workspaces VALUES ('workspace', 'project', '/tmp', 'root identity', 'discovery');
            INSERT INTO session_bindings VALUES ('session', 1, 'project', 'workspace', 'relative');
            INSERT INTO session_environments VALUES ('session', 'original machine');
            INSERT INTO mcp_oauth_credentials VALUES ('principal', 'original machine', 'server', 'credential bytes', 'timestamp');
            UPDATE threads SET frozen_context = 'frozen bytes', inherited_context = 'inherited bytes', snapshot_at_message_id = 'message', agent_status = 'done', hidden = 1, cancel_policy = 'detach', message_count = 1")
            .execute(&pool).await.unwrap();
        let transport = Arc::new(SqliteTransport {
            pool,
            writes: AtomicUsize::new(0),
            fail_column_drop: AtomicBool::new(false),
            change_schema: AtomicBool::new(false),
            drop_reply: AtomicBool::new(false),
            truncate_reply: AtomicBool::new(false),
        });
        let store = Self::store_for(&transport, StoreAccess::ReadWrite);
        let StoreIdentityRead::Present(snapshot) = store.read_identity().await.unwrap() else {
            panic!("missing fixture identity")
        };
        Self {
            _directory: directory,
            transport,
            snapshot,
        }
    }

    fn store_for(transport: &Arc<SqliteTransport>, access: StoreAccess) -> RemoteStore {
        let gate = Arc::new(ConnectionGate::default());
        RemoteStore::new(transport.clone(), access, gate.mint(), gate)
    }

    fn store(&self, access: StoreAccess) -> RemoteStore {
        Self::store_for(&self.transport, access)
    }

    async fn version(&self) -> i64 {
        sqlx::query_scalar("SELECT schema_version FROM peri_store_meta")
            .fetch_one(&self.transport.pool)
            .await
            .unwrap()
    }

    async fn assert_old_state(&self) {
        assert_eq!(self.version().await, 10);
        let (goal,): (String,) = sqlx::query_as("SELECT objective FROM thread_goals")
            .fetch_one(&self.transport.pool)
            .await
            .unwrap();
        assert_eq!(goal, "old goal");
        let (config, cache, epoch): (String, String, i64) =
            sqlx::query_as("SELECT config, cached_context, context_cache_epoch FROM threads")
                .fetch_one(&self.transport.pool)
                .await
                .unwrap();
        assert_eq!(
            (config.as_str(), cache.as_str(), epoch),
            (r#"{"model":"keep"}"#, "old cache", 5)
        );
    }
}

#[tokio::test]
async fn remote_schema_upgrade_preserves_identity_ledger_config_and_rowid_history() {
    let fixture = Fixture::new().await;
    let retained_sql = "SELECT 'threads', json_array(id, title, cwd, created_at, updated_at, message_count, parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, frozen_context, inherited_context, agent_status) FROM threads
        UNION ALL SELECT 'projects', json_array(id, locator, object_identity) FROM projects
        UNION ALL SELECT 'workspaces', json_array(id, project_id, root, root_identity, discovery) FROM workspaces
        UNION ALL SELECT 'bindings', json_array(thread_id, schema_version, project_id, workspace_id, relative_cwd) FROM session_bindings
        UNION ALL SELECT 'environments', json_array(thread_id, machine_id) FROM session_environments
        UNION ALL SELECT 'oauth', json_array(principal_id, machine_id, server_key, credentials_blob, updated_at) FROM mcp_oauth_credentials
        ORDER BY 1, 2";
    let before: Vec<(String, String)> = sqlx::query_as(retained_sql)
        .fetch_all(&fixture.transport.pool)
        .await
        .unwrap();
    let store = fixture.store(StoreAccess::ReadWrite);
    assert!(matches!(
        open_step(
            StoreIdentityRead::Present(fixture.snapshot.clone()),
            StoreAccess::ReadWrite
        )
        .unwrap(),
        OpenStep::Upgrade(_)
    ));
    schema_upgrade::upgrade(&store, &fixture.snapshot)
        .await
        .unwrap();
    assert_eq!(fixture.version().await, 11);
    let after: Vec<(String, String)> = sqlx::query_as(retained_sql)
        .fetch_all(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(after, before);
    let (identity, timestamp, config): (String, String, String) = sqlx::query_as(
        "SELECT store_id, peri_store_meta.created_at, config FROM peri_store_meta, threads",
    )
    .fetch_one(&fixture.transport.pool)
    .await
    .unwrap();
    assert_eq!(
        (identity.as_str(), timestamp.as_str(), config.as_str()),
        (
            "existing-store",
            "original timestamp",
            r#"{"model":"keep"}"#
        )
    );
    let (receipt,): (String,) = sqlx::query_as("SELECT receipt FROM peri_op_ledger")
        .fetch_one(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(receipt, "receipt");
    let history: (i64, String, bool, String) =
        sqlx::query_as("SELECT rowid, content, excluded, projection FROM messages")
            .fetch_one(&fixture.transport.pool)
            .await
            .unwrap();
    assert_eq!(
        history,
        (
            7,
            "original content".into(),
            true,
            "original projection".into()
        )
    );
    let (retired,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE name = 'thread_goals'")
            .fetch_one(&fixture.transport.pool)
            .await
            .unwrap();
    assert_eq!(retired, 0);
    let (columns,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pragma_table_xinfo('threads') WHERE name IN ('cached_context', 'context_cache_epoch')").fetch_one(&fixture.transport.pool).await.unwrap();
    assert_eq!(columns, 0);
    assert!(matches!(
        open_step(store.read_identity().await.unwrap(), StoreAccess::ReadWrite).unwrap(),
        OpenStep::Existing(_)
    ));
    assert_eq!(fixture.transport.writes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn remote_schema_upgrade_readonly_access_never_sends_a_mutation() {
    let fixture = Fixture::new().await;
    let reader = fixture.store(StoreAccess::ReadOnly);
    assert!(matches!(
        open_step(reader.read_identity().await.unwrap(), StoreAccess::ReadOnly).unwrap(),
        OpenStep::Existing(_)
    ));
    let error = schema_upgrade::upgrade(&reader, &fixture.snapshot)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    assert_eq!(fixture.transport.writes.load(Ordering::SeqCst), 0);
    fixture.assert_old_state().await;
}

#[tokio::test]
async fn remote_schema_upgrade_completes_an_identity_only_initialization_without_replacing_identity(
) {
    let fixture = Fixture::new().await;
    sqlx::raw_sql("DROP TABLE thread_goals; DROP TABLE messages; DROP TABLE threads; DROP TABLE session_bindings; DROP TABLE session_environments; DROP TABLE workspaces; DROP TABLE projects")
        .execute(&fixture.transport.pool).await.unwrap();
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();
    assert_eq!(fixture.version().await, 11);
    let (identity,): (String,) = sqlx::query_as("SELECT store_id FROM peri_store_meta")
        .fetch_one(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(identity, "existing-store");
    let (columns,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pragma_table_xinfo('threads')")
        .fetch_one(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(columns, 14);
}

#[tokio::test]
async fn remote_schema_upgrade_failed_drop_rolls_back_goals_columns_and_version() {
    let fixture = Fixture::new().await;
    fixture
        .transport
        .fail_column_drop
        .store(true, Ordering::SeqCst);
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap_err();
    fixture.assert_old_state().await;
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();
    assert_eq!(fixture.version().await, 11);
}

#[tokio::test]
async fn remote_schema_upgrade_detects_dependencies_created_after_its_snapshot() {
    let fixture = Fixture::new().await;
    fixture
        .transport
        .change_schema
        .store(true, Ordering::SeqCst);
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap_err();
    fixture.assert_old_state().await;
    let (extensions,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE name = 'late_extension'")
            .fetch_one(&fixture.transport.pool)
            .await
            .unwrap();
    assert_eq!(extensions, 1);
}

#[tokio::test]
async fn remote_schema_upgrade_rejects_unrecognized_goal_layout_before_writing() {
    let fixture = Fixture::new().await;
    sqlx::query("ALTER TABLE thread_goals ADD COLUMN unknown_data TEXT")
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    let error = schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Unsupported
    ));
    assert_eq!(fixture.transport.writes.load(Ordering::SeqCst), 0);
    fixture.assert_old_state().await;
}

#[tokio::test]
async fn remote_schema_upgrade_rejects_missing_history_columns_without_advancing_version() {
    let fixture = Fixture::new().await;
    sqlx::query("ALTER TABLE messages DROP COLUMN projection")
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    let error = schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Unsupported
    ));
    assert_eq!(fixture.transport.writes.load(Ordering::SeqCst), 0);
    fixture.assert_old_state().await;
}

#[tokio::test]
async fn remote_schema_upgrade_lost_or_incomplete_reply_is_uncertain_and_reopen_reads_committed_version(
) {
    for lost in [true, false] {
        let fixture = Fixture::new().await;
        if lost {
            fixture.transport.drop_reply.store(true, Ordering::SeqCst);
        } else {
            fixture
                .transport
                .truncate_reply
                .store(true, Ordering::SeqCst);
        }
        let error =
            schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
                .await
                .unwrap_err();
        assert!(matches!(
            error.kind(),
            SessionResourceErrorKind::PersistenceUncertain { .. }
        ));
        assert_eq!(fixture.version().await, 11);
        let reopened = fixture.store(StoreAccess::ReadWrite);
        assert!(matches!(
            open_step(
                reopened.read_identity().await.unwrap(),
                StoreAccess::ReadWrite
            )
            .unwrap(),
            OpenStep::Existing(_)
        ));
        assert_eq!(fixture.transport.writes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn remote_schema_upgrade_removes_recognized_execution_table_but_keeps_ledger() {
    let fixture = Fixture::new().await;
    sqlx::query(LEGACY_EXECUTION_SQL)
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO execution_runs VALUES ('session', 7, 0), ('orphan', 9, 0)")
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();
    assert_eq!(fixture.version().await, 11);
    let (retired,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('execution_runs', 'thread_goals')",
    )
    .fetch_one(&fixture.transport.pool)
    .await
    .unwrap();
    assert_eq!(retired, 0);
    let (receipt,): (String,) =
        sqlx::query_as("SELECT receipt FROM peri_op_ledger WHERE operation_id='operation'")
            .fetch_one(&fixture.transport.pool)
            .await
            .unwrap();
    assert_eq!(receipt, "receipt");
}

#[tokio::test]
async fn remote_schema_upgrade_refuses_unknown_execution_state_without_writing() {
    let fixture = Fixture::new().await;
    sqlx::query("CREATE TABLE execution_runs (thread_id TEXT PRIMARY KEY, generation INTEGER NOT NULL, clean BOOLEAN NOT NULL, extension_data TEXT)")
        .execute(&fixture.transport.pool).await.unwrap();
    let error = schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Unsupported
    ));
    assert_eq!(fixture.transport.writes.load(Ordering::SeqCst), 0);
    fixture.assert_old_state().await;
}

#[tokio::test]
async fn remote_schema_upgrade_rollback_restores_execution_rows_and_version() {
    let fixture = Fixture::new().await;
    sqlx::query(LEGACY_EXECUTION_SQL)
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO execution_runs VALUES ('session', 7, 0)")
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    fixture
        .transport
        .fail_column_drop
        .store(true, Ordering::SeqCst);
    assert!(
        schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
            .await
            .is_err()
    );
    fixture.assert_old_state().await;
    let execution: (String, i64, bool) =
        sqlx::query_as("SELECT thread_id, generation, clean FROM execution_runs")
            .fetch_one(&fixture.transport.pool)
            .await
            .unwrap();
    assert_eq!(execution, ("session".to_owned(), 7, false));
}
