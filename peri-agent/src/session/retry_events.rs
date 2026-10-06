//! Retry 事件转发（L5：自 peri-acp/src/session/retry_events.rs 迁入；
//! ACP 保留 re-export 桥）。
//!
//! 把 `peri_model::RetryObservation` 翻译为 `ExecutorEvent::LlmRetrying`
//! 交给当前 turn 的 `AgentEventHandler`。
//!
//! 转发器是 session 级组件（§2 生命周期跟随 session），随 L5 executor 拆分
//! 物理迁入本层；池化模型跨 turn 复用时会烘焙本转发器，每次 stage 装配用
//! 当前 turn 的 `event_handler` 调用 `set`，发射时读取最新 handler。

use std::sync::Arc;

use peri_acp_types::event::{AgentEventHandler, ExecutorEvent};
use peri_model::{RetryObservation, RetryObserver};

/// 将 `RetryObservation` 翻译为 `ExecutorEvent::LlmRetrying` 并交给 handler。
pub(crate) fn translate_observation(
    observation: &RetryObservation,
    handler: &Arc<dyn AgentEventHandler>,
) {
    handler.on_event(ExecutorEvent::LlmRetrying {
        attempt: observation.attempt() as usize,
        max_attempts: observation.max_attempts() as usize,
        delay_ms: observation.delay().as_millis() as u64,
        error: observation.error_kind().to_string(),
        diagnostic: observation
            .diagnostic()
            .cloned()
            .map(peri_acp_types::error::SafeModelErrorDiagnostic::from_model),
    });
}

/// 将 `AgentEventHandler` 包装为 `RetryObserver`：重试观测直接翻译为
/// `ExecutorEvent::LlmRetrying` 交给 handler。
pub fn retry_observer_for(handler: Arc<dyn AgentEventHandler>) -> Arc<dyn RetryObserver> {
    let forwarder = RetryEventForwarder::new();
    forwarder.set(Some(handler));
    forwarder.as_retry_observer()
}

/// 与边界错误共用白名单投影；不得格式化原始 error、body 或 cause chain。
pub(crate) fn log_interruption(
    diagnostic: &peri_model::ModelErrorDiagnostic,
    attempts: u32,
    max_attempts: u32,
) {
    tracing::warn!(
        provider = diagnostic.provider().unwrap_or("unknown"),
        request_id = diagnostic.request_id(),
        attempts,
        max_attempts,
        error_kind = diagnostic.category_name(),
        transport = diagnostic.transport().map(|kind| kind.to_string()),
        http_status = diagnostic.status(),
        protocol = diagnostic.protocol().map(|kind| kind.to_string()),
        "模型流中断 (stream_interrupted)"
    );
}

/// Session 级可更新 retry 事件转发器。
///
/// 池化模型跨 turn 复用时会烘焙本转发器；每次 stage 装配用当前 turn 的
/// `event_handler` 调用 `set`，发射时读取最新 handler，避免首 turn handler 陈旧。
#[derive(Clone, Default)]
pub struct RetryEventForwarder {
    handler: Arc<parking_lot::RwLock<Option<Arc<dyn AgentEventHandler>>>>,
}

impl RetryEventForwarder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, handler: Option<Arc<dyn AgentEventHandler>>) {
        *self.handler.write() = handler;
    }

    pub fn as_retry_observer(&self) -> Arc<dyn RetryObserver> {
        Arc::new(self.clone())
    }
}

impl RetryObserver for RetryEventForwarder {
    fn on_retry(&self, observation: RetryObservation) {
        if let Some(handler) = self.handler.read().clone() {
            translate_observation(&observation, &handler);
        }
    }

    fn on_interrupted(&self, observation: RetryObservation) -> bool {
        let Some(diagnostic) = observation.diagnostic() else {
            return false;
        };
        // 日志不依赖 turn handler，池化模型暂未绑定 handler 时也不能静默。
        log_interruption(
            diagnostic,
            observation.attempt(),
            observation.max_attempts(),
        );
        true
    }
}
