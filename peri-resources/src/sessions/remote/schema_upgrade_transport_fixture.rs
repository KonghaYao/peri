// SQLite transport and fault injection for the remote migration fixtures.

pub(super) struct SqliteTransport {
    pub(super) pool: SqlitePool,
    pub(super) writes: AtomicUsize,
    pub(super) read_batches: AtomicUsize,
    fail_column_drop: AtomicBool,
    fail_recovery_drop: AtomicBool,
    change_schema: AtomicBool,
    /// 批次下发前的并发写入（测试用 SQL 文本）：模拟两批读取与批次之间别的连接改了库。
    ///
    /// 文本由测试在运行时拼（`String` 而不是 `&'static str`）：并发写入要引用夹具里
    /// 运行期才知道的 id 与 rowid。
    pub(super) before_batch: Mutex<Option<String>>,
    drop_reply: AtomicBool,
    truncate_reply: AtomicBool,
}

impl SqliteTransport {
    /// 干净夹具：只给一个真 SQLite 池，故障开关全部关闭。
    ///
    /// 给测试自己建库的同类（见 `session_history_page_test.rs`）复用同一份传输面，
    /// 不必知道故障开关的字段形状。
    pub(super) fn for_test(pool: SqlitePool) -> Self {
        Self {
            pool,
            writes: AtomicUsize::new(0),
            read_batches: AtomicUsize::new(0),
            fail_column_drop: AtomicBool::new(false),
            fail_recovery_drop: AtomicBool::new(false),
            change_schema: AtomicBool::new(false),
            before_batch: Mutex::new(None),
            drop_reply: AtomicBool::new(false),
            truncate_reply: AtomicBool::new(false),
        }
    }
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
    // 生产 SDK 把全部 `SQLITE_CONSTRAINT*` 折成 `Constraint`；夹具按同一口径映射，
    // 不能只认 UNIQUE/FK（批内守卫在单行表上以 NOT NULL 形式触发）。
    let constraint = error.as_database_error().is_some_and(|error| {
        matches!(
            error.kind(),
            sqlx::error::ErrorKind::UniqueViolation
                | sqlx::error::ErrorKind::ForeignKeyViolation
                | sqlx::error::ErrorKind::NotNullViolation
                | sqlx::error::ErrorKind::CheckViolation
                | sqlx::error::ErrorKind::ExclusionViolation
        )
    });
    if constraint {
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
            // 语句文本来自本文件与同模块测试的静态夹具，不含外部输入。
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
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
        let indexed_entries = statements.iter().enumerate();
        for (index, statement) in indexed_entries {
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
        self.read_batches.fetch_add(1, Ordering::SeqCst);
        let mut connection = self.pool.acquire().await.map_err(error_of)?;
        let mut transaction = connection
            .begin_with("BEGIN DEFERRED")
            .await
            .map_err(error_of)?;
        let mut results = Vec::new();
        for statement in statements {
            if statement.sql.contains("FROM pragma_table_xinfo") {
                return Err(SdkError::Error(
                    "v1 Hrana does not support table-valued pragma_table_xinfo".into(),
                ));
            }
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
