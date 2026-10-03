//! 远端操作身份、收据与终态封闭（C §5.1 内部机制）。
//!
//! 机制要点（实现不得偏离，证据见 `cloud_mutation_test.rs`）：
//!
//! - **资格先于效果**：一次 mutation 是**一个原子批**，第一条语句就是资格写入
//!   （`operation_id` 主键 INSERT），其后才是业务效果；资格与效果同生共死。
//! - **同一唯一键空间**：封闭记录与原身份记录竞争**同一张表的同一主键**。另建
//!   「closed 表/独立索引」不构成互斥，因为串行化只保证先后顺序，不阻止原请求其后
//!   照常提交业务变更。
//! - **收据与操作身份只在本模块内**：`OperationId`/`Receipt` 是 `pub(super)`，
//!   组合层与业务侧拿不到；Debug 一律脱敏。

use std::fmt;

use sha2::{Digest, Sha256};
use turso_serverless::Value;

use peri_acp_types::thread::ThreadId;

use super::sql::{text_at, StatementSpec};

/// 远端 op_ledger 表：每个 operation 一行，`operation_id` 是主键（唯一键空间）。
pub(super) const OP_LEDGER_TABLE: &str = "peri_op_ledger";

/// 已生效：业务效果与资格在同一事务里提交。
pub(super) const STATE_APPLIED: &str = "applied";
/// 终态封闭：原请求确定从未生效，且不可能再生效。
pub(super) const STATE_CLOSED: &str = "closed";

pub(super) const CREATE_OP_LEDGER_SQL: &str = "CREATE TABLE IF NOT EXISTS peri_op_ledger (
    operation_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    digest TEXT NOT NULL,
    state TEXT NOT NULL,
    receipt TEXT,
    updated_at TEXT NOT NULL
)";

const QUALIFY_SQL: &str = "INSERT INTO peri_op_ledger
    (operation_id, kind, digest, state, receipt, updated_at)
    VALUES (?1, ?2, ?3, 'applied', ?4, ?5)";

const CLOSURE_SQL: &str = "INSERT INTO peri_op_ledger
    (operation_id, kind, digest, state, receipt, updated_at)
    VALUES (?1, ?2, ?3, 'closed', NULL, ?4)";

const RESOLVE_SQL: &str =
    "SELECT state, receipt, digest FROM peri_op_ledger WHERE operation_id = ?1";

/// 一次远端操作的内部身份；不接受外部构造。
///
/// **操作 id 由每次调用铸造，不由内容派生**。这一点是身份模型的要害：内容派生的 id 会把
/// 「同内容的后一次领域调用」判成历史重放并静默丢掉效果（状态 A→B→A、标题 x→y→x、
/// 同边界 rewind 再追加再 rewind 都会撞上）。id 唯一后，资格冲突只可能来自**同一次**
/// 操作：终态封闭写的是同一张表的**同一主键**，所以封闭与资格天然互斥（封闭先提交 ⇒ 原请求
/// 此后不可能再生效；对方已提交 ⇒ 读回原收据）。重试令牌不经过调用方，也不由内容推导。
///
/// v10 撤销本机操作日志后，[`OperationId::from_record`] 在生产路径上已无消费者（原用途是
/// 从本机记录取回原 id 参与跨进程封闭竞争）；它留给测试构造「同一次操作」的等价场景，
/// 是否随该能力一并删除归「统一 schema」段决定。
///
/// 输入摘要（[`OperationIdentity::digest`]）不参与身份生成，只做**一致性校验**：
/// 同一 id 被复用时摘要必须一致，否则是身份冲突而不是重放。
#[derive(Clone, PartialEq, Eq)]
pub(super) struct OperationId(String);

impl OperationId {
    /// 每次新的领域调用铸造一个唯一 id。
    ///
    /// 形状是 `{thread}.{uuid}`：唯一性来自 uuid，thread 前缀让远端账本行仍能与会话
    /// 对应（账本无 thread 列），也便于按会话回收本轮实验对象。不是内容派生：同样的
    /// 输入连续调用两次得到两个不同 id。
    pub(super) fn mint(thread: &ThreadId) -> Self {
        Self(format!(
            "{}.{}",
            thread.as_str(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// 由本机日志里的记录还原（恢复路径：复用已落盘的原 id，不重新铸造）。
    pub(super) fn from_record(value: &str) -> Self {
        Self(value.to_owned())
    }

    /// 确定性 id：只在机制实验中用于**故意**制造同一个 id 的重放（生产路径不得借此
    /// 派生身份，见类型文档）。
    #[cfg(test)]
    pub(super) fn scoped(scope: &str, label: &str) -> Self {
        Self(format!("{scope}.{label}"))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "OperationId(<opaque>)")
    }
}

/// 私有收据：证明「这一行对应的是那次已生效的写入」。
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Receipt(String);

impl Receipt {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Receipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Receipt(<opaque>)")
    }
}

/// 一次操作的完整内部身份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OperationIdentity {
    pub(super) operation_id: OperationId,
    pub(super) kind: String,
    pub(super) digest: String,
    pub(super) receipt: Receipt,
}

impl OperationIdentity {
    /// 组装身份：摘要取自调用方给出的输入片段，收据随机生成（不由此推导，
    /// 否则「返回原收据」与「重算一个收据」无法区分）。
    pub(super) fn new(operation_id: OperationId, kind: &str, inputs: &[&str]) -> Self {
        let digest = input_digest(inputs);
        Self::with_digest(operation_id, kind, digest)
    }

    /// 按已知摘要还原身份（恢复路径用本机记录里的摘要，不重算一份可能与原操作不同的）。
    ///
    /// 收据仍是新生成的随机值：还原方在意的是「以同一身份参与唯一键竞争」，不需要原收据
    /// （真需要原收据时从远端账本读回）。
    pub(super) fn with_digest(operation_id: OperationId, kind: &str, digest: String) -> Self {
        let receipt = Receipt(format!("v1:{}", uuid::Uuid::new_v4().simple()));
        Self {
            operation_id,
            kind: kind.to_owned(),
            digest,
            receipt,
        }
    }
}

/// 输入摘要：定长、不可逆，长度前缀避免拼接歧义；不把输入原文带进记录。
pub(super) fn input_digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 资格写入：必须是原子批的第一条语句。
pub(super) fn qualify_statement(identity: &OperationIdentity, now: &str) -> StatementSpec {
    StatementSpec::new(
        QUALIFY_SQL,
        vec![
            Value::Text(identity.operation_id.as_str().to_owned()),
            Value::Text(identity.kind.clone()),
            Value::Text(identity.digest.clone()),
            Value::Text(identity.receipt.as_str().to_owned()),
            Value::Text(now.to_owned()),
        ],
    )
}

/// Qualification and owner validation are one SQL statement in the business
/// transaction. A stale token turns the NOT NULL `kind` column into a
/// constraint error, so no later effect can commit.
pub(super) fn owner_guard_statement(
    identity: &OperationIdentity,
    root: &ThreadId,
    token: Option<&peri_acp_types::workspace::ExecutionOwnerToken>,
) -> StatementSpec {
    match token {
        Some(token) => StatementSpec::new(
            "INSERT INTO peri_op_ledger (operation_id, kind, digest, state, receipt, updated_at)
             VALUES (?1, (SELECT CASE WHEN EXISTS (
               SELECT 1 FROM session_execution_owners WHERE root_id = ?2 AND epoch = ?3
                 AND nonce = ?4 AND released = 0
                 AND expires_at_unix > CAST(strftime('%s','now') AS INTEGER)
             ) THEN 'execution_guard' ELSE NULL END), 'owner', 'applied', NULL, datetime('now'))",
            vec![
                Value::Text(format!("{}:owner", identity.operation_id.as_str())),
                Value::Text(root.clone()),
                Value::Integer(token.epoch),
                Value::Text(token.nonce.clone()),
            ],
        ),
        None => StatementSpec::new(
            "INSERT INTO peri_op_ledger (operation_id, kind, digest, state, receipt, updated_at)
             VALUES (?1, (SELECT CASE WHEN NOT EXISTS (
               SELECT 1 FROM session_execution_owners WHERE root_id = ?2
             ) THEN 'execution_guard' ELSE NULL END), 'owner', 'applied', NULL, datetime('now'))",
            vec![
                Value::Text(format!("{}:owner", identity.operation_id.as_str())),
                Value::Text(root.clone()),
            ],
        ),
    }
}

/// 终态封闭：插入终结行，与资格写竞争同一主键。
pub(super) fn closure_statement(identity: &OperationIdentity, now: &str) -> StatementSpec {
    StatementSpec::new(
        CLOSURE_SQL,
        vec![
            Value::Text(identity.operation_id.as_str().to_owned()),
            Value::Text(identity.kind.clone()),
            Value::Text(identity.digest.clone()),
            Value::Text(now.to_owned()),
        ],
    )
}

/// 只读解析：这一行现在是什么状态。
pub(super) fn resolve_statement(operation_id: &OperationId) -> StatementSpec {
    StatementSpec::new(
        RESOLVE_SQL,
        vec![Value::Text(operation_id.as_str().to_owned())],
    )
}

/// 一行账本的解码结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LedgerRow {
    /// 已生效：附带原收据与**原操作摘要**（摘要用于判断这次冲突是不是同一次操作）。
    Applied { receipt: Receipt, digest: String },
    /// 已封闭：确定从未生效。
    Closed,
    /// 没有这一行。
    Absent,
    /// 行存在但形状无法解释：不猜。
    Malformed,
}

/// 解码 `state, receipt, digest` 三列；形状不符一律 `Malformed`，不推断。
pub(super) fn decode_row(values: &[Value]) -> LedgerRow {
    let Some(state) = text_at(values, 0) else {
        return LedgerRow::Malformed;
    };
    match state {
        STATE_APPLIED => match (text_at(values, 1), text_at(values, 2)) {
            (Some(receipt), Some(digest)) => LedgerRow::Applied {
                receipt: Receipt(receipt.to_owned()),
                digest: digest.to_owned(),
            },
            _ => LedgerRow::Malformed,
        },
        STATE_CLOSED => LedgerRow::Closed,
        _ => LedgerRow::Malformed,
    }
}
