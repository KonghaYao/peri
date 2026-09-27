//! 身份初始化竞争的离线回归：**只有本事务确切插入元数据行的那一次才算创建**。
//!
//! 共享的云测试库不能重置（`peri_store_meta` 的身份是权威事实，重建它就等于伪造历史），
//! 所以竞争在**本地真引擎**上复现：语句、事务边界、受影响行数的含义、失败分类与竞争判定
//! 全部复用生产同一条代码（`schema::initialization_plan` / `classify_batch_failure` /
//! `initialization_evidence` / `open_step` / `open_verdict` / `existing_identity_from_read`），
//! 被替换的只有传输（本地 SQLite 而不是 SQL over HTTP）。
//!
//! | 场景 | 期望 |
//! | --- | --- |
//! | 两个全新安装面各自读到同一个空库 | 两边都是「未初始化」（竞争窗口成立） |
//! | 两边各自初始化 | 恰好一个 `Created`；败方读回胜者身份且结论是 `Existing` |
//! | 已有数据的库被再次打开 | `Existing`（与竞败同一个结论），不自动认领、不执行 |
//! | 批的结果未知／确定未生效／无插入证据 | 不产生创建事实，也不发身份 |
//!
//! 边界：云端共享库的竞争实验不在本文件内——那需要把库重置成空（禁止操作）。云端只覆盖
//! 「再次初始化返回既有身份」这一侧（`cloud_mutation_test.rs`，`#[ignore]` 显式执行）。

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteRow};
use sqlx::{Connection, Row};
use turso_serverless::{Error as SdkError, Value};

use peri_acp_types::session_resources::{
    MutationOutcome, SessionResourceErrorKind, SessionResourceResult,
};

use super::mutation::{
    classify_batch_failure, existing_identity_from_read, initialization_evidence, BatchFailure,
    InitializationEvidence, StoreAccess,
};
use super::schema::{
    identity_read_plan, initialization_plan, inserted_meta_row, interpret_identity_read, StoreId,
    StoreIdentityOutcome, StoreIdentityRead, StoreSnapshot, META_INSERT_INDEX,
    REMOTE_SCHEMA_VERSION, STORE_CONTRACT,
};
use super::session_data::{open_step, open_verdict, OpenStep, StoreInitialization};
use super::sql::StatementSpec;
use super::RemoteFailureClass;

// ── 真引擎 seam：传输换成本地 SQLite，语句与判定仍是生产那一条 ─────────────────────

/// 打开一条指向同一个库文件的连接（两个安装面对的是同一个物理存储）。
async fn connect(path: &Path) -> SqliteConnection {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    SqliteConnection::connect_with(&options)
        .await
        .expect("local engine connection")
}

/// 一条托管事务批：`BEGIN IMMEDIATE` → 逐条执行 → `COMMIT`，失败即整批回滚。
///
/// 对应 `RemoteStore::run_managed_batch`：受影响行数的含义相同（返回行的语句报 0，与 SDK
/// 一致），失败分类走同一个 [`classify_batch_failure`]。
async fn run_plan(
    conn: &mut SqliteConnection,
    plan: &[StatementSpec],
) -> Result<Vec<u64>, BatchFailure> {
    let mut counts = Vec::with_capacity(plan.len());
    let mut transaction = conn.begin_with("BEGIN IMMEDIATE").await.expect("begin");
    for (index, statement) in plan.iter().enumerate() {
        match execute(&mut transaction, statement).await {
            Ok(affected) => counts.push(affected),
            Err(error) => {
                // 托管批的自动回滚：回滚也失败时连类别都无从确定，只能判未决。
                if transaction.rollback().await.is_err() {
                    return Err(BatchFailure::Unknown {
                        class: RemoteFailureClass::Unknown,
                    });
                }
                return Err(batch_failure_of_local_engine(index, &error));
            }
        }
    }
    match transaction.commit().await {
        Ok(()) => Ok(counts),
        Err(_) => Err(BatchFailure::Unknown {
            class: RemoteFailureClass::Unknown,
        }),
    }
}

async fn execute(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    spec: &StatementSpec,
) -> Result<u64, sqlx::Error> {
    let query = with_params(sqlx::query(spec.sql), &spec.params);
    if spec.is_read_only() {
        query.fetch_all(&mut **transaction).await?;
        return Ok(0);
    }
    Ok(query.execute(&mut **transaction).await?.rows_affected())
}

/// 本地引擎的拒绝 → 生产同一条批失败分类。
///
/// 拒绝本身来自真实 SQLite（主键冲突就是它自己的结论），这里只把形状换成 SDK 的对应形态
/// （失败语句下标 + `Constraint`）再交给生产的 [`classify_batch_failure`]——判定不在测试里
/// 另立一套。
fn batch_failure_of_local_engine(index: usize, error: &sqlx::Error) -> BatchFailure {
    let unique_key = error
        .as_database_error()
        .is_some_and(|database| database.is_unique_violation());
    let inner = if unique_key {
        SdkError::Constraint("local engine rejected the unique key".to_owned())
    } else {
        SdkError::Error("local engine rejected the statement".to_owned())
    };
    classify_batch_failure(&SdkError::BatchStatementFailed {
        index,
        error: Box::new(inner),
        results: Vec::new(),
    })
}

/// 只读身份读取：生产同一条读取计划 + 同一处形状判定。
async fn read_identity(conn: &mut SqliteConnection) -> StoreIdentityRead {
    let plan = identity_read_plan();
    if fetch_rows(conn, &plan[0]).await.is_empty() {
        return interpret_identity_read(false, None);
    }
    let row = fetch_rows(conn, &plan[1]).await.into_iter().next();
    interpret_identity_read(true, row.as_deref())
}

/// 生产 `RemoteStore::initialize_store` 的引擎等价物：铸造身份 → 跑真实初始化 SQL →
/// 用同一处证据判定；没有建立身份时必须读回既有身份，不得拿铸造值冒充。
async fn initialize(
    conn: &mut SqliteConnection,
) -> SessionResourceResult<(StoreId, StoreIdentityOutcome)> {
    let minted = StoreId::mint();
    let plan = initialization_plan(&minted, "2026-09-26T00:00:00+00:00");
    let evidence = initialization_evidence(&run_plan(conn, &plan).await);
    let outcome = match evidence {
        InitializationEvidence::Created => StoreIdentityOutcome::Created(minted.clone()),
        // 没有建立身份：既有身份必须读回（与生产 `initialize_store` 同一条分支）。
        InitializationEvidence::Existing => {
            StoreIdentityOutcome::Existing(existing_identity_from_read(read_identity(conn).await)?)
        }
        // 确定未生效或无法证明：不发身份（与生产同一条分支）。
        InitializationEvidence::Failed(class) => return Err(class.into_session_resource_error()),
    };
    Ok((minted, outcome))
}

async fn fetch_rows(conn: &mut SqliteConnection, spec: &StatementSpec) -> Vec<Vec<Value>> {
    with_params(sqlx::query(spec.sql), &spec.params)
        .fetch_all(&mut *conn)
        .await
        .expect("local engine read")
        .iter()
        .map(decode_row)
        .collect()
}

fn with_params<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
    params: &'q [Value],
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments> {
    for value in params {
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

fn decode_row(row: &SqliteRow) -> Vec<Value> {
    (0..row.len())
        .map(|index| decode_cell(row, index))
        .collect()
}

/// 单元格 → SDK 的值（列顺序与类型原样，交给生产解码判定）。
fn decode_cell(row: &SqliteRow, index: usize) -> Value {
    use sqlx::{TypeInfo, ValueRef};
    let raw = row.try_get_raw(index).expect("cell readable");
    if raw.is_null() {
        return Value::Null;
    }
    match raw.type_info().name() {
        "INTEGER" => Value::Integer(row.get::<i64, _>(index)),
        "REAL" => Value::Real(row.get::<f64, _>(index)),
        "BLOB" => Value::Blob(row.get::<Vec<u8>, _>(index)),
        _ => Value::Text(row.get::<String, _>(index)),
    }
}

// ── 回归 ───────────────────────────────────────────────────────────────────────

/// 两个全新安装面对同一个空库竞争：只有胜者建立身份，败方读回的是胜者建立的权威身份。
///
/// 这是共享云库上无法复现的一段（重置别人的库等于伪造历史），所以在这里用真引擎跑；
/// 语句、事务边界与判定（`initialization_plan` / `open_step` / `open_verdict`）都是生产
/// 那一条，被替换的只有传输。
#[tokio::test]
async fn only_the_winner_of_the_identity_race_creates_the_identity() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let store_path = dir.path().join("store.db");

    // 竞争窗口的前半段：两个安装面各自读到同一个空库。
    let mut winner_conn = connect(&store_path).await;
    let mut loser_conn = connect(&store_path).await;
    assert_eq!(
        read_identity(&mut winner_conn).await,
        StoreIdentityRead::Uninitialized
    );
    assert_eq!(
        read_identity(&mut loser_conn).await,
        StoreIdentityRead::Uninitialized
    );
    // 空库上的只读打开在建任何东西之前就被拒绝。
    let refusal = open_step(StoreIdentityRead::Uninitialized, StoreAccess::ReadOnly)
        .expect_err("read-only open of an empty store is refused");
    assert!(matches!(
        refusal.kind(),
        SessionResourceErrorKind::Unsupported
    ));

    // 胜者：本事务确切插入了元数据行。
    let (winner_minted, winner) = initialize(&mut winner_conn)
        .await
        .expect("winner initializes");
    assert_eq!(winner, StoreIdentityOutcome::Created(winner_minted.clone()));

    // 败方：真实唯一键冲突，读回胜者身份——结论是既有身份，不是本次创建。
    let (loser_minted, loser) = initialize(&mut loser_conn)
        .await
        .expect("loser reads back the winner");
    assert_eq!(loser, StoreIdentityOutcome::Existing(winner_minted.clone()));

    let (winner_id, winner_initialization) = open_verdict(winner);
    let (loser_id, loser_initialization) = open_verdict(loser);
    assert_eq!(winner_id, winner_minted);
    assert_eq!(loser_id, winner_minted, "败方用的仍是胜者建立的权威身份");
    assert_ne!(
        loser_minted, winner_minted,
        "两个安装面各自铸造各自的候选身份"
    );
    assert_eq!(
        winner_initialization,
        StoreInitialization::CreatedByThisOpen
    );
    assert_eq!(loser_initialization, StoreInitialization::Existing);
}

/// 库已有身份（与竞败同一个结论）：后来者只读回既有身份，不自动认领、不重建。
#[tokio::test]
async fn a_store_that_already_has_an_identity_is_never_rebuilt() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let store_path = dir.path().join("store.db");

    let mut first = connect(&store_path).await;
    let (minted, outcome) = initialize(&mut first).await.expect("first initializes");
    assert_eq!(outcome, StoreIdentityOutcome::Created(minted.clone()));

    // 后来者读到的是已经存在的身份：本次打开没有建立任何东西。
    let mut later = connect(&store_path).await;
    let decision = open_step(read_identity(&mut later).await, StoreAccess::ReadWrite)
        .expect("existing store is readable");
    assert_eq!(decision, OpenStep::Existing(minted.clone()));
    let (id, initialization) = open_verdict(StoreIdentityOutcome::Existing(minted.clone()));
    assert_eq!(id, minted);
    assert_eq!(
        initialization,
        StoreInitialization::Existing,
        "已有身份只能得到 Existing：没有第二次创建"
    );

    // 元数据行没有被第二次写入：仍是第一次那一个身份。
    assert_eq!(
        read_identity(&mut later).await,
        StoreIdentityRead::Present(StoreSnapshot {
            store_id: minted,
            schema_version: REMOTE_SCHEMA_VERSION,
            contract: STORE_CONTRACT.to_owned(),
        }),
        "再次打开不得改写既有身份"
    );
}

/// 批的结果不足以证明插入时，一律不产生创建事实，也不发身份。
#[test]
fn unproven_results_never_create_an_identity() {
    // 丢响应、超时、回滚失败：本机无从证明本事务插入过元数据行。
    for class in [
        RemoteFailureClass::Transport,
        RemoteFailureClass::Timeout,
        RemoteFailureClass::Unknown,
    ] {
        assert_eq!(
            initialization_evidence(&Err(BatchFailure::Unknown { class })),
            InitializationEvidence::Failed(class)
        );
        // 生产路径把未知结果映射成领域失败（打开失败，不给身份、不给执行资格）；
        // `Failed` 不携带身份，也没有任何分支能从它发出身份。
        let error = class.into_session_resource_error();
        assert_eq!(error.effect(), MutationOutcome::NotApplied);
    }
    // 确定未生效：同样不发身份。
    assert_eq!(
        initialization_evidence(&Err(BatchFailure::NotApplied {
            class: RemoteFailureClass::Timeout,
            index: None,
        })),
        InitializationEvidence::Failed(RemoteFailureClass::Timeout)
    );
    // 批成功却没有插入证据（受影响 0 行）：不认领创建，只能读回既有身份。
    assert_eq!(
        initialization_evidence(&Ok(vec![0; META_INSERT_INDEX + 2])),
        InitializationEvidence::Existing
    );
    // 唯一键冲突：既有身份必须读回，不能拿本次铸造的候选值冒充。
    assert_eq!(
        initialization_evidence(&Err(BatchFailure::NotApplied {
            class: RemoteFailureClass::Constraint,
            index: Some(META_INSERT_INDEX),
        })),
        InitializationEvidence::Existing
    );
    // 创建的唯一证据是本事务在元数据 INSERT 上确切影响一行。
    let mut created = vec![0; META_INSERT_INDEX + 2];
    created[META_INSERT_INDEX] = 1;
    assert!(inserted_meta_row(&created));
    assert_eq!(
        initialization_evidence(&Ok(created)),
        InitializationEvidence::Created
    );
    assert!(!inserted_meta_row(&[0; META_INSERT_INDEX + 2]));
    assert!(
        !inserted_meta_row(&[1, 1, 0, 0]),
        "别的语句影响行数不算插入证据"
    );
}

/// 读不回既有身份时拒绝读回：不猜、不覆盖，也不把「读懂了但没有身份」当成空库可用。
#[test]
fn readback_refuses_identities_it_cannot_interpret() {
    assert!(existing_identity_from_read(StoreIdentityRead::Uninitialized).is_err());
    assert!(existing_identity_from_read(StoreIdentityRead::Malformed).is_err());
    let unrecognized = StoreSnapshot {
        store_id: StoreId::mint(),
        schema_version: REMOTE_SCHEMA_VERSION,
        contract: "peri.session.store/v1".to_owned(),
    };
    let error = existing_identity_from_read(StoreIdentityRead::Present(unrecognized))
        .expect_err("foreign contract is refused");
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Unsupported
    ));
    assert!(StoreSnapshot {
        store_id: StoreId::mint(),
        schema_version: REMOTE_SCHEMA_VERSION,
        contract: STORE_CONTRACT.to_owned(),
    }
    .matches_build());
}

/// seam 自检：身份读取的形状判定与生产读取共用一条规则（表不存在 / 表在无行 / 形状不符）。
#[test]
fn identity_read_shape_rules_are_shared() {
    assert_eq!(
        interpret_identity_read(false, None),
        StoreIdentityRead::Uninitialized
    );
    assert_eq!(
        interpret_identity_read(true, None),
        StoreIdentityRead::Uninitialized
    );
    assert_eq!(
        interpret_identity_read(true, Some(&[Value::Null, Value::Null, Value::Null])),
        StoreIdentityRead::Malformed
    );
    let store_id = StoreId::mint();
    let row = [
        Value::Integer(REMOTE_SCHEMA_VERSION),
        Value::Text(store_id.as_str().to_owned()),
        Value::Text(STORE_CONTRACT.to_owned()),
    ];
    assert_eq!(
        interpret_identity_read(true, Some(&row)),
        StoreIdentityRead::Present(StoreSnapshot {
            store_id,
            schema_version: REMOTE_SCHEMA_VERSION,
            contract: STORE_CONTRACT.to_owned(),
        })
    );
}
