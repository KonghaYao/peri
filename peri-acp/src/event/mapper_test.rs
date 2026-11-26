use agent_client_protocol::schema::v1::ContentBlock;
use agent_client_protocol::schema::v1::PlanEntryPriority;
use agent_client_protocol::schema::v1::PlanEntryStatus;
use agent_client_protocol::schema::v1::SessionUpdate;
use agent_client_protocol_schema::v1::{
    ToolCallContent, ToolCallStatus, ToolCallUpdateFields, ToolKind,
};
use peri_acp_types::command::{CommandFeedback, FeedbackChannel, FeedbackLevel};
use peri_acp_types::event::{
    BackgroundTaskResult, CompactStrategy, CompactTrigger, ExecutorEvent, TodoEntry, TodoStatus,
};
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::tools::ToolDefinition;
use peri_acp_types::PeriCaps;
use peri_model::{StopReason, TokenUsage};
#[test]
fn test_llm_call_end_maps_to_enriched_usage_update() {
    let event = ExecutorEvent::LlmCallEnd {
        step: 1,
        model: "claude-sonnet-4-20250514".to_string(),
        output: "Hello".to_string(),
        usage: Some(TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(10),
            cache_read_input_tokens: Some(200),
        }),
        stop_reason: Some(StopReason::EndTurn),
        request_id: Some("req-123".to_string()),
        source_agent_id: None,
    };
    let caps = PeriCaps {
        token_stats: true,
        ..Default::default()
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &caps);
    assert_eq!(mapped.len(), 1, "应产出 1 个 MappedEvent");

    let m = &mapped[0];
    assert_eq!(m.updates.len(), 1, "应包含 1 个 SessionUpdate");

    match &m.updates[0] {
        SessionUpdate::UsageUpdate(usage) => {
            assert_eq!(usage.used, 100);
            assert_eq!(usage.size, 200_000);
            let meta = usage.meta.as_ref().expect("_meta 应包含详细 usage");
            assert_eq!(meta.get("inputTokens").unwrap().as_u64(), Some(100));
            assert_eq!(meta.get("outputTokens").unwrap().as_u64(), Some(50));
            assert_eq!(meta.get("cacheCreationTokens").unwrap().as_u64(), Some(10));
            assert_eq!(meta.get("cacheReadTokens").unwrap().as_u64(), Some(200));
            assert_eq!(
                meta.get("model").unwrap().as_str(),
                Some("claude-sonnet-4-20250514")
            );
            assert_eq!(meta.get("stopReason").unwrap().as_str(), Some("end_turn"));
            assert_eq!(
                meta.get("requestId").unwrap().as_str(),
                Some("req-123"),
                "requestId 必须从 LlmCallEnd.request_id 透传到 meta，不得随 usage 迁移丢失"
            );
        }
        other => panic!("预期 UsageUpdate，实际: {:?}", other),
    }
}

#[test]
fn test_llm_call_end_no_optional_fields() {
    // 无缓存 token、无 stop_reason 时 _meta 不含可选字段
    let event = ExecutorEvent::LlmCallEnd {
        step: 2,
        model: "gpt-4o".to_string(),
        output: String::new(),
        usage: Some(TokenUsage {
            input_tokens: 200,
            output_tokens: 30,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        }),
        stop_reason: None,
        request_id: None,
        source_agent_id: None,
    };
    let caps = PeriCaps {
        token_stats: true,
        ..Default::default()
    };
    let mapped = crate::event::mapper::map_event(&event, 128_000, &caps);
    assert_eq!(mapped.len(), 1);

    match &mapped[0].updates[0] {
        SessionUpdate::UsageUpdate(usage) => {
            assert_eq!(usage.used, 200);
            let meta = usage.meta.as_ref().unwrap();
            assert!(meta.get("cacheCreationTokens").is_none());
            assert!(meta.get("cacheReadTokens").is_none());
            assert!(meta.get("stopReason").is_none());
        }
        other => panic!("预期 UsageUpdate，实际: {:?}", other),
    }
}

#[test]
fn test_auxiliary_llm_usage_preserves_source_agent_id() {
    let event = ExecutorEvent::LlmCallEnd {
        step: 1,
        model: "aux-model".into(),
        output: String::new(),
        usage: Some(TokenUsage::new(100, 1)),
        stop_reason: Some(StopReason::EndTurn),
        request_id: Some("aux-request".into()),
        source_agent_id: Some("child-agent".into()),
    };
    let mapped = crate::event::mapper::map_event(
        &event,
        200_000,
        &PeriCaps {
            token_stats: true,
            ..Default::default()
        },
    );
    assert_eq!(mapped[0].source_agent_id.as_deref(), Some("child-agent"));
    assert!(matches!(
        mapped[0].updates[0],
        SessionUpdate::UsageUpdate(_)
    ));
}

#[test]
fn test_llm_call_end_no_usage_filtered() {
    let event = ExecutorEvent::LlmCallEnd {
        step: 1,
        model: "test".to_string(),
        output: "ERROR".to_string(),
        usage: None,
        stop_reason: None,
        request_id: None,
        source_agent_id: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    assert!(
        mapped.iter().all(|m| m.updates.is_empty()),
        "LlmCallEnd usage=None 应被过滤"
    );
}

// ── Non-Category-① 变体：wildcard 无 SessionUpdate ────────────────────────
// 所有非 Category ① 变体现在通过 wildcard 产生空 updates

fn assert_no_session_update(event: &ExecutorEvent, label: &str) {
    let mapped = crate::event::mapper::map_event(event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1, "{} 应产出 1 个 MappedEvent", label);
    assert!(
        mapped[0].updates.is_empty(),
        "{} 不应产生 SessionUpdate",
        label
    );
}

#[test]
fn test_context_warning_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::ContextWarning {
            used_tokens: 150_000,
            total_tokens: 200_000,
            percentage: 75.0,
        },
        "ContextWarning",
    );
}

#[test]
fn test_llm_retrying_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::LlmRetrying {
            attempt: 2,
            max_attempts: 3,
            delay_ms: 1000,
            error: "timeout".to_string(),
            diagnostic: None,
        },
        "LlmRetrying",
    );
}

#[test]
fn test_tool_end_carries_title() {
    // ToolEnd 映射为 ToolCallUpdate 时必须携带 title（工具名）
    let event = ExecutorEvent::ToolEnd {
        images: Vec::new(),
        message_id: MessageId::new(),
        tool_call_id: "tc-123".to_string(),
        name: "Bash".to_string(),
        output: "ok".to_string(),
        is_error: false,
        source_agent_id: None,
        subagent_failure: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].updates.len(), 1);

    match &mapped[0].updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(update.tool_call_id.0.as_ref(), "tc-123");
            // title 必须携带工具名，不能为空
            let title = update.fields.title.as_deref().unwrap_or("");
            assert_eq!(title, "Bash", "ToolCallUpdate.title 应为工具名");
        }
        other => panic!("预期 ToolCallUpdate，实际: {:?}", other),
    }
}

#[test]
fn test_tool_end_projects_safe_subagent_failure_allowlist() {
    let failure = peri_acp_types::error::SafeSubagentFailure::new(
        "child-1",
        peri_acp_types::error::SafeModelErrorDiagnostic::from_model(
            peri_model::ModelError::http_status(429, "provider.example", Some("req-1"))
                .diagnostic(),
        ),
    )
    .expect("valid safe failure");
    let event = ExecutorEvent::ToolEnd {
        images: Vec::new(),
        message_id: MessageId::new(),
        tool_call_id: "tc-child".to_string(),
        name: "delegate".to_string(),
        output: "child failed".to_string(),
        is_error: true,
        source_agent_id: None,
        subagent_failure: Some(failure),
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    let SessionUpdate::ToolCallUpdate(update) = &mapped[0].updates[0] else {
        panic!("expected ToolCallUpdate");
    };
    let meta = update
        .meta
        .as_ref()
        .expect("safe facts should be projected");
    assert_eq!(
        meta["peri"]["subagentFailure"]["child_thread_id"],
        "child-1"
    );
    assert_eq!(meta["peri"]["subagentFailure"]["diagnostic"]["status"], 429);
    assert!(!serde_json::to_string(meta)
        .unwrap()
        .contains("raw provider body"));
}

#[test]
fn test_tool_end_success_writes_standard_output_content() {
    // ToolEnd 成功 → status=completed + 标准 content（Text block）+ rawOutput
    let event = ExecutorEvent::ToolEnd {
        images: Vec::new(),
        message_id: MessageId::new(),
        tool_call_id: "tc-ok".to_string(),
        name: "Bash".to_string(),
        output: "done".to_string(),
        is_error: false,
        source_agent_id: None,
        subagent_failure: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].updates.len(), 1);
    match &mapped[0].updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(
                update.fields.status,
                Some(ToolCallStatus::Completed),
                "成功状态应为 completed"
            );
            assert_eq!(tool_call_output_text(&update.fields), "done");
            assert!(
                update.fields.raw_output.is_some(),
                "raw_output 必须保留以维持机器消费兼容"
            );
        }
        other => panic!("预期 ToolCallUpdate，实际: {other:?}"),
    }
}

/// 工具图片走 ACP 标准 content 块：文本投影与 `rawOutput` 保持字符串语义，
/// 图片以 `ToolCallContent::Content(ContentBlock::Image)` 追加。
#[test]
fn test_tool_end_images_use_standard_content_and_keep_raw_output_text() {
    let event = ExecutorEvent::ToolEnd {
        message_id: MessageId::new(),
        tool_call_id: "tc-img".to_string(),
        name: "Read".to_string(),
        output: "Image read\n[image: image/png]".to_string(),
        is_error: false,
        images: vec![peri_acp_types::tools::ToolImage {
            mime_type: "image/png".to_string(),
            data: "AQID".to_string(),
        }],
        source_agent_id: None,
        subagent_failure: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    match &mapped[0].updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            let content = update.fields.content.as_ref().expect("content 必须存在");
            assert!(
                matches!(
                    &content[0],
                    ToolCallContent::Content(content)
                        if matches!(&content.content, ContentBlock::Text(_))
                ),
                "首个块保持文本投影"
            );
            let image = content.iter().find_map(|block| match block {
                ToolCallContent::Content(content) => match &content.content {
                    ContentBlock::Image(image) => Some(image),
                    _ => None,
                },
                _ => None,
            });
            let image = image.expect("图片必须以标准 content 块透传");
            assert_eq!(image.data, "AQID");
            assert_eq!(image.mime_type, "image/png");
            assert_eq!(
                tool_call_output_text(&update.fields),
                "Image read\n[image: image/png]",
                "文本投影不得重复图片载荷"
            );
            assert_eq!(
                update.fields.raw_output,
                Some(serde_json::Value::String(
                    "Image read\n[image: image/png]".to_string()
                )),
                "rawOutput 保持字符串（旧消费方兼容）"
            );
        }
        other => panic!("预期 ToolCallUpdate，实际: {other:?}"),
    }
}

#[test]
fn test_tool_end_typed_projection_keeps_status_summary_in_raw_output() {
    let output = "head\n[Execution status: failed, exit_code: 7, output_ref: /tmp/full-output.txt]";
    let event = ExecutorEvent::ToolEnd {
        images: Vec::new(),
        message_id: MessageId::new(),
        tool_call_id: "tc-evidence".to_string(),
        name: "Bash".to_string(),
        output: output.to_string(),
        is_error: true,
        source_agent_id: None,
        subagent_failure: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    match &mapped[0].updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
            assert!(tool_call_output_text(&update.fields).contains("status: failed"));
            assert_eq!(
                update.fields.raw_output,
                Some(serde_json::Value::String(output.to_string()))
            );
        }
        other => panic!("预期 ToolCallUpdate，实际: {other:?}"),
    }
}

#[test]
fn test_tool_end_preserves_read_truncation_metadata_in_standard_content() {
    let output = "     1\talpha\n[Output truncated: 12000 bytes total; showing lines 1..=1 of 800; continue reading with offset=2]";
    let event = ExecutorEvent::ToolEnd {
        images: Vec::new(),
        message_id: MessageId::new(),
        tool_call_id: "tc-read-truncated".to_string(),
        name: "Read".to_string(),
        output: output.to_string(),
        is_error: false,
        source_agent_id: None,
        subagent_failure: None,
    };

    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    match &mapped[0].updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(tool_call_output_text(&update.fields), output);
            assert_eq!(
                update.fields.raw_output,
                Some(serde_json::Value::String(output.to_string()))
            );
        }
        other => panic!("预期 ToolCallUpdate，实际: {other:?}"),
    }
}

#[test]
fn test_tool_end_failure_writes_standard_output_content() {
    // ToolEnd 失败 → status=failed + 错误文本写入标准 content + rawOutput 仍存在
    let event = ExecutorEvent::ToolEnd {
        images: Vec::new(),
        message_id: MessageId::new(),
        tool_call_id: "tc-err".to_string(),
        name: "Bash".to_string(),
        output: "command not found".to_string(),
        is_error: true,
        source_agent_id: None,
        subagent_failure: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    match &mapped[0].updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
            assert_eq!(tool_call_output_text(&update.fields), "command not found");
            assert!(
                update.fields.raw_output.is_some(),
                "raw_output 必须保留以维持机器消费兼容"
            );
        }
        other => panic!("预期 ToolCallUpdate，实际: {other:?}"),
    }
}

#[test]
fn test_tool_end_failure_empty_output_uses_stable_fallback() {
    // 失败且底层文本为空 → 标准 content 使用稳定非空 fallback，
    // 不允许前端因空串静默丢弃；raw_output 保持原样。
    let event = ExecutorEvent::ToolEnd {
        images: Vec::new(),
        message_id: MessageId::new(),
        tool_call_id: "tc-empty".to_string(),
        name: "Bash".to_string(),
        output: String::new(),
        is_error: true,
        source_agent_id: None,
        subagent_failure: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    match &mapped[0].updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
            let text = tool_call_output_text(&update.fields);
            assert_eq!(text, "Tool execution failed", "fallback 文案必须稳定非空");
            assert!(
                !text.contains("SECRET") && !text.contains("panic"),
                "fallback 不得携带内部细节"
            );
            assert!(
                update.fields.raw_output.is_some(),
                "raw_output 必须保留以维持机器消费兼容"
            );
        }
        other => panic!("预期 ToolCallUpdate，实际: {other:?}"),
    }
}

#[test]
fn test_tool_end_failure_wire_shape() {
    // wire 形态锁定（P0 序列化）：ToolCallUpdate 展开后必须同时含
    // status=failed、content（标准 output 文本块）与 rawOutput，
    // 标准客户端可仅凭标准字段感知失败。
    let event = ExecutorEvent::ToolEnd {
        images: Vec::new(),
        message_id: MessageId::new(),
        tool_call_id: "tc-wire".to_string(),
        name: "Bash".to_string(),
        output: "boom".to_string(),
        is_error: true,
        source_agent_id: None,
        subagent_failure: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    let SessionUpdate::ToolCallUpdate(update) = &mapped[0].updates[0] else {
        panic!("预期 ToolCallUpdate")
    };
    let value = serde_json::to_value(update).unwrap();
    let obj = value.as_object().unwrap();
    assert_eq!(obj.get("status").unwrap().as_str(), Some("failed"));
    let content = obj.get("content").unwrap().as_array().unwrap();
    assert_eq!(content.len(), 1, "标准 output 应为单个文本块");
    let text_block = &content[0]["content"];
    assert_eq!(text_block["type"], "text");
    assert_eq!(text_block["text"], "boom");
    assert!(obj.contains_key("rawOutput"), "rawOutput 必须保留");
}

/// 提取 `ToolCallUpdateFields.content` 中唯一 Text block 的文本。
///
/// 图片块可以追加在文本之后（工具结果图片），但文本块必须恰好一个。
fn tool_call_output_text(fields: &ToolCallUpdateFields) -> String {
    let content = fields
        .content
        .as_deref()
        .expect("标准 output content 必须存在");
    let texts: Vec<&str> = content
        .iter()
        .filter_map(|block| match block {
            ToolCallContent::Content(c) => match &c.content {
                ContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 1, "标准 output 应恰好一个文本块");
    texts[0].to_string()
}

#[test]
fn test_stop_reason_wire_format() {
    // legacy wire format：与历史 StopReason Display 一致。
    // peri_model::StopReason 无 Display，经 mapper 本地 helper 显式映射。
    for (reason, expected) in [
        (StopReason::EndTurn, "end_turn"),
        (StopReason::ToolUse, "tool_use"),
        (StopReason::MaxTokens, "max_tokens"),
        (StopReason::Continue, "continue"),
        (
            StopReason::Other {
                value: "custom".to_string(),
            },
            "custom",
        ),
    ] {
        assert_eq!(
            crate::event::mapper::stop_reason_wire(&reason),
            expected,
            "wire format 不匹配"
        );
    }
}

// ── Category ①: SessionUpdate 变体 ──────────────────────────────────────────

#[test]
fn test_ai_reasoning_maps_to_session_update() {
    // AiReasoning → AgentThoughtChunk SessionUpdate
    let event = ExecutorEvent::AiReasoning {
        message_id: peri_acp_types::messages::MessageId::new(),
        text: "let me think...".to_string(),
        source_agent_id: None,
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1, "应产出 1 个 MappedEvent");
    assert_eq!(mapped[0].updates.len(), 1, "应包含 1 个 SessionUpdate");
    assert!(
        mapped[0].source_agent_id.is_none(),
        "主 agent reasoning 无 source_agent_id"
    );
    match &mapped[0].updates[0] {
        SessionUpdate::AgentThoughtChunk(chunk) => {
            // 验证 ContentChunk 内含 Text ContentBlock
            match &chunk.content {
                ContentBlock::Text(tc) => {
                    assert_eq!(tc.text, "let me think...");
                }
                other => panic!("预期 Text ContentBlock，实际: {:?}", other),
            }
        }
        other => panic!("预期 AgentThoughtChunk，实际: {:?}", other),
    }
}

#[test]
fn test_ai_reasoning_with_source_agent_id_forwards_to_notifier() {
    // SubAgent reasoning → 应携带 source_agent_id，使 TUI notifier
    // 的 agent_thought_chunk handler 正确路由到 SubAgentGroup
    let event = ExecutorEvent::AiReasoning {
        message_id: peri_acp_types::messages::MessageId::new(),
        text: "subagent thinking...".to_string(),
        source_agent_id: Some("sa-1".to_string()),
    };
    let mapped = crate::event::mapper::map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(
        mapped[0].source_agent_id.as_deref(),
        Some("sa-1"),
        "SubAgent reasoning 应携带 source_agent_id"
    );
    match &mapped[0].updates[0] {
        SessionUpdate::AgentThoughtChunk(chunk) => match &chunk.content {
            ContentBlock::Text(tc) => assert_eq!(tc.text, "subagent thinking..."),
            other => panic!("预期 Text，实际: {other:?}"),
        },
        other => panic!("预期 AgentThoughtChunk，实际: {other:?}"),
    }
}

include!("mapper_test_part2_test.rs");
