//! 远程版本标记、store 身份与只读检查（C §4、§6）。
//!
//! 「两个存储模式一致」之后，远端会话表**就是**本地形状（`sessions::canonical` 是那份 DDL
//! 的唯一来源），所以版本号也同源：`REMOTE_SCHEMA_VERSION` 直接取本机的
//! `CURRENT_SCHEMA_VERSION`。两者仍有一处执行器差异，且只有这一处——远端写不了
//! `PRAGMA user_version`（服务端直接拒绝，实测见母 issue §9.8 探测项 5b），版本标记只能落在
//! 普通表的单行事实里（[`STORE_META_TABLE`]）。差异是**载体**，不是版本代数。
//!
//! 契约标签 [`STORE_CONTRACT`] 在统一时推进到 `v2`：形状变了（表名、列名、绑定所在表、
//! 历史顺序的载体），拿着 v1 标签的库会被 [`acceptance`] / `matches_build` 判为不认识——
//! 这是有意的 fail-closed。旧形状的库**不迁移、不覆盖**（新库无历史数据，迁移路径没有被
//! 需求），具体拒绝点见 `session_data` 的旧形状探测。
//!
//! 三条硬规则：
//!
//! - **读不写**：身份/版本读取只有 SELECT（`identity_read_plan` 可离线断言）。
//! - **未知不覆盖**：版本高于本构建、契约不符或形状不可解释时一律拒绝，不做 DDL、
//!   不猜列形状；初始化只在「确定不存在」时创建，已存在时读回而不改写。
//! - **唯一身份**：`store_id` 由首次初始化竞争产生（元数据行主键），失败者读胜者，
//!   不各造一个 StoreId；竞争结论是**结构化的**（[`StoreIdentityOutcome`]：胜者 `Created`、
//!   败者与读已有库同为 `Existing`），首次登记资格只跟 `Created` 走，不由「打开前看到空库」
//!   或「读回里现在有身份」推导。只有本事务确切插入元数据行并提交才算创建——结果未知
//!   （丢响应、超时）不产生创建事实。

use std::fmt;

use turso_serverless::Value;

use super::sql::{int_at, text_at, StatementSpec};

/// 本构建写入并接受的远端 schema 版本：**与本机 `CURRENT_SCHEMA_VERSION` 同一个常量**。
///
/// 两端形状相同，版本就不该各自一份；任何一侧推进版本，另一侧跟着走。
pub(super) const REMOTE_SCHEMA_VERSION: i64 = crate::sessions::sqlite_store::CURRENT_SCHEMA_VERSION;

/// 远程存储契约标签：形状 + 语义代数，和版本一起决定「这是不是我们认识的那个库」。
///
/// `v2` = 远端会话表改为 canonical 形状（统一前是 `peri_sessions` 的混合形状）。
pub(super) const STORE_CONTRACT: &str = "peri.session.store/v2";

/// 统一之前的远端会话表：出现它们说明这是一个**旧形状的库**（不是空库）。
///
/// 只在「写打开 + 尚未初始化」这条路径上探测一次；命中即拒绝，不迁移也不覆盖。
pub(super) const LEGACY_SHAPE_TABLES: &[&str] = &["peri_sessions", "peri_session_messages"];

/// 旧形状探测（只读、全绑定）：库里有几张本构建不认识的旧会话表。
pub(super) const COUNT_LEGACY_TABLES_SQL: &str = "SELECT COUNT(*) FROM sqlite_master
    WHERE type = 'table' AND name IN (?1, ?2)";

/// 单行元数据表：`singleton` 固定 0，主键即身份竞争的同一唯一键空间。
pub(super) const STORE_META_TABLE: &str = "peri_store_meta";

const TABLE_EXISTS_SQL: &str = "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1";

const SELECT_META_SQL: &str =
    "SELECT schema_version, store_id, contract FROM peri_store_meta WHERE singleton = 0";

pub(super) const CREATE_STORE_META_SQL: &str = "CREATE TABLE IF NOT EXISTS peri_store_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 0),
    schema_version INTEGER NOT NULL,
    store_id TEXT NOT NULL,
    contract TEXT NOT NULL,
    created_at TEXT NOT NULL
)";

const INSERT_STORE_META_SQL: &str = "INSERT INTO peri_store_meta
    (singleton, schema_version, store_id, contract, created_at)
    VALUES (0, ?1, ?2, ?3, ?4)";

/// 远端持久化的存储身份（权威来源是远端 `peri_store_meta`，不是 URL 别名）。
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
    /// 本构建是否认识这个库：契约一致且版本可接受。
    pub(super) fn matches_build(&self) -> bool {
        self.contract == STORE_CONTRACT
            && matches!(acceptance(self.schema_version), SchemaAcceptance::Accept)
    }
}

/// 版本判定（纯函数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SchemaAcceptance {
    /// 本构建可读写。
    Accept,
    /// 高于本构建：拒绝，不迁移、不降级写入。
    TooNew,
    /// 低于本构建或非法：拒绝，不猜。
    Unusable,
}

pub(super) fn acceptance(version: i64) -> SchemaAcceptance {
    if version == REMOTE_SCHEMA_VERSION {
        SchemaAcceptance::Accept
    } else if version > REMOTE_SCHEMA_VERSION {
        SchemaAcceptance::TooNew
    } else {
        SchemaAcceptance::Unusable
    }
}

/// 只读身份读取的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum StoreIdentityRead {
    /// 本任务 schema 尚未初始化（表不存在，或表存在但没有元数据行）。
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

/// 初始化批的结果是否证明**本事务插入了**元数据行。
///
/// 只有 `META_INSERT_INDEX` 那条语句受影响行数为 1 才算：批成功但该语句没有插入任何行
/// （行已存在、语句被跳过）不构成创建；批失败（唯一键冲突、超时、丢响应）时根本没有这个
/// 结果，也无从证明。这是 created 的唯一证据，不由「打开前看到空库」推导。
pub(super) fn inserted_meta_row(counts: &[u64]) -> bool {
    counts.get(META_INSERT_INDEX) == Some(&1)
}

/// 只读读取的结果判定（纯函数）：表是否存在 + 元数据行原样交给它。
///
/// 表不存在与「表在但行不在」都是未初始化；行在但形状不可解释是 `Malformed`——不猜身份，
/// 也不把读不懂当成「空库可用」。真引擎 seam 与生产读取共用这一处判定。
pub(super) fn interpret_identity_read(
    table_exists: bool,
    row: Option<&[Value]>,
) -> StoreIdentityRead {
    if !table_exists {
        return StoreIdentityRead::Uninitialized;
    }
    match row {
        Some(values) => match decode_identity(values) {
            Some(snapshot) => StoreIdentityRead::Present(snapshot),
            None => StoreIdentityRead::Malformed,
        },
        None => StoreIdentityRead::Uninitialized,
    }
}

/// 只读身份读取的语句计划：全部是 SELECT。
///
/// `sqlite_master` 只用来判断表是否存在（其可用性由 cloud 实验断言，不作为身份判据）；
/// 表存在时再读元数据行，缺列等形状问题会以读失败上报，绝不会触发写。
pub(super) fn identity_read_plan() -> Vec<StatementSpec> {
    vec![
        StatementSpec::new(
            TABLE_EXISTS_SQL,
            vec![Value::Text(STORE_META_TABLE.to_owned())],
        ),
        StatementSpec::bare(SELECT_META_SQL),
    ]
}

/// 初始化计划里元数据 INSERT 的下标：身份竞争发生在这一条。
///
/// 与 [`initialization_plan`] 的语句顺序绑定（建表在前，所以它不是 0），由离线测试守住；
/// 初始化遇到该下标的唯一键冲突时读回胜者，不覆盖。
pub(super) const META_INSERT_INDEX: usize = 2;

/// 初始化计划：同一原子批内建表、写入本机铸造的身份、读回。
///
/// 语句顺序固定：两条 `CREATE TABLE IF NOT EXISTS`（已存在即 no-op，绝不改写）→
/// 元数据行 INSERT（主键冲突即失去身份竞争）→ SELECT 读回本次写入的事实。
pub(super) fn initialization_plan(store_id: &StoreId, now: &str) -> Vec<StatementSpec> {
    vec![
        StatementSpec::bare(CREATE_STORE_META_SQL),
        StatementSpec::bare(super::ledger::CREATE_OP_LEDGER_SQL),
        StatementSpec::new(
            INSERT_STORE_META_SQL,
            vec![
                Value::Integer(REMOTE_SCHEMA_VERSION),
                Value::Text(store_id.as_str().to_owned()),
                Value::Text(STORE_CONTRACT.to_owned()),
                Value::Text(now.to_owned()),
            ],
        ),
        StatementSpec::bare(SELECT_META_SQL),
    ]
}

/// 解码元数据行（`schema_version, store_id, contract`）；形状不符即 `None`。
pub(super) fn decode_identity(values: &[Value]) -> Option<StoreSnapshot> {
    let schema_version = int_at(values, 0)?;
    let store_id = text_at(values, 1)?;
    let contract = text_at(values, 2)?;
    Some(StoreSnapshot {
        store_id: StoreId(store_id.to_owned()),
        schema_version,
        contract: contract.to_owned(),
    })
}
