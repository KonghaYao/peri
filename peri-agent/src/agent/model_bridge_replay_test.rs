//! Responses provider state persistence at the Agent/model boundary.

use super::{convert_model_message, AgentModelBridge};
use crate::messages::{BaseMessage, ContentBlock, MessageContent};
use peri_model::{JsonObject, ModelMessage, ToolCall};
use serde_json::json;

#[test]
fn responses_replay_survives_canonical_json_restart_and_model_roundtrip() {
    let call = ToolCall::new(
        "call_1",
        "Read",
        JsonObject::from_value(json!({"path":"a.rs"})).unwrap(),
    );
    let original = ModelMessage::assistant(
        vec![
            peri_model::ContentBlock::reasoning("check the file"),
            peri_model::ContentBlock::text("Reading now"),
            peri_model::ContentBlock::ToolUse {
                tool_call: call.clone(),
            },
            peri_model::ContentBlock::ProviderState {
                provider: "openai_responses".into(),
                data: json!({
                    "version": 1,
                    "binding": {"endpoint":"https://api.openai.com/v1/responses","model":"model","credential_ref":"profile:one","profile":"chatgpt_plan"},
                    "items": [
                        {"id":"rs_1","type":"reasoning","encrypted_content":"opaque-ciphertext","summary":[{"type":"summary_text","text":"check the file"}]},
                        {"id":"msg_1","type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"Reading now"}]},
                        {"id":"fc_1","type":"function_call","call_id":"call_1","namespace":"peri","name":"Read","arguments":"{\"path\":\"a.rs\"}"}
                    ]
                }),
            },
        ],
        vec![call],
    );
    let canonical = convert_model_message(&original).unwrap();
    let saved = serde_json::to_vec(&canonical).unwrap();
    drop(canonical);
    let restored: BaseMessage = serde_json::from_slice(&saved).unwrap();
    assert_eq!(restored.content(), "Reading now");
    assert_eq!(restored.tool_calls().len(), 1);
    assert_eq!(restored.tool_calls()[0].id, "call_1");
    assert_eq!(
        AgentModelBridge::convert_message(&restored).unwrap(),
        original
    );
}

#[test]
fn provider_state_is_rejected_outside_assistant_or_inside_tool_result() {
    let state = ContentBlock::ProviderState {
        provider: "openai_responses".into(),
        data: json!({"version":1,"items":[]}),
    };
    for message in [
        BaseMessage::human(MessageContent::Blocks(vec![state.clone()])),
        BaseMessage::system(MessageContent::Blocks(vec![state.clone()])),
        BaseMessage::tool_result("call", MessageContent::Blocks(vec![state.clone()])),
        BaseMessage::ai_from_blocks(vec![ContentBlock::tool_result("call", vec![state], false)]),
    ] {
        assert!(AgentModelBridge::convert_message(&message)
            .unwrap_err()
            .to_string()
            .contains("provider replay state"));
    }
}
