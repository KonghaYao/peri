//! 远程连接：本 crate 内唯一接触 SDK 的地方。
//!
//! 只提供连接、只读事实与只读参数绑定回环。写路径（新建/追加/compact/fork/删除）
//! 以及 C §5.1 的 P1–P7 前置条件未实测前不在这里出现——没有半成品 mutation，
//! 也没有「先连上再假装能写」的中间态。
//!
//! 请求有界：SDK 0.1.3 公开面不提供客户端超时配置，因此本层用 `tokio::time::timeout`
//! 兜住上界；超时返回 `Timeout`，不改变远端结果的分类（§7）。

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::session_resources::SessionResourceError;
use turso_serverless::{BatchStatement, Builder, Connection, TransactionBehavior, Value};

use super::credentials::SessionStoreCredential;
use super::endpoint::RemoteEndpoint;
use super::failure::{self, RemoteFailureClass};
use super::sql::StatementSpec;

/// 单次远程调用的时间预算；正式预算由 C-01/F 的测量确定。
pub(super) const REQUEST_BUDGET: Duration = Duration::from_secs(20);

/// 预算内的结果：保留 SDK 原始错误，供上层按结构分类（mutation 必须区分
/// 「整批回滚」与「回滚失败」，折叠成类别会丢掉确定性信息）。
pub(super) enum Budgeted<T> {
    Done(T),
    Failed(turso_serverless::Error),
    Exceeded,
}

/// 预算内执行 SDK 调用；超时归 `Exceeded`，不推断远端是否生效。
pub(super) async fn within_budget<T>(
    future: impl Future<Output = turso_serverless::Result<T>>,
    budget: Duration,
) -> Budgeted<T> {
    match crate::sessions::timeout::timeout(budget, future).await {
        Ok(Ok(value)) => Budgeted::Done(value),
        Ok(Err(error)) => Budgeted::Failed(error),
        Err(_) => Budgeted::Exceeded,
    }
}

/// 建立 SDK 连接：URL 由端点定死，凭证只在此处进入 SDK 调用。
pub(super) async fn connect_sdk(
    endpoint: &RemoteEndpoint,
    credential: &SessionStoreCredential,
) -> Result<Connection, SessionResourceError> {
    let builder = Builder::new_remote(endpoint.sdk_url()).with_auth_token(credential.expose());
    let database = bounded(builder.build()).await.map_err(into_error)?;
    database
        .connect()
        .map_err(|error| failure::classify(&error).into_session_resource_error())
}

/// 参数绑定回环事实：每个布尔都是「绑定的哨兵原值读回」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BindRoundtrip {
    pub text_ok: bool,
    pub int64_ok: bool,
    pub null_ok: bool,
}

/// 只读引擎事实（不写库、不建对象）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineReadFacts {
    /// 引擎报告的 SQLite 方言版本；引擎不支持该函数时为 `None`。
    pub sqlite_version: Option<String>,
}

/// 一条已确认的远程连接；不暴露底层 SDK 类型。
pub(crate) struct RemoteConnection {
    connection: Connection,
}

impl RemoteConnection {
    /// 连接：引擎/URL 已由 [`RemoteEndpoint`] 定死，凭证只在此处进入 SDK 调用。
    pub(crate) async fn connect(
        endpoint: &RemoteEndpoint,
        credential: &SessionStoreCredential,
    ) -> Result<Self, SessionResourceError> {
        let connection = connect_sdk(endpoint, credential).await?;
        Ok(Self { connection })
    }

    /// 只读参数绑定回环：绑定 text / 64 位整数 / NULL 并原值读回，不做任何写入。
    pub(crate) async fn bind_roundtrip(
        &self,
        sentinel_text: &str,
        sentinel_int: i64,
    ) -> Result<BindRoundtrip, SessionResourceError> {
        let mut rows = bounded(self.connection.query(
            "SELECT ? AS probe_text, ? AS probe_int, ? AS probe_null",
            (sentinel_text, sentinel_int, Option::<String>::None),
        ))
        .await
        .map_err(into_error)?;
        let row = bounded(rows.next())
            .await
            .map_err(into_error)?
            .ok_or_else(|| into_error(RemoteFailureClass::NotFound))?;
        Ok(BindRoundtrip {
            text_ok: matches!(read(&row, 0)?, Value::Text(text) if text == sentinel_text),
            int64_ok: matches!(read(&row, 1)?, Value::Integer(value) if value == sentinel_int),
            null_ok: matches!(read(&row, 2)?, Value::Null),
        })
    }

    /// 只读引擎事实：`sqlite_version()`。函数不被引擎支持时返回 `None`
    /// （不把「不支持」伪装成版本号，也不因此判连接失败）；其余失败按原分类上报。
    pub(crate) async fn engine_read_facts(&self) -> Result<EngineReadFacts, SessionResourceError> {
        let mut rows =
            match bounded(self.connection.query("SELECT sqlite_version() AS v", ())).await {
                Ok(rows) => rows,
                Err(RemoteFailureClass::Unsupported) => {
                    return Ok(EngineReadFacts {
                        sqlite_version: None,
                    });
                }
                Err(class) => return Err(into_error(class)),
            };
        let sqlite_version = match bounded(rows.next()).await.map_err(into_error)? {
            Some(row) => match read(&row, 0)? {
                Value::Text(text) => Some(text),
                _ => None,
            },
            None => None,
        };
        Ok(EngineReadFacts { sqlite_version })
    }

    /// 关闭连接：非 `Ok` 按分类上报，不静默吞掉。
    ///
    /// 事实边界（`turso_serverless` 0.1.3 源码）：`Connection::close` 恒返回 `Ok(())`——
    /// 它只复位本地会话流，远端 `StreamRequest::Close` 的错误被显式忽略。因此成功只证明
    /// **本机传输面**走完了关闭，**不**证明服务端连接/流已释放，也不证明任何未知的远端写
    /// 没有执行（后者由持久化未决锚点与恢复回答）。
    pub(crate) async fn close(self) -> Result<(), SessionResourceError> {
        bounded(self.connection.close()).await.map_err(into_error)
    }
}

fn read(row: &turso_serverless::Row, index: usize) -> Result<Value, SessionResourceError> {
    row.get_value(index)
        .map_err(|error| failure::classify(&error).into_session_resource_error())
}

/// 一条连接上可用的传输面：**本 crate 唯一真正调用 SDK 语句入口的地方**。
///
/// 结果在传输边界就解码成值矩阵（行、每段结果集、受影响行数），上层只处理确定性与事务
/// 语义，不再接触 SDK 类型。这条边界也是故障注入的落点：假传输能在本地确定地复现
/// 「在途请求被丢弃之后，这条连接不再可用」，而不必联网，也不必改动上层的判定逻辑。
///
/// 失败按 SDK 原样返回（**不**折叠成类别）：mutation 必须区分「整批回滚」与「回滚失败」，
/// 折叠会丢掉确定性信息。
#[async_trait]
pub(super) trait RemoteTransport: Send + Sync {
    /// 单条语句读取：返回全部行（每行按列序原样）。
    async fn sql_values(&self, spec: &StatementSpec) -> turso_serverless::Result<Vec<Vec<Value>>>;

    /// 托管事务批（`BEGIN IMMEDIATE` … `COMMIT`）：返回每条语句的受影响行数。
    async fn managed_batch(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<u64>>;

    /// 只读一致读（同一请求内的 `BEGIN DEFERRED` … `COMMIT`）：返回每段结果集的行。
    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<Vec<Vec<Value>>>>;

    /// 连接当前是否处于自动提交。这是驱动侧缓存的事实，不做服务端往返。
    fn is_autocommit(&self) -> turso_serverless::Result<bool>;

    /// 关闭连接：非 `Ok` 按分类上报，不静默吞掉。
    ///
    /// 生产实现下 `Ok` 只代表本地传输面走完了关闭（SDK 会吞掉远端关闭错误，见
    /// `RemoteConnection::close` 的说明），上层不得把它当作服务端资源已释放的证据。
    async fn close(&self) -> turso_serverless::Result<()>;
}

/// 生产传输：一条已确认的 SDK 连接。
/// Emscripten 上的 HTTP Future 也必须在调用方的 Tokio reactor 中被 poll；
/// 脱离它另用 JS event loop 驱动会在 socket 建连时因缺少 reactor 而 panic。
pub(super) struct SdkTransport {
    connection: Connection,
}

impl SdkTransport {
    pub(super) fn new(connection: Connection) -> Self {
        Self { connection }
    }
}

#[async_trait]
impl RemoteTransport for SdkTransport {
    async fn sql_values(&self, spec: &StatementSpec) -> turso_serverless::Result<Vec<Vec<Value>>> {
        collect_values(&self.connection, spec).await
    }

    async fn managed_batch(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<u64>> {
        execute_batch(&self.connection, statements, TransactionBehavior::Immediate).await
    }

    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<Vec<Vec<Value>>>> {
        read_batch(&self.connection, statements).await
    }

    fn is_autocommit(&self) -> turso_serverless::Result<bool> {
        self.connection.is_autocommit()
    }

    async fn close(&self) -> turso_serverless::Result<()> {
        self.connection.close().await
    }
}

async fn collect_values(
    connection: &Connection,
    spec: &StatementSpec,
) -> turso_serverless::Result<Vec<Vec<Value>>> {
    let mut rows = connection.query(spec.sql, spec.params.clone()).await?;
    let mut collected = Vec::new();
    while let Some(row) = rows.next().await? {
        collected.push(columns_of(&row)?);
    }
    Ok(collected)
}

async fn execute_batch(
    connection: &Connection,
    statements: Vec<StatementSpec>,
    behavior: TransactionBehavior,
) -> turso_serverless::Result<Vec<u64>> {
    let results = connection
        .transactional_batch(build_batch(statements)?, behavior)
        .await?;
    Ok(results
        .iter()
        .map(|result| result.rows_affected())
        .collect())
}

async fn read_batch(
    connection: &Connection,
    statements: Vec<StatementSpec>,
) -> turso_serverless::Result<Vec<Vec<Vec<Value>>>> {
    let results = connection
        .transactional_batch(build_batch(statements)?, TransactionBehavior::Deferred)
        .await?;
    results
        .iter()
        .map(|result| {
            result
                .rows()
                .iter()
                .map(columns_of)
                .collect::<turso_serverless::Result<Vec<_>>>()
        })
        .collect()
}

/// 静态 SQL + 位置绑定参数 → SDK 批语句。
fn build_batch(statements: Vec<StatementSpec>) -> turso_serverless::Result<Vec<BatchStatement>> {
    statements
        .into_iter()
        .map(|spec| BatchStatement::new(spec.sql, spec.params))
        .collect()
}

/// 一行 → 值向量（列顺序原样保留）。
fn columns_of(row: &turso_serverless::Row) -> turso_serverless::Result<Vec<Value>> {
    (0..row.column_count())
        .map(|index| row.get_value(index))
        .collect()
}

fn into_error(class: RemoteFailureClass) -> SessionResourceError {
    class.into_session_resource_error()
}

/// 预算内执行 SDK 调用；超时归 `Timeout`，不推断远端是否生效。
///
/// 只读路径用这个折叠版本；mutation 路径必须用 [`within_budget`] 保留原始错误结构
/// （见 `mutation::classify_batch_failure`）。
async fn bounded<T>(
    future: impl Future<Output = turso_serverless::Result<T>>,
) -> Result<T, RemoteFailureClass> {
    match within_budget(future, REQUEST_BUDGET).await {
        Budgeted::Done(value) => Ok(value),
        Budgeted::Failed(sdk_error) => Err(failure::classify(&sdk_error)),
        Budgeted::Exceeded => Err(RemoteFailureClass::Timeout),
    }
}
