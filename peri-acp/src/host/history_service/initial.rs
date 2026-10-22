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

use peri_acp_types::history_v2::{
    HistoryAnchor, HistoryBackendError, HistoryInitialKind, HistoryInitialRequest,
    HistoryInitialResult, HistoryLimits, HistoryLiveBoundary, HistoryV2Backend, HISTORY_V2_VERSION,
};
use serde_json::Value;

use super::error::{
    initial_error, invalid_params, invalid_params_message, protocol_not_negotiated_error,
    ServiceFailure,
};
use super::ledger::{fingerprint, Admission, ExpiryReason};
use super::{build_page, AttemptGuard, HistoryV2Service};
use crate::host::live_delivery::LiveDelivery;
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
    pub(crate) async fn initial(
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
        // 设计 §6.3 第 4 步：首屏装配前建立本连接/本会话的实时缓冲与序号边界。
        // 只有真正执行的 initial 才换流；同 requestKey 重放沿用既有流（见 `replay_initial`）。
        let (live_boundary, mut live_guard) = match context.live.as_deref() {
            Some(live) => (
                live.begin(&context.session_id),
                Some(PausedLiveGuard::new(live, &context.session_id)),
            ),
            None => (context.live_boundary.clone(), None),
        };
        let mut guard = self.attempt_guard(key, attempt, fingerprint);

        // 同一连接重复 `session/load` 不得累积窗口：新首屏替换旧窗口，旧窗口按
        // closed 身份处理（后续 page/body 返回 `window_closed`）。其他会话窗口保留。
        self.close_session(&context.session_id);

        let opened = match self
            .begin_window(
                backend.as_ref(),
                &context.session_id,
                &HistoryAnchor::Latest,
                request.limit,
                0,
                request.target_bytes,
                context.limits.max_windows_per_connection,
            )
            .await
        {
            Ok(opened) => opened,
            Err(failure) => return self.fail_initial(&mut guard, failure, &context),
        };

        let result = HistoryInitialResult {
            version: HISTORY_V2_VERSION,
            kind: HistoryInitialKind::Initial,
            request_key: request.request_key,
            execution_ready: context.execution_ready,
            runtime_generation: context.runtime_generation.clone(),
            delivery_id: new_delivery_id(),
            live_boundary,
            page: build_page(&self.cursor_secret, &opened.access, opened.data),
        };
        if !self.issue_body_refs(&opened.access.window_id, &result.page) {
            self.remove_window(&opened.access.window_id, false);
            return self.fail_initial(&mut guard, ServiceFailure::window_expired(), &context);
        }
        if result.validate().is_err() {
            self.remove_window(&opened.access.window_id, false);
            return self.fail_initial(&mut guard, ServiceFailure::source_unavailable(), &context);
        }
        let value = match serde_json::to_value(&result) {
            Ok(value) => value,
            Err(_) => {
                self.remove_window(&opened.access.window_id, false);
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
                self.remove_window(&opened.access.window_id, false);
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
            self.remove_window(&opened.access.window_id, false);
            return self.fail_initial(&mut guard, ServiceFailure::budget_exceeded(), &context);
        }

        self.succeed(
            &mut guard,
            value,
            Some(opened.access.window_id.clone()),
            true,
        );
        if let Some(guard) = live_guard.as_mut() {
            guard.disarm();
        }
        Ok(result)
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

/// initial 装配期的暂停流清理：失败、取消或超时返回时移除尚未发布的首屏流。
///
/// 成功路径显式 `disarm`；此后流的生命周期由响应发送方的 flush/close 接管。
struct PausedLiveGuard<'a> {
    live: &'a LiveDelivery,
    session_id: &'a str,
    armed: bool,
}

impl<'a> PausedLiveGuard<'a> {
    fn new(live: &'a LiveDelivery, session_id: &'a str) -> Self {
        Self {
            live,
            session_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PausedLiveGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.live.close_if_paused(self.session_id);
        }
    }
}
