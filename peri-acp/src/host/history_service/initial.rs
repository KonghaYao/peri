//! History V2 `initial` 服务层（设计 §6.1–6.5、§12.1）。
//!
//! initial 与只读 `open` 共用同一个窗口创建路径：同一次后端读取同时产出首屏、快照、
//! snapshotToken/readTicket 与 home cursor，禁止为交付再读一次。delivery 账本复用连接级
//! 幂等表，因此 requestKey 在 initial/open 之间共享高水位；initial 的过期语义是
//! `delivery_expired`，不是普通操作的 `operation_expired`。
//!
//! 本模块不解析 `session/load` 的 wire 参数（由 `host::history_v2_load` 在装配前完成）；
//! executionReady 的真实值由调用方计算，服务只透传并在失败 effect 中如实表达。

use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::history_v2::{
    HistoryAnchor, HistoryBackendError, HistoryInitialKind, HistoryInitialRequest,
    HistoryInitialResult, HistoryLimits, HistoryLiveBoundary, HistoryV2Backend, HISTORY_V2_VERSION,
};
use serde_json::Value;

use super::error::{
    initial_error, initial_timeout_error, invalid_params, invalid_params_message,
    protocol_not_negotiated_error, ServiceFailure,
};
use super::ledger::{fingerprint, Admission, ExpiryReason};
use super::{build_page, AttemptGuard, HistoryV2Service, ServiceState};
use crate::host::live_delivery::{LiveCandidate, LiveDelivery};
use crate::transport::types::AcpError;

/// 路由层交给 initial 的连接事实。`session_id` 来自外层 `session/load` 参数。
pub(crate) struct HistoryInitialContext {
    pub(crate) session_id: String,
    pub(crate) backend: Option<Arc<dyn HistoryV2Backend>>,
    pub(crate) negotiated: bool,
    pub(crate) limits: HistoryLimits,
    pub(crate) runtime_generation: String,
    /// 由 `session/load` 在恢复装配后计算：正文仍驻留 Host 且会话未关闭。
    pub(crate) execution_ready: bool,
    /// 本连接的实时投递注册表。生产 V2 load 必须提供；只有服务层单元测试退化为
    /// 使用 `live_boundary` 的固定边界。
    pub(crate) live: Option<Arc<LiveDelivery>>,
    /// 无实时注册表时的固定边界（测试/无连接上下文）。
    pub(crate) live_boundary: HistoryLiveBoundary,
    /// 完整 `session/load` 响应中除 initial 结果本身以外的字节数（标准配置字段、
    /// JSON-RPC 包络与 `_meta` 键）。预算复核必须把它计入 `maxResponseBytes`。
    pub(crate) reserved_response_bytes: u64,
}

impl HistoryV2Service {
    /// 构建 `session/load` 首屏。成功时返回与 open 共用快照/票据的 initial 结果。
    ///
    /// FIX-05：先准备、后提交。准备阶段（窗口候选、候选实时流、页面、bodyRef、校验、
    /// 预算）不撤销旧窗口或旧实时流；只有单锁提交成功后才关闭旧窗口并切换候选流。
    ///
    /// 总期限使用独立的 `initial_timeout`（生产 60s，与 `session/load` 冷启动一致）；
    /// 超时 drop 内部 future 会触发窗口/候选 guard，旧状态保持不变。
    pub(crate) async fn initial(
        &self,
        request: HistoryInitialRequest,
        context: HistoryInitialContext,
    ) -> Result<HistoryInitialResult, AcpError> {
        let execution_ready = context.execution_ready;
        let runtime_generation = context.runtime_generation.clone();
        match tokio::time::timeout(self.initial_timeout, self.initial_inner(request, context)).await
        {
            Ok(result) => result,
            Err(_) => Err(initial_timeout_error(execution_ready, &runtime_generation)),
        }
    }

    async fn initial_inner(
        &self,
        request: HistoryInitialRequest,
        context: HistoryInitialContext,
    ) -> Result<HistoryInitialResult, AcpError> {
        request.validate(&context.limits).map_err(invalid_params)?;
        if !context.negotiated {
            return Err(protocol_not_negotiated_error());
        }
        validate_context(&context)?;
        let Some(backend) = context.backend.clone() else {
            return Err(initial_error(
                ServiceFailure::from_backend(HistoryBackendError::BackendUnavailable),
                context.execution_ready,
                &context.runtime_generation,
            ));
        };

        let key = request.request_key.get();
        let fingerprint = initial_fingerprint(&context.session_id, &request);
        let attempt = match self.admit_with(key, fingerprint, ExpiryReason::DeliveryExpired) {
            Ok(Admission::Replay(value)) => {
                return replay_initial(self, value, key, &context);
            }
            Ok(Admission::Execute { attempt }) => attempt,
            Err(failure) => return Err(initial_failure(&context, failure)),
        };
        let mut guard = self.attempt_guard(key, attempt, fingerprint);

        // ── 准备阶段 ──────────────────────────────────────────────────────
        // 窗口先同步预留（不读后端），随后立即建立实时候选流，最后才调用 backend.open。
        // 后端读取期间的事件进入候选缓冲，旧 streamId/序号/缓冲保持不变。
        let reserved = match self.reserve_window(
            &context.session_id,
            context.limits.max_windows_per_connection,
            true,
        ) {
            Ok(reserved) => reserved,
            Err(failure) => return self.fail_initial(&mut guard, failure, &context),
        };
        let candidate = context
            .live
            .as_deref()
            .map(|live| live.begin_candidate(&context.session_id));
        let mut candidate_guard = match (context.live.as_deref(), candidate.as_ref()) {
            (Some(live), Some(candidate)) => Some(LiveCandidateGuard::new(
                live,
                &context.session_id,
                candidate.candidate_id.clone(),
            )),
            _ => None,
        };
        let live_boundary = candidate
            .as_ref()
            .map(|candidate| candidate.boundary.clone())
            .unwrap_or_else(|| context.live_boundary.clone());

        let opened = match self
            .open_window(
                reserved,
                backend.as_ref(),
                &context.session_id,
                &HistoryAnchor::Latest,
                request.limit,
                0,
                request.target_bytes,
            )
            .await
        {
            Ok(opened) => opened,
            Err(failure) => return self.fail_initial(&mut guard, failure, &context),
        };
        let mut window_guard =
            PreparedWindowGuard::new(&self.state, opened.access.window_id.clone());

        let page = build_page(&self.cursor_secret, &opened.access, opened.data);
        if !self.issue_body_refs(&opened.access.window_id, &page) {
            return self.fail_initial(&mut guard, ServiceFailure::window_expired(), &context);
        }
        // 页面字节 + 外层 load 固定字段的预算预检；最终结果还会在候选边界确定后再查一次。
        let page_bytes = match serde_json::to_vec(&page) {
            Ok(bytes) => bytes,
            Err(_) => {
                return self.fail_initial(
                    &mut guard,
                    ServiceFailure::source_unavailable(),
                    &context,
                );
            }
        };
        if (page_bytes.len() as u64).saturating_add(context.reserved_response_bytes)
            > context.limits.max_response_bytes
        {
            return self.fail_initial(&mut guard, ServiceFailure::budget_exceeded(), &context);
        }

        let result = HistoryInitialResult {
            version: HISTORY_V2_VERSION,
            kind: HistoryInitialKind::Initial,
            request_key: request.request_key,
            execution_ready: context.execution_ready,
            runtime_generation: context.runtime_generation.clone(),
            delivery_id: new_delivery_id(),
            live_boundary,
            page,
        };
        if result.validate().is_err() {
            return self.fail_initial(&mut guard, ServiceFailure::source_unavailable(), &context);
        }
        let value = match serde_json::to_value(&result) {
            Ok(value) => value,
            Err(_) => {
                return self.fail_initial(
                    &mut guard,
                    ServiceFailure::source_unavailable(),
                    &context,
                );
            }
        };
        let bytes = match serde_json::to_vec(&value) {
            Ok(bytes) => bytes,
            Err(_) => {
                return self.fail_initial(
                    &mut guard,
                    ServiceFailure::source_unavailable(),
                    &context,
                );
            }
        };
        if (bytes.len() as u64).saturating_add(context.reserved_response_bytes)
            > context.limits.max_response_bytes
        {
            return self.fail_initial(&mut guard, ServiceFailure::budget_exceeded(), &context);
        }

        // ── 提交阶段 ──────────────────────────────────────────────────────
        match self.commit_initial(
            &mut guard,
            &opened.access.window_id,
            candidate.as_ref(),
            value,
            &context,
        ) {
            Ok(()) => {
                window_guard.disarm();
                if let Some(candidate_guard) = candidate_guard.as_mut() {
                    candidate_guard.disarm();
                }
                Ok(result)
            }
            Err(failure) => self.fail_initial(&mut guard, failure, &context),
        }
    }

    /// 单锁提交：候选窗口仍存在、会话未关闭、账本 attempt 仍有效时才生效。
    ///
    /// 成功顺序：提交候选实时流 → 关闭同会话旧窗口（记录 closed 身份）→ 写成功账本。
    /// 任何一步失败都不关闭旧窗口、不写成功记录；候选由 guard 回收。
    fn commit_initial(
        &self,
        guard: &mut AttemptGuard<'_>,
        window_id: &str,
        candidate: Option<&LiveCandidate>,
        value: Value,
        context: &HistoryInitialContext,
    ) -> Result<(), ServiceFailure> {
        let now = self.clock.now();
        let mut state = self.state.lock();
        state.windows.prune(now);
        if state.windows.get(window_id).is_none() {
            // 会话/窗口已被外部关闭：关闭优先，不复活旧窗口或旧流。
            return Err(ServiceFailure::window_expired());
        }
        if !state
            .ledger
            .is_current(guard.key, guard.attempt, guard.fingerprint)
        {
            return Err(ServiceFailure::delivery_expired());
        }
        if let Some(candidate) = candidate {
            let Some(live) = context.live.as_deref() else {
                return Err(ServiceFailure::delivery_expired());
            };
            if !live.commit_candidate(&context.session_id, &candidate.candidate_id) {
                return Err(ServiceFailure::delivery_expired());
            }
        }
        // 提交成功后才让旧窗口进入 closed；失败路径不会执行到这里。
        state
            .windows
            .remove_session_except(&context.session_id, Some(window_id), now, true);
        state.ledger.finish_success(
            guard.key,
            guard.attempt,
            guard.fingerprint,
            value,
            Some(window_id.to_owned()),
            true,
            now,
        );
        guard.disarm();
        Ok(())
    }

    fn fail_initial<T>(
        &self,
        guard: &mut AttemptGuard<'_>,
        failure: ServiceFailure,
        context: &HistoryInitialContext,
    ) -> Result<T, AcpError> {
        let now = self.clock.now();
        self.state
            .lock()
            .ledger
            .finish_failure(guard.key, guard.attempt, guard.fingerprint, now);
        guard.disarm();
        Err(initial_failure(context, failure))
    }
}

fn initial_failure(context: &HistoryInitialContext, failure: ServiceFailure) -> AcpError {
    initial_error(
        failure,
        context.execution_ready,
        &context.runtime_generation,
    )
}

fn validate_context(context: &HistoryInitialContext) -> Result<(), AcpError> {
    if context.runtime_generation.is_empty() {
        return Err(invalid_params_message(
            "history initial requires runtimeGeneration",
        ));
    }
    // 有实时注册表时边界由 `begin` 生成；固定边界仅服务无连接上下文的测试路径。
    if context.live.is_none() && context.live_boundary.stream_id.is_empty() {
        return Err(invalid_params_message(
            "history initial requires liveBoundary.streamId",
        ));
    }
    Ok(())
}

/// 绑定 session/action/版本/预算；`requestKey` 由账本单独作键，不进入指纹。
fn initial_fingerprint(session_id: &str, request: &HistoryInitialRequest) -> [u8; 32] {
    let fields = serde_json::json!({
        "action": "initial",
        "sessionId": session_id,
        "version": request.version,
        "requestKey": request.request_key.get().to_string(),
        "limit": request.limit,
        "targetBytes": request.target_bytes,
    });
    fingerprint(&fields)
}

/// 重放前复核 delivery 绑定；任何执行/流身份变化都显式过期，不能返回旧事实。
fn replay_initial(
    service: &HistoryV2Service,
    value: Value,
    key: u64,
    context: &HistoryInitialContext,
) -> Result<HistoryInitialResult, AcpError> {
    let replayed = serde_json::from_value::<HistoryInitialResult>(value)
        .ok()
        .filter(|result| {
            let stream_still_current = match context.live.as_deref() {
                // 重放不换流：存储的结果必须仍由本连接该会话的当前流承载。
                Some(live) => {
                    live.has_stream_with_id(&context.session_id, &result.live_boundary.stream_id)
                }
                None => result.live_boundary.stream_id == context.live_boundary.stream_id,
            };
            result.request_key.get() == key
                && result.execution_ready == context.execution_ready
                && result.runtime_generation == context.runtime_generation
                && stream_still_current
                && result.validate().is_ok()
        });
    if let Some(result) = replayed {
        return Ok(result);
    }
    // 同一 requestKey 的 delivery 已不能代表当前执行/流事实；保留高水位并显式过期。
    service.state.lock().ledger.expire(key);
    Err(initial_failure(context, ServiceFailure::delivery_expired()))
}

fn new_delivery_id() -> String {
    // UUIDv4 的随机位不可从连接内其他 id 推导；不参与票据校验，只用于客户端关联。
    format!("dl_{}", uuid::Uuid::new_v4().simple())
}

/// initial 准备期的候选窗口清理：失败、取消或 drop 时移除尚未提交的候选窗口。
///
/// 与 `OpeningGuard` 不同，候选窗口在后端读取成功后已升级为 `Ready`；这里按 id
/// 丢弃它（不记录 closed 身份），旧窗口不受影响。提交成功路径显式 `disarm`。
struct PreparedWindowGuard<'a> {
    state: &'a Mutex<ServiceState>,
    window_id: String,
    armed: bool,
}

impl<'a> PreparedWindowGuard<'a> {
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

impl Drop for PreparedWindowGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.state.lock().windows.discard(&self.window_id);
        }
    }
}

/// 候选实时流清理：失败、取消、超时或 drop 时回收候选缓冲，旧流保持不变。
struct LiveCandidateGuard<'a> {
    live: &'a LiveDelivery,
    session_id: &'a str,
    candidate_id: String,
    armed: bool,
}

impl<'a> LiveCandidateGuard<'a> {
    fn new(live: &'a LiveDelivery, session_id: &'a str, candidate_id: String) -> Self {
        Self {
            live,
            session_id,
            candidate_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for LiveCandidateGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.live
                .abort_candidate(self.session_id, &self.candidate_id);
        }
    }
}
