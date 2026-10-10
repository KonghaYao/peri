//! Remote store opening verdict and legacy-shape probe.

use super::super::{
    mutation::{incomplete_reply, RemoteStore, StoreAccess},
    schema::{self, StoreId, StoreIdentityOutcome, StoreIdentityRead},
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

/// 身份读取 + 访问意图 → 打开的下一步（纯函数，真引擎 seam 与生产共用）。
///
/// 三条拒绝路径都不猜：版本/契约不认识、元数据形状不可解释、只读打开遇上尚未初始化的
/// store（没有 schema 就没有会话事实可读，也不越权建表）。
pub(in crate::sessions::remote) fn open_step(
    read: StoreIdentityRead,
    access: StoreAccess,
) -> SessionResourceResult<OpenStep> {
    match read {
        StoreIdentityRead::Present(snapshot) if snapshot.matches_build() => {
            Ok(OpenStep::Existing(snapshot.store_id))
        }
        StoreIdentityRead::Present(snapshot) if snapshot.readable() => match access {
            StoreAccess::ReadOnly => Ok(OpenStep::Existing(snapshot.store_id)),
            StoreAccess::ReadWrite => Ok(OpenStep::Upgrade(snapshot)),
        },
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
