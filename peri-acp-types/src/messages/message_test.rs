use super::*;

#[test]
fn test_ai_from_blocks_extracts_tool_calls() {
    let blocks = vec![
        ContentBlock::text("I'll use a tool"),
        ContentBlock::tool_use("id1", "Bash", serde_json::json!({"command": "ls"})),
    ];
    let msg = BaseMessage::ai_from_blocks(blocks);
    assert!(msg.has_tool_calls());
    assert_eq!(msg.tool_calls().len(), 1);
    assert_eq!(msg.tool_calls()[0].name, "Bash");
}

#[test]
fn test_base_message_content_blocks_lazy_parse() {
    let msg = BaseMessage::ai(MessageContent::Blocks(vec![
        ContentBlock::reasoning("thinking..."),
        ContentBlock::text("answer"),
    ]));
    let blocks = msg.content_blocks();
    assert_eq!(blocks.len(), 2);
    assert!(matches!(blocks[0], ContentBlock::Reasoning { .. }));
    assert_eq!(blocks[1].as_text(), Some("answer"));
}

#[test]
fn test_human_message_multimodal() {
    let msg = BaseMessage::human(MessageContent::Blocks(vec![
        ContentBlock::text("What's in this image?"),
        ContentBlock::image_url("https://example.com/image.jpg"),
    ]));
    let blocks = msg.content_blocks();
    assert_eq!(blocks.len(), 2);
    assert!(matches!(blocks[1], ContentBlock::Image { .. }));
}

#[test]
fn test_message_id_generated() {
    // 不同消息的 id 应不同
    let m1 = BaseMessage::human("hello");
    let m2 = BaseMessage::human("hello");
    assert_ne!(m1.id(), m2.id(), "两条消息 id 应不同");

    // 序列化/反序列化后 id 保持一致
    let json = serde_json::to_string(&m1).unwrap();
    let restored: BaseMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.id(), m1.id(), "反序列化后 id 应保持不变");
}

#[test]
fn test_tool_call_id_persistence() {
    // 模拟完整的工具调用流程：
    // 1. AI 消息包含 tool_calls（id=toolu_123）
    // 2. Tool 消息的 tool_call_id 也是 toolu_123
    use crate::messages::ContentBlock;
    let blocks = vec![
        ContentBlock::text("I'll read a file"),
        ContentBlock::tool_use("toolu_123", "Read", serde_json::json!({"path": "test.txt"})),
    ];
    let ai_msg = BaseMessage::ai_from_blocks(blocks);

    // 验证 AI 消息包含 tool_calls
    let tcs = ai_msg.tool_calls();
    assert_eq!(tcs.len(), 1);
    assert_eq!(tcs[0].id, "toolu_123");
    assert_eq!(tcs[0].name, "Read");

    // 序列化
    let json = serde_json::to_string(&ai_msg).unwrap();

    // 反序列化
    let restored: BaseMessage = serde_json::from_str(&json).unwrap();

    // 验证 tool_calls 仍然存在
    let tcs = restored.tool_calls();
    assert_eq!(tcs.len(), 1, "反序列化后 tool_calls 应该保留");
    assert_eq!(tcs[0].id, "toolu_123");

    // 模拟 Tool 消息
    let tool_msg = BaseMessage::tool_result("toolu_123", "file content");
    let tool_json = serde_json::to_string(&tool_msg).unwrap();
    let restored_tool: BaseMessage = serde_json::from_str(&tool_json).unwrap();

    if let BaseMessage::Tool { tool_call_id, .. } = restored_tool {
        assert_eq!(tool_call_id, "toolu_123");
    } else {
        unreachable!("Tool 消息反序列化失败");
    }
}

#[test]
fn test_tool_execution_evidence_persists_and_legacy_payload_is_unknown() {
    let evidence = crate::tools::ToolExecutionEvidence {
        status: crate::tools::ToolExecutionStatus::Failed,
        exit_code: Some(9),
        output_ref: Some("/tmp/full-output.txt".into()),
        output_truncated: true,
        task_id: None,
    };
    let message =
        BaseMessage::tool_result_with_execution("call-1", "bounded", true, Some(evidence.clone()));
    let restored: BaseMessage =
        serde_json::from_str(&serde_json::to_string(&message).unwrap()).unwrap();
    let BaseMessage::Tool {
        execution: Some(restored_evidence),
        is_error,
        ..
    } = restored
    else {
        panic!("typed evidence should survive message persistence");
    };
    assert!(is_error);
    assert_eq!(restored_evidence, evidence);

    let mut legacy_json =
        serde_json::to_value(BaseMessage::tool_result("call-legacy", "old result")).unwrap();
    legacy_json
        .as_object_mut()
        .expect("tool message object")
        .remove("execution");
    let legacy: BaseMessage = serde_json::from_value(legacy_json).unwrap();
    assert!(matches!(
        legacy,
        BaseMessage::Tool {
            execution: None,
            ..
        }
    ));
}

#[test]
fn test_tool_message_roundtrips_safe_retry_failure_facts() {
    let failure = crate::error::SafeSubagentFailure::new(
        "child-retry",
        crate::error::SafeModelErrorDiagnostic::from_model(
            peri_model::ModelError::retry_exhausted(3, peri_model::RetryErrorKind::HttpStatus)
                .expect("valid attempts")
                .diagnostic(),
        ),
    )
    .expect("valid safe child failure");
    let message = BaseMessage::tool_result_with_execution_and_failure(
        "call-retry",
        "child failed after retries",
        true,
        None,
        Some(failure.clone()),
    );
    let restored: BaseMessage =
        serde_json::from_str(&serde_json::to_string(&message).unwrap()).unwrap();
    let BaseMessage::Tool {
        subagent_failure: Some(restored_failure),
        ..
    } = restored
    else {
        panic!("safe child failure should survive canonical message serde");
    };
    assert_eq!(restored_failure, failure);
    assert_eq!(restored_failure.diagnostic().retry_attempts(), Some(3));
}

#[test]
fn test_tool_message_persists_safe_subagent_failure_facts_only() {
    let failure = crate::error::SafeSubagentFailure::new(
        "child-123",
        crate::error::SafeModelErrorDiagnostic::from_model(
            peri_model::ModelError::http_status(429, "provider.example", Some("req-123"))
                .diagnostic(),
        ),
    )
    .expect("safe child failure");
    let message = BaseMessage::tool_result_with_execution_and_failure(
        "call-1",
        "child-123\nmodel_error_status: 429",
        true,
        None,
        Some(failure),
    );
    let wire = serde_json::to_string(&message).expect("serialize tool message");
    assert!(wire.contains("child_thread_id"));
    assert!(wire.contains("model_error_status"));
    assert!(!wire.contains("request body"));
    let restored: BaseMessage = serde_json::from_str(&wire).expect("deserialize tool message");
    assert!(matches!(
        restored,
        BaseMessage::Tool {
            subagent_failure: Some(_),
            ..
        }
    ));
}
