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
        events_v2::{EventBus, EventBusConfig, RenderEvent},
        react::ReactLLM,
    },
    error::AgentError,
    messages::{BaseMessage, ContentBlock, MessageContent},
    session::turn::TurnId,
};
use peri_acp_types::identity::AgentId;

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
    let bridge = AgentModelBridge::from_arc(Arc::new(FakeModel));
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let streaming = StreamingContext {
        event_bus: Arc::new(bus),
        turn_id: TurnId::new(),
        agent_id: AgentId::new(),
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

    // v2 直发（v1 ExecutorEvent 流式中间态已退役）：TextDelta → RenderEvent::TextChunk
    let first = handles
        .render_rx
        .try_recv()
        .expect("应收到 RenderEvent::TextChunk");
    assert!(matches!(
        first,
        RenderEvent::TextChunk { chunk, .. } if chunk == "answer"
    ));
    // [Fix think-end] 首个带 id/name 的 ToolCallDelta 提前 emit ToolStarted
    //（input=Null——参数尚未流式生成，由 dispatch 的正式 ToolStarted 经
    // TUI start_tool 重复 id upsert 填充）
    let started = handles
        .render_rx
        .try_recv()
        .expect("应收到提前的 RenderEvent::ToolStarted");
    assert!(matches!(
        started,
        RenderEvent::ToolStarted {
            tool_call_id,
            name,
            input,
            ..
        } if tool_call_id == "call_1" && name == "shell" && input == serde_json::Value::Null
    ));
    // Usage / Completed 不产生额外 Render 事件
    assert!(
        handles.render_rx.try_recv().is_err(),
        "不应有额外 Render 事件"
    );
}

/// 纯「思考 → 工具」（无正文）模型流：验证 thinking 结束后首个带 id/name 的
/// ToolCallDelta 提前 emit ToolStarted（input=Null），且后续 delta 幂等不再发。
struct ThinkingToolModel;

#[async_trait]
impl Model for ThinkingToolModel {
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
                vec![peri_model::ContentBlock::Reasoning {
                    text: "think".into(),
                    signature: None,
                }],
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
                Ok(ModelStreamEvent::ReasoningDelta {
                    text: "think".into(),
                }),
                Ok(ModelStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("shell".into()),
                    arguments_delta: r#"{"command":"pwd"}"#.into(),
                }),
                // 后续参数 delta 无 id/name：不应重复发 ToolStarted
                Ok(ModelStreamEvent::ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    arguments_delta: String::new(),
                }),
                Ok(ModelStreamEvent::Usage(TokenUsage::new(3, 5))),
                Ok(ModelStreamEvent::Completed(response)),
            ]),
            cancellation,
        ))
    }
}

#[tokio::test]
async fn bridge_emits_tool_started_on_first_tool_call_delta() {
    // [Fix think-end] 思考完直接调工具（无正文）：首个带 id/name 的
    // ToolCallDelta 到达即提前发 ToolStarted（工具块开始 = 推理结束），
    // TUI 收到后立即冻结推理动画，不再空转到 dispatch_tools 的正式
    // ToolStarted（模型流结束后才发出）。
    let bridge = AgentModelBridge::from_arc(Arc::new(ThinkingToolModel));
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let streaming = StreamingContext {
        event_bus: Arc::new(bus),
        turn_id: TurnId::new(),
        agent_id: AgentId::new(),
        cancel: CancellationToken::new(),
    };

    let reasoning = bridge
        .generate_reasoning(&[BaseMessage::human("hello")], &[], Some(streaming))
        .await
        .unwrap();
    assert_eq!(reasoning.thought, "think");
    assert_eq!(reasoning.tool_calls.len(), 1);
    assert_eq!(reasoning.tool_calls[0].id, "call_1");

    // 事件序：ThinkingChunk → ToolStarted（提前，input=Null）→ 无更多 Render
    let first = handles
        .render_rx
        .try_recv()
        .expect("应收到 RenderEvent::ThinkingChunk");
    assert!(matches!(
        first,
        RenderEvent::ThinkingChunk { chunk, .. } if chunk == "think"
    ));
    let started = handles
        .render_rx
        .try_recv()
        .expect("应收到提前的 RenderEvent::ToolStarted");
    assert!(matches!(
        started,
        RenderEvent::ToolStarted {
            tool_call_id,
            name,
            input,
            ..
        } if tool_call_id == "call_1" && name == "shell" && input == serde_json::Value::Null
    ));
    // 第二个 ToolCallDelta（无 id/name）幂等：不再发；Usage/Completed 无 Render
    assert!(
        handles.render_rx.try_recv().is_err(),
        "不应有额外 Render 事件"
    );
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
    let bridge = AgentModelBridge::from_arc(Arc::new(CancellingModel));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let streaming = StreamingContext {
        event_bus: Arc::new(bus),
        turn_id: TurnId::new(),
        agent_id: AgentId::new(),
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
        handles.render_rx.try_recv().is_err(),
        "取消后不得 emit 任何 RenderEvent"
    );
}

#[tokio::test]
async fn bridge_stops_emitting_events_after_mid_stream_cancellation() {
    let bridge = AgentModelBridge::from_arc(Arc::new(HalfStreamingModel));
    let cancel = CancellationToken::new();
    let cancel_on_first_render = cancel.clone();
    let (bus, handles) = EventBus::new(EventBusConfig::default());
    // v2 直发后无法在发射点挂钩 cancel：观察任务在首个 Render 事件到达时取消，
    // 并统计取消后的残留事件（bridge 必须在 poll 下一项前中断，不得再 emit）。
    let mut handles_for_watcher = handles;
    let watcher = tokio::spawn(async move {
        let first = handles_for_watcher.render_rx.recv().await;
        if first.is_none() {
            return 0; // 通道关闭（未收到事件）
        }
        cancel_on_first_render.cancel();
        let mut extra = 0;
        while handles_for_watcher.render_rx.recv().await.is_some() {
            extra += 1;
        }
        extra
    });
    let streaming = StreamingContext {
        event_bus: Arc::new(bus),
        turn_id: TurnId::new(),
        agent_id: AgentId::new(),
        cancel,
    };

    let result = bridge
        .generate_reasoning(&[BaseMessage::human("hello")], &[], Some(streaming))
        .await;

    let extra_events = watcher.await.unwrap();
    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "流中途取消应映射为 Interrupted，实际 {:?}",
        result
    );
    assert_eq!(
        extra_events, 0,
        "取消后不得再 emit 事件（残留 {} 个）",
        extra_events
    );
}
