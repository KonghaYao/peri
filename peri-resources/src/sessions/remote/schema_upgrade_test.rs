//! 远端单条迁移（`v2` 契约 10|11 → 11|`v5`）的批次、守卫与拒绝面。
//!
//! 传输夹具是 SQLite 适配器扮演的「远端服务端」：外键关闭（远端服务端不强制外键），
//! 迁移作为**一个受管批次**逐条语句执行，失败整批回滚。夹具的输入库手写压缩前形状，
//! 不引用当前 `canonical` 的建表清单——升级实现改动时夹具不该跟着漂移。

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
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
use super::schema::{self, StoreIdentityRead, StoreShapeRead, StoreSnapshot};
use super::schema_upgrade;
use super::session_data::{open_step, probe_shape, OpenStep};
use super::sql::StatementSpec;
use crate::sessions::{
    canonical::{V10_CREATE_INDEXES, V10_CREATE_TABLES},
    schema_cleanup::{LEGACY_EXECUTION_SQL, LEGACY_GOALS_SQL},
};

/// 压缩前形状（契约 `v2`，版本 10|11）的登记行数据；契约标签是升级来源。
pub(super) const PREVIOUS_CONTRACT: &str = "peri.session.store/v2";
pub(super) const DISCOVERY: &str = r#"{"root":"/tmp","common_dir":null,"private_dir":null}"#;

pub(super) struct SqliteTransport {
    pub(super) pool: SqlitePool,
    pub(super) writes: AtomicUsize,
    fail_column_drop: AtomicBool,
    fail_recovery_drop: AtomicBool,
    change_schema: AtomicBool,
    /// 批次下发前的并发写入（测试用 SQL 字面量）：模拟两批读取与批次之间别的连接改了库。
    pub(super) before_batch: Mutex<Option<&'static str>>,
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
            sqlx::query("CREATE TABLE late_extension (id TEXT)")
                .execute(&self.pool)
                .await
                .map_err(error_of)?;
        }
        // 守卫不跨 await（否则这个 future 不是 `Send`）。
        let concurrent = self.before_batch.lock().unwrap().take();
        if let Some(sql) = concurrent {
            // 语句文本来自本文件的静态夹具，不含外部输入。
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
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
            let fail = (statement.sql == "ALTER TABLE threads DROP COLUMN cached_context"
                && self.fail_column_drop.swap(false, Ordering::SeqCst))
                || (statement.sql == "DROP TABLE IF EXISTS session_control_state"
                    && self.fail_recovery_drop.swap(false, Ordering::SeqCst));
            let result = if fail {
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

pub(super) struct Fixture {
    _directory: tempfile::TempDir,
    pub(super) transport: Arc<SqliteTransport>,
    pub(super) snapshot: StoreSnapshot,
}

impl Fixture {
    /// 压缩前契约的库：登记语义的 `workspaces`、执行环境表、machine 作用域凭证，
    /// 外加迁移要清掉的退役对象（`thread_goals`、`execution_runs`、旧缓存列）。
    pub(super) async fn new() -> Self {
        Self::new_with_foreign_keys(false).await
    }

    /// 连接级父行检查可打开的同款夹具：只给「`DROP TABLE` 的隐式删除真的会级联」这条失败
    /// 模式当观察点用。远端服务端的读数默认是 0，夹具默认也保持 0；这里打开它是在本地把
    /// 「归位失效」这一档变成可控实验，不是在声明远端会强制外键。
    pub(super) async fn new_with_foreign_keys(enforce: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("remote.db"))
            .create_if_missing(true)
            .foreign_keys(enforce);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        for statement in V10_CREATE_TABLES {
            sqlx::query(*statement).execute(&pool).await.unwrap();
        }
        for statement in V10_CREATE_INDEXES {
            sqlx::raw_sql(sqlx::AssertSqlSafe((*statement).to_owned()))
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
        sqlx::query(LEGACY_EXECUTION_SQL)
            .execute(&pool)
            .await
            .unwrap();
        // 语句文本来自本文件的静态夹具，不含外部输入。
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE threads ADD COLUMN cached_context TEXT;
            ALTER TABLE threads ADD COLUMN context_cache_epoch INTEGER NOT NULL DEFAULT 0;
            INSERT INTO peri_store_meta VALUES (0, 10, 'existing-store', '{PREVIOUS_CONTRACT}', 'original timestamp');
            INSERT INTO peri_op_ledger VALUES ('operation', 'append', 'digest', 'applied', 'receipt', 'original timestamp');
            INSERT INTO threads (id, cwd, created_at, updated_at, config, cached_context, context_cache_epoch) VALUES ('session', '/tmp', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', '{{\"model\":\"keep\"}}', 'old cache', 5);
            INSERT INTO thread_goals VALUES ('session', 'goal', 'old goal', 'active', NULL, 0, 0, 1, 1);
            INSERT INTO messages (rowid, message_id, thread_id, role, content, excluded, projection) VALUES (7, 'message', 'session', 'user', 'original content', 1, 'original projection');
            INSERT INTO execution_runs VALUES ('session', 7, 0);
            INSERT INTO projects VALUES ('project', 'locator', 'identity');
            INSERT INTO workspaces VALUES ('11111111-1111-4111-8111-111111111111', 'project', '/tmp', 'root identity', '{DISCOVERY}');
            INSERT INTO session_bindings VALUES ('session', 1, 'project', '11111111-1111-4111-8111-111111111111', '');
            INSERT INTO session_environments VALUES ('session', 'original machine');
            INSERT INTO mcp_oauth_credentials VALUES ('principal', 'original machine', 'server', 'credential bytes', 'timestamp');
            UPDATE threads SET frozen_context = 'frozen bytes', inherited_context = 'inherited bytes', snapshot_at_message_id = 'message', agent_status = 'done', hidden = 1, cancel_policy = 'detach', message_count = 1"
        )))
        .execute(&pool)
        .await
        .unwrap();
        let transport = Arc::new(SqliteTransport {
            pool,
            writes: AtomicUsize::new(0),
            fail_column_drop: AtomicBool::new(false),
            fail_recovery_drop: AtomicBool::new(false),
            change_schema: AtomicBool::new(false),
            before_batch: Mutex::new(None),
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

    pub(super) fn store(&self, access: StoreAccess) -> RemoteStore {
        Self::store_for(&self.transport, access)
    }

    /// 把夹具标成另一个代数与契约（构造拒绝面与 v11 输入）。
    pub(super) async fn declare(&self, version: i64, contract: &str) {
        sqlx::query("UPDATE peri_store_meta SET schema_version = ?1, contract = ?2")
            .bind(version)
            .bind(contract)
            .execute(&self.transport.pool)
            .await
            .unwrap();
    }

    pub(super) async fn version(&self) -> i64 {
        sqlx::query_scalar("SELECT schema_version FROM peri_store_meta")
            .fetch_one(&self.transport.pool)
            .await
            .unwrap()
    }

    pub(super) async fn contract(&self) -> String {
        sqlx::query_scalar("SELECT contract FROM peri_store_meta")
            .fetch_one(&self.transport.pool)
            .await
            .unwrap()
    }

    /// 计数断言（语句由调用点给出，不含外部输入）。
    pub(super) async fn count(&self, sql: String) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(&self.transport.pool)
            .await
            .unwrap()
    }

    /// 只读读回一份形状声明并判定：语句与判定都是生产打开路径用的那一份。
    pub(super) async fn shape_verdict(&self, probe: schema::ShapeProbe) -> StoreShapeRead {
        let rows: Vec<Vec<Value>> = sqlx::query(probe.sql())
            .fetch_all(&self.transport.pool)
            .await
            .unwrap()
            .iter()
            .map(values)
            .collect();
        let tables = schema::decode_column_map(&rows).unwrap();
        probe.check(&tables)
    }

    /// 升级前的旧事实：目标行、配置、旧缓存列都还在原位。
    pub(super) async fn assert_old_state(&self) {
        assert_eq!(self.version().await, 10);
        assert_eq!(self.contract().await, PREVIOUS_CONTRACT);
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

/// 生产探测路径（`probe_shape`）的结论：真语句 + 真身份读取，与打开路径同一处接线。
pub(super) async fn probed_shape(store: &RemoteStore) -> Option<StoreShapeRead> {
    let read = store.read_identity().await.unwrap();
    probe_shape(store, &read).await.unwrap()
}

/// 退役的表与列在升级后消失，其余对象与行原样保留；版本与契约一次推进；终点形状 == canonical。
async fn assert_upgraded(fixture: &Fixture) {
    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    // 升级终点必须逐列等于 canonical（生产判定入口判定，不看建表文本）。
    assert_eq!(
        fixture.shape_verdict(schema::ShapeProbe::Current).await,
        StoreShapeRead::Consistent,
        "升级终点的会话表形状必须逐列等于 canonical"
    );
    // `DROP TABLE` 把被重建表上的索引一并删掉：终点要带齐整份 canonical 索引集。
    for index in [
        "idx_messages_thread_id",
        "idx_bindings_project",
        "idx_bindings_workspace",
        "idx_threads_updated",
        "idx_threads_workspace_archived",
    ] {
        assert_eq!(
            fixture
                .count(format!(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = '{index}'"
                ))
                .await,
            1,
            "{index} 必须在升级终点存在"
        );
    }
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_environments'".to_owned())
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('execution_runs', 'thread_goals')".to_owned())
            .await,
        1,
        "退役的执行表清掉，业务扩展表 thread_goals 保留"
    );
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM pragma_table_xinfo('threads') WHERE name IN ('cached_context', 'context_cache_epoch')".to_owned())
            .await,
        0
    );
    // 结果守卫不误报：父行检查关闭时重建链不动历史，行数与搬到之前一致。
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM messages".to_owned())
            .await,
        1,
        "重建链没有清掉历史消息"
    );
}

#[tokio::test]
async fn upgrade_moves_a_released_store_to_the_current_generation_in_one_batch() {
    let fixture = Fixture::new().await;
    let retained_sql = "SELECT 'threads', json_array(id, title, cwd, created_at, updated_at, message_count, parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, frozen_context, inherited_context, agent_status) FROM threads
        UNION ALL SELECT 'projects', json_array(id, locator, object_identity) FROM projects
        UNION ALL SELECT 'bindings', json_array(thread_id, schema_version, project_id, relative_cwd) FROM session_bindings
        ORDER BY 1, 2";
    let before: Vec<(String, String)> = sqlx::query_as(retained_sql)
        .fetch_all(&fixture.transport.pool)
        .await
        .unwrap();
    let store = fixture.store(StoreAccess::ReadWrite);
    assert!(matches!(
        open_step(
            StoreIdentityRead::Present(fixture.snapshot.clone()),
            Some(fixture.shape_verdict(schema::ShapeProbe::Input).await),
            StoreAccess::ReadWrite
        )
        .unwrap(),
        OpenStep::Upgrade(_)
    ));

    schema_upgrade::upgrade(&store, &fixture.snapshot)
        .await
        .unwrap();

    assert_upgraded(&fixture).await;
    let after: Vec<(String, String)> = sqlx::query_as(retained_sql)
        .fetch_all(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(after, before);
    let workspace: (String, String, String, String, String) = sqlx::query_as(
        "SELECT machine_id, path, path_source, project_id, identity FROM workspaces",
    )
    .fetch_one(&fixture.transport.pool)
    .await
    .unwrap();
    assert_eq!(workspace.0, "original machine");
    assert_eq!(workspace.1, "/tmp");
    assert_eq!(workspace.2, "unverified");
    assert_eq!(workspace.3, "project");
    assert_eq!(workspace.4, "root identity");
    let binding: (String, String, String) = sqlx::query_as(
        "SELECT workspace_id, discovery_snapshot, evidence_origin FROM session_bindings",
    )
    .fetch_one(&fixture.transport.pool)
    .await
    .unwrap();
    assert_eq!(binding.0, "11111111-1111-4111-8111-111111111111");
    assert_eq!(binding.1, DISCOVERY);
    assert_eq!(binding.2, "legacy_last_observation");
    let thread: (String, i64) =
        sqlx::query_as("SELECT workspace_id, archived FROM threads WHERE id = 'session'")
            .fetch_one(&fixture.transport.pool)
            .await
            .unwrap();
    assert_eq!(thread.0, binding.0);
    assert_eq!(thread.1, 0);
    let machines: Vec<(String, String)> =
        sqlx::query_as("SELECT id, identity_kind FROM machines ORDER BY id")
            .fetch_all(&fixture.transport.pool)
            .await
            .unwrap();
    assert_eq!(machines.len(), 2);
    assert!(machines
        .iter()
        .any(|(id, kind)| id == "original machine" && kind == "legacy_unknown"));
    assert!(
        machines.iter().any(|(_, kind)| kind == "known"),
        "当前机器也要可查询"
    );
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM mcp_oauth_credentials".to_owned())
            .await,
        0,
        "machine 作用域凭证无法证明 Workspace 归属"
    );
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
    assert_eq!(
        fixture
            .count(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'session_close_intents'"
                    .to_owned()
            )
            .await,
        1
    );
    assert!(matches!(
        open_step(
            store.read_identity().await.unwrap(),
            probed_shape(&store).await,
            StoreAccess::ReadWrite
        )
        .unwrap(),
        OpenStep::Existing(_)
    ));
    assert_eq!(
        fixture.transport.writes.load(Ordering::SeqCst),
        1,
        "整段迁移是一个受管批次"
    );
}

/// 版本 11 的 `v2` 库（v4 内部用过、未正式发布）：形状就是搬运输入，同一条批次接纳。
#[tokio::test]
async fn upgrade_accepts_the_internal_version_11_shape() {
    let fixture = Fixture::new().await;
    fixture.declare(11, PREVIOUS_CONTRACT).await;
    let StoreIdentityRead::Present(snapshot) = fixture
        .store(StoreAccess::ReadWrite)
        .read_identity()
        .await
        .unwrap()
    else {
        panic!("missing identity")
    };
    assert!(matches!(
        open_step(
            StoreIdentityRead::Present(snapshot.clone()),
            Some(fixture.shape_verdict(schema::ShapeProbe::Input).await),
            StoreAccess::ReadWrite
        )
        .unwrap(),
        OpenStep::Upgrade(_)
    ));

    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &snapshot)
        .await
        .unwrap();

    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    assert_eq!(
        fixture.shape_verdict(schema::ShapeProbe::Current).await,
        StoreShapeRead::Consistent
    );
    assert_eq!(
        fixture
            .count(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'session_environments'".to_owned()
            )
            .await,
        0
    );
}

/// 只有身份与账本的空库（v2 契约但一张 canonical 表都没有）：直接建当前形状，身份不换。
#[tokio::test]
async fn upgrade_builds_the_current_shape_for_an_identity_only_store() {
    let fixture = Fixture::new().await;
    sqlx::raw_sql(
        "DROP TABLE thread_goals; DROP TABLE execution_runs; DROP TABLE messages; DROP TABLE threads;
         DROP TABLE session_bindings; DROP TABLE session_environments; DROP TABLE workspaces; DROP TABLE projects;
         DROP TABLE mcp_oauth_credentials",
    )
    .execute(&fixture.transport.pool)
    .await
    .unwrap();

    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();

    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    assert_eq!(
        fixture.shape_verdict(schema::ShapeProbe::Current).await,
        StoreShapeRead::Consistent,
        "空库分支建出的形状也要逐列等于 canonical"
    );
    let (identity,): (String,) = sqlx::query_as("SELECT store_id FROM peri_store_meta")
        .fetch_one(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(identity, "existing-store");
    let (columns,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pragma_table_xinfo('threads')")
        .fetch_one(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(columns, 16);
    for table in [
        "machines",
        "session_bindings",
        "mcp_oauth_credentials",
        "session_close_intents",
    ] {
        assert_eq!(
            fixture
                .count(format!(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
                ))
                .await,
            1,
            "{table}"
        );
    }
}

/// 只读访问不发写批次，旧状态原样。
#[tokio::test]
async fn upgrade_requires_a_writable_store() {
    let fixture = Fixture::new().await;
    let reader = fixture.store(StoreAccess::ReadOnly);
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

/// 历史表缺列：形状不是本构建认识的输入，拒绝且不发批次。
#[tokio::test]
async fn upgrade_rejects_missing_history_columns_without_writing() {
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

/// 拒绝面：未发布的中间代（12..19 每一个）、其他契约标签、更早的版本，一律不认识。
#[tokio::test]
async fn upgrade_refuses_generations_and_contracts_outside_the_compatible_one() {
    for (version, contract) in [
        (9, PREVIOUS_CONTRACT),
        (12, PREVIOUS_CONTRACT),
        (13, PREVIOUS_CONTRACT),
        (14, PREVIOUS_CONTRACT),
        (15, PREVIOUS_CONTRACT),
        (16, PREVIOUS_CONTRACT),
        (17, PREVIOUS_CONTRACT),
        (18, PREVIOUS_CONTRACT),
        (19, PREVIOUS_CONTRACT),
        (10, "peri.session.store/v3"),
        (11, "peri.session.store/v4"),
        (10, "unknown-contract"),
    ] {
        let fixture = Fixture::new().await;
        fixture.declare(version, contract).await;
        let StoreIdentityRead::Present(snapshot) = fixture
            .store(StoreAccess::ReadWrite)
            .read_identity()
            .await
            .unwrap()
        else {
            panic!("missing identity")
        };
        // 版本判定：12..19 都比本构建新，不认识就不迁移。
        if version > schema::REMOTE_SCHEMA_VERSION {
            assert_eq!(
                schema::acceptance(version),
                schema::SchemaAcceptance::TooNew,
                "({version}, {contract}) 必须判成 TooNew"
            );
            assert!(!snapshot.readable());
        }
        // 身份读取之后 `open_step` 就拒绝：连形状探测都不做（`None`）。
        for access in [StoreAccess::ReadOnly, StoreAccess::ReadWrite] {
            let refused =
                open_step(StoreIdentityRead::Present(snapshot.clone()), None, access).unwrap_err();
            assert!(
                matches!(refused.kind(), SessionResourceErrorKind::Unsupported),
                "({version}, {contract}, {access:?}) 必须在打开路径被拒绝"
            );
        }
        let error = schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &snapshot)
            .await
            .unwrap_err();
        assert!(
            matches!(error.kind(), SessionResourceErrorKind::Unsupported),
            "({version}, {contract}) 必须被拒绝"
        );
        assert_eq!(fixture.transport.writes.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.version().await, version);
        assert_eq!(fixture.contract().await, contract);
    }
}

/// 快照读完之后元数据被改写：批次开头的守卫让整批失败，库保持原样。
#[tokio::test]
async fn upgrade_guards_the_meta_snapshot_before_mutating() {
    for mutation in [
        "UPDATE peri_store_meta SET store_id = 'replacement-store'",
        "UPDATE peri_store_meta SET contract = 'unknown-contract'",
    ] {
        let fixture = Fixture::new().await;
        sqlx::query(mutation)
            .execute(&fixture.transport.pool)
            .await
            .unwrap();
        let before: (i64, String, String) =
            sqlx::query_as("SELECT schema_version, store_id, contract FROM peri_store_meta")
                .fetch_one(&fixture.transport.pool)
                .await
                .unwrap();
        assert!(
            schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
                .await
                .is_err()
        );
        let after: (i64, String, String) =
            sqlx::query_as("SELECT schema_version, store_id, contract FROM peri_store_meta")
                .fetch_one(&fixture.transport.pool)
                .await
                .unwrap();
        assert_eq!(before, after);
        assert_eq!(
            fixture
                .count("SELECT COUNT(*) FROM sqlite_master WHERE name = 'legacy_execution_registrations'".to_owned())
                .await,
            0
        );
    }
}

/// 快照读完之后多出一个对象：对象数守卫让整批失败，扩展对象留在库里。
#[tokio::test]
async fn upgrade_detects_objects_created_after_its_snapshot() {
    let fixture = Fixture::new().await;
    fixture
        .transport
        .change_schema
        .store(true, Ordering::SeqCst);
    assert!(
        schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
            .await
            .is_err()
    );
    fixture.assert_old_state().await;
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM sqlite_master WHERE name = 'late_extension'".to_owned())
            .await,
        1
    );
}

/// 退役对象形状不认识（多一列）：拒绝，不猜内容。
#[tokio::test]
async fn upgrade_refuses_unknown_retired_shapes_without_writing() {
    let fixture = Fixture::new().await;
    sqlx::query("ALTER TABLE execution_runs ADD COLUMN extension_data TEXT")
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

/// 批次中途失败：整批回滚，旧状态与版本保持，重试能成功。
#[tokio::test]
async fn upgrade_rolls_back_a_failed_batch_and_retries() {
    let fixture = Fixture::new().await;
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
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM execution_runs".to_owned())
            .await,
        1,
        "退役表在回滚后仍在"
    );

    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();

    assert_upgraded(&fixture).await;
}

/// 回复丢失或行数不完整：结论是「不确定」，但库已经提交；重新读身份看到的就是真实代数。
#[tokio::test]
async fn upgrade_reports_an_uncertain_reply_but_reopen_reads_the_committed_generation() {
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
        // 单条批次已经提交：库停在当前代数，重开的结论是「本构建直接可用」。
        assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
        assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
        let reopened = fixture.store(StoreAccess::ReadWrite);
        assert!(matches!(
            open_step(
                reopened.read_identity().await.unwrap(),
                probed_shape(&reopened).await,
                StoreAccess::ReadWrite
            )
            .unwrap(),
            OpenStep::Existing(_)
        ));
        assert_eq!(fixture.transport.writes.load(Ordering::SeqCst), 1);
    }
}

// Full remote application behavior is tested with the same SQL transport fixture.
mod full_remote_tests {
    include!("full_remote_test.rs");
}
