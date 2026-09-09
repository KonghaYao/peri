//! Tests for projection — 投影类型单元测试 + render_llm_view 协议测试

use super::projection::{
    estimate_projection_chars, render_llm_view, MessageProjectionDirective, MicroCompactPlan,
    PersistedDirectiveRestore, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
    ProviderCapabilities, ProviderProtocol, PROJECTION_POLICY_VERSION,
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
    use peri_acp_types::store::MessageFlags;

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
    use peri_acp_types::store::MessageFlags;

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
    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION,),
        PersistedDirectiveRestore::Invalid
    ));
}

#[test]
fn test_message_flags_with_projection_serde_roundtrip() {
    use peri_acp_types::store::MessageFlags;

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

#[test]
fn estimate_projection_chars_ignores_directive_targeting_reminder() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let mut transcript = MessageTranscript::new();
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("estimate_test".into()),
            kind: "status".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "control".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let id = transcript.append_system_reminder(reminder);
    let action = ProjectionActionEntry {
        message_id: id,
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 1,
            keep_tail: 1,
            preserve_recovery_handle: true,
        },
    };

    assert_eq!(estimate_projection_chars(&transcript, &[action]), (0, 0));
    assert_eq!(transcript.flags(id), Default::default());
}

#[test]
fn canonical_reminder_renders_once_with_and_without_directives() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("compact_test".into()),
            kind: "done".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "compact reminder".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let mut transcript = MessageTranscript::new();
    let reminder_id = transcript.append_system_reminder(reminder);
    let normal_id = transcript.append(BaseMessage::human("normal"));

    for plan in [
        MicroCompactPlan::default(),
        MicroCompactPlan {
            actions: vec![ProjectionActionEntry {
                message_id: normal_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::Keep,
            }],
            ..Default::default()
        },
    ] {
        let projected =
            render_llm_view(&transcript, &plan, &ProviderCapabilities::default()).unwrap();
        let reminders: Vec<_> = projected
            .iter()
            .filter(|message| message.id() == reminder_id)
            .collect();
        assert_eq!(reminders.len(), 1);
        assert!(matches!(reminders[0], BaseMessage::Human { .. }));
        assert!(!reminders[0].content().is_empty());
        assert_eq!(
            reminders[0].content().matches("</system-reminder>").count(),
            1
        );
    }
}

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
    assert_eq!(
        arguments, &tool_input,
        "ToolCall arguments 必须保持 canonical 原值"
    );

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
fn regression_glob_short_pattern_projection_contains_no_compact_note() {
    // 回归验证：短参数工具调用投影后不含 _compact_note 占位
    // 原始报错：{"_compact_note":"tool input compacted"} → The 'pattern' parameter is required
    let transcript = transcript_with_tool_exchange(
        "tc_1",
        serde_json::json!({"pattern": "**/*.rs"}),
        "ok",
        false,
    );
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
                fields: vec![],
                keep_head: 350,
                keep_tail: 100,
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
    assert_eq!(
        tool_calls[0].arguments,
        serde_json::json!({"pattern": "**/*.rs"}),
        "短参数投影应为 no-op，保持原样"
    );
    assert!(
        !tool_calls[0]
            .arguments
            .to_string()
            .contains("_compact_note"),
        "投影视图不应出现 _compact_note 占位"
    );
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
fn test_plan_from_persisted_directives_empty_transcript_is_absent() {
    let transcript = MessageTranscript::new();
    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION,),
        PersistedDirectiveRestore::Absent
    ));
}

#[test]
fn test_plan_from_persisted_directives_version_mismatch_is_invalid() {
    let mut transcript = MessageTranscript::new();
    let message = BaseMessage::human("hello");
    let message_id = message.id();
    transcript.append(message);
    transcript.set_flags_projection(
        message_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION + 1,
            entries: vec![],
        },
    );

    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION,),
        PersistedDirectiveRestore::Invalid
    ));
}

#[test]
fn test_plan_from_persisted_directives_truncated_without_directive_is_invalid() {
    let mut transcript = MessageTranscript::new();
    let message = BaseMessage::human("legacy content");
    let message_id = message.id();
    transcript.append(message);
    transcript.set_truncated(message_id, true);

    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION,),
        PersistedDirectiveRestore::Invalid
    ));
}

#[test]
fn test_plan_from_persisted_directives_wrong_message_id_is_invalid() {
    let mut transcript = MessageTranscript::new();
    let message = BaseMessage::tool_result("tc", "x".repeat(600));
    let message_id = message.id();
    transcript.append(message);
    transcript.set_flags_projection(
        message_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: MessageId::new(),
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 10,
                    keep_tail: 10,
                    preserve_recovery_handle: false,
                },
            }],
        },
    );

    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION,),
        PersistedDirectiveRestore::Invalid
    ));
}

#[test]
fn test_persisted_restore_keeps_only_independent_tool_result_action() {
    let mut transcript = transcript_with_tool_exchange(
        "tc_legacy",
        serde_json::json!({"prompt": "x".repeat(600)}),
        &"R".repeat(600),
        false,
    );
    let (ai_id, result_id, canonical_content, canonical_tool_calls) = {
        let visible = transcript.visible_messages();
        let (canonical_content, canonical_tool_calls) = match &visible[0] {
            BaseMessage::Ai {
                content,
                tool_calls,
                ..
            } => (content.clone(), tool_calls.clone()),
            _ => unreachable!(),
        };
        (
            visible[0].id(),
            visible[1].id(),
            canonical_content,
            canonical_tool_calls,
        )
    };
    transcript.set_flags_projection(
        ai_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![
                ProjectionActionEntry {
                    message_id: ai_id,
                    target: ProjectionTarget::ToolCall {
                        tool_call_id: "tc_legacy".into(),
                    },
                    action: ProjectionAction::CompactToolInput {
                        fields: vec!["prompt".into()],
                        keep_head: 10,
                        keep_tail: 10,
                    },
                },
                ProjectionActionEntry {
                    message_id: ai_id,
                    target: ProjectionTarget::Message,
                    action: ProjectionAction::Exclude,
                },
                ProjectionActionEntry {
                    message_id: ai_id,
                    target: ProjectionTarget::ContentBlock { index: 1 },
                    action: ProjectionAction::CompactText { max_chars: 1 },
                },
            ],
        },
    );
    transcript.set_flags_projection(
        result_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: result_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 20,
                    keep_tail: 20,
                    preserve_recovery_handle: false,
                },
            }],
        },
    );

    let PersistedDirectiveRestore::Valid(plan) =
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION)
    else {
        panic!("当前可解码 legacy directive 应恢复为安全子集");
    };
    assert!(matches!(
        &plan.actions[..],
        [ProjectionActionEntry {
            message_id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult { .. },
        }] if *message_id == result_id
    ));

    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("安全子集应可渲染");
    let BaseMessage::Ai {
        content,
        tool_calls,
        ..
    } = &projected[0]
    else {
        panic!("第一条消息应为 Ai");
    };
    assert_eq!(tool_calls, &canonical_tool_calls);
    assert_eq!(content, &canonical_content);
    assert!(projected[1].content().contains("字符已省略"));
}

#[test]
fn test_render_llm_view_from_persisted_tool_result_directive() {
    let mut transcript = transcript_with_tool_exchange(
        "tc_e2e",
        serde_json::json!({"cmd": "ls -la"}),
        &"A".repeat(800),
        false,
    );
    let result_id = transcript.visible_messages()[1].id();
    transcript.set_flags_projection(
        result_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: result_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 50,
                    keep_tail: 50,
                    preserve_recovery_handle: false,
                },
            }],
        },
    );

    let PersistedDirectiveRestore::Valid(plan) =
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION)
    else {
        panic!("应恢复 ToolResult action");
    };
    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("render_llm_view 应成功");
    assert!(projected[1].content().contains("字符已省略"));
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

    assert_eq!(
        estimate_projection_chars(&transcript, &actions),
        (0, 0),
        "ToolUse 输入受硬保护，不应计入可回收字符"
    );
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
    assert_eq!(single_estimate, (0, 0));
}

#[test]
fn duplicate_or_conflicting_tool_result_actions_fail_closed_for_render_and_estimate() {
    let transcript = transcript_with_tool_exchange(
        "tc_conflict",
        serde_json::json!({"path": "f"}),
        &"x".repeat(600),
        false,
    );
    let message_id = transcript.visible_messages()[1].id();
    let compact = ProjectionAction::CompactToolResult {
        keep_head: 10,
        keep_tail: 10,
        preserve_recovery_handle: false,
    };

    for actions in [
        vec![
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::Message,
                action: compact.clone(),
            },
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::Message,
                action: compact.clone(),
            },
        ],
        vec![
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::ContentBlock { index: 0 },
                action: compact.clone(),
            },
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 1_000,
                    keep_tail: 1_000,
                    preserve_recovery_handle: false,
                },
            },
        ],
    ] {
        let plan = MicroCompactPlan {
            actions: actions.clone(),
            ..Default::default()
        };
        let rendered =
            render_llm_view(&transcript, &plan, &ProviderCapabilities::default()).unwrap();
        assert_eq!(rendered[1].content(), "x".repeat(600));
        assert_eq!(estimate_projection_chars(&transcript, &actions), (0, 0));
    }
}

#[test]
fn test_renderer_preserves_tool_use_for_illegal_block_actions() {
    let first_input = serde_json::json!({"content": "A".repeat(600)});
    let second_input = serde_json::json!({"command": "printf safe"});
    let blocks = vec![
        ContentBlock::text("before"),
        ContentBlock::tool_use("tc_1", "Write", first_input.clone()),
        ContentBlock::text("between"),
        ContentBlock::tool_use("tc_2", "Bash", second_input.clone()),
        ContentBlock::text("after"),
    ];
    let mut transcript = MessageTranscript::new();
    let message = BaseMessage::ai_from_blocks(blocks.clone());
    let message_id = message.id();
    transcript.append(message);
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        actions: vec![
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::ContentBlock { index: 1 },
                action: ProjectionAction::Exclude,
            },
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::ContentBlock { index: 3 },
                action: ProjectionAction::CompactText { max_chars: 1 },
            },
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::ToolCall {
                    tool_call_id: "tc_1".into(),
                },
                action: ProjectionAction::CompactToolInput {
                    fields: vec!["content".into()],
                    keep_head: 1,
                    keep_tail: 1,
                },
            },
        ],
        ..Default::default()
    };

    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("非法组合应防御性 no-op");
    let BaseMessage::Ai {
        content,
        tool_calls,
        ..
    } = &projected[0]
    else {
        panic!("应为 Ai 消息");
    };
    assert_eq!(content, &MessageContent::Blocks(blocks));
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0].id, "tc_1");
    assert_eq!(tool_calls[0].name, "Write");
    assert_eq!(tool_calls[0].arguments, first_input);
    assert_eq!(tool_calls[1].id, "tc_2");
    assert_eq!(tool_calls[1].name, "Bash");
    assert_eq!(tool_calls[1].arguments, second_input);
}

#[test]
fn test_estimator_fails_closed_for_duplicate_tool_result_actions() {
    let mut transcript = MessageTranscript::new();
    let result = BaseMessage::tool_result("tc", "x".repeat(600));
    let result_id = result.id();
    transcript.append(result);
    let action = ProjectionActionEntry {
        message_id: result_id,
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 10,
            keep_tail: 10,
            preserve_recovery_handle: false,
        },
    };

    assert_eq!(
        estimate_projection_chars(&transcript, &[action.clone(), action]),
        (0, 0)
    );
}
