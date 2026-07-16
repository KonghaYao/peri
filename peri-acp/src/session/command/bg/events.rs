//! BgCommand 事件发出辅助函数。
//!
//! 集中所有 `event_sink.push_event(...)` 调用模板，统一第三参 `context_window = 0` 与
//! `ExecutorEvent::TextChunk` / `SubagentStarted` 构造。消除原来 bg.rs 中 5 处 push_event
//! 近乎相同模板（空参数提示/LLM 构造失败/spawn 失败/SubagentStarted 推送/确认消息）。

use std::fmt::Display;
use std::sync::Arc;

use peri_agent::agent::events::ExecutorEvent;
use peri_agent::messages::MessageId;

use crate::session::event_sink::EventSink;

/// 发出 `/bg` 用法提示。
pub async fn emit_bg_usage_hint(sink: &Arc<dyn EventSink>, session_id: &str) {
    sink.push_event(
        session_id,
        &ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "用法: /bg <任务描述>\n".into(),
            source_agent_id: None,
        },
        0,
    )
    .await;
}

/// 发出 LLM 构造失败的错误提示。
pub async fn emit_bg_llm_error(sink: &Arc<dyn EventSink>, session_id: &str) {
    sink.push_event(
        session_id,
        &ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "✗ 后台任务启动失败: 无法构造 LLM 实例（请检查 peri-config.toml 的 Provider 配置）\n".into(),
            source_agent_id: None,
        },
        0,
    )
    .await;
}

/// 发出后台任务 spawn 失败的错误提示。
pub async fn emit_bg_spawn_error(sink: &Arc<dyn EventSink>, session_id: &str, error: impl Display) {
    sink.push_event(
        session_id,
        &ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: format!("✗ 后台任务启动失败: {error}\n"),
            source_agent_id: None,
        },
        0,
    )
    .await;
}

/// 同步推送 `SubagentStarted` 事件到 ACP transport。
///
/// 走 event_sink 直接入 TransportEventSink channel（in-memory mpsc），
/// 保证 TUI 端按 FIFO 顺序处理：SubagentStarted 必先于 Done 到达。
pub async fn emit_bg_started(
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    started_event: &ExecutorEvent,
) {
    sink.push_event(session_id, started_event, 0).await;
}

/// 发出后台任务启动确认消息（prompt 自动 CJK-safe truncation: chars().take(80)）。
pub async fn emit_bg_confirmation(sink: &Arc<dyn EventSink>, session_id: &str, prompt: &str) {
    let truncated: String = prompt.chars().take(80).collect();
    sink.push_event(
        session_id,
        &ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: format!("◆ 后台任务已启动: {truncated}\n"),
            source_agent_id: None,
        },
        0,
    )
    .await;
}
