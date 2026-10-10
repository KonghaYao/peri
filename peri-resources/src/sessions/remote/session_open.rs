//! Remote store opening verdict and legacy-shape probe.

use super::super::{
    mutation::{incomplete_reply, RemoteStore, StoreAccess},
    schema::{self, StoreId, StoreIdentityOutcome, StoreIdentityRead, StoreShapeRead},
    sql::{int_at, StatementSpec},
};
use super::{unsupported_behavior, StoreInitialization};
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use turso_serverless::Value;

/// Resolve the execution machine only after the remote store's identity has
/// been read. A read-only Native open must not create a local machine ID.
pub(super) async fn resolve_open_machine_id(
    supplied: Option<String>,
    access: StoreAccess,
) -> SessionResourceResult<String> {
    if let Some(machine_id) = supplied {
        return Ok(machine_id);
    }
    if access == StoreAccess::ReadWrite {
        crate::sessions::machine::initialize().await.map_err(|_| {
            SessionResourceError::new(SessionResourceErrorKind::Unavailable {
                detail: "machine identity initialization failed".to_owned(),
            })
        })?;
    }
    Ok(crate::sessions::machine::current()
        .unwrap_or_default()
        .to_owned())
}

/// 只读身份读取之后的下一步（纯函数结论）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sessions::remote) enum OpenStep {
    /// 已有本构建认识的身份：本次打开没有建立任何东西。
    Existing(StoreId),
    Upgrade(schema::StoreSnapshot),
    /// 明确为空：写打开在这里才继续初始化；只读打开到这一步就拒绝。
    NeedsInitialization,
}

/// 身份读取 + 形状探测 + 访问意图 → 打开的下一步（纯函数，真引擎 seam 与生产共用）。
///
/// 拒绝路径都不猜：版本/契约不认识、元数据形状不可解释、会话表形状与契约声明的代数**不符**、
/// 只读打开遇上尚未初始化或**还没有会话表**的 store（没有会话事实可读，也不越权建表）。
pub(in crate::sessions::remote) fn open_step(
    read: StoreIdentityRead,
    shape: Option<StoreShapeRead>,
    access: StoreAccess,
) -> SessionResourceResult<OpenStep> {
    match read {
        StoreIdentityRead::Present(snapshot) if snapshot.matches_build() => {
            accept_shape(shape, access)?;
            Ok(OpenStep::Existing(snapshot.store_id))
        }
        StoreIdentityRead::Present(snapshot) if snapshot.readable() => {
            accept_shape(shape, access)?;
            match access {
                StoreAccess::ReadOnly => Ok(OpenStep::Existing(snapshot.store_id)),
                StoreAccess::ReadWrite => Ok(OpenStep::Upgrade(snapshot)),
            }
        }
        StoreIdentityRead::Present(_) => {
            Err(unsupported_behavior("unrecognized remote store schema"))
        }
        StoreIdentityRead::Malformed => Err(super::super::session_codec::corrupt(
            "remote session store metadata is not interpretable",
        )),
        StoreIdentityRead::Uninitialized => match access {
            StoreAccess::ReadOnly => Err(unsupported_behavior(
                "read-only open of an uninitialized remote store",
            )),
            StoreAccess::ReadWrite => Ok(OpenStep::NeedsInitialization),
        },
    }
}

/// 形状探测（只读、单条 SELECT）：契约与版本决定探哪一份声明（[`schema::shape_probe`]）。
///
/// 只发 SELECT——探测不写、不建表，也不改任何行；探测结果只服务本次打开的判定，**不是迁移
/// 依据**：写打开的迁移批次自己再读一次形状并在批内守着它（见 `schema_upgrade`）。
/// 本构建不读的库（契约/版本不认识，或元数据本身读不出身份）没有可探的形状，返回 `None`——
/// 那一面的拒绝由身份读取单独给出。
pub(in crate::sessions::remote) async fn probe_shape(
    store: &RemoteStore,
    read: &StoreIdentityRead,
) -> SessionResourceResult<Option<StoreShapeRead>> {
    let StoreIdentityRead::Present(snapshot) = read else {
        return Ok(None);
    };
    let Some(probe) = schema::shape_probe(snapshot) else {
        return Ok(None);
    };
    let rows = store.fetch_rows(&StatementSpec::bare(probe.sql())).await?;
    Ok(Some(probe.check(&schema::decode_column_map(&rows)?)))
}

/// 形状判定：本构建读得动的库必须探测过，且形状要么相符、要么**还没建立**。
///
/// - `Consistent`：按契约声明的形状读写。
/// - `Unbuilt`：会话表一张都没建，**写打开**照旧在打开路径上按各自契约补齐（本代库的幂等
///   DDL）或升级（`v2` 库的空库分支）；**只读打开拒绝**——没有会话事实可读，也不越权建表，
///   与「只读打开尚未初始化的 store」同一条规则。
/// - `Drifted`：拒绝，并把**具体哪个判定失败**记进日志（[`refuse_drifted_shape`]）。
fn accept_shape(shape: Option<StoreShapeRead>, access: StoreAccess) -> SessionResourceResult<()> {
    match shape {
        Some(StoreShapeRead::Consistent) => Ok(()),
        Some(StoreShapeRead::Unbuilt) if access == StoreAccess::ReadWrite => Ok(()),
        Some(StoreShapeRead::Unbuilt) => Err(unsupported_behavior(
            "read-only open of a remote store without its session tables",
        )),
        Some(StoreShapeRead::Drifted(detail)) => Err(refuse_drifted_shape(&detail)),
        // 读得动的库必须被探测过：调用点漏探是内部矛盾，不静默放行。
        None => Err(SessionResourceError::new(
            SessionResourceErrorKind::Internal {
                detail: "remote session shape was not probed".to_owned(),
            },
        )),
    }
}

/// 形状不符的拒绝：分类沿用「不认识的 store schema」（`Unsupported`），诊断走日志。
///
/// `SessionResourceErrorKind::Unsupported` 不携带 detail，而这一路最需要回答的正是「为什么
/// 拒绝」——判定来源（`schema_shape`）给出的那句「哪张表/哪一列不符」按 `warn` 记录，
/// 不含会话内容与绑定值。
fn refuse_drifted_shape(detail: &str) -> SessionResourceError {
    tracing::warn!(
        detail,
        "unrecognized remote store schema: session tables drifted from the shape their contract declares"
    );
    SessionResourceError::new(SessionResourceErrorKind::Unsupported)
}

/// 身份竞争的结论 → 权威身份 + 本次打开的初始化事实（首次登记资格的唯一映射）。
///
/// `CreatedByThisOpen` 只能由 `Created` 产生：竞败方读回的胜者身份也是 `Existing`，
/// 所以败方没有首次登记资格，但它用的仍是同一个权威身份。
pub(in crate::sessions::remote) fn open_verdict(
    outcome: StoreIdentityOutcome,
) -> (StoreId, StoreInitialization) {
    match outcome {
        StoreIdentityOutcome::Created(store_id) => {
            (store_id, StoreInitialization::CreatedByThisOpen)
        }
        StoreIdentityOutcome::Existing(store_id) => (store_id, StoreInitialization::Existing),
    }
}

/// 旧形状探测（只读、单条 SELECT）：写打开遇上未初始化的库时，先问一次「这里有没有
/// 统一之前的远端会话表」。
///
/// 命中即拒绝（`Unsupported`）：这不是空库，而是一个本构建不认识的旧形状库。
/// **不自动迁移**——迁移要重命名表并搬运每一行，而本段的运行前提是新库没有历史数据；
/// **也不覆盖**——覆盖等于替使用者丢掉他看不见的数据。只发一条只读语句，不建表、不写行。
pub(super) async fn refuse_legacy_shape(store: &RemoteStore) -> SessionResourceResult<()> {
    let row = store
        .fetch_row(&StatementSpec::new(
            schema::COUNT_LEGACY_TABLES_SQL,
            schema::LEGACY_SHAPE_TABLES
                .iter()
                .map(|table| Value::Text((*table).to_owned()))
                .collect(),
        ))
        .await?;
    // 读不到计数不是「没有旧表」：形状不完整的读取按未决上报，不放行初始化。
    let count = row
        .as_ref()
        .and_then(|values| int_at(values, 0))
        .ok_or_else(|| incomplete_reply("legacy shape probe returned no count"))?;
    if count > 0 {
        return Err(unsupported_behavior(
            "remote store has the pre-unification session tables; it is not migrated automatically",
        ));
    }
    Ok(())
}
