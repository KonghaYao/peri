use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use peri_acp_types::session_resources::{CloseSettlement, SessionResourceErrorKind};
use sqlx::{
    sqlite::{SqliteArguments, SqlitePoolOptions, SqliteRow},
    Connection, Row, Sqlite, SqlitePool, TypeInfo, ValueRef,
};
use turso_serverless::{Error as SdkError, Value};

use super::connection::RemoteTransport;
use super::generation::ConnectionGate;
use super::mutation::{RemoteStore, StoreAccess};
use super::schema::{self, StoreIdentityRead, StoreSnapshot};
use super::session_data::open_step;
use super::sql::StatementSpec;
use crate::sessions::{
    canonical::CREATE_TABLES,
    schema_cleanup::{LEGACY_EXECUTION_SQL, LEGACY_GOALS_SQL},
};

#[tokio::test]
async fn remote_v13_refusal_preserves_owner_tables_and_session_facts() {
    let fixture = Fixture::legacy_v2(13).await;
    fixture.transport.drop_reply.store(true, Ordering::SeqCst);
    fixture.assert_refused().await;
}

#[tokio::test]
async fn remote_v13_refusal_never_sends_writes_even_with_lost_reply() {
    let fixture = Fixture::legacy_v2(13).await;
    fixture.transport.drop_reply.store(true, Ordering::SeqCst);
    fixture.assert_refused().await;
}

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

#[tokio::test]
async fn v11_remote_refusal_preserves_history_and_machine_credentials() {
    let fixture = Fixture::new().await;
    sqlx::raw_sql("DROP TABLE thread_goals; ALTER TABLE threads DROP COLUMN cached_context; ALTER TABLE threads DROP COLUMN context_cache_epoch; UPDATE peri_store_meta SET schema_version=11").execute(&fixture.transport.pool).await.unwrap();
    fixture.assert_refused().await;
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("remote.db"))
            .create_if_missing(true)
            .foreign_keys(false);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        for statement in CREATE_TABLES {
            sqlx::query(
                if statement.contains("CREATE TABLE IF NOT EXISTS messages") {
                    LEGACY_MESSAGES_SQL
                } else {
                    *statement
                },
            )
            .execute(&pool)
            .await
            .unwrap();
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
            INSERT INTO workspaces VALUES ('11111111-1111-4111-8111-111111111111', 'project', '/tmp', 'root identity', '{\"root\":\"/tmp\",\"common_dir\":null,\"private_dir\":null}');
            INSERT INTO session_bindings VALUES ('session', 1, 'project', '11111111-1111-4111-8111-111111111111', '');
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
async fn remote_legacy_refusal_preserves_identity_ledger_config_and_rowid_history() {
    let fixture = Fixture::new().await;
    fixture.assert_refused().await;
    fixture.assert_old_state().await;
}

#[tokio::test]
async fn remote_legacy_readonly_refusal_never_sends_a_mutation() {
    let fixture = Fixture::new().await;
    fixture.assert_refused().await;
    fixture.assert_old_state().await;
}

#[tokio::test]
async fn remote_legacy_identity_only_store_is_refused_without_reinitialization() {
    let fixture = Fixture::new().await;
    sqlx::raw_sql("DROP TABLE thread_goals; DROP TABLE messages; DROP TABLE threads; DROP TABLE session_bindings; DROP TABLE session_environments; DROP TABLE workspaces; DROP TABLE projects")
        .execute(&fixture.transport.pool).await.unwrap();
    fixture.assert_refused().await;
}

#[tokio::test]
async fn remote_legacy_refusal_never_attempts_column_drop() {
    let fixture = Fixture::new().await;
    fixture
        .transport
        .fail_column_drop
        .store(true, Ordering::SeqCst);
    fixture.assert_refused().await;
    fixture.assert_old_state().await;
}

#[tokio::test]
async fn remote_legacy_refusal_never_attempts_schema_mutation() {
    let fixture = Fixture::new().await;
    fixture
        .transport
        .change_schema
        .store(true, Ordering::SeqCst);
    fixture.assert_refused().await;
    fixture.assert_old_state().await;
}

#[tokio::test]
async fn remote_schema_upgrade_rejects_unrecognized_goal_layout_before_writing() {
    let fixture = Fixture::new().await;
    sqlx::query("ALTER TABLE thread_goals ADD COLUMN unknown_data TEXT")
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    fixture.assert_refused().await;
}

#[tokio::test]
async fn remote_schema_upgrade_rejects_missing_history_columns_without_advancing_version() {
    let fixture = Fixture::new().await;
    sqlx::query("ALTER TABLE messages DROP COLUMN projection")
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    fixture.assert_refused().await;
}

#[tokio::test]
async fn remote_legacy_refusal_precedes_lost_or_incomplete_write_reply() {
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
        fixture.assert_refused().await;
        fixture.assert_old_state().await;
    }
}

#[tokio::test]
async fn remote_legacy_refusal_preserves_recognized_execution_rows_and_ledger() {
    let fixture = Fixture::new().await;
    sqlx::query(LEGACY_EXECUTION_SQL)
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO execution_runs VALUES ('session', 7, 0), ('orphan', 9, 0)")
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    fixture.assert_refused().await;
    fixture.assert_old_state().await;
}

#[tokio::test]
async fn remote_schema_upgrade_refuses_unknown_execution_state_without_writing() {
    let fixture = Fixture::new().await;
    sqlx::query("CREATE TABLE execution_runs (thread_id TEXT PRIMARY KEY, generation INTEGER NOT NULL, clean BOOLEAN NOT NULL, extension_data TEXT)")
        .execute(&fixture.transport.pool).await.unwrap();
    fixture.assert_refused().await;
}

#[tokio::test]
async fn remote_legacy_refusal_preserves_execution_rows_and_version() {
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
    fixture.assert_refused().await;
    fixture.assert_old_state().await;
}

// Full remote application behavior is tested with the same SQL transport fixture.
mod full_remote_tests {
    include!("full_remote_test.rs");
}

const LEGACY_MESSAGES_SQL: &str = r#"CREATE TABLE IF NOT EXISTS messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT
)"#;
const LEGACY_V2_SQL: &str = r#"CREATE TABLE IF NOT EXISTS machines (
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
);"#;

async fn database_evidence(pool: &SqlitePool) -> Vec<(String, Vec<String>)> {
    let objects: Vec<String> = sqlx::query_scalar("SELECT json_array(type,name,tbl_name,sql) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").fetch_all(pool).await.unwrap();
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(pool).await.unwrap();
    let mut evidence = vec![("schema".into(), objects)];
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
                .bind(&table)
                .fetch_all(pool)
                .await
                .unwrap();
        let expressions = columns
            .iter()
            .map(|column| format!("quote(\"{}\")", column.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" || ',' || ");
        let statement = format!(
            "SELECT {expressions} FROM \"{}\" ORDER BY rowid",
            table.replace('"', "\"\"")
        );
        let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
            .fetch_all(pool)
            .await
            .unwrap();
        evidence.push((table, rows));
    }
    evidence
}

impl Fixture {
    async fn assert_refused(&self) {
        let before = database_evidence(&self.transport.pool).await;
        for access in [StoreAccess::ReadOnly, StoreAccess::ReadWrite] {
            let store = self.store(access);
            let error = open_step(store.read_identity().await.unwrap(), access).unwrap_err();
            assert!(matches!(
                error.kind(),
                SessionResourceErrorKind::Unsupported
            ));
        }
        assert_eq!(self.transport.writes.load(Ordering::SeqCst), 0);
        assert_eq!(database_evidence(&self.transport.pool).await, before);
    }

    async fn legacy_v2(version: i64) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(directory.path().join("remote.db"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::raw_sql(LEGACY_V2_SQL).execute(&pool).await.unwrap();
        sqlx::query(schema::CREATE_STORE_META_SQL)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(super::ledger::CREATE_OP_LEDGER_SQL)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO peri_store_meta VALUES (0,?1,'legacy-store','peri.session.store/v3','original timestamp')").bind(version).execute(&pool).await.unwrap();
        sqlx::raw_sql("INSERT INTO machines VALUES ('machine','legacy','known');
            INSERT INTO workspaces VALUES ('workspace','machine','/old','unverified');
            INSERT INTO threads(id,cwd,created_at,updated_at,workspace_id,frozen_context) VALUES ('session','/old','now','now','workspace','frozen bytes');
            INSERT INTO messages(message_id,thread_id,role,content,projection) VALUES ('message','session','user','original content','original projection');
            INSERT INTO session_close_intents VALUES ('session','original close timestamp');").execute(&pool).await.unwrap();
        if version == 13 {
            sqlx::raw_sql("CREATE TABLE session_execution_owners(root_id TEXT PRIMARY KEY REFERENCES threads(id),epoch INTEGER NOT NULL,nonce TEXT NOT NULL,expires_at_unix INTEGER NOT NULL,released INTEGER NOT NULL);
                CREATE TABLE session_execution_workspace_descriptors(root_id TEXT PRIMARY KEY REFERENCES session_execution_owners(root_id),owner_epoch INTEGER NOT NULL,endpoint TEXT NOT NULL,key_identity TEXT NOT NULL,agent_generation_id TEXT NOT NULL,unsupported_async_owners INTEGER NOT NULL);
                INSERT INTO session_execution_owners VALUES ('session',7,'retired',0,0);
                INSERT INTO session_execution_workspace_descriptors VALUES ('session',7,'retired','retired','retired',0);").execute(&pool).await.unwrap();
        }
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
            panic!("missing legacy identity")
        };
        Self {
            _directory: directory,
            transport,
            snapshot,
        }
    }

    async fn fresh() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(directory.path().join("remote.db"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        for statement in super::session_schema::initialization_plan() {
            sqlx::query(statement.sql).execute(&pool).await.unwrap();
        }
        sqlx::query(super::ledger::CREATE_OP_LEDGER_SQL)
            .execute(&pool)
            .await
            .unwrap();
        let store_id = schema::StoreId::mint();
        for statement in schema::initialization_plan(&store_id, "now") {
            query(&statement).execute(&pool).await.unwrap();
        }
        sqlx::raw_sql("INSERT INTO machines VALUES ('machine','current','known'); INSERT INTO workspaces VALUES ('workspace','machine','/current','discovered'); INSERT INTO threads(id,cwd,created_at,updated_at,workspace_id) VALUES ('session','/current','now','now','workspace')").execute(&pool).await.unwrap();
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
            panic!("missing fresh identity")
        };
        Self {
            _directory: directory,
            transport,
            snapshot,
        }
    }
}
