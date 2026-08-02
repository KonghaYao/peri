use std::sync::Arc;

use async_trait::async_trait;
use futures::{stream, StreamExt};
use peri_model::{
    JsonObject, Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, ModelStreamEvent, PreparedModelRequest, StopReason, TokenUsage, ToolCall,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{convert_model_message, stop_reason_display, AgentModelBridge, StreamingContext};
use crate::{
    agent::{
        compact_v2::projection::{ProviderCapabilities, ProviderProtocol},
        events::{ExecutorEvent, FnEventHandler},
        react::ReactLLM,
    },
    error::AgentError,
    messages::{BaseMessage, ContentBlock, MessageContent},
};

struct FakeModel;

#[async_trait]
impl Model for FakeModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_reasoning: true,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        let response = ModelResponse::new(
            ModelMessage::assistant(
                vec![
                    peri_model::ContentBlock::Reasoning {
                        text: "think".into(),
                        signature: Some("signature".into()),
                    },
                    peri_model::ContentBlock::text("answer"),
                ],
                vec![ToolCall::new(
                    "call_1",
                    "shell",
                    JsonObject::from_value(json!({"command": "pwd"})).unwrap(),
                )],
            ),
            StopReason::ToolUse,
            Some(TokenUsage::new(3, 5)),
            Some("request_1".into()),
        )?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![
                Ok(ModelStreamEvent::TextDelta {
                    text: "answer".into(),
                }),
                Ok(ModelStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("shell".into()),
                    arguments_delta: r#"{"command":"pwd"}"#.into(),
                }),
                Ok(ModelStreamEvent::Usage(TokenUsage::new(3, 5))),
                Ok(ModelStreamEvent::Completed(response)),
            ]),
            cancellation,
        ))
    }
}

#[tokio::test]
async fn bridge_preserves_completed_message_and_only_emits_visible_deltas() {
    let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let received_events = Arc::clone(&events);
    let bridge = AgentModelBridge::from_arc(Arc::new(FakeModel));
    let streaming = StreamingContext {
        event_handler: Arc::new(FnEventHandler(move |event| {
            received_events.lock().push(event);
        })),
        message_id: crate::messages::MessageId::new(),
        cancel: CancellationToken::new(),
    };

    let reasoning = bridge
        .generate_reasoning(&[BaseMessage::human("hello")], &[], Some(streaming))
        .await
        .unwrap();

    assert_eq!(reasoning.final_answer.as_deref(), Some("answer"));
    assert_eq!(reasoning.thought, "think");
    assert_eq!(reasoning.tool_calls.len(), 1);
    assert_eq!(reasoning.tool_calls[0].id, "call_1");
    assert_eq!(reasoning.tool_calls[0].name, "shell");
    assert_eq!(reasoning.tool_calls[0].input, json!({"command": "pwd"}));
    assert_eq!(reasoning.usage.unwrap().input_tokens, 3);
    assert_eq!(reasoning.stop_reason, StopReason::ToolUse);

    let events = events.lock();
    assert!(matches!(events[0], ExecutorEvent::TextChunk { .. }));
    assert!(!events
        .iter()
        .any(|event| matches!(event, ExecutorEvent::ToolStart { .. })));
}

#[test]
fn provider_capabilities_are_mapped_conservatively() {
    let bridge = AgentModelBridge::from_arc(Arc::new(FakeModel));
    assert_eq!(
        bridge.provider_capabilities(),
        ProviderCapabilities {
            protocol: ProviderProtocol::Generic,
            signed_reasoning_must_be_whole: false,
        }
    );
}

/// 上报 Anthropic 协议的模型：验证 provider_capabilities 忠实映射协议身份。
///
/// FakeModel 未覆盖 `prepare_request`（默认返回 Err → 保守回退 Generic）；
/// 此模型覆盖它，证明协议身份来自 prepared request 投影，而不是一律硬编码。
struct AnthropicProtocolModel;

#[async_trait]
impl Model for AnthropicProtocolModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    fn prepare_request(&self, _request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
        PreparedModelRequest::observe(
            peri_model::ProviderProtocol::Anthropic,
            "claude-test",
            url::Url::parse("https://api.anthropic.com").expect("valid URL"),
            serde_json::json!({}),
            std::collections::BTreeMap::new(),
        )
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        Ok(ModelStream::with_parent_cancellation(
            stream::pending::<ModelResult<ModelStreamEvent>>(),
            cancellation,
        ))
    }
}

#[test]
fn provider_capabilities_map_anthropic_protocol_faithfully() {
    let bridge = AgentModelBridge::from_arc(Arc::new(AnthropicProtocolModel));
    assert_eq!(
        bridge.provider_capabilities(),
        ProviderCapabilities {
            protocol: ProviderProtocol::Anthropic,
            signed_reasoning_must_be_whole: true,
        }
    );
}

#[test]
fn redacted_reasoning_roundtrips_through_agent_content() {
    let message = ModelMessage::assistant(
        vec![peri_model::ContentBlock::RedactedReasoning {
            data: Some("opaque-reasoning".into()),
        }],
        Vec::new(),
    );

    let agent_message = convert_model_message(&message).expect("model content converts to Agent");
    let roundtrip = AgentModelBridge::convert_message(&agent_message)
        .expect("Agent must preserve standard redacted reasoning");

    assert_eq!(roundtrip, message);
}

#[test]
fn unknown_agent_content_fails_closed() {
    let error =
        AgentModelBridge::convert_message(&BaseMessage::human(MessageContent::Blocks(vec![
            ContentBlock::Unknown(json!({"type": "provider_only"})),
        ])))
        .unwrap_err();
    assert!(error.to_string().contains("unsupported agent content"));
}

#[test]
fn stop_reason_display_matches_legacy_wire_format() {
    assert_eq!(stop_reason_display(&StopReason::EndTurn), "end_turn");
    assert_eq!(stop_reason_display(&StopReason::ToolUse), "tool_use");
    assert_eq!(stop_reason_display(&StopReason::MaxTokens), "max_tokens");
    assert_eq!(
        stop_reason_display(&StopReason::Other {
            value: "custom".into()
        }),
        "custom"
    );
}

/// 流永远 pending，绝不产出 Completed；取消由 bridge 侧处理。
struct CancellingModel;

#[async_trait]
impl Model for CancellingModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        Ok(ModelStream::with_parent_cancellation(
            stream::pending::<ModelResult<ModelStreamEvent>>(),
            cancellation,
        ))
    }
}

/// 取消前先 emit 一个 TextDelta，随后永久 pending。
struct HalfStreamingModel;

#[async_trait]
impl Model for HalfStreamingModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::TextDelta {
                text: "partial".into(),
            })])
            .chain(stream::pending::<ModelResult<ModelStreamEvent>>()),
            cancellation,
        ))
    }
}

#[tokio::test]
async fn bridge_maps_precancelled_token_to_interrupted_without_events() {
    let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let received_events = Arc::clone(&events);
    let bridge = AgentModelBridge::from_arc(Arc::new(CancellingModel));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let streaming = StreamingContext {
        event_handler: Arc::new(FnEventHandler(move |event| {
            received_events.lock().push(event);
        })),
        message_id: crate::messages::MessageId::new(),
        cancel,
    };

    let result = bridge
        .generate_reasoning(&[BaseMessage::human("hello")], &[], Some(streaming))
        .await;

    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "预取消 token 应映射为 Interrupted，实际 {:?}",
        result
    );
    assert!(
        events.lock().is_empty(),
        "取消后不得 emit 任何 ExecutorEvent"
    );
}

#[tokio::test]
async fn bridge_stops_emitting_events_after_mid_stream_cancellation() {
    let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let received_events = Arc::clone(&events);
    let bridge = AgentModelBridge::from_arc(Arc::new(HalfStreamingModel));
    let cancel = CancellationToken::new();
    let cancel_in_handler = cancel.clone();
    let streaming = StreamingContext {
        event_handler: Arc::new(FnEventHandler(move |event| {
            received_events.lock().push(event.clone());
            // 首个可见增量到达时立即取消 → bridge 必须在 poll 下一项前中断
            if matches!(event, ExecutorEvent::TextChunk { .. }) {
                cancel_in_handler.cancel();
            }
        })),
        message_id: crate::messages::MessageId::new(),
        cancel,
    };

    let result = bridge
        .generate_reasoning(&[BaseMessage::human("hello")], &[], Some(streaming))
        .await;

    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "流中途取消应映射为 Interrupted，实际 {:?}",
        result
    );
    let events = events.lock();
    assert_eq!(events.len(), 1, "取消后不得再 emit 事件");
    assert!(matches!(events[0], ExecutorEvent::TextChunk { .. }));
}
