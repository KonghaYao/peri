//! `peri.historyWindowV1` Host coordinator 与 open/page/seek/body 路由。
//!
//! 执行顺序（计划 §5.2）：连接级能力/schema → sessions 短锁取 generation/structure/ticket
//! → **锁外**调用 `SessionResources` → 返回前短锁二次校验。任何 DB I/O 都不在 sessions
//! 全局锁内；窗口读取不取 prompt lock、不广播、不装配、不刷新 input queue。
//!
//! coordinator 只保存轻量事实：ticket 绑定、snapshot/tail/checkpoint、rewind 状态与
//! in-flight 计数。历史正文始终在存储侧，响应按页装配后立即释放。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use peri_acp_types::history_window::{
    HistoryCheckpoint, HistoryCommittedTail, HistoryEntryId, HistoryInheritedSnapshot,
    HistoryReadTicket, HistoryReadTicketBinding, HistorySource, HistoryWindowCursor,
    HistoryWindowDirection, HistoryWindowError, HistoryWindowMetadata, HistoryWindowOpenRequest,
    HistoryWindowPageRequest, HistoryWindowRequest, HistoryWindowSeekRequest,
    HISTORY_WINDOW_DEFAULT_BYTE_BUDGET, HISTORY_WINDOW_DEFAULT_CHUNK_SIZE,
    HISTORY_WINDOW_MAX_CHUNK_SIZE, HISTORY_WINDOW_PROJECTION_VERSION,
};
use peri_acp_types::session_resources::{
    HistoryWindowPagePlan, HistoryWindowReadIntent, HistoryWindowRows,
    HistoryWindowTailExpectation, SessionResourceError,
};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::PeriCaps;

use crate::transport::types::AcpError;

use super::{AcpServerConfig, SessionState, SharedSessions};

mod commit_observer;
mod inherited;
mod oversize;
mod state;
mod visibility;
mod wire;

#[cfg(test)]
mod commit_tail_test;

pub(crate) use commit_observer::StoreCompactCommitObserver;

/// 票据有效期：短期只读句柄；过期后必须重新 `open`。
const TICKET_TTL_MS: i64 = 5 * 60 * 1000;

/// 窗口通知的可注入发送 seam；生产实现把通知写到该 ticket 所属连接。
#[async_trait::async_trait]
pub(crate) trait WindowNotificationSink: Send + Sync {
    async fn send(&self, session_id: &str, payload: Value) -> Result<(), AcpError>;
}

/// 签发票据/接收通知的连接身份。
#[derive(Clone)]
pub(crate) struct WindowConnection {
    pub(crate) id: String,
    pub(crate) sink: Arc<dyn WindowNotificationSink>,
}

impl WindowConnection {
    pub(crate) fn new(id: impl Into<String>, sink: Arc<dyn WindowNotificationSink>) -> Self {
        Self {
            id: id.into(),
            sink,
        }
    }
}

/// 绑定到单个 ACP 连接 transport 的通知实现。
pub(crate) struct TransportWindowSink {
    transport: Arc<dyn crate::transport::AcpTransport>,
}

impl TransportWindowSink {
    pub(crate) fn new(transport: Arc<dyn crate::transport::AcpTransport>) -> Self {
        Self { transport }
    }
}

#[async_trait::async_trait]
impl WindowNotificationSink for TransportWindowSink {
    async fn send(&self, _session_id: &str, payload: Value) -> Result<(), AcpError> {
        self.transport
            .send_notification("peri/history_committed", payload)
            .await
    }
}

/// 已准备好、但**尚未发布**的新窗口票据。
///
/// open 的页读取可能失败；准备阶段不得替换旧 ticket。只有页读取与全部校验成功后，
/// 才在短锁内 `publish` 原子替换。
pub(crate) struct PreparedWindowTicket {
    opaque: HistoryReadTicket,
    binding: HistoryReadTicketBinding,
    metadata: HistoryWindowMetadata,
    sink: Arc<dyn WindowNotificationSink>,
    /// 本票据已暴露的 oversize descriptor 身份与票据级预算；publish 时随票据转移。
    oversize_entries: Arc<std::sync::Mutex<oversize::OversizeMemory>>,
    /// 本票据内的受众可见性结论；publish 时随票据转移，revoke 即整体丢弃。
    visibility: visibility::VisibilityCache,
    inherited_unsupported: bool,
    inherited: Option<HistoryInheritedSnapshot>,
}

/// 一条已签发的窗口票据及其绑定的 snapshot metadata。
#[derive(Clone)]
pub(crate) struct IssuedWindowTicket {
    opaque: HistoryReadTicket,
    binding: HistoryReadTicketBinding,
    metadata: HistoryWindowMetadata,
    ticket_epoch: u64,
    /// 该 ticket 所属连接的通知 seam；只用于定向投递，不授权任何读取。
    sink: Arc<dyn WindowNotificationSink>,
    /// 本 snapshot 已向客户端暴露的 oversize descriptor 集合；body 只允许读取这些条目。
    oversize_entries: Arc<std::sync::Mutex<oversize::OversizeMemory>>,
    /// 会话存在冻结 inherited 前缀，但本实现未分段读取；响应必须显式告知客户端。
    inherited_unsupported: bool,
    /// 本窗口可见的冻结 inherited 快照（own 窗口也会报告）。
    inherited: Option<HistoryInheritedSnapshot>,
    /// 本票据内的受众可见性结论；与 ticket 同生命周期。
    visibility: visibility::VisibilityCache,
}

impl IssuedWindowTicket {
    fn remembers_oversize(&self, entry_id: peri_acp_types::messages::MessageId) -> bool {
        self.oversize_entries
            .lock()
            .map(|memory| memory.remembers(entry_id))
            // 锁中毒时记忆不可信：按 overflowed 处理，强制回存储重取校验。
            .unwrap_or(true)
    }

    fn remember_oversize(&self, entries: &[(peri_acp_types::messages::MessageId, u64)]) {
        if let Ok(mut memory) = self.oversize_entries.lock() {
            memory.remember(entries);
        }
    }

    /// 集合已溢出：body 不能只凭身份集合判定，必须按稳定身份重新校验。
    fn oversize_overflowed(&self) -> bool {
        self.oversize_entries
            .lock()
            .map(|memory| memory.overflowed())
            .unwrap_or(true)
    }

    #[cfg(test)]
    fn force_oversize_overflow(&self) {
        if let Ok(mut memory) = self.oversize_entries.lock() {
            memory.force_overflow();
        }
    }

    /// 完整受众解析次数；回归测试用它证明分块请求没有重扫全文。
    #[cfg(test)]
    pub(crate) fn visibility_full_scans(&self) -> usize {
        self.visibility.full_scans()
    }
}

/// Host 侧窗口协调器：只保存轻量事实，不保存历史正文。
#[derive(Default)]
pub(crate) struct HistoryWindowCoordinator {
    ticket: Option<IssuedWindowTicket>,
    ticket_epoch: u64,
    pub(crate) rewind_in_progress: bool,
    pub(crate) committed_tail: Option<HistoryCommittedTail>,
    pub(crate) checkpoint_id: Option<String>,
    pub(crate) summary_entry_id: Option<String>,
    /// 已发送过 `peri/history_committed` 的 checkpoint（按 checkpointId 幂等）。
    pub(crate) last_notified_checkpoint: Option<String>,
    /// 当前 HandedOff attempt 的活跃标记；Drop guard 翻转它，下一次读取据此冷恢复。
    pub(crate) handed_off_alive: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl HistoryWindowCoordinator {
    /// 准备一张新票据；不修改当前 ticket/epoch，失败时旧窗口保持可用。
    fn prepare(
        metadata: HistoryWindowMetadata,
        source: HistorySource,
        connection: &WindowConnection,
        inherited_unsupported: bool,
        inherited: Option<HistoryInheritedSnapshot>,
    ) -> PreparedWindowTicket {
        let now = now_unix_ms();
        let binding = HistoryReadTicketBinding {
            connection_id: connection.id.clone(),
            session_id: metadata.session_id.clone(),
            session_generation: metadata.session_generation.clone(),
            structure_version: metadata.structure_version,
            snapshot_token: metadata.snapshot_token.clone(),
            source,
            committed_tail: metadata.committed_tail.clone(),
            projection_version: HISTORY_WINDOW_PROJECTION_VERSION,
            issued_at_unix_ms: now,
            expires_at_unix_ms: now.saturating_add(TICKET_TTL_MS),
            nonce: uuid::Uuid::now_v7().to_string(),
        };
        PreparedWindowTicket {
            opaque: HistoryReadTicket::new(uuid::Uuid::now_v7().to_string()),
            binding,
            metadata,
            sink: Arc::clone(&connection.sink),
            oversize_entries: Arc::new(std::sync::Mutex::new(oversize::OversizeMemory::new())),
            visibility: visibility::VisibilityCache::new(),
            inherited_unsupported,
            inherited,
        }
    }

    /// 原子发布已准备票据：旧 ticket 在此刻才失效。
    fn publish(&mut self, prepared: PreparedWindowTicket) -> IssuedWindowTicket {
        self.ticket_epoch = self.ticket_epoch.saturating_add(1);
        let issued = IssuedWindowTicket {
            opaque: prepared.opaque,
            binding: prepared.binding,
            metadata: prepared.metadata,
            ticket_epoch: self.ticket_epoch,
            sink: prepared.sink,
            oversize_entries: prepared.oversize_entries,
            inherited_unsupported: prepared.inherited_unsupported,
            inherited: prepared.inherited,
            visibility: prepared.visibility,
        };
        self.ticket = Some(issued.clone());
        issued
    }

    fn current(&self) -> Option<&IssuedWindowTicket> {
        self.ticket.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn has_ticket(&self) -> bool {
        self.ticket.is_some()
    }

    /// 当前票据的完整受众解析次数；没有票据时为 `None`。
    #[cfg(test)]
    pub(crate) fn visibility_full_scans(&self) -> Option<usize> {
        self.ticket
            .as_ref()
            .map(IssuedWindowTicket::visibility_full_scans)
    }

    /// 测试专用：把当前票据的 oversize 记忆推进到溢出态。
    #[cfg(test)]
    pub(crate) fn force_oversize_overflow(&mut self) {
        if let Some(ticket) = self.ticket.as_ref() {
            ticket.force_oversize_overflow();
        }
    }

    /// 撤销当前票据；旧 epoch 的读请求在返回前复核时被拒绝。
    pub(crate) fn revoke_ticket(&mut self) {
        self.ticket = None;
        self.ticket_epoch = self.ticket_epoch.saturating_add(1);
    }

    /// rewind 进入：停止签发/接受新票据，旧票据保留到提交结论。
    pub(crate) fn begin_rewind(&mut self) {
        self.rewind_in_progress = true;
    }

    /// rewind 结束：durable 成功才清空提交元数据；失败只恢复可读状态。
    pub(crate) fn finish_rewind(&mut self, applied: bool) {
        self.rewind_in_progress = false;
        if applied {
            self.revoke_ticket();
            self.committed_tail = None;
            self.checkpoint_id = None;
            self.summary_entry_id = None;
            self.last_notified_checkpoint = None;
        }
    }

    /// 持久化结果不确定：撤销票据并丢弃不可信提交元数据，窗口 fail-closed。
    pub(crate) fn invalidate_for_uncertain_persistence(&mut self) {
        self.rewind_in_progress = false;
        self.revoke_ticket();
        self.committed_tail = None;
        self.checkpoint_id = None;
        self.summary_entry_id = None;
        self.last_notified_checkpoint = None;
    }

    /// 发布一次已确认提交的轻量事实；同 checkpoint 幂等，tail 全局单调（回退整体拒绝）。
    pub(crate) fn publish_commit(
        &mut self,
        checkpoint: Option<&HistoryCheckpoint>,
        tail: Option<&HistoryCommittedTail>,
    ) {
        // 单调整体守卫：已发布 tail 与下一事实都存在时只接受前进/相等。
        // 回退整体拒绝（checkpoint/tail 都不动），不区分是否同一 checkpoint，
        // 避免两套规则。已知的清空路径先走 finish_rewind/invalidate。
        if let (Some(previous), Some(next)) = (self.committed_tail.as_ref(), tail) {
            if next.position < previous.position {
                tracing::warn!(
                    previous = previous.position,
                    next = next.position,
                    "history window: refusing a non-monotonic committed tail"
                );
                return;
            }
        }
        if let Some(checkpoint) = checkpoint {
            if self.checkpoint_id.as_deref() == Some(checkpoint.checkpoint_id.as_str()) {
                // 同一 checkpoint 去重，但 tail 仍要随之后的 append 前进。
                if tail.is_some() {
                    self.committed_tail = tail.cloned();
                }
                return;
            }
            self.checkpoint_id = Some(checkpoint.checkpoint_id.clone());
            self.summary_entry_id = Some(checkpoint.checkpoint_id.clone());
            self.committed_tail = tail.cloned();
        } else {
            // 存储明确回答「当前没有可重建 checkpoint」时清除旧 checkpoint，
            // 避免 rewind/重新 load 后继续发布上一代的提交身份。
            self.checkpoint_id = None;
            self.summary_entry_id = None;
            self.committed_tail = tail.cloned();
        }
    }
}

/// `HistoryWindowError` → ACP 错误（稳定 code + `data.reason`）。
pub(crate) fn window_error(error: HistoryWindowError) -> AcpError {
    AcpError::new(i64::from(error.code()), error.to_string())
        .with_data(json!({ "reason": error.reason() }))
}

/// `SessionResourceError` → ACP 错误；窗口分类优先，其余按同一映射表 fail-closed。
pub(crate) fn resource_window_error(error: SessionResourceError) -> AcpError {
    match error.history_window_error() {
        Some(window) => window_error(window.clone()),
        None => window_error(HistoryWindowError::from(error)),
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn invalid(detail: impl Into<String>) -> AcpError {
    window_error(HistoryWindowError::invalid_request(detail))
}

fn invalidated(detail: impl Into<String>) -> AcpError {
    window_error(HistoryWindowError::window_invalidated(detail))
}

fn ticket_invalid(detail: impl Into<String>) -> AcpError {
    window_error(HistoryWindowError::ReadTicketInvalid {
        detail: detail.into(),
    })
}

fn snapshot_mismatch(detail: impl Into<String>) -> AcpError {
    window_error(HistoryWindowError::SnapshotMismatch {
        detail: detail.into(),
    })
}

fn anchor_unavailable(detail: impl Into<String>) -> AcpError {
    window_error(HistoryWindowError::anchor_unavailable(detail))
}

/// open/page/seek 的统一入口。
pub(crate) async fn handle(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    caps: &PeriCaps,
    session_id: &str,
    request: HistoryWindowRequest,
    connection: &WindowConnection,
) -> Result<Value, AcpError> {
    match request {
        HistoryWindowRequest::Open(request) => {
            open(cfg, sessions, caps, session_id, request, connection).await
        }
        HistoryWindowRequest::Page(request) => {
            page(cfg, sessions, caps, session_id, request, connection).await
        }
        HistoryWindowRequest::Seek(request) => {
            seek(cfg, sessions, caps, session_id, request, connection).await
        }
    }
}

async fn open(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    caps: &PeriCaps,
    session_id: &str,
    request: HistoryWindowOpenRequest,
    connection: &WindowConnection,
) -> Result<Value, AcpError> {
    let plan = request.validate().map_err(window_error)?;
    let (source, anchor) = match &plan.anchor {
        peri_acp_types::history_window::HistoryWindowAnchor::Latest => (HistorySource::Own, None),
        peri_acp_types::history_window::HistoryWindowAnchor::Entry { entry_id, source } => (
            source.clone(),
            Some(state::parse_anchor(entry_id, session_id)?),
        ),
    };

    let (wire_session_id, thread_id, generation, structure_version, expected_epoch) = {
        let states = sessions.lock().await;
        let state = state::readable_state(&states, session_id)?;
        (
            state.session_id.clone(),
            state.thread_id.clone(),
            state.session_generation,
            state.structure_version,
            state.history_window.ticket_epoch,
        )
    };
    let bounds = cfg
        .session_resources
        .history_window_bounds(&ThreadId::from(thread_id.clone()))
        .await
        .map_err(resource_window_error)?;

    let inherited_snapshot = bounds.inherited.snapshot().cloned();
    let source = inherited::canonical_source(&source, inherited_snapshot.as_ref())?;
    let metadata = HistoryWindowMetadata {
        session_id: wire_session_id.clone(),
        session_generation: generation.to_string(),
        structure_version,
        snapshot_token: uuid::Uuid::now_v7().to_string(),
        committed_tail: source.is_own().then(|| bounds.own_latest.clone()).flatten(),
        checkpoint_id: source
            .is_own()
            .then(|| {
                bounds
                    .checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.checkpoint_id.clone())
            })
            .flatten(),
    };
    // 准备但不发布：页读取失败时旧 ticket/旧 snapshot 必须继续可用。
    let prepared = HistoryWindowCoordinator::prepare(
        metadata.clone(),
        source.clone(),
        connection,
        false,
        inherited_snapshot,
    );
    let page_plan = HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Open,
        source: source.clone(),
        anchor,
        direction: plan.direction,
        include_anchor: anchor.is_some(),
        before_limit: plan.before_limit,
        after_limit: plan.after_limit,
        byte_budget: plan.byte_budget,
        tail: HistoryWindowTailExpectation::Fixed(metadata.committed_tail.clone()),
    };
    let rows = cfg
        .session_resources
        .load_history_window_page(&ThreadId::from(thread_id.clone()), &page_plan)
        .await
        .map_err(resource_window_error);
    let rows = wire::filter_rows(
        cfg,
        &thread_id,
        &source,
        &prepared.opaque,
        &prepared.visibility,
        rows,
    )
    .await;
    let oversize = wire::oversize_descriptors(rows.as_ref().ok());
    let rows = rows?;
    inherited::validate_rows(&source, &rows)?;
    // 所有读取与校验成功后才原子替换 ticket；旧 ticket 在此刻之前一直有效。
    let issued = {
        let mut states = sessions.lock().await;
        let state = state::readable_state_mut(&mut states, session_id)?;
        if state.session_generation != generation || state.structure_version != structure_version {
            return Err(invalidated(
                "session generation or structure version changed",
            ));
        }
        if state.history_window.ticket_epoch != expected_epoch {
            return Err(ticket_invalid(
                "history window changed while opening; retry the open",
            ));
        }
        state
            .history_window
            .publish_commit(bounds.checkpoint.as_ref(), bounds.own_latest.as_ref());
        let issued = state.history_window.publish(prepared);
        issued.remember_oversize(&oversize);
        issued
    };
    Ok(wire::response(
        &metadata.session_id,
        &thread_id,
        &metadata,
        &issued,
        &source,
        rows,
        caps,
    ))
}

async fn page(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    caps: &PeriCaps,
    session_id: &str,
    request: HistoryWindowPageRequest,
    connection: &WindowConnection,
) -> Result<Value, AcpError> {
    let direction = request.validate().map_err(window_error)?;
    let (thread_id, generation, structure_version, issued) = prepare_ticket_read(
        sessions,
        session_id,
        &connection.id,
        &request.snapshot_token,
        &request.read_ticket,
    )
    .await?;
    let raw_cursor = if direction == HistoryWindowDirection::Before {
        request.before_cursor.as_deref()
    } else {
        request.after_cursor.as_deref()
    }
    .ok_or_else(|| invalid("exactly one cursor is required"))?;
    let cursor = match HistoryWindowCursor::decode(raw_cursor, &thread_id) {
        Ok(cursor) => cursor,
        Err(error) => {
            return Err(window_error(error));
        }
    };
    if cursor.direction != direction {
        return Err(invalid("cursor direction does not match the request"));
    }
    if let Err(error) = cursor.verify_against(&issued.metadata, &issued.binding.source) {
        return Err(window_error(error));
    }
    let page_plan = HistoryWindowPagePlan::page(
        issued.binding.source.clone(),
        cursor.anchor,
        direction,
        request.limit as usize,
        HISTORY_WINDOW_DEFAULT_BYTE_BUDGET as usize,
        issued.binding.committed_tail.clone(),
    );
    let result = cfg
        .session_resources
        .load_history_window_page(&ThreadId::from(thread_id.clone()), &page_plan)
        .await
        .map_err(resource_window_error);
    let result = wire::filter_rows(
        cfg,
        &thread_id,
        &issued.binding.source,
        &issued.opaque,
        &issued.visibility,
        result,
    )
    .await;
    let oversize = wire::oversize_descriptors(result.as_ref().ok());
    finalize(
        sessions,
        session_id,
        generation,
        structure_version,
        &issued,
        &oversize,
    )
    .await?;
    let rows = result?;
    inherited::validate_rows(&issued.binding.source, &rows)?;
    Ok(wire::response(
        &issued.metadata.session_id,
        &thread_id,
        &issued.metadata,
        &issued,
        &issued.binding.source,
        rows,
        caps,
    ))
}

async fn seek(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    caps: &PeriCaps,
    session_id: &str,
    request: HistoryWindowSeekRequest,
    connection: &WindowConnection,
) -> Result<Value, AcpError> {
    let (before_limit, after_limit) = request.validate().map_err(window_error)?;
    let anchor = state::parse_anchor(&request.anchor.entry_id, session_id)?;
    let (thread_id, generation, structure_version, issued) = prepare_ticket_read(
        sessions,
        session_id,
        &connection.id,
        &request.snapshot_token,
        &request.read_ticket,
    )
    .await?;
    let source = issued.binding.source.clone();
    match (&request.anchor.source, &source) {
        (HistorySource::Own, HistorySource::Own) => {}
        (
            HistorySource::Inherited {
                frozen_snapshot_id,
                ancestor_thread_id,
            },
            HistorySource::Inherited {
                frozen_snapshot_id: ticket_snapshot,
                ancestor_thread_id: ticket_ancestor,
            },
        ) => {
            if frozen_snapshot_id != ticket_snapshot {
                return Err(snapshot_mismatch(
                    "seek inherited snapshot does not match the active window",
                ));
            }
            if let Some(expected) = ancestor_thread_id {
                if ticket_ancestor.as_ref() != Some(expected) {
                    return Err(snapshot_mismatch(
                        "seek inherited ancestor does not match the active window",
                    ));
                }
            }
        }
        _ => {
            return Err(snapshot_mismatch(
                "seek source does not match the active window",
            ))
        }
    }
    let page_plan = HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Seek,
        source: source.clone(),
        anchor: Some(anchor),
        direction: HistoryWindowDirection::Around,
        include_anchor: true,
        before_limit,
        after_limit,
        byte_budget: HISTORY_WINDOW_DEFAULT_BYTE_BUDGET as usize,
        tail: HistoryWindowTailExpectation::Fixed(issued.binding.committed_tail.clone()),
    };
    let result = cfg
        .session_resources
        .load_history_window_page(&ThreadId::from(thread_id.clone()), &page_plan)
        .await
        .map_err(resource_window_error);
    let result = wire::filter_rows(
        cfg,
        &thread_id,
        &issued.binding.source,
        &issued.opaque,
        &issued.visibility,
        result,
    )
    .await;
    let oversize = wire::oversize_descriptors(result.as_ref().ok());
    finalize(
        sessions,
        session_id,
        generation,
        structure_version,
        &issued,
        &oversize,
    )
    .await?;
    let rows = result?;
    inherited::validate_rows(&issued.binding.source, &rows)?;
    Ok(wire::response(
        &issued.metadata.session_id,
        &thread_id,
        &issued.metadata,
        &issued,
        &issued.binding.source,
        rows,
        caps,
    ))
}

/// 短锁取票并校验 snapshot/ticket；返回后所有 DB I/O 在锁外。
async fn prepare_ticket_read(
    sessions: &SharedSessions,
    session_id: &str,
    connection_id: &str,
    snapshot_token: &str,
    read_ticket: &str,
) -> Result<(String, u64, u64, IssuedWindowTicket), AcpError> {
    let mut states = sessions.lock().await;
    let state = state::readable_state_mut(&mut states, session_id)?;
    let issued = state
        .history_window
        .current()
        .cloned()
        .ok_or_else(|| ticket_invalid("no active history read ticket"))?;
    if issued.binding.connection_id != connection_id {
        return Err(ticket_invalid(
            "readTicket belongs to a different connection; reopen the window",
        ));
    }
    if read_ticket != issued.opaque.as_str() {
        return Err(ticket_invalid("readTicket is not the active ticket"));
    }
    if snapshot_token != issued.metadata.snapshot_token {
        return Err(snapshot_mismatch(
            "snapshotToken does not belong to the active ticket",
        ));
    }
    // ticket 绑定的是 open 时的 live 事实；当前 state 已变化时必须在进入存储前拒绝。
    if issued.metadata.session_id != state.session_id
        || issued.metadata.session_generation != state.session_generation.to_string()
        || issued.metadata.structure_version != state.structure_version
    {
        return Err(invalidated(
            "ticket generation or structure version is stale",
        ));
    }
    issued
        .binding
        .validate_at(now_unix_ms(), &issued.metadata, &issued.binding.source)
        .map_err(window_error)?;
    Ok((
        state.thread_id.clone(),
        state.session_generation,
        state.structure_version,
        issued,
    ))
}

/// 返回前二次校验；任一不匹配即丢弃已读正文。
async fn finalize(
    sessions: &SharedSessions,
    session_id: &str,
    generation: u64,
    structure_version: u64,
    issued: &IssuedWindowTicket,
    oversize_entries: &[(peri_acp_types::messages::MessageId, u64)],
) -> Result<(), AcpError> {
    let mut states = sessions.lock().await;
    let state = match states.get_mut(session_id) {
        Some(state) => state,
        None => {
            return Err(invalidated("session is no longer live"));
        }
    };
    if state.session_generation != generation || state.structure_version != structure_version {
        return Err(invalidated(
            "session generation or structure version changed",
        ));
    }
    if state.closing {
        return Err(window_error(HistoryWindowError::SessionClosing));
    }
    if state.history_window.rewind_in_progress {
        return Err(window_error(HistoryWindowError::MutationInProgress));
    }
    let current = state
        .history_window
        .current()
        .ok_or_else(|| ticket_invalid("history read ticket was revoked"))?;
    if current.ticket_epoch != issued.ticket_epoch
        || current.opaque.as_str() != issued.opaque.as_str()
        || current.metadata.snapshot_token != issued.metadata.snapshot_token
    {
        return Err(ticket_invalid(
            "history read ticket changed during the read",
        ));
    }
    // 所有校验通过后才登记 descriptor；失败不得污染票据记忆。
    issued.remember_oversize(oversize_entries);
    Ok(())
}

/// `session/history/body`：有界正文分块。ticket 只授权窗口读取，不授权会话访问。
pub(crate) async fn handle_body_chunk(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    params: &Value,
    connection_id: &str,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("missing sessionId"))?
        .to_owned();
    let raw_entry = params
        .get("entryId")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing entryId"))?;
    let entry_id = state::parse_anchor(raw_entry, &session_id)?;
    let offset = match params.get("offset") {
        None => 0,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| invalid("offset must be an unsigned integer"))?,
    };
    let max_bytes = match params.get("maxBytes") {
        None => u64::from(HISTORY_WINDOW_DEFAULT_CHUNK_SIZE),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| invalid("maxBytes must be an unsigned integer"))?,
    };
    let max_bytes = u32::try_from(max_bytes)
        .ok()
        .filter(|value| (1..=HISTORY_WINDOW_MAX_CHUNK_SIZE).contains(value))
        .ok_or_else(|| invalid("maxBytes is out of range"))?;
    let read_ticket = params
        .get("readTicket")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing readTicket"))?;

    let snapshot_token = params
        .get("snapshotToken")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing snapshotToken"))?;
    let (thread_id, generation, structure_version, issued) = prepare_ticket_read(
        sessions,
        &session_id,
        connection_id,
        snapshot_token,
        read_ticket,
    )
    .await?;
    let source = issued.binding.source.clone();
    let tail = issued.binding.committed_tail.clone();
    if source.is_own() && tail.is_none() {
        return Err(anchor_unavailable("snapshot has no committed rows"));
    }
    if !issued.remembers_oversize(entry_id) {
        return Err(anchor_unavailable(
            "entry is not an oversize descriptor of this snapshot",
        ));
    }
    // 记忆溢出后身份集合不再是权威：按稳定 entryId 在固定 snapshot 内重取一页，
    // 只接受 Store 仍判定为 oversized 的条目；不得用位置/索引猜身份。
    if issued.oversize_overflowed()
        && !wire::is_oversize_descriptor(cfg, &thread_id, &source, tail.as_ref(), entry_id).await?
    {
        return Err(anchor_unavailable(
            "entry is not an oversize descriptor of this snapshot",
        ));
    }
    // 已登记 descriptor 不是受众授权；每次 body 请求都重新检查持久内容。
    if !wire::body_visible(
        cfg,
        &thread_id,
        &source,
        tail.as_ref(),
        &issued.opaque,
        &issued.visibility,
        entry_id,
    )
    .await?
    {
        return Err(anchor_unavailable("entry is not visible to this client"));
    }
    let result = cfg
        .session_resources
        .load_history_body_chunk(
            &ThreadId::from(thread_id.clone()),
            source,
            entry_id,
            offset,
            max_bytes,
            tail.as_ref(),
            &issued.opaque,
        )
        .await
        .map_err(resource_window_error);
    finalize(
        sessions,
        &session_id,
        generation,
        structure_version,
        &issued,
        &[],
    )
    .await?;
    let chunk = result?;
    chunk
        .validate(
            &HistoryEntryId::new(&thread_id, entry_id).encode(),
            offset,
            max_bytes,
        )
        .map_err(window_error)?;
    Ok(wire::chunk_value(chunk))
}

/// durable compact 确认后向已打开 V1 窗口的连接发送一次幂等轻量通知。
///
/// 通知丢失不影响提交事实；客户端仍可用 `open(latest)` 对齐。只有当前 session 仍有
/// 活动 window ticket 时才发送，避免把 Peri 私有通知送给未使用 V1 的连接。
pub(crate) async fn notify_commit_if_open(sessions: &SharedSessions, session_id: &str) {
    let (sink, payload) = {
        let mut states = sessions.lock().await;
        let Some(state) = states.get_mut(session_id) else {
            return;
        };
        let Some(ticket) = state.history_window.current() else {
            return;
        };
        let sink = Arc::clone(&ticket.sink);
        let Some(checkpoint_id) = state.history_window.checkpoint_id.clone() else {
            return;
        };
        if state.history_window.last_notified_checkpoint.as_deref() == Some(checkpoint_id.as_str())
        {
            return;
        }
        state.history_window.last_notified_checkpoint = Some(checkpoint_id.clone());
        (
            sink,
            json!({
                "sessionId": state.session_id,
                "sessionGeneration": state.session_generation.to_string(),
                "structureVersion": state.structure_version,
                "checkpointId": checkpoint_id,
                "summaryEntryId": state.history_window.summary_entry_id,
                "committedTail": state
                    .history_window
                    .committed_tail
                    .as_ref()
                    .map(HistoryCommittedTail::encode),
                "hint": { "kind": "open", "anchor": { "kind": "latest" } },
            }),
        )
    };
    // 只投递到签发 ticket 的连接；发送失败不改变 durable 事实，且已按 checkpoint 标记，
    // 避免对不可达连接反复重试。
    if let Err(error) = sink.send(session_id, payload).await {
        tracing::warn!(session_id, error = %error, "history commit notification failed");
    }
}

/// 供 `finish_prompt_turn` / Compact 提交观察者在 durable compact 确认后更新
/// 轻量提交事实。
pub(crate) fn publish_commit(
    state: &mut SessionState,
    checkpoint: Option<&HistoryCheckpoint>,
    tail: Option<&HistoryCommittedTail>,
) {
    state.history_window.publish_commit(checkpoint, tail);
}

/// 供 rewind / persistence-uncertain 路径撤销票据。
pub(crate) fn revoke(state: &mut SessionState) {
    state.history_window.revoke_ticket();
}

pub(crate) fn invalidate(state: &mut SessionState) {
    state.history_window.invalidate_for_uncertain_persistence();
}

pub(crate) fn begin_rewind(state: &mut SessionState) {
    state.history_window.begin_rewind();
}

pub(crate) fn finish_rewind(state: &mut SessionState, applied: bool) {
    state.history_window.finish_rewind(applied);
}
