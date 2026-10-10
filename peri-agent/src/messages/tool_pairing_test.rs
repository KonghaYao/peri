use super::*;
use crate::agent::compact_v2::projection::{render_persisted_llm_view, ProviderCapabilities};
use crate::agent::model_bridge::AgentModelBridge;
use crate::session::transcript::MessageTranscript;
use serde_json::json;

fn assistant(ids: &[&str]) -> BaseMessage {
    BaseMessage::ai_with_tool_calls(
        "calling tools",
        ids.iter()
            .map(|id| ToolCallRequest::new(*id, "Read", json!({})))
            .collect(),
    )
}

fn serialized(messages: &[BaseMessage]) -> serde_json::Value {
    serde_json::to_value(messages).unwrap()
}

#[test]
fn missing_result_precedes_new_input_and_does_not_claim_execution_failure() {
    let repaired = repair_model_tool_pairing(vec![
        assistant(&["call-missing"]),
        BaseMessage::human("continue"),
    ])
    .unwrap();
    assert_eq!(repaired.len(), 3);
    match &repaired[1] {
        BaseMessage::Tool {
            tool_call_id,
            is_error,
            content,
            execution,
            ..
        } => {
            assert_eq!(tool_call_id, "call-missing");
            assert!(*is_error);
            assert!(execution.is_none());
            assert!(content.text_content().contains("unknown"));
            assert!(content
                .text_content()
                .contains("Do not automatically repeat"));
        }
        other => panic!("expected placeholder, got {other:?}"),
    }
    assert!(matches!(repaired[2], BaseMessage::Human { .. }));
    assert!(AgentModelBridge::convert_messages(&repaired).is_ok());
}

#[test]
fn valid_parallel_results_and_signed_content_are_unchanged() {
    let messages = vec![
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning_with_signature("thinking", "signature"),
            ContentBlock::tool_use("first", "Read", json!({})),
            ContentBlock::tool_use("second", "Read", json!({})),
        ]),
        BaseMessage::tool_error("second", "actual error"),
        BaseMessage::tool_result("first", "actual output"),
        BaseMessage::human("continue"),
    ];
    let expected = serialized(&messages);
    assert_eq!(
        serialized(&repair_model_tool_pairing(messages).unwrap()),
        expected
    );
}

#[test]
fn partial_parallel_results_keep_actual_evidence_and_fill_only_missing_ids() {
    let actual = BaseMessage::tool_result("second", "actual output");
    let expected = serialized(std::slice::from_ref(&actual));
    let repaired =
        repair_model_tool_pairing(vec![assistant(&["first", "second"]), actual]).unwrap();
    assert_eq!(serialized(&repaired[1..2]), expected);
    assert!(
        matches!(&repaired[2], BaseMessage::Tool { tool_call_id, is_error: true, .. } if tool_call_id == "first")
    );
    assert_eq!(
        serialized(&repair_model_tool_pairing(repaired.clone()).unwrap()),
        serialized(&repaired)
    );
}

#[test]
fn misplaced_result_is_relocated_only_in_model_view_without_losing_content() {
    let actual = BaseMessage::tool_result("first", "actual output");
    let expected = serialized(std::slice::from_ref(&actual));
    let repaired = repair_model_tool_pairing(vec![
        assistant(&["first"]),
        BaseMessage::human("continue"),
        actual,
    ])
    .unwrap();
    assert_eq!(serialized(&repaired[1..2]), expected);
    assert!(matches!(repaired[2], BaseMessage::Human { .. }));
}

#[test]
fn ambiguous_or_orphan_history_fails_before_send() {
    for messages in [
        vec![assistant(&["same", "same"])],
        vec![assistant(&["same"]), assistant(&["same"])],
        vec![
            assistant(&["same"]),
            BaseMessage::tool_result("same", "one"),
            BaseMessage::tool_result("same", "two"),
        ],
        vec![BaseMessage::tool_result("orphan", "output")],
        vec![
            BaseMessage::tool_result("early", "output"),
            assistant(&["early"]),
        ],
    ] {
        assert!(repair_model_tool_pairing(messages).is_err());
    }
}

#[test]
fn legacy_block_only_call_is_paired_and_cached_for_model_bridge() {
    let message: BaseMessage = serde_json::from_value(json!({
        "role": "assistant",
        "id": crate::messages::MessageId::new(),
        "content": [{"type": "tool_use", "id": "legacy", "name": "Read", "input": {}}]
    }))
    .unwrap();
    let repaired = repair_model_tool_pairing(vec![message]).unwrap();
    assert_eq!(repaired[0].tool_calls()[0].id, "legacy");
    assert_eq!(repaired.len(), 2);
    assert!(AgentModelBridge::convert_messages(&repaired).is_ok());
}

#[test]
fn loaded_history_projection_does_not_modify_canonical_history() {
    let history = vec![assistant(&["missing"]), BaseMessage::human("continue")];
    let transcript = MessageTranscript::new().with_own_payloads(
        history
            .into_iter()
            .map(peri_acp_types::store::PersistedPayload::Message)
            .collect(),
    );
    let before = serialized(&transcript.visible_model_messages().unwrap());
    for capabilities in [
        ProviderCapabilities::anthropic(),
        ProviderCapabilities::openai(),
    ] {
        let view = render_persisted_llm_view(&transcript, &capabilities).unwrap();
        assert_eq!(view.len(), 3);
        assert!(AgentModelBridge::convert_messages(&view).is_ok());
        assert_eq!(
            serialized(&transcript.visible_model_messages().unwrap()),
            before
        );
    }
}

#[test]
fn anthropic_request_has_all_tool_results_immediately_after_each_call() {
    use peri_model::{AnthropicConfig, AnthropicModel, Model, ModelRequest, ModelRuntimeConfig};

    let mut transcript = MessageTranscript::new().with_ancestor(vec![assistant(&["inherited"])]);
    transcript.append_batch(vec![
        BaseMessage::human("continue after loading"),
        assistant(&["first", "second"]),
        BaseMessage::tool_result("second", "real output"),
        BaseMessage::human("continue again"),
    ]);
    let before = serialized(&transcript.visible_model_messages().unwrap());
    let view = render_persisted_llm_view(&transcript, &ProviderCapabilities::anthropic()).unwrap();
    let model = AnthropicModel::new(
        AnthropicConfig::new(
            "https://example.test/".parse().unwrap(),
            "test",
            "claude-test",
        )
        .with_runtime(ModelRuntimeConfig::with_full_observation()),
    );
    let request = ModelRequest::new(AgentModelBridge::convert_messages(&view).unwrap());
    let prepared = model.prepare_request(&request).unwrap();
    let messages = prepared.body().as_value()["messages"].as_array().unwrap();
    let mut checked_calls = 0;
    for (index, message) in messages.iter().enumerate() {
        if message["role"] != "assistant" {
            continue;
        }
        let content = message["content"].as_array().unwrap();
        for block in content.iter().filter(|block| block["type"] == "tool_use") {
            let next = &messages[index + 1];
            assert_eq!(next["role"], "user");
            assert!(next["content"].as_array().unwrap().iter().any(|result| {
                result["type"] == "tool_result" && result["tool_use_id"] == block["id"]
            }));
            checked_calls += 1;
        }
    }
    assert_eq!(checked_calls, 3);
    assert_eq!(
        serialized(&transcript.visible_model_messages().unwrap()),
        before
    );
}

#[test]
fn conflicting_call_cache_fails_without_overwriting_original_content() {
    let message = BaseMessage::ai_with_tool_calls(
        MessageContent::blocks(vec![ContentBlock::tool_use("same", "Write", json!({}))]),
        vec![ToolCallRequest::new("same", "Read", json!({}))],
    );
    assert!(repair_model_tool_pairing(vec![message]).is_err());
}

#[test]
fn duplicate_content_calls_are_rejected_even_when_cache_has_only_one() {
    let message = BaseMessage::ai_with_tool_calls(
        MessageContent::blocks(vec![
            ContentBlock::tool_use("same", "Read", json!({})),
            ContentBlock::tool_use("same", "Read", json!({})),
        ]),
        vec![ToolCallRequest::new("same", "Read", json!({}))],
    );
    assert!(repair_model_tool_pairing(vec![message]).is_err());
}

#[test]
fn adjacent_inline_results_are_not_duplicated() {
    use peri_model::{AnthropicConfig, AnthropicModel, Model, ModelRequest};

    let messages = vec![
        assistant(&["inline"]),
        BaseMessage::human(MessageContent::blocks(vec![ContentBlock::tool_result(
            "inline",
            vec![ContentBlock::text("real output")],
            false,
        )])),
    ];
    let expected = serialized(&messages);
    let repaired = repair_model_tool_pairing(messages).unwrap();
    assert_eq!(serialized(&repaired), expected);
    let model = AnthropicModel::new(AnthropicConfig::new(
        "https://example.test/".parse().unwrap(),
        "test",
        "claude-test",
    ));
    let request = ModelRequest::new(AgentModelBridge::convert_messages(&repaired).unwrap());
    let prepared = model.prepare_request(&request).unwrap();
    let body = prepared.body().as_value();
    let blocks = body["messages"][1]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["tool_use_id"], "inline");
}

#[test]
fn inline_result_precedes_text_only_in_anthropic_request_view() {
    use peri_model::{AnthropicConfig, AnthropicModel, Model, ModelRequest};

    let transcript = MessageTranscript::new().with_own_payloads(vec![
        peri_acp_types::store::PersistedPayload::Message(assistant(&["inline"])),
        peri_acp_types::store::PersistedPayload::Message(BaseMessage::human(
            MessageContent::blocks(vec![
                ContentBlock::text("before"),
                ContentBlock::tool_result("inline", vec![ContentBlock::text("real output")], false),
                ContentBlock::text("after"),
            ]),
        )),
    ]);
    let canonical = serialized(&transcript.visible_model_messages().unwrap());
    let view = render_persisted_llm_view(&transcript, &ProviderCapabilities::anthropic()).unwrap();
    assert_eq!(
        serialized(&transcript.visible_model_messages().unwrap()),
        canonical
    );
    let model = AnthropicModel::new(AnthropicConfig::new(
        "https://example.test/".parse().unwrap(),
        "test",
        "claude-test",
    ));
    let request = ModelRequest::new(AgentModelBridge::convert_messages(&view).unwrap());
    let prepared = model.prepare_request(&request).unwrap();
    let blocks = prepared.body().as_value()["messages"][1]["content"]
        .as_array()
        .unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0]["tool_use_id"], "inline");
    assert_eq!(blocks[1]["text"], "before");
    assert_eq!(blocks[2]["text"], "after");
}

#[test]
fn raw_inline_result_cannot_reach_anthropic_request() {
    let raw = BaseMessage::human(MessageContent::raw(vec![json!({
        "type": "tool_result",
        "tool_use_id": "raw",
        "content": [{"type": "text", "text": "real output"}],
        "is_error": false
    })]));
    let view = repair_model_tool_pairing(vec![assistant(&["raw"]), raw]).unwrap();
    let error = AgentModelBridge::convert_messages(&view).unwrap_err();
    assert!(error.to_string().contains("raw provider content"));
}

#[test]
fn incomplete_or_displaced_inline_results_fail_before_send() {
    let inline = || {
        BaseMessage::human(MessageContent::blocks(vec![ContentBlock::tool_result(
            "first",
            vec![ContentBlock::text("real output")],
            false,
        )]))
    };
    for messages in [
        vec![assistant(&["first", "second"]), inline()],
        vec![assistant(&["first"]), BaseMessage::human("later"), inline()],
        vec![
            assistant(&["first"]),
            inline(),
            BaseMessage::tool_result("first", "again"),
        ],
    ] {
        assert!(repair_model_tool_pairing(messages).is_err());
    }
}

#[test]
fn fork_and_compaction_keep_visible_ancestor_gap_without_reviving_excluded_pair() {
    use peri_acp_types::store::MessageFlags;
    use std::collections::HashMap;

    let excluded_call = assistant(&["old"]);
    let excluded_result = BaseMessage::tool_result("old", "old output");
    let mut transcript = MessageTranscript::new()
        .with_ancestor(vec![assistant(&["inherited"])])
        .with_own_payloads(vec![
            peri_acp_types::store::PersistedPayload::Message(excluded_call.clone()),
            peri_acp_types::store::PersistedPayload::Message(excluded_result.clone()),
        ]);
    transcript.set_flags_batch(HashMap::from([
        (
            excluded_call.id(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        ),
        (
            excluded_result.id(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        ),
    ]));
    transcript.append(BaseMessage::human("summary and new input"));
    let canonical = serialized(&transcript.visible_model_messages().unwrap());
    let view = render_persisted_llm_view(&transcript, &ProviderCapabilities::anthropic()).unwrap();
    assert_eq!(view.len(), 3);
    assert!(
        matches!(&view[1], BaseMessage::Tool { tool_call_id, is_error: true, .. } if tool_call_id == "inherited")
    );
    assert_eq!(
        serialized(&transcript.visible_model_messages().unwrap()),
        canonical
    );
    assert!(AgentModelBridge::convert_messages(&view).is_ok());
}

#[test]
fn persisted_result_projection_and_interrupted_tail_share_the_repaired_view() {
    use crate::agent::compact_v2::projection::{
        MessageProjectionDirective, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
        PROJECTION_POLICY_VERSION,
    };

    let completed_result = BaseMessage::tool_result("complete", "long output ".repeat(100));
    let result_id = completed_result.id();
    let mut transcript = MessageTranscript::new().with_own_payloads(vec![
        peri_acp_types::store::PersistedPayload::Message(assistant(&["complete"])),
        peri_acp_types::store::PersistedPayload::Message(completed_result),
        peri_acp_types::store::PersistedPayload::Message(assistant(&["interrupted"])),
    ]);
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
    transcript.append(BaseMessage::human("continue"));
    let view = render_persisted_llm_view(&transcript, &ProviderCapabilities::anthropic()).unwrap();
    assert_eq!(view.len(), 5);
    assert!(view[1].content().len() < 1_200);
    assert!(
        matches!(&view[3], BaseMessage::Tool { tool_call_id, is_error: true, .. } if tool_call_id == "interrupted")
    );
    assert!(matches!(view[4], BaseMessage::Human { .. }));
    assert!(AgentModelBridge::convert_messages(&view).is_ok());
}
