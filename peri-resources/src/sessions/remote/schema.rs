//! PostgreSQL store identity. Every object, the version marker and its shape
//! signature are committed together. Read-only opens only query the catalog.

use std::fmt;

use super::sql::{self, StatementSpec, Value};

pub(super) const REMOTE_SCHEMA_VERSION: i64 = 1;
pub(super) const STORE_CONTRACT: &str = "peri.session.postgres/v1";

const TABLE_EXISTS_SQL: &str = "SELECT c.relname::text FROM pg_catalog.pg_class c
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = current_schema() AND c.relname = 'peri_store_meta'";

const SELECT_META_SQL: &str =
    "SELECT schema_version, store_id, contract FROM peri_store_meta WHERE singleton = 0";

pub(super) const CREATE_STORE_META_SQL: &str = "CREATE TABLE IF NOT EXISTS peri_store_meta (
    singleton BIGINT PRIMARY KEY CHECK (singleton = 0),
    schema_version BIGINT NOT NULL,
    store_id TEXT NOT NULL,
    contract TEXT NOT NULL,
    created_at TEXT NOT NULL,
    shape_signature TEXT NOT NULL DEFAULT ''
)";

const INSERT_STORE_META_SQL: &str = "INSERT INTO peri_store_meta
    (singleton, schema_version, store_id, contract, created_at, shape_signature)
    VALUES (0, $1, $2, $3, $4, '')";

// This runs under the transaction's initialization lock. IF NOT EXISTS in
// shared canonical DDL may only see objects built by this very transaction.
// Keep the identity INSERT the only insert into peri_store_meta in this batch.
// HTTP proxies report the conflicting constraint but omit the statement index.
const REFUSE_COLLISIONS_SQL: &str = "UPDATE peri_store_meta SET schema_version = NULL
    WHERE singleton = 0 AND EXISTS (
        SELECT 1 FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = current_schema() AND c.relname <> 'peri_store_meta'
          AND c.relname::text = ANY(string_to_array($1, ','))
    )";

const RECORD_SHAPE_SQL: &str = concat!(
    "UPDATE peri_store_meta SET shape_signature = (",
    include_str!("schema_signature.sql"),
    ") WHERE singleton = 0"
);

pub(super) const VERIFY_SHAPE_SQL: &str = concat!(
    "SELECT (shape_signature = (",
    include_str!("schema_signature.sql"),
    "))::integer FROM peri_store_meta WHERE singleton = 0 AND store_id = $2"
);

#[derive(Clone, PartialEq, Eq)]
pub(super) struct StoreId(String);

impl StoreId {
    /// 首次初始化时铸造：随机、无外部输入、可安全记录（身份，不是凭证）。
    pub(super) fn mint() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StoreId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "StoreId({})", self.0)
    }
}

/// 已初始化的远端存储事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StoreSnapshot {
    pub(super) store_id: StoreId,
    pub(super) schema_version: i64,
    pub(super) contract: String,
}

impl StoreSnapshot {
    pub(super) fn matches_build(&self) -> bool {
        self.contract == STORE_CONTRACT && self.schema_version == REMOTE_SCHEMA_VERSION
    }
}
/// 只读身份读取的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum StoreIdentityRead {
    /// 本任务 schema 尚未初始化（表不存在）。
    Uninitialized,
    /// 已初始化。
    Present(StoreSnapshot),
    /// 表与行都在，但形状无法解释：不猜、不覆盖。
    Malformed,
}

/// 身份竞争的结论：**谁**在这次竞争里建立了身份。
///
/// 与 [`StoreIdentityRead`] 的区别是时点与主语：读取回答「库里现在有什么」，本类型回答
/// 「本次初始化自己做了什么」。竞败方读回的身份与胜者相同（唯一键决定只有一个身份），
/// 但结论是 [`Existing`](Self::Existing)——首次登记资格只属于胜者，不能靠读回结果推定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum StoreIdentityOutcome {
    /// 本事务确切插入了元数据行并提交：本次是胜者，这个身份由本次打开建立。
    Created(StoreId),
    /// 元数据唯一键已被占用：库里已有身份，本次没有建立任何东西。
    Existing(StoreId),
}

impl StoreIdentityOutcome {
    /// 权威身份：胜者是本次铸造值，败者与读已有库是读回的既有值。
    pub(super) fn store_id(&self) -> &StoreId {
        match self {
            Self::Created(store_id) | Self::Existing(store_id) => store_id,
        }
    }
}

pub(super) fn identity_read_plan() -> [StatementSpec; 2] {
    [
        StatementSpec::bare(TABLE_EXISTS_SQL),
        StatementSpec::bare(SELECT_META_SQL),
    ]
}

pub(super) fn interpret_identity_read(
    table_exists: bool,
    row: Option<&[Value]>,
) -> StoreIdentityRead {
    if !table_exists {
        return StoreIdentityRead::Uninitialized;
    }
    // Our creation transaction cannot leave an empty metadata table.
    match row.and_then(decode_identity) {
        Some(snapshot) => StoreIdentityRead::Present(snapshot),
        None => StoreIdentityRead::Malformed,
    }
}

pub(super) fn decode_identity(values: &[Value]) -> Option<StoreSnapshot> {
    let schema_version = sql::int_at(values, 0)?;
    let store_id = sql::text_at(values, 1)?;
    let contract = sql::text_at(values, 2)?;
    if store_id.is_empty() {
        return None;
    }
    Some(StoreSnapshot {
        store_id: StoreId(store_id.to_owned()),
        schema_version,
        contract: contract.to_owned(),
    })
}

// Writers acquire the transaction-scoped lock before executing this plan.
// The loser of an initialization race reaches the occupied singleton first;
// a different shape or a collision on a session table rolls back all new DDL.
pub(super) const META_INSERT_INDEX: usize = 1;

pub(super) fn inserted_meta_row(counts: &[u64]) -> bool {
    counts.get(META_INSERT_INDEX) == Some(&1)
}

pub(super) fn initialization_plan(store_id: &StoreId, now: &str) -> Vec<StatementSpec> {
    let reserved = Value::Text(super::session_schema::reserved_objects());
    let mut plan = vec![
        StatementSpec::bare(CREATE_STORE_META_SQL),
        StatementSpec::new(
            INSERT_STORE_META_SQL,
            vec![
                Value::Integer(REMOTE_SCHEMA_VERSION),
                Value::Text(store_id.as_str().to_owned()),
                Value::Text(STORE_CONTRACT.to_owned()),
                Value::Text(now.to_owned()),
            ],
        ),
        StatementSpec::new(REFUSE_COLLISIONS_SQL, vec![reserved.clone()]),
        StatementSpec::bare(super::ledger::CREATE_OP_LEDGER_SQL),
    ];
    plan.extend(super::session_schema::initialization_plan());
    plan.push(StatementSpec::new(RECORD_SHAPE_SQL, vec![reserved]));
    plan
}
