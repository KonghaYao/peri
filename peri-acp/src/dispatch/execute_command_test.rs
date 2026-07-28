//! Tests for execute_command

use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::{
    agent::{events::ExecutorEvent, AgentCancellationToken},
    messages::BaseMessage,
};

use super::*;
use crate::{provider::PeriConfig, session::event_sink::EventSink};

/// 令 `/compact` 的无模型错误事件保持 pending，确保外层取消分支获选。
struct PendingEventSink;

#[async_trait]
impl EventSink for PendingEventSink {
    async fn push_event(&self, _session_id: &str, _event: &ExecutorEvent, _context_window: u32) {
        std::future::pending::<()>().await;
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str) {}
}

#[test]
fn test_extract_params_basic() {
    let params = serde_json::json!({
        "sessionId": "s1",
        "command": "/bg",
        "args": "do something"
    });
    let (sid, cmd, args) = extract_execute_command_params(&params).unwrap();
    assert_eq!(sid, "s1");
    assert_eq!(cmd, "/bg");
    assert_eq!(args.as_str().unwrap(), "do something");
}

#[test]
fn test_extract_params_session_id_underscore() {
    let params = serde_json::json!({
        "session_id": "s2",
        "command": "/compact"
    });
    let (sid, cmd, args) = extract_execute_command_params(&params).unwrap();
    assert_eq!(sid, "s2");
    assert_eq!(cmd, "/compact");
    assert!(args.is_null());
}

#[test]
fn test_extract_params_missing_session_id() {
    let params = serde_json::json!({
        "command": "/bg"
    });
    let err = extract_execute_command_params(&params).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("sessionId"));
}

#[test]
fn test_extract_params_missing_command() {
    let params = serde_json::json!({
        "sessionId": "s1"
    });
    let err = extract_execute_command_params(&params).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("command"));
}

#[test]
fn test_extract_params_json_args() {
    let params = serde_json::json!({
        "sessionId": "s1",
        "command": "/rewind",
        "args": { "target_message_id": "abc", "revert_files": true }
    });
    let (sid, cmd, args) = extract_execute_command_params(&params).unwrap();
    assert_eq!(sid, "s1");
    assert_eq!(cmd, "/rewind");
    assert_eq!(args["target_message_id"], "abc");
    assert_eq!(args["revert_files"], true);
}

/// 外层取消应保留调用方传入的完整 history，而不是返回空消息列表。
#[tokio::test]
async fn test_execute_command_outer_cancel_preserves_history() {
    let params = serde_json::json!({
        "sessionId": "cancelled-compact",
        "command": "/compact",
    });
    let history = vec![
        BaseMessage::human("first message"),
        BaseMessage::ai("second message"),
    ];
    let expected_messages = serde_json::to_value(&history).unwrap();
    let cancel = AgentCancellationToken::new();
    cancel.cancel();
    let peri_config = Arc::new(PeriConfig::default());
    let event_sink: Arc<dyn EventSink> = Arc::new(PendingEventSink);

    let result = execute_command(
        &params,
        history,
        "/tmp",
        &peri_config,
        &event_sink,
        None,
        &cancel,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result["stop_reason"], "Cancelled");
    assert_eq!(result["messages"], expected_messages);
}
