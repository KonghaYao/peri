//! Select and execute the guarded migration path for a store snapshot.

use super::super::mutation::RemoteStore;
use super::super::schema;
use super::super::schema::StoreSnapshot;
use peri_acp_types::session_resources::SessionResourceError;
use peri_acp_types::session_resources::SessionResourceErrorKind;
use peri_acp_types::session_resources::SessionResourceResult;

pub(super) fn unsupported() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Unsupported)
}

/// 搬运路径的升级来源：压缩前的存储契约（`v2`）+ 版本 10|11。
pub(super) fn legacy_migration_source(snapshot: &StoreSnapshot) -> bool {
    matches!(snapshot.schema_version, 10 | 11)
        && snapshot.contract == schema::PREVIOUS_STORE_CONTRACT
}

pub(in crate::sessions::remote) async fn upgrade(
    store: &RemoteStore,
    snapshot: &StoreSnapshot,
) -> SessionResourceResult<()> {
    // 当前 Machine 也要在批次里立起来（与本机迁移同一条语句）。写打开在此之前从不需要
    // 本机身份，所以这里显式初始化一次：拿不到身份就不发批次。
    crate::sessions::machine::initialize()
        .await
        .map_err(|_| unsupported())?;
    let current_machine = crate::sessions::machine::current().map_err(|_| unsupported())?;
    let read = super::input::read_input(store).await?;
    // 每条路径的批次都含 `DROP TABLE threads` 或 `DROP TABLE workspaces`：服务端的
    // `foreign_keys` 是跨连接共享的可变状态，若被别的连接打开，隐式删除会级联清掉
    // `messages`。写打开与这里一样先归位。
    store.force_parent_checks_off().await?;
    if super::plan::additive_source(snapshot) {
        // 上一代/再上一代当前形状（v13|12|11）：先补 `thread_mq` 与两列，再跑 v15 清理。
        // 批次仍是原子的，终点就是当前形状。
        return store
            .apply_schema_upgrade(super::plan::additive_upgrade_plan(
                snapshot,
                current_machine,
                &read.binding_snapshots,
                &read.projects,
            )?)
            .await;
    }
    if super::plan::v15_cleanup_source(snapshot) {
        // v14 → v15：只删列、删表并投影绑定证据，不搬运任何会话事实。
        return store
            .apply_schema_upgrade(super::plan::v15_cleanup_plan(
                snapshot,
                &read,
                current_machine,
            )?)
            .await;
    }
    if !legacy_migration_source(snapshot) {
        return Err(unsupported());
    }
    store
        .apply_schema_upgrade(super::plan::upgrade_plan(snapshot, &read, current_machine)?)
        .await
}
