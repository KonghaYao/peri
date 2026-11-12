//! History V2 请求的准入与执行（FIX-04）。
//!
//! 拆成两个阶段：
//! - [`HistoryV2Service::admit_request`]：同步完成 64 KiB 检查、解码、参数/协商/后端校验与
//!   幂等账本登记。必须在连接接收循环内调用，保证 requestKey 高水位按接收顺序推进；
//! - [`HistoryV2Service::execute`]：锁外 await 后端，返回后在**单次短锁**内复核窗口、
//!   快照、票据、请求尝试与关闭状态，再写成功账本。失败不发布成功响应。
//!
//! 准入本身不 await 后端；执行任务由连接层的在途配额与任务所有权约束。

use std::sync::Arc;

use peri_acp_types::history_v2::{
    HistoryAnchor, HistoryBackendError, HistoryBodyChunk, HistoryBodyKind, HistoryBodyRequest,
    HistoryCloseRequest, HistoryClosedKind, HistoryClosedResult, HistoryDirection, HistoryLimits,
    HistoryOpenRequest, HistoryPageData, HistoryPageRequest, HistoryRequest, HistorySeekRequest,
    HistoryV2Backend, HistoryWindowPage, HISTORY_V2_VERSION,
};
use serde_json::Value;

use super::cursor::CursorError;
use super::error::{
    default_error, invalid_params, invalid_params_message, timeout_error, ServiceFailure,
};
use super::ledger::{fingerprint, Admission};
use super::state::WindowCancel;
use super::{
    bounded_page, bounded_value, build_page, page_body_refs, params_within_limit, resolve_access,
    snapshot_changed, valid_body_shape, AttemptGuard, HistoryServiceRequest, HistoryV2Service,
};
use crate::transport::types::AcpError;

/// 准入结果：要么原样重放缓存响应，要么得到一个自包含的执行对象。
pub(crate) enum HistoryAdmission {
    Replay(Value),
    Execute(Box<HistoryExecution>),
}

/// 已登记、等待执行的一次历史操作。
///
/// 持有请求、attempt、requestKey 指纹、后端与协商预算；不持有 `ServiceState` 锁。
pub(crate) struct HistoryExecution {
    key: u64,
    attempt: u64,
    fingerprint: [u8; 32],
    backend: Arc<dyn HistoryV2Backend>,
    limits: HistoryLimits,
    action: HistoryAction,
}

enum HistoryAction {
    Open(HistoryOpenRequest),
    Page(HistoryPageRequest),
    Seek(HistorySeekRequest),
    Body(HistoryBodyRequest),
    Close(HistoryCloseRequest),
}

impl HistoryAction {
    fn request_key(&self) -> u64 {
        match self {
            Self::Open(request) => request.request_key.get(),
            Self::Page(request) => request.request_key.get(),
            Self::Seek(request) => request.request_key.get(),
            Self::Body(request) => request.request_key.get(),
            Self::Close(request) => request.request_key.get(),
        }
    }
}

impl HistoryExecution {
    /// close 是快速路径：接收循环内直接执行，不占用在途读配额。
    pub(crate) fn is_close(&self) -> bool {
        matches!(self.action, HistoryAction::Close(_))
    }
}

impl HistoryV2Service {
    /// 同步准入：不得 await 后端。
    ///
    /// 顺序：入站字节上限 → 解码/参数校验 → 协商/后端 → requestKey 指纹与账本登记。
    pub(crate) fn admit_request(
        &self,
        params: &Value,
        options: HistoryServiceRequest,
    ) -> Result<HistoryAdmission, AcpError> {
        if !params_within_limit(params) {
            return Err(invalid_params_message(
                "history request parameters exceed 64 KiB",
            ));
        }
        let request = HistoryRequest::decode(params).map_err(invalid_params)?;
        request.validate(&options.limits).map_err(invalid_params)?;
        if !options.negotiated {
            return Err(super::protocol_not_negotiated_error());
        }
        let Some(backend) = options.backend else {
            return Err(super::backend_unavailable_error());
        };
        // 形状/参数错误必须在占用 requestKey 之前返回；见各 action 的前置校验。
        let action = match request {
            HistoryRequest::Open(request) => HistoryAction::Open(request),
            HistoryRequest::Page(request) => HistoryAction::Page(request),
            HistoryRequest::Seek(request) => {
                if !matches!(request.anchor, HistoryAnchor::Entry(_)) {
                    return Err(invalid_params_message("seek anchor must be an entry"));
                }
                HistoryAction::Seek(request)
            }
            HistoryRequest::Body(request) => {
                if request.max_bytes > u64::from(options.limits.max_body_chunk_bytes) {
                    return Err(invalid_params_message(
                        "maxBytes exceeds the negotiated body chunk limit",
                    ));
                }
                HistoryAction::Body(request)
            }
            HistoryRequest::Close(request) => HistoryAction::Close(request),
        };
        let key = action.request_key();
        let fingerprint = fingerprint(params);
        match self.admit(key, fingerprint)? {
            Admission::Replay(value) => Ok(HistoryAdmission::Replay(value)),
            Admission::Execute { attempt } => {
                Ok(HistoryAdmission::Execute(Box::new(HistoryExecution {
                    key,
                    attempt,
                    fingerprint,
                    backend,
                    limits: options.limits,
                    action,
                })))
            }
        }
    }

    /// 标记一次已准入但未能启动的执行（配额饱和/宿主关闭）为可重试。
    pub(crate) fn reject_busy(&self, execution: &HistoryExecution) -> AcpError {
        self.mark_retryable(execution);
        default_error(ServiceFailure::busy())
    }

    /// 连接已取消：标记可重试并返回取消错误（调用方不得发送成功响应）。
    pub(crate) fn reject_cancelled(&self, execution: &HistoryExecution) -> AcpError {
        self.mark_retryable(execution);
        AcpError::new(-32800, "request cancelled")
    }

    fn mark_retryable(&self, execution: &HistoryExecution) {
        let now = self.clock.now();
        self.state.lock().ledger.finish_failure(
            execution.key,
            execution.attempt,
            execution.fingerprint,
            now,
        );
    }

    /// 执行已准入操作。总期限覆盖后端读取与提交；超时 drop 内部 future 会触发守卫，
    /// 把仍属于本 attempt 的账本记录标记为可重试。
    pub(crate) async fn execute(&self, execution: HistoryExecution) -> Result<Value, AcpError> {
        match tokio::time::timeout(self.request_timeout, self.execute_inner(execution)).await {
            Ok(result) => result,
            Err(_) => Err(timeout_error()),
        }
    }

    async fn execute_inner(&self, execution: HistoryExecution) -> Result<Value, AcpError> {
        let mut guard = self.attempt_guard(execution.key, execution.attempt, execution.fingerprint);
        let HistoryExecution {
            backend,
            limits,
            action,
            ..
        } = execution;
        match action {
            HistoryAction::Open(request) => {
                self.execute_open(request, &mut guard, backend, &limits)
                    .await
            }
            HistoryAction::Page(request) => {
                self.execute_page(request, &mut guard, backend, &limits)
                    .await
            }
            HistoryAction::Seek(request) => {
                self.execute_seek(request, &mut guard, backend, &limits)
                    .await
            }
            HistoryAction::Body(request) => {
                self.execute_body(request, &mut guard, backend, &limits)
                    .await
            }
            HistoryAction::Close(request) => self.execute_close(request, &mut guard),
        }
    }

    async fn execute_open(
        &self,
        request: HistoryOpenRequest,
        guard: &mut AttemptGuard<'_>,
        backend: Arc<dyn HistoryV2Backend>,
        limits: &HistoryLimits,
    ) -> Result<Value, AcpError> {
        let opened = match self
            .begin_window(
                backend.as_ref(),
                &request.session_id,
                &request.anchor,
                request.before,
                request.after,
                request.target_bytes,
                limits.max_windows_per_connection,
                false,
            )
            .await
        {
            Ok(opened) => opened,
            Err(failure) => return self.fail_and(guard, failure),
        };
        match self.commit_open(guard, opened, limits) {
            Ok(value) => Ok(value),
            Err(failure) => self.fail_and(guard, failure),
        }
    }

    /// open 的单锁提交：窗口已由 `begin_window` 注册；这里复核尝试后装配并写成功账本。
    /// 失败时移除未发布的窗口，不留下成功记录。
    fn commit_open(
        &self,
        guard: &mut AttemptGuard<'_>,
        opened: super::window::OpenedWindow,
        limits: &HistoryLimits,
    ) -> Result<Value, ServiceFailure> {
        let now = self.clock.now();
        let mut state = self.state.lock();
        state.windows.prune(now);
        if state.windows.get(&opened.access.window_id).is_none() {
            // 准备期间会话被关闭：不得为已移除的窗口发布成功页。
            return Err(ServiceFailure::window_expired());
        }
        if !state
            .ledger
            .is_current(guard.key, guard.attempt, guard.fingerprint)
        {
            state.windows.discard(&opened.access.window_id);
            return Err(ServiceFailure::operation_expired());
        }
        let page = build_page(&self.cursor_secret, &opened.access, opened.data);
        let body_refs = page_body_refs(&page);
        if !body_refs.is_empty()
            && !state
                .windows
                .issue_body_refs(&opened.access.window_id, body_refs)
        {
            state.windows.discard(&opened.access.window_id);
            return Err(ServiceFailure::window_expired());
        }
        let value = match bounded_page(page, limits) {
            Ok(value) => value,
            Err(failure) => {
                state.windows.discard(&opened.access.window_id);
                return Err(failure);
            }
        };
        state.ledger.finish_success(
            guard.key,
            guard.attempt,
            guard.fingerprint,
            value.clone(),
            Some(opened.access.window_id),
            true,
            now,
        );
        guard.disarm();
        Ok(value)
    }

    async fn execute_page(
        &self,
        request: HistoryPageRequest,
        guard: &mut AttemptGuard<'_>,
        backend: Arc<dyn HistoryV2Backend>,
        limits: &HistoryLimits,
    ) -> Result<Value, AcpError> {
        let access = {
            let now = self.clock.now();
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
            Err(failure) => return self.fail_and(guard, failure),
        };
        let raw_cursor = match &request.cursor {
            Some(value) => {
                match super::cursor::decode(&self.cursor_secret, &request.window_id, value) {
                    Ok(decoded) if decoded.direction == request.direction => decoded.raw,
                    Ok(_) | Err(CursorError::Invalid) => {
                        return self.fail_and(guard, ServiceFailure::invalid_cursor());
                    }
                    Err(CursorError::CrossWindow) => {
                        return self.fail_and(guard, ServiceFailure::cross_window_cursor());
                    }
                }
            }
            None => {
                let home = match request.direction {
                    HistoryDirection::Before => access.before_home.clone(),
                    HistoryDirection::After => access.after_home.clone(),
                };
                match home {
                    Some(raw) => raw,
                    None => return self.fail_and(guard, ServiceFailure::invalid_cursor()),
                }
            }
        };
        let page_result = tokio::select! {
            biased;
            _ = access.cancel.cancelled() => {
                return self.fail_and(guard, window_cancel_failure(&access.cancel));
            }
            result = backend.page(
                &access.snapshot,
                &raw_cursor,
                request.direction,
                request.limit,
            ) => result,
        };
        let data = match page_result {
            Ok(data) => data,
            Err(error) => {
                if error == HistoryBackendError::SnapshotInvalidated {
                    self.remove_window(&request.window_id, false);
                }
                return self.fail_and(guard, ServiceFailure::from_backend(error));
            }
        };
        match self.commit_page(
            guard,
            &request.session_id,
            &request.window_id,
            &request.snapshot_token,
            &request.read_ticket,
            data,
            false,
            limits,
        ) {
            Ok(value) => Ok(value),
            Err(failure) => self.fail_and(guard, failure),
        }
    }

    async fn execute_seek(
        &self,
        request: HistorySeekRequest,
        guard: &mut AttemptGuard<'_>,
        backend: Arc<dyn HistoryV2Backend>,
        limits: &HistoryLimits,
    ) -> Result<Value, AcpError> {
        let HistoryAnchor::Entry(anchor) = request.anchor.clone() else {
            return Err(invalid_params_message("seek anchor must be an entry"));
        };
        let access = {
            let now = self.clock.now();
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
            Err(failure) => return self.fail_and(guard, failure),
        };
        let seek_result = tokio::select! {
            biased;
            _ = access.cancel.cancelled() => {
                return self.fail_and(guard, window_cancel_failure(&access.cancel));
            }
            result = backend.seek(&access.snapshot, &anchor, request.before, request.after) => result,
        };
        let data = match seek_result {
            Ok(data) => data,
            Err(error) => {
                if error == HistoryBackendError::SnapshotInvalidated {
                    self.remove_window(&request.window_id, false);
                }
                return self.fail_and(guard, ServiceFailure::from_backend(error));
            }
        };
        match self.commit_page(
            guard,
            &request.session_id,
            &request.window_id,
            &request.snapshot_token,
            &request.read_ticket,
            data,
            true,
            limits,
        ) {
            Ok(value) => Ok(value),
            Err(failure) => self.fail_and(guard, failure),
        }
    }

    /// page/seek 共用的单锁提交。
    ///
    /// 复核窗口、票据、快照与 requestKey attempt 后，才更新 home cursor、登记 bodyRef、
    /// 写成功账本。close 之后到达的迟到结果在这里得到 `window_closed`，不会重新发布。
    #[allow(clippy::too_many_arguments)] // 显式列出提交需要复核的身份字段。
    fn commit_page(
        &self,
        guard: &mut AttemptGuard<'_>,
        session_id: &str,
        window_id: &str,
        snapshot_token: &str,
        read_ticket: &str,
        data: HistoryPageData,
        update_homes: bool,
        limits: &HistoryLimits,
    ) -> Result<Value, ServiceFailure> {
        let now = self.clock.now();
        let mut state = self.state.lock();
        let access = resolve_access(
            &mut state,
            session_id,
            window_id,
            snapshot_token,
            read_ticket,
            now,
        )?;
        if snapshot_changed(&access.snapshot, &data) {
            state.windows.remove(window_id, now, false);
            return Err(ServiceFailure::snapshot_invalidated());
        }
        let before_cursor = data.before_cursor.clone();
        let after_cursor = data.after_cursor.clone();
        let page: HistoryWindowPage = build_page(&self.cursor_secret, &access, data);
        let body_refs = page_body_refs(&page);
        let value = bounded_page(page, limits)?;
        if !state
            .ledger
            .is_current(guard.key, guard.attempt, guard.fingerprint)
        {
            return Err(ServiceFailure::operation_expired());
        }
        if update_homes {
            state
                .windows
                .set_homes(window_id, before_cursor, after_cursor);
        }
        if !body_refs.is_empty() && !state.windows.issue_body_refs(window_id, body_refs) {
            return Err(ServiceFailure::window_expired());
        }
        state.ledger.finish_success(
            guard.key,
            guard.attempt,
            guard.fingerprint,
            value.clone(),
            Some(window_id.to_owned()),
            true,
            now,
        );
        guard.disarm();
        Ok(value)
    }

    async fn execute_body(
        &self,
        request: HistoryBodyRequest,
        guard: &mut AttemptGuard<'_>,
        backend: Arc<dyn HistoryV2Backend>,
        limits: &HistoryLimits,
    ) -> Result<Value, AcpError> {
        let access = {
            let now = self.clock.now();
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
            Err(failure) => return self.fail_and(guard, failure),
        };
        let body_result = tokio::select! {
            biased;
            _ = access.cancel.cancelled() => {
                return self.fail_and(guard, window_cancel_failure(&access.cancel));
            }
            result = backend.body(
                &access.snapshot,
                &request.body_ref,
                request.offset.get(),
                request.max_bytes,
            ) => result,
        };
        let slice = match body_result {
            Ok(slice) => slice,
            Err(error) => {
                if error == HistoryBackendError::SnapshotInvalidated {
                    self.remove_window(&request.window_id, false);
                }
                return self.fail_and(guard, ServiceFailure::from_backend(error));
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
            _ => return self.fail_and(guard, ServiceFailure::source_unavailable()),
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
            return self.fail_and(guard, ServiceFailure::source_unavailable());
        }
        let value = match serde_json::to_value(&chunk) {
            Ok(value) => value,
            Err(_) => {
                return self.fail_and(guard, ServiceFailure::source_unavailable());
            }
        };
        match self.commit_body(guard, &request, value, limits) {
            Ok(value) => Ok(value),
            Err(failure) => self.fail_and(guard, failure),
        }
    }

    /// body 的单锁提交：窗口/session/票据仍有效、attempt 仍是当前尝试才入账。
    fn commit_body(
        &self,
        guard: &mut AttemptGuard<'_>,
        request: &HistoryBodyRequest,
        value: Value,
        limits: &HistoryLimits,
    ) -> Result<Value, ServiceFailure> {
        let now = self.clock.now();
        let mut state = self.state.lock();
        resolve_access(
            &mut state,
            &request.session_id,
            &request.window_id,
            &request.snapshot_token,
            &request.read_ticket,
            now,
        )?;
        let value = bounded_value(value, limits)?;
        if !state
            .ledger
            .is_current(guard.key, guard.attempt, guard.fingerprint)
        {
            return Err(ServiceFailure::operation_expired());
        }
        state.ledger.finish_success(
            guard.key,
            guard.attempt,
            guard.fingerprint,
            value.clone(),
            Some(request.window_id.clone()),
            true,
            now,
        );
        guard.disarm();
        Ok(value)
    }

    fn execute_close(
        &self,
        request: HistoryCloseRequest,
        guard: &mut AttemptGuard<'_>,
    ) -> Result<Value, AcpError> {
        match self.commit_close(guard, &request) {
            Ok(value) => Ok(value),
            Err(failure) => self.fail_and(guard, failure),
        }
    }

    /// close 的单锁提交：幂等关闭并记录 closed 身份。尝试已被接管时不关闭窗口。
    fn commit_close(
        &self,
        guard: &mut AttemptGuard<'_>,
        request: &HistoryCloseRequest,
    ) -> Result<Value, ServiceFailure> {
        let now = self.clock.now();
        let mut state = self.state.lock();
        state.windows.prune(now);
        if !state
            .ledger
            .is_current(guard.key, guard.attempt, guard.fingerprint)
        {
            return Err(ServiceFailure::operation_expired());
        }
        if let Some(record) = state.windows.get(&request.window_id) {
            if record.session_id != request.session_id {
                return Err(ServiceFailure::permission_denied());
            }
            if record.snapshot_token != request.snapshot_token {
                return Err(ServiceFailure::snapshot_invalidated());
            }
            if record.read_ticket != request.read_ticket {
                return Err(ServiceFailure::ticket_invalid());
            }
            state.windows.remove(&request.window_id, now, true);
        }
        // 不存在或已关闭：幂等返回同形结果，不泄露其他主体窗口。
        let result = HistoryClosedResult {
            version: HISTORY_V2_VERSION,
            kind: HistoryClosedKind::Closed,
            window_id: request.window_id.clone(),
        };
        let value =
            serde_json::to_value(&result).map_err(|_| ServiceFailure::source_unavailable())?;
        state.ledger.finish_success(
            guard.key,
            guard.attempt,
            guard.fingerprint,
            value.clone(),
            Some(request.window_id.clone()),
            false,
            now,
        );
        guard.disarm();
        Ok(value)
    }
}

/// 窗口取消后的领域原因：显式关闭/会话拆除映射 `window_closed`，过期/清理映射
/// `window_expired`。两者都不得发布迟到结果。
fn window_cancel_failure(cancel: &WindowCancel) -> ServiceFailure {
    if cancel.was_closed() {
        ServiceFailure::window_closed()
    } else {
        ServiceFailure::window_expired()
    }
}
