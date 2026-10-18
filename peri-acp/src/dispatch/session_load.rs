//! 会话有效历史读取（含继承区）与历史分页读取。
//!
//! 存储访问经 [`Controller::sessions`]（ARC-BOUNDARY-001 方向：ACP 不直操存储，
//! 统一经 Controller 通道），拿到的是有效逻辑上下文：继承区在前、未排除的自有
//! payload 在后。
//!
//! ## 历史分页的 ACP 侧错误映射
//!
//! 游标不可用必须**可区分**，且绝不降级成空页或一次正常 load（REQ-H02、验收 A25）。
//! 映射只在这里做一次：
//!
//! | 存储错误 | JSON-RPC code | 客户端处置 |
//! | --- | --- | --- |
//! | [`HistoryCursorError::Malformed`] | `-32602` | 坏请求：清掉本地游标，按初始 load 重来 |
//! | [`HistoryCursorError::CrossSession`] | `-32011` | 状态串了会话：丢弃该游标 |
//! | [`HistoryCursorError::Expired`] | `-32012` | 范围已变（rewind/删除）：刷新相关窗口 |
//! | `NotFound` | `-32602` | 会话不存在：不要凭空创建 |
//! | 其他（`Unsupported` / `Corrupt` / `Unavailable` / `Timeout` …） | `-32603` | 服务端故障，原样上报 |
//!
//! `-32011` / `-32012` 取 JSON-RPC 保留的 server error 段（`-32000..=-32099`）中本仓
//! 未占用的取值（既有占用：`-32000` turn 执行失败、`-32001`/`-32002` MCP-over-ACP、
//! `-32010` 忙/关闭）。`data.reason` 保留稳定分类，供客户端在只读消息时也能分支。
//!
//! `-32013` 沿用同一段与同一约定：`session/rewind` 的边界目标在写入时已不在存储里
//! （`data.reason = "rewind_boundary_missing"`）。它与本模块的 `Expired` 语义相邻但不同：
//! 游标过期是「读的范围变了」，这是一个**写请求没生效**——存储一行未动，调用方要先重读
//! 再决定，绝不当成「已回退」。构造点在 [`crate::dispatch::rewind`]。

use crate::transport::types::AcpError;
use peri_acp_types::history_page::{HistoryCursor, HistoryCursorError, HistoryPage, RangeToken};
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind};
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

/// 有界读取一页历史（更早方向），游标错误按模块文档的映射转成可区分错误。
pub async fn load_history_page(
    controller: &Controller,
    thread_id: &str,
    cursor: &HistoryCursor,
    limit: usize,
    byte_budget: usize,
) -> Result<HistoryPage, AcpError> {
    controller
        .sessions()
        .load_history_page(
            &ThreadId::from(thread_id.to_string()),
            Some(cursor),
            limit,
            byte_budget,
        )
        .await
        .map_err(history_page_error)
}

/// 初始 load 的分页身份：首个游标（`None` = 更早方向没有可展示内容）与浏览快照身份。
#[derive(Clone, Debug)]
pub struct InitialHistoryCursor {
    pub next_cursor: Option<HistoryCursor>,
    /// `snapshotToken`：同一次浏览的范围身份，与游标携带的是同一份事实。
    pub snapshot_token: RangeToken,
}

/// 为初始 load 铸造第一个游标（REQ-H01-1、验收 A15）。
///
/// 锚点 = 有效视图里最旧的**可展示**自有 payload，**跳过继承/祖先前缀**：首屏第一条可能是
/// 继承条目，它在本线程没有行——拿它当锚点会让第一次翻页立刻 `Expired`，或者把浏览
/// 带进祖先区。`ActiveSessionSnapshot` 的 `inherited` 与 `payloads` 是分开给的，
/// 自有区即 `payloads`（顺序与完整读取一致），这里不需要在 ACP 层重新推导边界。
///
/// 可展示性取自 [`PersistedPayload::displayable_to_client`]——与分页出口装配 `items` 的过滤
/// **同一条**规则，不在这里另立判据。这不是洁癖：subagent / fork / bg 子会话的自有区第一条
/// 是会话身份 System 条目（`session/subagent/factory/spawn.rs`），它既不被 Full 排除、
/// 又永远不进入客户端渲染；拿它（或任何 model-only reminder）当锚点，会让比它更早的
/// 归档原文一页都取不到，锚点本身就是浏览范围起点时更会直接谎称「没有更早历史」。
/// 首屏不展示的条目不该占据「已交付」的位置：锚点是**已交付集合的下边界**。
///
/// 有效视图里一条可展示条目都没有时（只有身份 System / model-only reminder）退到**最新**
/// 一条**有效**自有 payload（`payloads.last()`）：它同样不在首屏展示，所以既不会重，也不会
/// 让比它更早的可展示归档行失去交付机会；是否收尾仍交给 `range_head` 判据——宁可多给一次
/// 空页，也不谎称没有更早历史。
///
/// 没有自有 payload（空会话），或锚点**就是**浏览范围起点（更早方向确实没有行）时
/// 回答 `nextCursor = null`：不编造游标，也不触发任何兜底摘要。
///
/// 代价：一次有效视图读取（边界单行 + 有效集合，已排除正文不读取）。它发生在初始
/// load 上，与「首屏只读有效范围」同一口径；调用方只在当前连接协商了分页能力时调用。
pub async fn initial_history_cursor(
    controller: &Controller,
    thread_id: &str,
) -> Result<InitialHistoryCursor, AcpError> {
    let id = ThreadId::from(thread_id.to_string());
    let resources = controller.sessions();
    let bounds = resources
        .history_page_bounds(&id)
        .await
        .map_err(history_page_error)?;
    let effective = resources
        .load_active_snapshot(&id)
        .await
        .map_err(history_page_error)?;
    let anchor = effective
        .payloads
        .iter()
        .find(|payload| payload.displayable_to_client())
        .or_else(|| effective.payloads.last())
        .map(PersistedPayload::id);
    let next_cursor = anchor.and_then(|anchor| {
        (bounds.range_head != Some(anchor))
            .then(|| HistoryCursor::new(id.clone(), anchor, bounds.range_token.clone()))
    });
    Ok(InitialHistoryCursor {
        next_cursor,
        snapshot_token: bounds.range_token,
    })
}

/// 存储侧分页错误 → ACP 错误（映射表见模块文档）。
pub fn history_page_error(error: SessionResourceError) -> AcpError {
    match error.kind() {
        SessionResourceErrorKind::Cursor(HistoryCursorError::Malformed { detail }) => {
            AcpError::new(-32602, format!("history cursor is malformed: {detail}"))
                .with_data(serde_json::json!({ "reason": "malformed_cursor" }))
        }
        SessionResourceErrorKind::Cursor(HistoryCursorError::CrossSession { expected, cursor }) => {
            AcpError::new(
                -32011,
                format!("history cursor belongs to session {cursor}, not {expected}"),
            )
            .with_data(serde_json::json!({ "reason": "cross_session_cursor" }))
        }
        SessionResourceErrorKind::Cursor(HistoryCursorError::Expired { thread_id }) => {
            AcpError::new(
                -32012,
                format!("history cursor has expired for session {thread_id}"),
            )
            .with_data(serde_json::json!({ "reason": "expired_cursor" }))
        }
        SessionResourceErrorKind::HistoryWindow(window) => {
            AcpError::new(i64::from(window.code()), window.to_string())
                .with_data(serde_json::json!({ "reason": window.reason() }))
        }
        SessionResourceErrorKind::NotFound => AcpError::new(-32602, "session not found"),
        _ => AcpError::new(-32603, format!("session history page load failed: {error}")),
    }
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
