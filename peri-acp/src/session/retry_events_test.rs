//! `RetryEventForwarder` 与 `retry_observer_for` 的单元测试。
//!
//! 测试对象是 `session::retry_events`（转发器翻译 RetryObservation →
//! ExecutorEvent::LlmRetrying），与 provider 模块无关，按位置表独立成文件。

use super::*;

/// [回归测试] `RetryEventForwarder` 把 `RetryObservation` 翻译为 `ExecutorEvent::LlmRetrying`。
///
/// 历史背景：上层从未注册 RetryObserver，重试完全静默。转发器是 session 级接线核心：
/// 发射时读取当前 turn 的 handler；`set(None)` 后必须静默（残留 handler 场景）。
#[test]
fn retry_event_forwarder_translates_observation_to_llm_retrying() {
    use std::sync::Mutex;

    let recorded = Arc::new(Mutex::new(Vec::new()));
    let handler: Arc<dyn peri_agent::agent::events::AgentEventHandler> =
        Arc::new(peri_agent::agent::events::FnEventHandler({
            let recorded = recorded.clone();
            move |event: peri_agent::agent::events::ExecutorEvent| {
                recorded.lock().expect("record lock").push(event);
            }
        }));

    let forwarder = RetryEventForwarder::new();
    forwarder.set(Some(handler));

    let observer = forwarder.as_retry_observer();
    observer.on_retry(peri_model::RetryObservation::new(
        2,
        3,
        std::time::Duration::from_millis(1000),
        peri_model::RetryErrorKind::Protocol,
    ));

    let guard = recorded.lock().expect("record lock");
    assert_eq!(guard.len(), 1);
    assert!(matches!(
        &guard[0],
        peri_agent::agent::events::ExecutorEvent::LlmRetrying {
            attempt: 2,
            max_attempts: 3,
            delay_ms: 1000,
            error,
        } if error == "protocol"
    ));
    drop(guard);

    // 清除 handler 后再次发射不得 panic（池化模型残留场景）。
    forwarder.set(None);
    observer.on_retry(peri_model::RetryObservation::new(
        1,
        3,
        std::time::Duration::ZERO,
        peri_model::RetryErrorKind::Transport,
    ));
    assert_eq!(recorded.lock().expect("record lock").len(), 1);
}
