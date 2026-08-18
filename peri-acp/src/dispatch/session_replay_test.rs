//! session_replay 行为测试：replay 的 Tool 消息必须与 live mapper 一致地
//! 写入标准 `content`（失败空文本用稳定 fallback），同时保留 rawOutput 与
//! replay meta。

use agent_client_protocol_schema::v1::{
    ContentBlock, SessionNotification, SessionUpdate, ToolCallContent, ToolCallStatus,
    ToolCallUpdateFields,
};
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::PeriCaps;

use super::*;

/// 收集 replay 通知的测试 sender。
struct CollectSender {
    updates: std::sync::Mutex<Vec<SessionUpdate>>,
}

#[async_trait::async_trait]
impl ReplaySender for CollectSender {
    async fn send(&self, notif: SessionNotification) -> Result<(), ReplayError> {
        self.updates.lock().unwrap().push(notif.update);
        Ok(())
    }
}

async fn collect_replay(history: Vec<BaseMessage>) -> Vec<SessionUpdate> {
    let sender = CollectSender {
        updates: std::sync::Mutex::new(Vec::new()),
    };
    let caps = PeriCaps {
        replay: true,
        ..PeriCaps::default()
    };
    replay_session_history("s1", &history, &sender, &caps)
        .await
        .expect("replay 发送失败");
    sender.updates.into_inner().unwrap()
}

/// 提取 `ToolCallUpdateFields.content` 中唯一 Text block 的文本。
fn tool_call_output_text(fields: &ToolCallUpdateFields) -> String {
    let content = fields
        .content
        .as_deref()
        .expect("标准 output content 必须存在");
    assert_eq!(content.len(), 1, "标准 output 应为单个文本块");
    match &content[0] {
        ToolCallContent::Content(c) => match &c.content {
            ContentBlock::Text(t) => t.text.clone(),
            other => panic!("预期 Text ContentBlock，实际: {other:?}"),
        },
        other => panic!("预期 ToolCallContent::Content，实际: {other:?}"),
    }
}

#[tokio::test]
async fn test_replay_tool_failure_writes_standard_output_raw_and_meta() {
    // replay 失败工具 → status=failed + 标准 content 文本 + rawOutput +
    // replay meta 同时存在，与 live mapper 形态一致。
    let updates = collect_replay(vec![BaseMessage::tool_error("tc-1", "some error")]).await;
    assert_eq!(updates.len(), 1);
    match &updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
            assert_eq!(tool_call_output_text(&update.fields), "some error");
            assert_eq!(
                update.fields.raw_output,
                Some(serde_json::Value::String("some error".to_string()))
            );
            let meta = update.meta.as_ref().expect("replay meta 必须存在");
            assert_eq!(meta.get("periReplay"), Some(&serde_json::Value::Bool(true)));
        }
        other => panic!("预期 ToolCallUpdate，实际: {other:?}"),
    }
}

#[tokio::test]
async fn test_replay_tool_success_writes_standard_output() {
    // replay 成功工具 → status=completed + 标准 content 文本 + rawOutput
    let updates = collect_replay(vec![BaseMessage::tool_result("tc-2", "ok")]).await;
    match &updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(update.fields.status, Some(ToolCallStatus::Completed));
            assert_eq!(tool_call_output_text(&update.fields), "ok");
            assert!(
                update.fields.raw_output.is_some(),
                "raw_output 必须保留以维持机器消费兼容"
            );
        }
        other => panic!("预期 ToolCallUpdate，实际: {other:?}"),
    }
}

#[tokio::test]
async fn test_replay_tool_failure_empty_text_uses_fallback() {
    // replay 失败且文本为空 → 标准 content 使用与 live mapper 相同的
    // 稳定非空 fallback；rawOutput 保持空串表达。
    let updates = collect_replay(vec![BaseMessage::tool_error("tc-3", "")]).await;
    match &updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
            assert_eq!(
                tool_call_output_text(&update.fields),
                "Tool execution failed",
                "fallback 文案必须与 live mapper 一致且非空"
            );
            assert_eq!(
                update.fields.raw_output,
                Some(serde_json::Value::String(String::new()))
            );
        }
        other => panic!("预期 ToolCallUpdate，实际: {other:?}"),
    }
}
