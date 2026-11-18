//! 连接级 History V2 协议服务（H2）。
//!
//! 职责边界：
//! - **准入**：严格解码/校验 `_peri/history`，未协商或后端缺失时显式失败；
//! - **幂等**：连接级 requestKey 账本，高水位常驻、成功响应可原样重放；
//! - **窗口**：每连接独立窗口注册表、票据、home cursor 与期限；
//! - **预算**：页面/正文序列化后复核 `maxResponseBytes`；
//! - **锁内不 await**：所有后端调用在锁外，返回后重新短锁校验窗口与 revision。
//!
//! 本模块不读取 SQLite、不改 TUI；后端口径由 [`HistoryV2Backend`] 提供，本地 own 实现
//! 见 `history_v2_adapter`，`session/load` 的 wire 准入见 [`super::history_v2_load`]。

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use peri_acp_types::history_v2::{
    DecimalU64, HistoryDirection, HistoryLimits, HistoryPageData, HistorySnapshotRef,
    HistorySubItem, HistoryV2Backend, HistoryWindowKind, HistoryWindowPage, HISTORY_V2_VERSION,
};
use serde_json::Value;

use crate::transport::types::AcpError;

mod cursor;
mod error;
mod initial;
mod ledger;
mod operations;
mod state;
mod window;

#[cfg(test)]
#[path = "history_service/backend_test.rs"]
pub(crate) mod backend_test;
#[cfg(test)]
#[path = "history_service/body_test.rs"]
mod body_test;
#[cfg(test)]
#[path = "history_service/cancel_test.rs"]
mod cancel_test;
#[cfg(test)]
#[path = "history_service/cursor_test.rs"]
mod cursor_test;
#[cfg(test)]
#[path = "history_service/initial_replacement_test.rs"]
mod initial_replacement_test;
#[cfg(test)]
#[path = "history_service/initial_test.rs"]
mod initial_test;
#[cfg(test)]
#[path = "history_service/service_test.rs"]
mod service_test;

pub(crate) use self::error::{
    backend_unavailable_error, initial_timeout_error, protocol_not_negotiated_error,
};
use self::error::{default_error, ServiceFailure};
pub(crate) use self::initial::HistoryInitialContext;
use self::ledger::{Admission, ExpiryReason, Ledger, LedgerLimits};
pub(crate) use self::operations::{HistoryAdmission, HistoryExecution};
use self::state::{HistoryClock, SystemClock, WindowAccess, WindowRegistry};

/// `_peri/history` 入站参数上限（设计 §12.4）：在解码为领域请求之前按序列化字节判定。
pub(super) const HISTORY_MAX_PARAMS_BYTES: usize = 64 * 1024;
/// page/seek/body/close/open 的统一总期限（设计 §12.4）；生产路径不可关闭。
pub(super) const HISTORY_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// initial 的独立总期限：与 `session/load` 的冷启动期限一致，不使用分页的 10 秒。
pub(super) const HISTORY_INITIAL_TIMEOUT: Duration = Duration::from_secs(60);
/// 同时执行的历史读（open/page/seek/body）上限；close 走快速路径不占配额。
///
/// 配额耗尽时接收循环**不等待**：立即把该 attempt 标记为可重试并返回 `capacity_exceeded`。
pub(crate) const HISTORY_MAX_IN_FLIGHT: usize = 4;

/// 路由层交给服务的连接事实与后端。
pub(crate) struct HistoryServiceRequest {
    negotiated: bool,
    backend: Option<Arc<dyn HistoryV2Backend>>,
    limits: HistoryLimits,
}

impl HistoryServiceRequest {
    pub(crate) fn new(
        negotiated: bool,
        backend: Option<Arc<dyn HistoryV2Backend>>,
        limits: HistoryLimits,
    ) -> Self {
        Self {
            negotiated,
            backend,
            limits,
        }
    }
}

/// 每连接随机 secret：两次 UUIDv4 的 16 字节拼接。不落盘、不写日志。
fn new_cursor_secret() -> [u8; 32] {
    let mut secret = [0u8; 32];
    secret[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    secret[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    secret
}

/// 计数 writer：只统计序列化字节，不复制参数，避免为 64 KiB 检查再分配一份缓冲。
struct ByteCounter {
    len: usize,
}

impl std::io::Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.len = self.len.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 入站参数上限（设计 §12.4）。超限在 `HistoryRequest::decode` 之前拒绝。
fn params_within_limit(params: &Value) -> bool {
    let mut counter = ByteCounter { len: 0 };
    match serde_json::to_writer(&mut counter, params) {
        Ok(()) => counter.len <= HISTORY_MAX_PARAMS_BYTES,
        Err(_) => false,
    }
}

struct ServiceState {
    ledger: Ledger,
    windows: WindowRegistry,
}

/// `Opening` 占位的 RAII 清理器（设计 §12.2）。
///
/// 请求 future 被 drop（连接取消、总期限耗尽、调用方丢弃）时同步移除仍处于
/// `Opening` 的占位；成功提交或明确失败时 disarm。Drop 中不 await。
struct OpeningGuard<'a> {
    state: &'a Mutex<ServiceState>,
    window_id: String,
    armed: bool,
}

impl<'a> OpeningGuard<'a> {
    fn new(state: &'a Mutex<ServiceState>, window_id: String) -> Self {
        Self {
            state,
            window_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OpeningGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.state.lock().windows.remove_if_opening(&self.window_id);
    }
}

/// 幂等账本 `InProgress` 记录的 RAII 清理器（设计 §12.2）。
///
/// 取消时把**仍属于本 attempt** 的记录标记为 `Retryable`，使同键可以立即重试；
/// 新 attempt 已接管时 `finish_failure` 的 attempt/fingerprint 校验会拒绝覆盖。
struct AttemptGuard<'a> {
    state: &'a Mutex<ServiceState>,
    clock: &'a dyn HistoryClock,
    key: u64,
    attempt: u64,
    fingerprint: [u8; 32],
    armed: bool,
}

impl AttemptGuard<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AttemptGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let now = self.clock.now();
        self.state
            .lock()
            .ledger
            .finish_failure(self.key, self.attempt, self.fingerprint, now);
    }
}

/// 每连接一个；所有可变状态在短锁内访问。
pub(crate) struct HistoryV2Service {
    state: Mutex<ServiceState>,
    clock: Arc<dyn HistoryClock>,
    /// 每连接随机生成；只用于 cursor MAC，不落盘、不写日志。
    cursor_secret: [u8; 32],
    request_timeout: Duration,
    /// initial 的独立总期限（冷启动允许比分页更久）。
    initial_timeout: Duration,
    /// 在途历史读配额（`HISTORY_MAX_IN_FLIGHT`）；只由连接层 `try_acquire` 使用。
    in_flight: Arc<tokio::sync::Semaphore>,
}

impl HistoryV2Service {
    /// 生产构造：系统单调时钟与默认账本预算。
    pub(crate) fn system() -> Self {
        Self::with_options(
            Arc::new(SystemClock::new()),
            LedgerLimits::default(),
            HISTORY_REQUEST_TIMEOUT,
            HISTORY_INITIAL_TIMEOUT,
        )
    }

    fn with_options(
        clock: Arc<dyn HistoryClock>,
        limits: LedgerLimits,
        request_timeout: Duration,
        initial_timeout: Duration,
    ) -> Self {
        Self {
            state: Mutex::new(ServiceState {
                ledger: Ledger::new(limits),
                windows: WindowRegistry::new(),
            }),
            clock,
            cursor_secret: new_cursor_secret(),
            request_timeout,
            initial_timeout,
            in_flight: Arc::new(tokio::sync::Semaphore::new(HISTORY_MAX_IN_FLIGHT)),
        }
    }

    #[cfg(test)]
    fn with_ledger_limits(clock: Arc<dyn HistoryClock>, limits: LedgerLimits) -> Self {
        Self::with_options(
            clock,
            limits,
            HISTORY_REQUEST_TIMEOUT,
            HISTORY_INITIAL_TIMEOUT,
        )
    }

    /// 测试构造：同时注入短的分页期限与 initial 期限。
    #[cfg(test)]
    fn with_ledger_limits_and_timeout(
        clock: Arc<dyn HistoryClock>,
        limits: LedgerLimits,
        timeout: Duration,
    ) -> Self {
        Self::with_options(clock, limits, timeout, timeout)
    }

    /// 会话级拆除：移除本连接上该会话的全部窗口，并记录 closed 身份。
    ///
    /// 只清理 History V2 状态，不触碰会话装配/执行；`session/close` 与
    /// `session/delete` 成功后在 host 处理路径调用。其他会话窗口不受影响。
    pub(crate) fn close_session(&self, session_id: &str) {
        let now = self.clock.now();
        self.state
            .lock()
            .windows
            .remove_session(session_id, now, true);
    }

    /// 处理一个已分流的 `_peri/history` 请求（服务层测试入口）。
    ///
    /// 生产连接循环拆成两步：接收循环内同步 [`Self::admit_request`]，随后在受配额与
    /// 任务所有权约束的后台任务里 [`Self::execute`]。这里保留原子组合语义供测试使用。
    #[cfg(test)]
    pub(crate) async fn handle(
        &self,
        params: &Value,
        options: HistoryServiceRequest,
    ) -> Result<Value, AcpError> {
        match self.admit_request(params, options)? {
            HistoryAdmission::Replay(value) => Ok(value),
            HistoryAdmission::Execute(execution) => self.execute(*execution).await,
        }
    }

    /// 尝试取得一个在途执行槽位；饱和时立即返回 `None`，由调用方拒绝本次 attempt。
    pub(crate) fn try_acquire_in_flight(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.in_flight).try_acquire_owned().ok()
    }

    fn admit(&self, key: u64, fingerprint: [u8; 32]) -> Result<Admission, AcpError> {
        self.admit_with(key, fingerprint, ExpiryReason::OperationExpired)
            .map_err(default_error)
    }

    fn admit_with(
        &self,
        key: u64,
        fingerprint: [u8; 32],
        expiry: ExpiryReason,
    ) -> Result<Admission, ServiceFailure> {
        let now = self.clock.now();
        let mut state = self.state.lock();
        let ServiceState { ledger, windows } = &mut *state;
        ledger.prune(now);
        windows.prune(now);
        ledger.begin(key, fingerprint, now, expiry, |window_id| {
            windows.is_live(window_id)
        })
    }

    fn attempt_guard<'a>(
        &'a self,
        key: u64,
        attempt: u64,
        fingerprint: [u8; 32],
    ) -> AttemptGuard<'a> {
        AttemptGuard {
            state: &self.state,
            clock: &*self.clock,
            key,
            attempt,
            fingerprint,
            armed: true,
        }
    }

    fn fail_and<T>(
        &self,
        guard: &mut AttemptGuard<'_>,
        failure: ServiceFailure,
    ) -> Result<T, AcpError> {
        let now = self.clock.now();
        self.state
            .lock()
            .ledger
            .finish_failure(guard.key, guard.attempt, guard.fingerprint, now);
        guard.disarm();
        Err(default_error(failure))
    }

    fn remove_window(&self, window_id: &str, record_closed: bool) {
        let now = self.clock.now();
        self.state
            .lock()
            .windows
            .remove(window_id, now, record_closed);
    }

    /// 把一页实际交付的 opaque `bodyRef` 记入窗口；窗口已失效时返回 false。
    fn issue_body_refs(&self, window_id: &str, page: &HistoryWindowPage) -> bool {
        let body_refs = page_body_refs(page);
        if body_refs.is_empty() {
            return true;
        }
        let now = self.clock.now();
        let mut state = self.state.lock();
        state.windows.prune(now);
        state.windows.issue_body_refs(window_id, body_refs)
    }

    #[cfg(test)]
    pub(crate) fn window_count(&self) -> usize {
        self.state.lock().windows.count()
    }

    #[cfg(test)]
    pub(crate) fn opening_window_count(&self) -> usize {
        self.state.lock().windows.opening_count()
    }

    #[cfg(test)]
    pub(super) fn ledger_entry_count(&self) -> usize {
        self.state.lock().ledger.entry_count()
    }

    #[cfg(test)]
    pub(crate) fn available_in_flight(&self) -> usize {
        self.in_flight.available_permits()
    }
}

/// 校验窗口 + 票据 + 会话，并刷新空闲期限。
fn resolve_access(
    state: &mut ServiceState,
    session_id: &str,
    window_id: &str,
    snapshot_token: &str,
    read_ticket: &str,
    now: Duration,
) -> Result<WindowAccess, ServiceFailure> {
    state.windows.prune(now);
    let closed = state.windows.was_closed(window_id);
    let Some(record) = state.windows.get(window_id) else {
        return Err(if closed {
            ServiceFailure::window_closed()
        } else {
            ServiceFailure::window_expired()
        });
    };
    if record.session_id != session_id {
        return Err(ServiceFailure::permission_denied());
    }
    if record.snapshot_token != snapshot_token {
        return Err(ServiceFailure::snapshot_invalidated());
    }
    if record.read_ticket != read_ticket {
        return Err(ServiceFailure::ticket_invalid());
    }
    let Some(snapshot) = record.snapshot.clone() else {
        return Err(ServiceFailure::window_expired());
    };
    let access = WindowAccess {
        window_id: record.window_id.clone(),
        snapshot_token: record.snapshot_token.clone(),
        read_ticket: record.read_ticket.clone(),
        session_id: record.session_id.clone(),
        snapshot,
        before_home: record.before_home.clone(),
        after_home: record.after_home.clone(),
        cancel: record.cancel_handle(),
    };
    state.windows.touch(window_id, now);
    Ok(access)
}

fn window_access(state: &ServiceState, window_id: &str) -> Option<WindowAccess> {
    let record = state.windows.get(window_id)?;
    let snapshot = record.snapshot.clone()?;
    Some(WindowAccess {
        window_id: record.window_id.clone(),
        snapshot_token: record.snapshot_token.clone(),
        read_ticket: record.read_ticket.clone(),
        session_id: record.session_id.clone(),
        snapshot,
        before_home: record.before_home.clone(),
        after_home: record.after_home.clone(),
        cancel: record.cancel_handle(),
    })
}

fn snapshot_changed(snapshot: &HistorySnapshotRef, data: &HistoryPageData) -> bool {
    snapshot.history_revision != data.history_revision
        || snapshot.projection_version != data.projection_version
        || snapshot.committed_tail != data.committed_tail
}

/// 未完成块必须严格推进且不能提前 EOF；完成块不得携带 `nextOffset`。
fn valid_body_shape(slice: &peri_acp_types::history_v2::HistoryBodySlice) -> bool {
    match (slice.complete, slice.next_offset) {
        (true, None) => slice.offset.get() <= slice.total_bytes.get(),
        (false, Some(next)) => {
            !slice.data.is_empty() && next > slice.offset && next.get() < slice.total_bytes.get()
        }
        _ => false,
    }
}

/// 一页交付里所有 opaque 子项的 `bodyRef`；其余子项没有外置正文。
fn page_body_refs(page: &HistoryWindowPage) -> Vec<String> {
    page.items
        .iter()
        .flat_map(|entry| entry.sub_items.iter())
        .filter_map(|sub_item| match sub_item {
            HistorySubItem::Opaque(opaque) => Some(opaque.body.body_ref.clone()),
            _ => None,
        })
        .collect()
}

fn build_page(
    secret: &[u8; 32],
    access: &WindowAccess,
    data: HistoryPageData,
) -> HistoryWindowPage {
    HistoryWindowPage {
        version: HISTORY_V2_VERSION,
        kind: HistoryWindowKind::Window,
        session_id: access.session_id.clone(),
        window_id: access.window_id.clone(),
        snapshot_token: access.snapshot_token.clone(),
        // wire 的 historyRevision 是**快照**的存储序 tail position，不是后端页的自报值；
        // snapshot_changed 已在装配前复核两者相等。
        history_revision: access.snapshot.history_revision,
        committed_tail: access
            .snapshot
            .committed_tail
            .as_ref()
            .map(|tail| DecimalU64::new(tail.position)),
        projection_version: data.projection_version,
        read_ticket: access.read_ticket.clone(),
        before_cursor: data
            .before_cursor
            .as_deref()
            .map(|raw| cursor::encode(secret, &access.window_id, HistoryDirection::Before, raw)),
        after_cursor: data
            .after_cursor
            .as_deref()
            .map(|raw| cursor::encode(secret, &access.window_id, HistoryDirection::After, raw)),
        items: data.entries,
        coverage: data.coverage,
        progress: data.progress,
        expires_at: None,
    }
}

fn bounded_page(page: HistoryWindowPage, limits: &HistoryLimits) -> Result<Value, ServiceFailure> {
    let value = serde_json::to_value(&page).map_err(|_| ServiceFailure::source_unavailable())?;
    bounded_value(value, limits)
}

fn bounded_value(value: Value, limits: &HistoryLimits) -> Result<Value, ServiceFailure> {
    let bytes = serde_json::to_vec(&value).map_err(|_| ServiceFailure::source_unavailable())?;
    if bytes.len() as u64 > limits.max_response_bytes {
        return Err(ServiceFailure::budget_exceeded());
    }
    Ok(value)
}
