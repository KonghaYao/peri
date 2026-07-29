//! Tests for projection — 投影类型单元测试 + render_llm_view 协议测试

use super::projection::{
    estimate_projection_chars, render_llm_view, MessageProjectionDirective, MicroCompactPlan,
    ProjectionAction, ProjectionActionEntry, ProjectionTarget, ProviderCapabilities,
    ProviderProtocol, PROJECTION_POLICY_VERSION,
};
use crate::messages::{BaseMessage, ContentBlock, MessageContent, MessageId};
use crate::session::transcript::MessageTranscript;

// ─── 辅助函数 ────────────────────────────────────────────────────────────────

/// 创建一个包含单条 Ai 消息（含 tool_use）和 ToolResult 的 transcript
fn transcript_with_tool_exchange(
    tool_call_id: &str,
    tool_input: serde_json::Value,
    tool_result_text: &str,
    is_error: bool,
) -> MessageTranscript {
    let mut t = MessageTranscript::new();
    let blocks = vec![
        ContentBlock::text("I'll use a tool"),
        ContentBlock::tool_use(tool_call_id, "Bash", tool_input),
    ];
    let ai_msg = BaseMessage::ai_from_blocks(blocks);
    t.append(ai_msg);
    if is_error {
        t.append(BaseMessage::tool_error(tool_call_id, tool_result_text));
    } else {
        t.append(BaseMessage::tool_result(tool_call_id, tool_result_text));
    }
    t
}

/// 创建一个包含含 Image block 的 Human 消息的 transcript
fn transcript_with_image(media_type: &str, data: &str) -> MessageTranscript {
    let mut t = MessageTranscript::new();
    t.append(BaseMessage::human(MessageContent::Blocks(vec![
        ContentBlock::text("What's in this image?"),
        ContentBlock::image_base64(media_type, data),
    ])));
    t
}

/// 创建一个包含 Document block 的 Human 消息的 transcript
fn transcript_with_document(title: Option<&str>, data: &str) -> MessageTranscript {
    let mut t = MessageTranscript::new();
    let mut blocks = vec![ContentBlock::text("Analyze this document")];
    blocks.push(ContentBlock::Document {
        source: crate::messages::DocumentSource::Base64 {
            media_type: "application/pdf".into(),
            data: data.into(),
        },
        title: title.map(|s| s.to_string()),
    });
    t.append(BaseMessage::human(MessageContent::Blocks(blocks)));
    t
}

// ─── 现有测试 ────────────────────────────────────────────────────────────────

#[test]
fn test_projection_directive_serde_roundtrip() {
    let msg_id = MessageId::new();
    let directive = MessageProjectionDirective {
        policy_version: PROJECTION_POLICY_VERSION,
        entries: vec![ProjectionActionEntry {
            message_id: msg_id,
            target: ProjectionTarget::ToolCall {
                tool_call_id: "tc_123".into(),
            },
            action: ProjectionAction::CompactToolInput {
                fields: vec!["command".into(), "description".into()],
                keep_head: 600,
                keep_tail: 200,
            },
        }],
    };
    let json = serde_json::to_string(&directive).expect("序列化失败");
    let restored: MessageProjectionDirective = serde_json::from_str(&json).expect("反序列化失败");
    assert_eq!(restored, directive);
    assert_eq!(restored.policy_version, PROJECTION_POLICY_VERSION);
    let ProjectionAction::CompactToolInput {
        fields,
        keep_head,
        keep_tail,
    } = &restored.entries[0].action
    else {
        panic!("应恢复 CompactToolInput action");
    };
    assert_eq!(fields, &["command", "description"]);
    assert_eq!(*keep_head, 600);
    assert_eq!(*keep_tail, 200);
}

#[test]
fn test_legacy_message_flags_deserialize_without_directive() {
    use crate::session::transcript::MessageFlags;

    let legacy_json = r#"{"truncated":true,"excluded":false}"#;
    let flags: MessageFlags = serde_json::from_str(legacy_json).expect("旧 JSON 反序列化失败");
    assert!(flags.truncated);
    assert!(!flags.excluded);
    assert!(
        flags.projection.is_none(),
        "旧 JSON 反序列化后 projection 应为 None"
    );
}

#[test]
fn test_legacy_v1_compact_tool_input_deserializes_but_is_rejected_by_policy_version() {
    use crate::session::transcript::MessageFlags;

    let mut transcript = MessageTranscript::new();
    let message = BaseMessage::human("legacy compacted content");
    let message_id = message.id();
    transcript.append(message);

    let legacy_v1_json = format!(
        r#"{{"truncated":true,"excluded":false,"projection":{{"policy_version":1,"entries":[{{"message_id":"{}","target":"Message","action":{{"CompactToolInput":{{"fields":["command"],"preserve_shape":true}}}}}}]}}}}"#,
        message_id.as_uuid()
    );
    let flags: MessageFlags =
        serde_json::from_str(&legacy_v1_json).expect("v1 CompactToolInput JSON 应可反序列化");
    let directive = flags
        .projection
        .expect("v1 JSON 应包含 projection directive");
    let ProjectionAction::CompactToolInput {
        fields,
        keep_head,
        keep_tail,
    } = &directive.entries[0].action
    else {
        panic!("应恢复 CompactToolInput action");
    };
    assert_eq!(fields, &["command"]);
    assert_eq!(*keep_head, 350);
    assert_eq!(*keep_tail, 100);

    transcript.set_flags_projection(message_id, directive);
    let error =
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION)
            .expect_err("旧 v1 directive 绝不能按当前整体替换语义应用");
    assert!(
        error
            .to_string()
            .contains(super::projection::DIRECTIVE_VERSION_MISMATCH),
        "旧 v1 directive 应因版本不匹配被拒绝"
    );
}

#[test]
fn test_message_flags_with_projection_serde_roundtrip() {
    use crate::session::transcript::MessageFlags;

    let msg_id = MessageId::new();
    let flags = MessageFlags {
        truncated: true,
        excluded: false,
        projection: Some(MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: msg_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::Keep,
            }],
        }),
    };
    let json = serde_json::to_string(&flags).expect("序列化失败");
    let restored: MessageFlags = serde_json::from_str(&json).expect("反序列化失败");
    assert_eq!(restored, flags);
}

#[test]
fn test_provider_capabilities_openai() {
    let caps = ProviderCapabilities::openai();
    assert_eq!(caps.protocol, ProviderProtocol::OpenAI);
    assert!(!caps.signed_reasoning_must_be_whole);
}

#[test]
fn test_provider_capabilities_anthropic() {
    let caps = ProviderCapabilities::anthropic();
    assert_eq!(caps.protocol, ProviderProtocol::Anthropic);
    assert!(caps.signed_reasoning_must_be_whole);
}

#[test]
fn test_provider_capabilities_default_safety() {
    let caps = ProviderCapabilities::default();
    assert_eq!(caps.protocol, ProviderProtocol::Generic);
    assert!(!caps.signed_reasoning_must_be_whole);
}

#[test]
fn test_projection_action_exclude_and_keep() {
    let entry = ProjectionActionEntry {
        message_id: MessageId::new(),
        target: ProjectionTarget::ContentBlock { index: 2 },
        action: ProjectionAction::Exclude,
    };
    let json = serde_json::to_string(&entry).unwrap();
    let restored: ProjectionActionEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, entry);
}

// ─── render_llm_view 协议测试 ─────────────────────────────────────────────────

#[test]
fn test_blocks_image_projection_removes_base64() {
    let transcript = transcript_with_image("image/png", "AAAAbase64payload==");
    let visible = transcript.visible_messages();
    let msg_id = visible[0].id();

    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![ProjectionActionEntry {
            message_id: msg_id,
            target: ProjectionTarget::ContentBlock { index: 1 },
            action: ProjectionAction::ReplaceMedia {
                placeholder: "image".to_string(),
            },
        }],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 1,
        ..Default::default()
    };

    let caps = ProviderCapabilities::default();
    let projected = render_llm_view(&transcript, &plan, &caps).expect("render_llm_view 应成功");

    // 投影后不应含 Base64 payload
    let blocks = projected[0].content_blocks();
    let has_base64 = blocks.iter().any(|b| {
        matches!(
            b,
            ContentBlock::Image {
                source: crate::messages::ImageSource::Base64 { .. }
            }
        )
    });
    assert!(!has_base64, "投影后不应包含 Base64 Image block");

    // 图片 block 应变成 Text 占位符
    let has_placeholder = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("图片已压缩")));
    assert!(has_placeholder, "投影后应包含图片占位文本");
}

#[test]
fn test_blocks_document_projection_removes_base64() {
    let transcript = transcript_with_document(Some("report.pdf"), "AAAApdfbase64payload==");
    let visible = transcript.visible_messages();
    let msg_id = visible[0].id();

    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![ProjectionActionEntry {
            message_id: msg_id,
            target: ProjectionTarget::ContentBlock { index: 1 },
            action: ProjectionAction::ReplaceMedia {
                placeholder: "doc".to_string(),
            },
        }],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 1,
        ..Default::default()
    };

    let caps = ProviderCapabilities::default();
    let projected = render_llm_view(&transcript, &plan, &caps).expect("render_llm_view 应成功");

    let blocks = projected[0].content_blocks();
    let has_doc_base64 = blocks.iter().any(|b| {
        matches!(
            b,
            ContentBlock::Document {
                source: crate::messages::DocumentSource::Base64 { .. },
                ..
            }
        )
    });
    assert!(!has_doc_base64, "投影后不应包含 Base64 Document block");

    // 标题应保留在占位文本中
    let has_title = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("report.pdf")));
    assert!(has_title, "投影后占位文本应包含文档标题");
}

#[test]
fn test_tool_input_projection_compacts_only_selected_long_field_and_syncs_tool_use() {
    let long_prompt = format!("{}尾部", "头部".repeat(300));
    let tool_input = serde_json::json!({
        "prompt": long_prompt,
        "required": "short",
        "unselected": "x".repeat(600),
        "nested": {"prompt": "x".repeat(600)},
        "items": ["x".repeat(600)],
    });
    let transcript = transcript_with_tool_exchange("tc_1", tool_input.clone(), "ok", false);
    let ai_msg_id = transcript.visible_messages()[0].id();
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![ProjectionActionEntry {
            message_id: ai_msg_id,
            target: ProjectionTarget::ToolCall {
                tool_call_id: "tc_1".into(),
            },
            action: ProjectionAction::CompactToolInput {
                fields: vec!["prompt".into()],
                keep_head: 10,
                keep_tail: 4,
            },
        }],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 1,
        ..Default::default()
    };

    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("render_llm_view 应成功");
    let BaseMessage::Ai {
        content,
        tool_calls,
        ..
    } = &projected[0]
    else {
        panic!("第一条消息应为 Ai 消息");
    };
    let arguments = &tool_calls[0].arguments;
    let object = arguments
        .as_object()
        .expect("Tool input 应保持 JSON object 根类型");
    let prompt = object["prompt"].as_str().expect("prompt 应为 string");
    assert!(prompt.starts_with("头部头部头部头部头部"));
    assert!(prompt.ends_with("部尾部"));
    assert!(prompt.contains("字符已省略"));
    assert_eq!(object["required"], "short");
    assert_eq!(object["unselected"], tool_input["unselected"]);
    assert_eq!(object["nested"], tool_input["nested"]);
    assert_eq!(object["items"], tool_input["items"]);
    assert!(!arguments.to_string().contains("_compact_note"));

    let tool_use = content
        .content_blocks()
        .into_iter()
        .find(|block| matches!(block, ContentBlock::ToolUse { id, .. } if id == "tc_1"))
        .expect("应保留 ToolUse block");
    let ContentBlock::ToolUse { input, .. } = tool_use else {
        unreachable!();
    };
    assert_eq!(
        input, *arguments,
        "ToolUse input 应与 tool_calls arguments 同步"
    );
}

#[test]
fn test_tool_input_projection_short_or_invalid_selected_fields_are_noops() {
    let cases = [
        serde_json::json!({"prompt": "short"}),
        serde_json::json!({"prompt": 42}),
        serde_json::json!({"other": "x".repeat(600)}),
        serde_json::json!(["x".repeat(600)]),
    ];

    for input in cases {
        let transcript = transcript_with_tool_exchange("tc_1", input.clone(), "ok", false);
        let ai_msg_id = transcript.visible_messages()[0].id();
        let plan = MicroCompactPlan {
            policy_version: PROJECTION_POLICY_VERSION,
            target_reclaim_tokens: 0,
            actions: vec![ProjectionActionEntry {
                message_id: ai_msg_id,
                target: ProjectionTarget::ToolCall {
                    tool_call_id: "tc_1".into(),
                },
                action: ProjectionAction::CompactToolInput {
                    fields: vec!["prompt".into()],
                    keep_head: 10,
                    keep_tail: 4,
                },
            }],
            estimated_before_tokens: 0,
            estimated_after_tokens: 0,
            estimated_tokens_saved: 0,
            ..Default::default()
        };

        let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
            .expect("render_llm_view 应成功");
        let BaseMessage::Ai { tool_calls, .. } = &projected[0] else {
            panic!("第一条消息应为 Ai 消息");
        };
        assert_eq!(tool_calls[0].arguments, input);
    }
}

#[test]
fn test_short_tool_result_compact_action_is_direct_render_noop() {
    let result_text = "short successful tool result";
    let transcript = transcript_with_tool_exchange(
        "tc_short",
        serde_json::json!({"command": "ls"}),
        result_text,
        false,
    );
    let result_msg_id = transcript.visible_messages()[1].id();
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![ProjectionActionEntry {
            message_id: result_msg_id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 350,
                keep_tail: 100,
                preserve_recovery_handle: false,
            },
        }],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 0,
        ..Default::default()
    };

    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("render_llm_view 应成功");
    assert_eq!(
        projected[1].content(),
        result_text,
        "短 ToolResult 即使手工附加 CompactToolResult action 也必须原样渲染"
    );
}

#[test]
fn test_tool_result_projection_compacts_complete_multi_block_text_stream() {
    let first = "A".repeat(300);
    let second = "B".repeat(300);
    let original_content = MessageContent::Blocks(vec![
        ContentBlock::text(first.clone()),
        ContentBlock::text(second.clone()),
    ]);
    let mut transcript = MessageTranscript::new();
    let ai = BaseMessage::ai_with_tool_calls(
        MessageContent::text("thinking"),
        vec![crate::messages::ToolCallRequest::new(
            "tc_multi",
            "Read",
            serde_json::json!({}),
        )],
    );
    transcript.append(ai);
    let tool = BaseMessage::tool_result("tc_multi", original_content);
    let tool_id = tool.id();
    transcript.append(tool);
    let action = ProjectionActionEntry {
        message_id: tool_id,
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 350,
            keep_tail: 100,
            preserve_recovery_handle: false,
        },
    };
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![action.clone()],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 0,
        ..Default::default()
    };

    let expected = format!(
        "{}\n... [150 字符已省略] ...\n{}",
        "A".repeat(300) + &"B".repeat(50),
        "B".repeat(100)
    );
    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("render_llm_view 应成功");
    let BaseMessage::Tool { content, .. } = &projected[1] else {
        panic!("第二条消息应为 Tool 消息");
    };
    assert!(matches!(content, MessageContent::Text(_)));
    assert_eq!(content.text_content(), expected);

    let (before, after) = estimate_projection_chars(&transcript, &[action]);
    assert_eq!(before, 600);
    assert_eq!(after, content.text_content().chars().count() as u64);
    assert!(before > after, "多 Text block 的整体估算应体现节省");
}

#[test]
fn test_tool_result_projection_keeps_multi_block_content_at_or_below_total_limit() {
    let original_content = MessageContent::Blocks(vec![
        ContentBlock::text("A".repeat(250)),
        ContentBlock::text("B".repeat(250)),
    ]);
    let mut transcript = MessageTranscript::new();
    let ai = BaseMessage::ai_with_tool_calls(
        MessageContent::text("thinking"),
        vec![crate::messages::ToolCallRequest::new(
            "tc_short_multi",
            "Read",
            serde_json::json!({}),
        )],
    );
    transcript.append(ai);
    let tool = BaseMessage::tool_result("tc_short_multi", original_content.clone());
    let tool_id = tool.id();
    transcript.append(tool);
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![ProjectionActionEntry {
            message_id: tool_id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 300,
                keep_tail: 200,
                preserve_recovery_handle: false,
            },
        }],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 0,
        ..Default::default()
    };

    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("render_llm_view 应成功");
    let BaseMessage::Tool { content, .. } = &projected[1] else {
        panic!("第二条消息应为 Tool 消息");
    };
    assert_eq!(content, &original_content);
}

#[test]
fn test_tool_result_projection_uses_action_policy_below_planner_threshold() {
    let original_content = MessageContent::Blocks(vec![
        ContentBlock::text("A".repeat(225)),
        ContentBlock::text("B".repeat(225)),
    ]);
    let mut transcript = MessageTranscript::new();
    let ai = BaseMessage::ai_with_tool_calls(
        MessageContent::text("thinking"),
        vec![crate::messages::ToolCallRequest::new(
            "tc_custom_policy",
            "Read",
            serde_json::json!({}),
        )],
    );
    transcript.append(ai);
    let tool = BaseMessage::tool_result("tc_custom_policy", original_content);
    let tool_id = tool.id();
    transcript.append(tool);
    let action = ProjectionActionEntry {
        message_id: tool_id,
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 200,
            keep_tail: 100,
            preserve_recovery_handle: false,
        },
    };
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![action.clone()],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 0,
        ..Default::default()
    };

    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("render_llm_view 应成功");
    let BaseMessage::Tool { content, .. } = &projected[1] else {
        panic!("第二条消息应为 Tool 消息");
    };
    assert!(matches!(content, MessageContent::Text(_)));
    assert!(content.text_content().contains("字符已省略"));

    let (before, after) = estimate_projection_chars(&transcript, &[action]);
    assert_eq!(before, 450);
    assert_eq!(after, content.text_content().chars().count() as u64);
    assert!(
        before > after,
        "手工 CompactToolResult action 应产生实际节省"
    );
}

#[test]
fn test_tool_result_projection_uses_exact_head_tail_format() {
    let result_text = format!("abc{}yz", "x".repeat(496));
    assert_eq!(result_text.chars().count(), 501);
    let transcript = transcript_with_tool_exchange(
        "tc_format",
        serde_json::json!({"command": "ls"}),
        &result_text,
        false,
    );
    let result_msg_id = transcript.visible_messages()[1].id();
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![ProjectionActionEntry {
            message_id: result_msg_id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 3,
                keep_tail: 2,
                preserve_recovery_handle: false,
            },
        }],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 1,
        ..Default::default()
    };

    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("render_llm_view 应成功");
    assert_eq!(projected[1].content(), "abc\n... [496 字符已省略] ...\nyz");
}

#[test]
fn test_tool_result_projection_keeps_head_tail() {
    let long_result = "AAAA".repeat(600); // 2400 字符
    let transcript = transcript_with_tool_exchange(
        "tc_1",
        serde_json::json!({"command": "ls"}),
        &long_result,
        false,
    );
    let visible = transcript.visible_messages();
    let result_msg_id = visible[1].id();

    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![ProjectionActionEntry {
            message_id: result_msg_id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 500,
                keep_tail: 200,
                preserve_recovery_handle: false,
            },
        }],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 1,
        ..Default::default()
    };

    let caps = ProviderCapabilities::default();
    let projected = render_llm_view(&transcript, &plan, &caps).expect("render_llm_view 应成功");

    // ToolResult 内容应被截断
    if let BaseMessage::Tool { content, .. } = &projected[1] {
        let text = content.text_content();
        assert!(text.len() < long_result.len(), "截断后应更短");
        assert!(text.contains("AAAA"), "截断后应保留头部内容");
        assert!(text.contains("字符已省略"), "截断后应包含省略标记");
    } else {
        panic!("第二条消息应为 Tool 消息");
    }
}

#[test]
fn test_error_tool_result_is_unchanged() {
    let error_text = "Permission denied: cannot access /root";
    let transcript = transcript_with_tool_exchange(
        "tc_1",
        serde_json::json!({"command": "ls /root"}),
        error_text,
        true, // is_error
    );
    let visible = transcript.visible_messages();
    let result_msg_id = visible[1].id();

    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![ProjectionActionEntry {
            message_id: result_msg_id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 10,
                keep_tail: 10,
                preserve_recovery_handle: false,
            },
        }],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 1,
        ..Default::default()
    };

    let caps = ProviderCapabilities::default();
    let projected = render_llm_view(&transcript, &plan, &caps).expect("render_llm_view 应成功");

    // 错误 ToolResult 应保持不变
    if let BaseMessage::Tool {
        content, is_error, ..
    } = &projected[1]
    {
        assert!(is_error, "错误消息 is_error 应为 true");
        let text = content.text_content();
        assert_eq!(text, error_text, "错误 ToolResult 内容不应被截断");
    } else {
        panic!("第二条消息应为 Tool 消息");
    }
}

#[test]
fn test_cjk_projection_uses_character_boundary() {
    let cjk_text = "你好🌍🚀".repeat(600); // 2400 CJK/emoji 字符
    let transcript =
        transcript_with_tool_exchange("tc_1", serde_json::json!({"cmd": "test"}), &cjk_text, false);
    let visible = transcript.visible_messages();
    let result_msg_id = visible[1].id();

    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![ProjectionActionEntry {
            message_id: result_msg_id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 100,
                keep_tail: 100,
                preserve_recovery_handle: false,
            },
        }],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 1,
        ..Default::default()
    };

    let caps = ProviderCapabilities::default();
    let projected = render_llm_view(&transcript, &plan, &caps).expect("render_llm_view 应成功");

    if let BaseMessage::Tool { content, .. } = &projected[1] {
        let text = content.text_content();
        assert!(text.len() < cjk_text.len(), "CJK 截断后应更短");
        // 不应出现字节切片错误（如乱码字符）
        assert!(text.contains('你'), "截断后应包含原始 CJK 字符");
    } else {
        panic!("第二条消息应为 Tool 消息");
    }
}

#[test]
fn test_signed_reasoning_not_partially_truncated() {
    // 创建含 signed reasoning 的消息
    let mut transcript = MessageTranscript::new();
    let ai_msg = BaseMessage::ai_from_blocks(vec![
        ContentBlock::reasoning_with_signature("step-by-step thinking", "sig_abc123"),
        ContentBlock::text("final answer"),
    ]);
    let msg_id = ai_msg.id();
    transcript.append(ai_msg);

    // 对 reasoning 所在的 ContentBlock 尝试 CompactText
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![ProjectionActionEntry {
            message_id: msg_id,
            target: ProjectionTarget::ContentBlock { index: 0 },
            action: ProjectionAction::CompactText { max_chars: 5 },
        }],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 1,
        ..Default::default()
    };

    let caps = ProviderCapabilities {
        protocol: ProviderProtocol::Anthropic,
        signed_reasoning_must_be_whole: true,
    };
    let projected = render_llm_view(&transcript, &plan, &caps).expect("render_llm_view 应成功");

    // 签名不应被局部截断
    let blocks = projected[0].content_blocks();
    for block in &blocks {
        if let ContentBlock::Reasoning { text, signature } = block {
            if signature.is_some() {
                // 有签名的 reasoning 文本应完整保留（project_block 对 Reasoning 不做截断）
                assert_eq!(
                    text, "step-by-step thinking",
                    "带签名的 reasoning 不应被截断"
                );
            }
        }
    }
}

#[test]
fn test_render_llm_view_no_actions_passthrough() {
    // 无任何 action 时，消息原样通过
    let transcript = transcript_with_tool_exchange(
        "tc_1",
        serde_json::json!({"command": "ls"}),
        "file1\nfile2",
        false,
    );
    let plan = MicroCompactPlan::default();
    let caps = ProviderCapabilities::default();

    let projected = render_llm_view(&transcript, &plan, &caps).expect("render_llm_view 应成功");
    assert_eq!(projected.len(), 2, "无 action 时应保留全部可见消息");
}

#[test]
fn test_human_and_system_are_unchanged() {
    let mut transcript = MessageTranscript::new();
    let human = BaseMessage::human("hello");
    let h_id = human.id();
    let sys = BaseMessage::system("You are helpful");
    let s_id = sys.id();
    transcript.append(human);
    transcript.append(sys);

    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![
            ProjectionActionEntry {
                message_id: h_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactText { max_chars: 1 },
            },
            ProjectionActionEntry {
                message_id: s_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::Exclude,
            },
        ],
        estimated_before_tokens: 0,
        estimated_after_tokens: 0,
        estimated_tokens_saved: 1,
        ..Default::default()
    };

    let caps = ProviderCapabilities::default();
    let projected = render_llm_view(&transcript, &plan, &caps).expect("render_llm_view 应成功");

    // Human/System 永不变
    assert_eq!(projected[0].content(), "hello");
    assert_eq!(projected[1].content(), "You are helpful");
}

// ─── plan_from_persisted_directives 测试 ────────────────────────────────────

#[test]
fn test_plan_from_persisted_directives_empty_transcript() {
    // transcript 中无任何 directive → 返回错误
    let transcript = MessageTranscript::new();
    let result =
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION);
    assert!(result.is_err(), "无 directive 的 transcript 应返回错误");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains(super::projection::NO_PERSISTED_DIRECTIVES),
        "错误消息应包含 NO_PERSISTED_DIRECTIVES"
    );
}

#[test]
fn test_plan_from_persisted_directives_version_mismatch_errors() {
    // policy_version 不匹配 → 错误
    let mut t = MessageTranscript::new();
    let msg = BaseMessage::human(MessageContent::text("hello"));
    let msg_id = msg.id();
    t.append(msg);
    t.set_flags_projection(
        msg_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION + 1, // 不匹配的版本
            entries: vec![ProjectionActionEntry {
                message_id: msg_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::Keep,
            }],
        },
    );

    let result = super::projection::plan_from_persisted_directives(&t, PROJECTION_POLICY_VERSION);
    assert!(result.is_err(), "policy_version 不匹配应返回错误");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains(super::projection::DIRECTIVE_VERSION_MISMATCH),
        "错误消息应包含 DIRECTIVE_VERSION_MISMATCH"
    );
}

#[test]
fn test_plan_from_persisted_directives_legacy_truncated_passthrough() {
    // G1 fail-closed: truncated=true + projection=None → 返回 CORRUPTED_PROJECTION 错误
    let mut t = MessageTranscript::new();
    let msg = BaseMessage::human(MessageContent::text("legacy content"));
    let msg_id = msg.id();
    t.append(msg);
    t.set_truncated(msg_id, true);
    // projection 保持 None（旧行为）

    let result = super::projection::plan_from_persisted_directives(&t, PROJECTION_POLICY_VERSION);
    assert!(
        result.is_err(),
        "旧 truncated 标记（无 directive）应返回 CORRUPTED_PROJECTION 错误"
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains(super::projection::CORRUPTED_PROJECTION),
        "应报告 CORRUPTED_PROJECTION"
    );
}

#[test]
fn test_plan_from_persisted_directives_stale_config_still_renders() {
    // 即使 planner 用新 config 生成空 plan，持久化 directive 仍有效
    let tool_call_id = "tc_stale";
    let mut t = MessageTranscript::new();
    let blocks = vec![
        ContentBlock::text("I'll use a tool"),
        ContentBlock::tool_use(tool_call_id, "Bash", serde_json::json!({"cmd": "ls"})),
    ];
    let ai_msg = BaseMessage::ai_from_blocks(blocks);
    let ai_msg_id = ai_msg.id();
    t.append(ai_msg);
    let tr_msg = BaseMessage::tool_result(tool_call_id, "long output here");
    let tr_msg_id = tr_msg.id();
    t.append(tr_msg);

    // 设置 projection directive（使用 set_flags_projection）
    t.set_flags_projection(
        ai_msg_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: ai_msg_id,
                target: ProjectionTarget::ToolCall {
                    tool_call_id: tool_call_id.to_string(),
                },
                action: ProjectionAction::CompactToolInput {
                    fields: vec![],
                    keep_head: 350,
                    keep_tail: 100,
                },
            }],
        },
    );
    t.set_flags_projection(
        tr_msg_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: tr_msg_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 500,
                    keep_tail: 200,
                    preserve_recovery_handle: true,
                },
            }],
        },
    );

    let result = super::projection::plan_from_persisted_directives(&t, PROJECTION_POLICY_VERSION);
    assert!(
        result.is_ok(),
        "持久化 directive 应在 stale config 下仍有效"
    );
    let plan = result.unwrap();
    assert_eq!(plan.actions.len(), 2, "应有 2 条 action entries");
}

#[test]
fn test_plan_from_persisted_directives_saves_by_character_difference() {
    let mut transcript = transcript_with_tool_exchange(
        "tc_persisted",
        serde_json::json!({"prompt": "x".repeat(501)}),
        "ok",
        false,
    );
    let message_id = transcript.visible_messages()[0].id();
    transcript.set_flags_projection(
        message_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::ToolCall {
                    tool_call_id: "tc_persisted".into(),
                },
                action: ProjectionAction::CompactToolInput {
                    fields: vec!["prompt".into()],
                    keep_head: 350,
                    keep_tail: 100,
                },
            }],
        },
    );

    let plan =
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION)
            .expect("应从持久化 directive 重建 plan");
    assert_eq!(plan.estimated_before_tokens, 125);
    assert_eq!(plan.estimated_after_tokens, 117);
    assert_eq!(plan.estimated_tokens_saved, 7);
}

#[test]
fn test_plan_from_persisted_directives_collects_all_directives() {
    // 多条消息各有 directive → 全部收集到一个 plan 中
    let mut t = MessageTranscript::new();
    let mut expected_count = 0u32;

    for i in 0..3 {
        let msg = BaseMessage::human(MessageContent::text(format!("msg {}", i)));
        let msg_id = msg.id();
        t.append(msg);

        t.set_flags_projection(
            msg_id,
            MessageProjectionDirective {
                policy_version: PROJECTION_POLICY_VERSION,
                entries: vec![ProjectionActionEntry {
                    message_id: msg_id,
                    target: ProjectionTarget::Message,
                    action: ProjectionAction::CompactText { max_chars: 10 },
                }],
            },
        );
        expected_count += 1;
    }

    let result = super::projection::plan_from_persisted_directives(&t, PROJECTION_POLICY_VERSION);
    assert!(result.is_ok(), "应收集所有 directive 消息");
    let plan = result.unwrap();
    assert_eq!(
        plan.actions.len(),
        expected_count as usize,
        "应收集所有 {} 条 directive",
        expected_count
    );
}

#[test]
fn test_render_llm_view_from_persisted_directives() {
    // 端到端：transcript + persisted directive → plan_from_persisted_directives → render_llm_view
    let tool_call_id = "tc_e2e";
    let mut t = MessageTranscript::new();
    let blocks = vec![
        ContentBlock::text("I'll use bash"),
        ContentBlock::tool_use(tool_call_id, "Bash", serde_json::json!({"cmd": "ls -la"})),
    ];
    let ai_msg = BaseMessage::ai_from_blocks(blocks);
    let ai_msg_id = ai_msg.id();
    t.append(ai_msg);
    let tr_msg = BaseMessage::tool_result(tool_call_id, "AAAAAAAA".repeat(100));
    let tr_msg_id = tr_msg.id();
    t.append(tr_msg);

    t.set_flags_projection(
        ai_msg_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: ai_msg_id,
                target: ProjectionTarget::ToolCall {
                    tool_call_id: tool_call_id.to_string(),
                },
                action: ProjectionAction::CompactToolInput {
                    fields: vec![],
                    keep_head: 350,
                    keep_tail: 100,
                },
            }],
        },
    );
    t.set_flags_projection(
        tr_msg_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: tr_msg_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 50,
                    keep_tail: 50,
                    preserve_recovery_handle: false,
                },
            }],
        },
    );

    let plan_result =
        super::projection::plan_from_persisted_directives(&t, PROJECTION_POLICY_VERSION);
    assert!(plan_result.is_ok(), "应从持久化 directive 重建 plan");
    let caps = ProviderCapabilities::default();
    let projected =
        render_llm_view(&t, &plan_result.unwrap(), &caps).expect("render_llm_view 应成功");

    // 验证 ToolResult 被截断
    let tr_projected = &projected[1];
    let text = tr_projected.content();
    assert!(text.len() < 800, "投影后 ToolResult 应被截断");
    assert!(text.contains("AAAA"), "投影后应保留头部内容");
}

#[test]
fn test_estimate_projection_chars_counts_only_actually_truncated_values() {
    let selected = "x".repeat(600);
    let unselected = "y".repeat(700);
    let transcript = transcript_with_tool_exchange(
        "tc_1",
        serde_json::json!({
            "selected": selected,
            "unselected": unselected,
            "short": "keep",
        }),
        "short result",
        false,
    );
    let visible = transcript.visible_messages();
    let actions = vec![
        ProjectionActionEntry {
            message_id: visible[0].id(),
            target: ProjectionTarget::ToolCall {
                tool_call_id: "tc_1".into(),
            },
            action: ProjectionAction::CompactToolInput {
                fields: vec!["missing".into(), "selected".into(), "short".into()],
                keep_head: 10,
                keep_tail: 5,
            },
        },
        ProjectionActionEntry {
            message_id: visible[1].id(),
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 10,
                keep_tail: 5,
                preserve_recovery_handle: false,
            },
        },
    ];

    let (before, after) = estimate_projection_chars(&transcript, &actions);
    let expected_after = format!(
        "{}\n... [585 字符已省略] ...\n{}",
        "x".repeat(10),
        "x".repeat(5)
    );
    assert_eq!(before, 600, "未选择字段和短 ToolResult 不应计入估算");
    assert_eq!(after, expected_after.chars().count() as u64);
}

#[test]
fn test_estimate_projection_chars_deduplicates_repeated_tool_input_fields() {
    let prompt = "x".repeat(600);
    let transcript = transcript_with_tool_exchange(
        "tc_1",
        serde_json::json!({"prompt": prompt}),
        "short result",
        false,
    );
    let message_id = transcript.visible_messages()[0].id();
    let single_field_action = ProjectionActionEntry {
        message_id,
        target: ProjectionTarget::ToolCall {
            tool_call_id: "tc_1".into(),
        },
        action: ProjectionAction::CompactToolInput {
            fields: vec!["prompt".into()],
            keep_head: 350,
            keep_tail: 100,
        },
    };
    let repeated_field_action = ProjectionActionEntry {
        message_id,
        target: ProjectionTarget::ToolCall {
            tool_call_id: "tc_1".into(),
        },
        action: ProjectionAction::CompactToolInput {
            fields: vec!["prompt".into(), "prompt".into()],
            keep_head: 350,
            keep_tail: 100,
        },
    };

    let single_estimate = estimate_projection_chars(&transcript, &[single_field_action]);
    let repeated_estimate = estimate_projection_chars(&transcript, &[repeated_field_action]);

    assert_eq!(repeated_estimate.0, single_estimate.0);
    assert_eq!(repeated_estimate.1, single_estimate.1);
    assert!(
        single_estimate.0 > single_estimate.1,
        "501+ 字符顶层 prompt 应产生真实节省"
    );
}
