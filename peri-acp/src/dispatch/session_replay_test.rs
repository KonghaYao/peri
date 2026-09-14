//! session_replay 行为测试：replay 的 Tool 消息必须与 live mapper 一致地
//! 写入标准 `content`（失败空文本用稳定 fallback），同时保留 rawOutput 与
//! replay meta。

use agent_client_protocol_schema::v1::{
    ContentBlock, SessionNotification, SessionUpdate, ToolCallContent, ToolCallStatus,
    ToolCallUpdateFields,
};
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
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

#[tokio::test]
async fn persisted_reminder_is_not_replayed_as_user_content() {
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Lifecycle,
            source: ReminderSource("replay_test".into()),
            kind: "complete".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Tui]),
            body: "system only".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let history = vec![
        PersistedPayload::Message(BaseMessage::human("user")),
        PersistedPayload::SystemReminder {
            id: peri_acp_types::messages::MessageId::new(),
            reminder,
        },
        PersistedPayload::Message(BaseMessage::ai("answer")),
    ];
    let sender = CollectSender {
        updates: std::sync::Mutex::new(Vec::new()),
    };
    replay_persisted_session_history("s1", &history, &sender, &PeriCaps::default())
        .await
        .unwrap();
    let updates = sender.updates.into_inner().unwrap();
    assert_eq!(updates.len(), 2);
    assert!(matches!(updates[0], SessionUpdate::UserMessageChunk(_)));
    assert!(matches!(updates[1], SessionUpdate::AgentMessageChunk(_)));
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
async fn test_replay_tool_failure_keeps_safe_subagent_diagnostic_meta() {
    let failure = peri_acp_types::error::SafeSubagentFailure::new(
        "child-1",
        peri_acp_types::error::SafeModelErrorDiagnostic::from_model(
            peri_model::ModelError::http_status(429, "provider.example", Some("req-1"))
                .diagnostic(),
        ),
    )
    .expect("valid safe failure");
    let message = BaseMessage::tool_result_with_execution_and_failure(
        "tc-safe",
        "child failed",
        true,
        None,
        Some(failure),
    );
    let updates = collect_replay(vec![message]).await;
    let SessionUpdate::ToolCallUpdate(update) = &updates[0] else {
        panic!("expected ToolCallUpdate");
    };
    let meta = update.meta.as_ref().expect("replay meta must exist");
    assert_eq!(meta["periReplay"], serde_json::Value::Bool(true));
    assert_eq!(
        meta["peri"]["subagentFailure"]["child_thread_id"],
        "child-1"
    );
    assert_eq!(meta["peri"]["subagentFailure"]["diagnostic"]["status"], 429);
    assert!(!serde_json::to_string(meta)
        .unwrap()
        .contains("provider body"));
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
async fn test_replay_typed_execution_message_keeps_bounded_text_and_failure_status() {
    let message = BaseMessage::tool_result_with_execution(
        "tc-evidence",
        "head\n[Execution status: failed, exit_code: 7, output_ref: /tmp/full-output.txt]",
        true,
        Some(peri_acp_types::tools::ToolExecutionEvidence {
            status: peri_acp_types::tools::ToolExecutionStatus::Failed,
            exit_code: Some(7),
            output_ref: Some("/tmp/full-output.txt".into()),
            output_truncated: true,
            task_id: None,
        }),
    );
    let updates = collect_replay(vec![message]).await;
    match &updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
            assert!(tool_call_output_text(&update.fields).contains("status: failed"));
            assert_eq!(
                update.fields.raw_output,
                Some(serde_json::Value::String(
                    "head\n[Execution status: failed, exit_code: 7, output_ref: /tmp/full-output.txt]"
                        .into()
                ))
            );
        }
        other => panic!("预期 ToolCallUpdate，实际: {other:?}"),
    }
}

#[tokio::test]
async fn test_replay_typed_execution_facts_project_summary_when_body_has_none() {
    let message = BaseMessage::tool_result_with_execution(
        "tc-facts",
        "raw body",
        true,
        Some(peri_acp_types::tools::ToolExecutionEvidence {
            status: peri_acp_types::tools::ToolExecutionStatus::RunningAfterTimeout,
            exit_code: None,
            output_ref: Some("/tmp/full-output.txt".into()),
            output_truncated: true,
            task_id: Some("shell-1".into()),
        }),
    );
    let updates = collect_replay(vec![message]).await;
    match &updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            let output = tool_call_output_text(&update.fields);
            assert!(output.contains("status: running_after_timeout"));
            assert!(output.contains("task_id: shell-1"));
            assert_eq!(output.matches("status: running_after_timeout").count(), 1);
            assert_eq!(
                update.fields.raw_output,
                Some(serde_json::Value::String(output))
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

#[tokio::test]
async fn test_investigation_compact_file_must_not_be_user_bubble() {
    let updates = collect_replay(vec![BaseMessage::human(
        "[最近读取的文件: /src/example.rs]\nfn example() {}",
    )])
    .await;
    assert!(
        updates.is_empty(),
        "内部文件上下文被回放成用户消息: {updates:?}"
    );
}
