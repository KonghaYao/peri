//! RewindCommand 事件发出辅助函数。
//!
//! 集中所有 `event_sink.push_event(...)` 调用模板，统一第三参 `context_window = 0` 与
//! `ExecutorEvent::RewindError` / `RewindCompleted` 变体构造。消除原来 rewind.rs 中
//! 3 处 push_event 近乎相同模板（参数解析失败/目标未找到/回滚完成）。

use std::fmt::Display;
use std::sync::Arc;

use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::messages::BaseMessage;

use crate::session::event_sink::EventSink;

/// 发出 rewind 参数解析失败的错误事件。
pub async fn emit_rewind_parse_error(
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    error: impl Display,
) {
    let msg = format!("rewind 参数解析失败: {error}");
    sink.push_event(session_id, &ExecutorEvent::RewindError { message: msg }, 0)
        .await;
}

/// 发出目标消息未找到的错误事件。
pub async fn emit_rewind_not_found(sink: &Arc<dyn EventSink>, session_id: &str, target_id: &str) {
    let msg = format!("rewind: 未找到目标消息 {target_id}");
    sink.push_event(session_id, &ExecutorEvent::RewindError { message: msg }, 0)
        .await;
}

/// 发出回滚完成事件。
pub async fn emit_rewind_completed(
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    summary: String,
    messages: Vec<BaseMessage>,
) {
    sink.push_event(
        session_id,
        &ExecutorEvent::RewindCompleted { summary, messages },
        0,
    )
    .await;
}
