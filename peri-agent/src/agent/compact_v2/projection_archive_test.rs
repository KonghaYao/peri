use crate::agent::compact_v2::projection::MessageProjectionDirective;
use crate::agent::compact_v2::projection::MicroCompactPlan;
use crate::agent::compact_v2::projection::ProjectionAction;
use crate::agent::compact_v2::projection::ProjectionActionEntry;
use crate::agent::compact_v2::projection::ProjectionTarget;
use crate::agent::compact_v2::projection::ProviderCapabilities;
use crate::agent::compact_v2::projection::PROJECTION_POLICY_VERSION;
use crate::messages::{BaseMessage, ContentBlock, MessageContent, ToolCallRequest};
use crate::session::MessageTranscript;

use peri_acp_types::store::PersistedPayload;

fn result(id: &str, text: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        id: None,
        tool_use_id: id.into(),
        content: vec![ContentBlock::text(text)],
        is_error: false,
    }
}

#[test]
fn test_existing_preview_reference_stays_with_sibling_after_partial_omission() {
    let mut transcript = MessageTranscript::new();
    let call = BaseMessage::ai_with_tool_calls(
        "Keep the other calls and this conclusion.",
        ["a", "b", "c"]
            .into_iter()
            .map(|id| ToolCallRequest::new(id, "Read", serde_json::json!({"path":id})))
            .collect(),
    );
    let untouched = result(
        "c",
        "Third-party result C must remain byte-for-byte unchanged.",
    );
    let user_text = ContentBlock::text("Retain this user constraint.");
    let results = BaseMessage::human(MessageContent::Blocks(vec![
        result("a", "Completed A"),
        result("b", &"B canonical historical output ".repeat(300)),
        untouched.clone(),
        user_text.clone(),
    ]));
    let call_id = transcript.append(call.clone());
    let results_id = transcript.append(results.clone());
    let preview_catalog = peri_acp_types::context_archive::build_catalog(
        "session".into(),
        "preview-b".into(),
        results_id,
        &[PersistedPayload::Message(results.clone())],
        "Incomplete historical B response".into(),
    )
    .expect("the build catalog fixture result is required for this assertion");
    let preview_ref = preview_catalog
        .reference()
        .expect("the reference fixture result is required for this assertion");
    transcript.set_flags_projection(
        results_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            archive_catalog: Some(preview_catalog),
            entries: vec![ProjectionActionEntry {
                message_id: results_id,
                target: ProjectionTarget::ContentBlock { index: 1 },
                action: ProjectionAction::CompactToolResult {
                    keep_head: 80,
                    keep_tail: 30,
                    preserve_recovery_handle: true,
                },
            }],
            ..Default::default()
        },
    );
    let retired = peri_acp_types::context_archive::build_catalog(
        "session".into(),
        "retire-a".into(),
        call_id,
        &[
            PersistedPayload::Message(call.clone()),
            PersistedPayload::Message(results.clone()),
        ],
        "Historical closed A exchange".into(),
    )
    .expect("the build catalog fixture result is required for this assertion");
    let actions = [call_id, results_id]
        .into_iter()
        .map(|message_id| ProjectionActionEntry {
            message_id,
            target: ProjectionTarget::ToolCall {
                tool_call_id: "a".into(),
            },
            action: ProjectionAction::OmitToolExchange {
                anchor_message_id: call_id,
                group_id: retired.group_id.clone(),
                tool_call_ids: vec!["a".into()],
            },
        })
        .collect::<Vec<_>>();
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        actions: actions.clone(),
        catalogs: vec![retired.clone()],
        receipt_budget_tokens: 2048,
        ..Default::default()
    };
    let caps = ProviderCapabilities::anthropic();
    let candidate =
        crate::agent::compact_v2::projection::render_micro_plan(&transcript, &plan, &caps)
            .expect("the render micro plan fixture result is required for this assertion");

    // Restore the combined persisted directives, including B's earlier block-index action.
    for action in actions {
        let mut directive = transcript
            .projection_ref(action.message_id)
            .cloned()
            .unwrap_or_default();
        directive.policy_version = PROJECTION_POLICY_VERSION;
        if action.message_id == call_id {
            directive.archive_catalog = Some(retired.clone());
        }
        directive.entries.push(action.clone());
        let restored = serde_json::from_value(
            serde_json::to_value(directive)
                .expect("to_value must accept this test fixture representation"),
        )
        .expect("from_value must accept this test fixture representation");
        transcript.set_flags_projection(action.message_id, restored);
    }
    let restored =
        crate::agent::compact_v2::projection::render_persisted_llm_view(&transcript, &caps)
            .expect("the render persisted llm view fixture result is required for this assertion");
    assert_eq!(
        serde_json::to_value(&candidate)
            .expect("to_value must accept this test fixture representation"),
        serde_json::to_value(&restored)
            .expect("to_value must accept this test fixture representation")
    );
    let result_blocks = restored
        .iter()
        .find(|message| message.id() == results_id)
        .expect("the find fixture result is required for this assertion")
        .content_blocks();
    assert_eq!(result_blocks.len(), 3);
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        ..
    } = &result_blocks[0]
    else {
        panic!("B must remain the first result after A is omitted");
    };
    assert_eq!(tool_use_id, "b");
    let preview = MessageContent::Blocks(content.clone()).text_content();
    assert!(preview.contains(&preview_ref));
    assert!(preview.len() < 1000);
    assert_eq!(result_blocks[1], untouched);
    assert_eq!(result_blocks[2], user_text);
    assert_eq!(
        restored[0].tool_calls(),
        &call.tool_calls()[1..],
        "retained inputs remain canonical"
    );
    assert_eq!(
        serde_json::to_value(
            transcript
                .get(results_id)
                .expect("transcript get fixture must contain the requested entry")
                .as_message()
        )
        .expect("the as message fixture result is required for this assertion"),
        serde_json::to_value(Some(&results))
            .expect("to_value must accept this test fixture representation")
    );
}

#[test]
fn persisted_v3_preview_rejects_missing_mismatched_and_unparseable_catalogs() {
    for case in ["missing", "identity", "reference"] {
        let mut transcript = MessageTranscript::new();
        let call = BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                "read",
                "Read",
                serde_json::json!({"path":"original.rs"}),
            )],
        );
        let output = BaseMessage::tool_result("read", "original output ".repeat(300));
        let call_id = transcript.append(call.clone());
        let id = transcript.append(output.clone());
        let catalog = match case {
            "missing" => None,
            "identity" => Some(
                peri_acp_types::context_archive::build_catalog(
                    "session".into(),
                    "other-anchor".into(),
                    call_id,
                    &[
                        PersistedPayload::Message(call),
                        PersistedPayload::Message(output.clone()),
                    ],
                    "result".into(),
                )
                .expect("the build catalog fixture result is required for this assertion"),
            ),
            "reference" => Some(
                peri_acp_types::context_archive::build_catalog(
                    "s".repeat(1100),
                    "oversized-reference".into(),
                    id,
                    &[PersistedPayload::Message(output.clone())],
                    "result".into(),
                )
                .expect("the build catalog fixture result is required for this assertion"),
            ),
            _ => unreachable!(),
        };
        transcript.set_flags_projection(
            id,
            MessageProjectionDirective {
                policy_version: PROJECTION_POLICY_VERSION,
                archive_catalog: catalog,
                entries: vec![ProjectionActionEntry {
                    message_id: id,
                    target: ProjectionTarget::Message,
                    action: ProjectionAction::CompactToolResult {
                        keep_head: 80,
                        keep_tail: 30,
                        preserve_recovery_handle: true,
                    },
                }],
                ..Default::default()
            },
        );
        let error = crate::agent::compact_v2::projection::render_persisted_llm_view(
            &transcript,
            &ProviderCapabilities::default(),
        )
        .expect_err("v3 preview cannot succeed without a usable original reference");
        assert!(
            error.to_string().contains("invalid context projection"),
            "{case}: {error}"
        );
        assert_eq!(
            serde_json::to_value(
                transcript
                    .get(id)
                    .expect("transcript get fixture must contain the requested entry")
                    .as_message()
            )
            .expect("the as message fixture result is required for this assertion"),
            serde_json::to_value(Some(&output))
                .expect("to_value must accept this test fixture representation"),
            "rejected projection leaves canonical output unchanged: {case}"
        );
    }
}

#[test]
fn persisted_v2_preview_without_catalog_keeps_legacy_bytes() {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::ai_with_tool_calls(
        "",
        vec![ToolCallRequest::new(
            "read",
            "Read",
            serde_json::json!({"path":"original.rs"}),
        )],
    ));
    let text = "original output ".repeat(300);
    let id = transcript.append(BaseMessage::tool_result("read", text.clone()));
    transcript.set_flags_projection(
        id,
        MessageProjectionDirective {
            policy_version: 2,
            entries: vec![ProjectionActionEntry {
                message_id: id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 80,
                    keep_tail: 30,
                    preserve_recovery_handle: true,
                },
            }],
            ..Default::default()
        },
    );
    let restored = crate::agent::compact_v2::projection::render_persisted_llm_view(
        &transcript,
        &ProviderCapabilities::default(),
    )
    .expect("the render persisted llm view fixture result is required for this assertion");
    let expected = crate::agent::compact_v2::projection::render::apply_head_tail(&text, 80, 30)
        .expect("the apply head tail fixture result is required for this assertion");
    assert_eq!(
        restored
            .iter()
            .find(|message| message.id() == id)
            .expect("the id fixture result is required for this assertion")
            .content(),
        expected
    );
}

#[test]
fn persisted_v3_rejects_text_and_media_actions_without_archive_contract() {
    let paired_items = [
        (
            ContentBlock::text("original prose".repeat(100)),
            ProjectionAction::CompactText { max_chars: 1 },
        ),
        (
            ContentBlock::image_base64("image/png", "AAAA"),
            ProjectionAction::ReplaceMedia {
                placeholder: "gone".into(),
            },
        ),
    ];
    for (block, action) in paired_items {
        let mut transcript = MessageTranscript::new();
        let id = transcript.append(BaseMessage::ai_from_blocks(vec![block]));
        transcript.set_flags_projection(
            id,
            MessageProjectionDirective {
                policy_version: PROJECTION_POLICY_VERSION,
                entries: vec![ProjectionActionEntry {
                    message_id: id,
                    target: ProjectionTarget::ContentBlock { index: 0 },
                    action,
                }],
                ..Default::default()
            },
        );
        assert!(
            crate::agent::compact_v2::projection::render_persisted_llm_view(
                &transcript,
                &ProviderCapabilities::default()
            )
            .is_err()
        );
    }
}
