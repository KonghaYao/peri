//! 会话有效历史读取（含继承区）。
//!
//! 存储访问经 [`Controller::sessions`]（ARC-BOUNDARY-001 方向：ACP 不直操存储，
//! 统一经 Controller 通道），拿到的是有效逻辑上下文：继承区在前、未排除的自有
//! payload 在后。

use crate::transport::types::AcpError;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::ThreadId;
use peri_controller::Controller;

/// Load the effective context for a session thread including ancestor snapshots.
///
/// Uses [`SessionResources::load_active_snapshot`] (via [`Controller::sessions`]): the
/// same logical order as the full read (inherited region + own payloads), minus the own
/// bodies released by a Full Compact — exclusion filtering happens in storage, so those
/// bodies never enter memory.
///
/// This is the publish source for `SessionState::history_payloads` (有效集合语义）；
/// 完整 canonical 历史（含已排除原文）另由 `load_session_history` 读取。
///
/// [`SessionResources::load_active_snapshot`]: peri_acp_types::session_resources::SessionResources::load_active_snapshot
pub async fn load_active_session_payloads(
    controller: &Controller,
    thread_id: &str,
) -> Result<Vec<PersistedPayload>, AcpError> {
    let snapshot = controller
        .sessions()
        .load_active_snapshot(&ThreadId::from(thread_id.to_string()))
        .await
        .map_err(|error| AcpError::new(-32603, format!("session history load failed: {error}")))?;
    let mut payloads = snapshot.inherited.payloads;
    payloads.extend(snapshot.payloads);
    Ok(payloads)
}

/// Load the full canonical history: inherited region + own payloads, **including** the
/// archived bodies a Full Compact excluded (exclusion is not a filter here).
///
/// 显式历史操作（rewind 候选/预览）的事实源：热态只保留有效集合，用热态回答
/// 「目标是否存在」会把 Full 归档的目标判成不存在。恢复路径不用这条读取
/// （它走 [`load_active_session_payloads`]）。
pub async fn load_canonical_session_payloads(
    controller: &Controller,
    thread_id: &str,
) -> Result<Vec<PersistedPayload>, AcpError> {
    controller
        .sessions()
        .load_session_history(&ThreadId::from(thread_id.to_string()))
        .await
        .map_err(|error| AcpError::new(-32603, format!("session history load failed: {error}")))
}
