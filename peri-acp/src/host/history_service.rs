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
    DecimalU64, HistoryAnchor, HistoryBackendError, HistoryBodyChunk, HistoryBodyKind,
    HistoryBodyRequest, HistoryCloseRequest, HistoryClosedKind, HistoryClosedResult,
    HistoryDirection, HistoryLimits, HistoryOpenRequest, HistoryPageData, HistoryPageRequest,
    HistoryRequest, HistorySeekRequest, HistorySnapshotRef, HistorySubItem, HistoryV2Backend,
    HistoryWindowKind, HistoryWindowPage, HISTORY_V2_VERSION,
};
use serde_json::Value;

use crate::transport::types::AcpError;

mod cursor;
mod error;
mod initial;
mod ledger;
mod state;
mod window;

#[cfg(test)]
#[path = "history_service/backend_test.rs"]
mod backend_test;
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
#[path = "history_service/initial_test.rs"]
mod initial_test;
#[cfg(test)]
#[path = "history_service/service_test.rs"]
mod service_test;

use self::cursor::CursorError;
pub(crate) use self::error::{
    backend_unavailable_error, initial_timeout_error, protocol_not_negotiated_error,
};
use self::error::{default_error, invalid_params, invalid_params_message, ServiceFailure};
pub(crate) use self::initial::HistoryInitialContext;
use self::ledger::{Admission, ExpiryReason, Ledger, LedgerLimits};
use self::state::{HistoryClock, SystemClock, WindowAccess, WindowRegistry};

/// `_peri/history` 入站参数上限（设计 §12.4）：在解码为领域请求之前按序列化字节判定。
pub(super) const HISTORY_MAX_PARAMS_BYTES: usize = 64 * 1024;
/// page/seek/body/close/open 的统一总期限（设计 §12.4）；生产路径不可关闭。
pub(super) const HISTORY_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

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
}

impl HistoryV2Service {
    /// 生产构造：系统单调时钟与默认账本预算。
    pub(crate) fn system() -> Self {
        Self::with_options(
            Arc::new(SystemClock::new()),
            LedgerLimits::default(),
            HISTORY_REQUEST_TIMEOUT,
        )
    }

    fn with_options(
        clock: Arc<dyn HistoryClock>,
        limits: LedgerLimits,
        request_timeout: Duration,
    ) -> Self {
        Self {
            state: Mutex::new(ServiceState {
                ledger: Ledger::new(limits),
                windows: WindowRegistry::new(),
            }),
            clock,
            cursor_secret: new_cursor_secret(),
            request_timeout,
        }
    }

    #[cfg(test)]
    fn with_ledger_limits(clock: Arc<dyn HistoryClock>, limits: LedgerLimits) -> Self {
        Self::with_options(clock, limits, HISTORY_REQUEST_TIMEOUT)
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

    /// 处理一个已分流的 `_peri/history` 请求。
    ///
    /// 顺序：入站字节上限 → 解码/校验 → 协商与后端 → 领域处理。整个处理受
    /// `request_timeout` 总期限约束；超时 drop 内部 future 会触发取消守卫。
    pub(crate) async fn handle(
        &self,
        params: &Value,
        options: HistoryServiceRequest,
    ) -> Result<Value, AcpError> {
        if !params_within_limit(params) {
            return Err(invalid_params_message(
                "history request parameters exceed 64 KiB",
            ));
        }
        match tokio::time::timeout(self.request_timeout, self.handle_inner(params, options)).await {
            Ok(result) => result,
            Err(_) => Err(self::error::timeout_error()),
        }
    }

    async fn handle_inner(
        &self,
        params: &Value,
        options: HistoryServiceRequest,
    ) -> Result<Value, AcpError> {
        let request = HistoryRequest::decode(params).map_err(invalid_params)?;
        request.validate(&options.limits).map_err(invalid_params)?;
        if !options.negotiated {
            return Err(protocol_not_negotiated_error());
        }
        let Some(backend) = options.backend else {
            return Err(backend_unavailable_error());
        };
        let fingerprint = ledger::fingerprint(params);
        match request {
            HistoryRequest::Open(request) => {
                self.open(request, fingerprint, backend, &options.limits)
                    .await
            }
            HistoryRequest::Page(request) => {
                self.page(request, fingerprint, backend, &options.limits)
                    .await
            }
            HistoryRequest::Seek(request) => {
                self.seek(request, fingerprint, backend, &options.limits)
                    .await
            }
            HistoryRequest::Body(request) => {
                self.body(request, fingerprint, backend, &options.limits)
                    .await
            }
            HistoryRequest::Close(request) => self.close(request, fingerprint).await,
        }
    }

    async fn open(
        &self,
        request: HistoryOpenRequest,
        fingerprint: [u8; 32],
        backend: Arc<dyn HistoryV2Backend>,
        limits: &HistoryLimits,
    ) -> Result<Value, AcpError> {
        let key = request.request_key.get();
        let attempt = match self.admit(key, fingerprint)? {
            Admission::Replay(value) => return Ok(value),
            Admission::Execute { attempt } => attempt,
        };
        let mut guard = self.attempt_guard(key, attempt, fingerprint);
        let opened = match self
            .begin_window(
                backend.as_ref(),
                &request.session_id,
                &request.anchor,
                request.before,
                request.after,
                request.target_bytes,
                limits.max_windows_per_connection,
            )
            .await
        {
            Ok(opened) => opened,
            Err(failure) => return self.fail_and(&mut guard, failure),
        };
        let page = build_page(&self.cursor_secret, &opened.access, opened.data);
        if !self.issue_body_refs(&opened.access.window_id, &page) {
            self.remove_window(&opened.access.window_id, false);
            return self.fail_and(&mut guard, ServiceFailure::window_expired());
        }
        let value = match bounded_page(page, limits) {
            Ok(value) => value,
            Err(failure) => {
                self.remove_window(&opened.access.window_id, false);
                return self.fail_and(&mut guard, failure);
            }
        };
        self.succeed(
            &mut guard,
            value.clone(),
            Some(opened.access.window_id),
            true,
        );
        Ok(value)
    }

    async fn page(
        &self,
        request: HistoryPageRequest,
        fingerprint: [u8; 32],
        backend: Arc<dyn HistoryV2Backend>,
        limits: &HistoryLimits,
    ) -> Result<Value, AcpError> {
        let key = request.request_key.get();
        let attempt = match self.admit(key, fingerprint)? {
            Admission::Replay(value) => return Ok(value),
            Admission::Execute { attempt } => attempt,
        };
        let mut guard = self.attempt_guard(key, attempt, fingerprint);
        let now = self.clock.now();
        let access = {
            let mut state = self.state.lock();
            resolve_access(
                &mut state,
                &request.session_id,
                &request.window_id,
                &request.snapshot_token,
                &request.read_ticket,
                now,
            )
        };
        let access = match access {
            Ok(access) => access,
            Err(failure) => return self.fail_and(&mut guard, failure),
        };
        let raw_cursor = match &request.cursor {
            Some(value) => match cursor::decode(&self.cursor_secret, &request.window_id, value) {
                Ok(decoded) if decoded.direction == request.direction => decoded.raw,
                Ok(_) | Err(CursorError::Invalid) => {
                    return self.fail_and(&mut guard, ServiceFailure::invalid_cursor());
                }
                Err(CursorError::CrossWindow) => {
                    return self.fail_and(&mut guard, ServiceFailure::cross_window_cursor());
                }
            },
            None => {
                let home = match request.direction {
                    HistoryDirection::Before => access.before_home.clone(),
                    HistoryDirection::After => access.after_home.clone(),
                };
                match home {
                    Some(raw) => raw,
                    None => {
                        return self.fail_and(&mut guard, ServiceFailure::invalid_cursor());
                    }
                }
            }
        };
        let data = match backend
            .page(
                &access.snapshot,
                &raw_cursor,
                request.direction,
                request.limit,
            )
            .await
        {
            Ok(data) => data,
            Err(error) => {
                if error == HistoryBackendError::SnapshotInvalidated {
                    self.remove_window(&request.window_id, false);
                }
                return self.fail_and(&mut guard, ServiceFailure::from_backend(error));
            }
        };
        let now = self.clock.now();
        let response = {
            let mut state = self.state.lock();
            let access = match resolve_access(
                &mut state,
                &request.session_id,
                &request.window_id,
                &request.snapshot_token,
                &request.read_ticket,
                now,
            ) {
                Ok(access) => access,
                Err(failure) => {
                    drop(state);
                    return self.fail_and(&mut guard, failure);
                }
            };
            if snapshot_changed(&access.snapshot, &data) {
                state.windows.remove(&request.window_id, now, false);
                drop(state);
                return self.fail_and(&mut guard, ServiceFailure::snapshot_invalidated());
            }
            let page = build_page(&self.cursor_secret, &access, data);
            if !state
                .windows
                .issue_body_refs(&request.window_id, page_body_refs(&page))
            {
                drop(state);
                return self.fail_and(&mut guard, ServiceFailure::window_expired());
            }
            page
        };
        let value = match bounded_page(response, limits) {
            Ok(value) => value,
            Err(failure) => return self.fail_and(&mut guard, failure),
        };
        self.succeed(&mut guard, value.clone(), Some(request.window_id), true);
        Ok(value)
    }

    async fn seek(
        &self,
        request: HistorySeekRequest,
        fingerprint: [u8; 32],
        backend: Arc<dyn HistoryV2Backend>,
        limits: &HistoryLimits,
    ) -> Result<Value, AcpError> {
        let key = request.request_key.get();
        let attempt = match self.admit(key, fingerprint)? {
            Admission::Replay(value) => return Ok(value),
            Admission::Execute { attempt } => attempt,
        };
        let mut guard = self.attempt_guard(key, attempt, fingerprint);
        let HistoryAnchor::Entry(anchor) = request.anchor.clone() else {
            return Err(invalid_params_message("seek anchor must be an entry"));
        };
        let now = self.clock.now();
        let access = {
            let mut state = self.state.lock();
            resolve_access(
                &mut state,
                &request.session_id,
                &request.window_id,
                &request.snapshot_token,
                &request.read_ticket,
                now,
            )
        };
        let access = match access {
            Ok(access) => access,
            Err(failure) => return self.fail_and(&mut guard, failure),
        };
        let data = match backend
            .seek(&access.snapshot, &anchor, request.before, request.after)
            .await
        {
            Ok(data) => data,
            Err(error) => {
                if error == HistoryBackendError::SnapshotInvalidated {
                    self.remove_window(&request.window_id, false);
                }
                return self.fail_and(&mut guard, ServiceFailure::from_backend(error));
            }
        };
        let now = self.clock.now();
        let response = {
            let mut state = self.state.lock();
            let access = match resolve_access(
                &mut state,
                &request.session_id,
                &request.window_id,
                &request.snapshot_token,
                &request.read_ticket,
                now,
            ) {
                Ok(access) => access,
                Err(failure) => {
                    drop(state);
                    return self.fail_and(&mut guard, failure);
                }
            };
            if snapshot_changed(&access.snapshot, &data) {
                state.windows.remove(&request.window_id, now, false);
                drop(state);
                return self.fail_and(&mut guard, ServiceFailure::snapshot_invalidated());
            }
            state.windows.set_homes(
                &request.window_id,
                data.before_cursor.clone(),
                data.after_cursor.clone(),
            );
            let page = build_page(&self.cursor_secret, &access, data);
            if !state
                .windows
                .issue_body_refs(&request.window_id, page_body_refs(&page))
            {
                drop(state);
                return self.fail_and(&mut guard, ServiceFailure::window_expired());
            }
            page
        };
        let value = match bounded_page(response, limits) {
            Ok(value) => value,
            Err(failure) => return self.fail_and(&mut guard, failure),
        };
        self.succeed(&mut guard, value.clone(), Some(request.window_id), true);
        Ok(value)
    }

    async fn body(
        &self,
        request: HistoryBodyRequest,
        fingerprint: [u8; 32],
        backend: Arc<dyn HistoryV2Backend>,
        limits: &HistoryLimits,
    ) -> Result<Value, AcpError> {
        if request.max_bytes > u64::from(limits.max_body_chunk_bytes) {
            return Err(invalid_params_message(
                "maxBytes exceeds the negotiated body chunk limit",
            ));
        }
        let key = request.request_key.get();
        let attempt = match self.admit(key, fingerprint)? {
            Admission::Replay(value) => return Ok(value),
            Admission::Execute { attempt } => attempt,
        };
        let mut guard = self.attempt_guard(key, attempt, fingerprint);
        let now = self.clock.now();
        let access = {
            let mut state = self.state.lock();
            match resolve_access(
                &mut state,
                &request.session_id,
                &request.window_id,
                &request.snapshot_token,
                &request.read_ticket,
                now,
            ) {
                Ok(access)
                    if state
                        .windows
                        .body_ref_issued(&request.window_id, &request.body_ref) =>
                {
                    Ok(access)
                }
                // bodyRef 不是 MAC cursor：只接受本窗口实际交付过的引用。
                // 不在集合内与条目不存在返回同一错误，不泄露存在性。
                Ok(_) => Err(ServiceFailure::from_backend(HistoryBackendError::NotFound)),
                Err(failure) => Err(failure),
            }
        };
        let access = match access {
            Ok(access) => access,
            Err(failure) => return self.fail_and(&mut guard, failure),
        };
        let slice = match backend
            .body(
                &access.snapshot,
                &request.body_ref,
                request.offset.get(),
                request.max_bytes,
            )
            .await
        {
            Ok(slice) => slice,
            Err(error) => {
                if error == HistoryBackendError::SnapshotInvalidated {
                    self.remove_window(&request.window_id, false);
                }
                return self.fail_and(&mut guard, ServiceFailure::from_backend(error));
            }
        };
        let slice = match slice {
            slice
                if slice.body_ref == request.body_ref
                    && slice.offset == request.offset
                    && valid_body_shape(&slice) =>
            {
                slice
            }
            _ => {
                return self.fail_and(&mut guard, ServiceFailure::source_unavailable());
            }
        };
        let chunk = HistoryBodyChunk {
            version: HISTORY_V2_VERSION,
            kind: HistoryBodyKind::Body,
            session_id: request.session_id.clone(),
            window_id: request.window_id.clone(),
            snapshot_token: request.snapshot_token.clone(),
            body_ref: slice.body_ref,
            content_version: slice.content_version,
            representation: slice.representation,
            encoding: slice.encoding,
            offset: slice.offset,
            data: slice.data,
            next_offset: slice.next_offset,
            complete: slice.complete,
            total_bytes: slice.total_bytes,
        };
        if chunk.validate_shape().is_err() {
            return self.fail_and(&mut guard, ServiceFailure::source_unavailable());
        }
        let value = serde_json::to_value(&chunk)
            .map_err(|_| invalid_params_message("body chunk could not be serialized"))?;
        let now = self.clock.now();
        {
            let mut state = self.state.lock();
            if let Err(failure) = resolve_access(
                &mut state,
                &request.session_id,
                &request.window_id,
                &request.snapshot_token,
                &request.read_ticket,
                now,
            ) {
                drop(state);
                return self.fail_and(&mut guard, failure);
            }
        }
        let value = match bounded_value(value, limits) {
            Ok(value) => value,
            Err(failure) => return self.fail_and(&mut guard, failure),
        };
        self.succeed(&mut guard, value.clone(), Some(request.window_id), true);
        Ok(value)
    }

    async fn close(
        &self,
        request: HistoryCloseRequest,
        fingerprint: [u8; 32],
    ) -> Result<Value, AcpError> {
        let key = request.request_key.get();
        let attempt = match self.admit(key, fingerprint)? {
            Admission::Replay(value) => return Ok(value),
            Admission::Execute { attempt } => attempt,
        };
        let mut guard = self.attempt_guard(key, attempt, fingerprint);
        let now = self.clock.now();
        {
            let mut state = self.state.lock();
            state.windows.prune(now);
            if let Some(record) = state.windows.get(&request.window_id) {
                if record.session_id != request.session_id {
                    drop(state);
                    return self.fail_and(&mut guard, ServiceFailure::permission_denied());
                }
                if record.snapshot_token != request.snapshot_token {
                    drop(state);
                    return self.fail_and(&mut guard, ServiceFailure::snapshot_invalidated());
                }
                if record.read_ticket != request.read_ticket {
                    drop(state);
                    return self.fail_and(&mut guard, ServiceFailure::ticket_invalid());
                }
                state.windows.remove(&request.window_id, now, true);
            }
            // 不存在或已关闭：幂等返回同形结果，不泄露其他主体窗口。
        }
        let result = HistoryClosedResult {
            version: HISTORY_V2_VERSION,
            kind: HistoryClosedKind::Closed,
            window_id: request.window_id.clone(),
        };
        let value = serde_json::to_value(&result)
            .map_err(|_| invalid_params_message("close result could not be serialized"))?;
        self.succeed(&mut guard, value.clone(), Some(request.window_id), false);
        Ok(value)
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

    fn succeed(
        &self,
        guard: &mut AttemptGuard<'_>,
        response: Value,
        window_id: Option<String>,
        requires_window: bool,
    ) {
        let now = self.clock.now();
        self.state.lock().ledger.finish_success(
            guard.key,
            guard.attempt,
            guard.fingerprint,
            response,
            window_id,
            requires_window,
            now,
        );
        guard.disarm();
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
    pub(super) fn window_count(&self) -> usize {
        self.state.lock().windows.count()
    }

    #[cfg(test)]
    pub(super) fn opening_window_count(&self) -> usize {
        self.state.lock().windows.opening_count()
    }

    #[cfg(test)]
    pub(super) fn ledger_entry_count(&self) -> usize {
        self.state.lock().ledger.entry_count()
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
