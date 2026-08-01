use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use async_trait::async_trait;
use futures::{stream, StreamExt};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::ProtocolErrorKind;

use super::{
    ContentBlock, JsonObject, Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse,
    ModelResult, ModelStream, ModelStreamEvent, StopReason, TokenUsage, ToolCall,
};

struct FakeModel {
    events: Vec<ModelResult<ModelStreamEvent>>,
    stream_calls: AtomicUsize,
}

impl FakeModel {
    fn with_events(events: Vec<ModelStreamEvent>) -> Self {
        Self {
            events: events.into_iter().map(Ok).collect(),
            stream_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Model for FakeModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(self.events.clone()),
            cancellation,
        ))
    }
}

struct PendingModel;

#[async_trait]
impl Model for PendingModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
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

fn request() -> ModelRequest {
    ModelRequest::new(vec![ModelMessage::user_text("hello")])
}

fn assistant_response(text: &str) -> ModelResponse {
    ModelResponse::new(
        ModelMessage::assistant_text(text),
        StopReason::EndTurn,
        None,
        None,
    )
    .unwrap()
}

#[tokio::test]
async fn test_complete_aggregates_all_standard_stream_events() {
    let model = FakeModel::with_events(vec![
        ModelStreamEvent::TextDelta {
            text: "hello ".into(),
        },
        ModelStreamEvent::ReasoningDelta {
            text: "first ".into(),
        },
        ModelStreamEvent::ToolCallDelta {
            index: 1,
            id: Some("call_2".into()),
            name: None,
            arguments_delta: "{\"path\":\"".into(),
        },
        ModelStreamEvent::ToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            name: Some("shell".into()),
            arguments_delta: "{\"command\":\"p".into(),
        },
        ModelStreamEvent::TextDelta {
            text: "world".into(),
        },
        ModelStreamEvent::ToolCallDelta {
            index: 1,
            id: None,
            name: Some("read".into()),
            arguments_delta: "Cargo.toml\"}".into(),
        },
        ModelStreamEvent::ReasoningDelta {
            text: "second".into(),
        },
        ModelStreamEvent::ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments_delta: "wd\"}".into(),
        },
        ModelStreamEvent::Usage(TokenUsage::new(1, 2)),
        ModelStreamEvent::Completed(
            ModelResponse::new(
                ModelMessage::assistant(Vec::new(), Vec::new()),
                StopReason::ToolUse,
                None,
                None,
            )
            .unwrap(),
        ),
    ]);

    let response = model
        .complete(request(), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(response.assistant_text(), Some("hello world".into()));
    assert_eq!(response.usage(), Some(&TokenUsage::new(1, 2)));
    let ModelMessage::Assistant {
        content,
        tool_calls,
    } = response.message()
    else {
        panic!("complete must return an assistant message");
    };
    assert_eq!(
        content,
        &vec![
            ContentBlock::text("hello world"),
            ContentBlock::reasoning("first second")
        ]
    );
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0].id(), "call_1");
    assert_eq!(tool_calls[0].name(), "shell");
    assert_eq!(
        tool_calls[0].arguments().as_map().get("command"),
        Some(&serde_json::json!("pwd"))
    );
    assert_eq!(tool_calls[1].id(), "call_2");
    assert_eq!(tool_calls[1].name(), "read");
    assert_eq!(
        tool_calls[1].arguments().as_map().get("path"),
        Some(&serde_json::json!("Cargo.toml"))
    );
    assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_complete_preserves_completed_response_fields() {
    let completed = ModelResponse::new(
        ModelMessage::assistant(
            vec![
                ContentBlock::text("provider text"),
                ContentBlock::reasoning("provider reasoning"),
            ],
            vec![ToolCall::new(
                "provider_call",
                "provider_tool",
                JsonObject::from_value(serde_json::json!({"value": 1})).unwrap(),
            )],
        ),
        StopReason::EndTurn,
        Some(TokenUsage::new(9, 10)),
        None,
    )
    .unwrap();
    let model = FakeModel::with_events(vec![
        ModelStreamEvent::TextDelta {
            text: "stream text".into(),
        },
        ModelStreamEvent::ReasoningDelta {
            text: "stream reasoning".into(),
        },
        ModelStreamEvent::ToolCallDelta {
            index: 0,
            id: Some("stream_call".into()),
            name: Some("stream_tool".into()),
            arguments_delta: "{}".into(),
        },
        ModelStreamEvent::Usage(TokenUsage::new(1, 2)),
        ModelStreamEvent::Completed(completed),
    ]);

    let response = model
        .complete(request(), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(response.assistant_text(), Some("provider text".into()));
    assert_eq!(response.usage(), Some(&TokenUsage::new(9, 10)));
    let ModelMessage::Assistant {
        content,
        tool_calls,
    } = response.message()
    else {
        panic!("complete must return an assistant message");
    };
    assert_eq!(
        content,
        &vec![
            ContentBlock::text("provider text"),
            ContentBlock::reasoning("provider reasoning")
        ]
    );
    assert_eq!(tool_calls[0].id(), "provider_call");
}

#[tokio::test]
async fn test_complete_rejects_invalid_tool_call_arguments() {
    for (arguments_delta, expected_kind) in [
        ("[]", ProtocolErrorKind::InvalidJsonObject),
        ("{", ProtocolErrorKind::ToolCallInvalidArguments),
    ] {
        let model = FakeModel::with_events(vec![
            ModelStreamEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("shell".into()),
                arguments_delta: arguments_delta.into(),
            },
            ModelStreamEvent::Completed(assistant_response("")),
        ]);

        let error = model
            .complete(request(), CancellationToken::new())
            .await
            .unwrap_err();

        assert_eq!(
            error.protocol_error().map(|error| error.kind()),
            Some(expected_kind)
        );
    }
}

#[tokio::test]
async fn test_complete_rejects_missing_tool_call_fields_with_explicit_kinds() {
    for (id, name, expected_kind) in [
        (
            None,
            Some("shell".into()),
            ProtocolErrorKind::ToolCallMissingId,
        ),
        (
            Some("call_1".into()),
            None,
            ProtocolErrorKind::ToolCallMissingName,
        ),
    ] {
        let model = FakeModel::with_events(vec![
            ModelStreamEvent::ToolCallDelta {
                index: 0,
                id,
                name,
                arguments_delta: "{}".into(),
            },
            ModelStreamEvent::Completed(assistant_response("")),
        ]);

        let error = model
            .complete(request(), CancellationToken::new())
            .await
            .unwrap_err();

        assert_eq!(
            error.protocol_error().map(|error| error.kind()),
            Some(expected_kind)
        );
    }
}

#[tokio::test]
async fn test_complete_rejects_stream_without_completed_event() {
    let model = FakeModel::with_events(vec![ModelStreamEvent::TextDelta {
        text: "partial".into(),
    }]);
    let error = model
        .complete(request(), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(ProtocolErrorKind::StreamEndedWithoutCompleted)
    );
}

#[tokio::test]
async fn test_complete_returns_cancelled_for_cancelled_token() {
    let model = FakeModel::with_events(vec![ModelStreamEvent::Completed(assistant_response(
        "hello",
    ))]);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = model.complete(request(), cancellation).await.unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(model.stream_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_complete_cancels_pending_stream_with_caller_token() {
    let cancellation = CancellationToken::new();
    let completion = PendingModel.complete(request(), cancellation.clone());
    futures::pin_mut!(completion);

    assert!(futures::poll!(completion.as_mut()).is_pending());
    cancellation.cancel();

    let error = timeout(Duration::from_millis(100), completion)
        .await
        .expect("caller cancellation must wake complete")
        .unwrap_err();
    assert!(error.is_cancelled());
}

#[tokio::test]
async fn test_model_stream_abort_is_scoped_to_stream_child_token() {
    let caller_cancellation = CancellationToken::new();
    let caller_cancellation_clone = caller_cancellation.clone();
    let mut aborted_stream = Box::pin(ModelStream::with_parent_cancellation(
        stream::pending::<ModelResult<ModelStreamEvent>>(),
        caller_cancellation.clone(),
    ));

    assert!(futures::poll!(aborted_stream.next()).is_pending());

    aborted_stream.abort();

    let error = timeout(Duration::from_millis(100), aborted_stream.next())
        .await
        .expect("stream abort must wake a pending consumer")
        .expect("cancelled stream must produce an item")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert!(!caller_cancellation.is_cancelled());
    assert!(!caller_cancellation_clone.is_cancelled());
    assert!(aborted_stream.next().await.is_none());

    let mut caller_cancelled_stream = Box::pin(ModelStream::with_parent_cancellation(
        stream::pending::<ModelResult<ModelStreamEvent>>(),
        caller_cancellation.clone(),
    ));
    let next = caller_cancelled_stream.next();
    futures::pin_mut!(next);
    assert!(futures::poll!(next.as_mut()).is_pending());

    caller_cancellation.cancel();

    let error = timeout(Duration::from_millis(100), next)
        .await
        .expect("caller cancellation must wake a pending consumer")
        .expect("cancelled stream must produce an item")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert!(caller_cancellation_clone.is_cancelled());
    assert!(caller_cancelled_stream.next().await.is_none());
}

#[tokio::test]
async fn test_model_stream_abort_returns_cancelled() {
    let stream = ModelStream::new(stream::iter(vec![Ok(ModelStreamEvent::TextDelta {
        text: "hello".into(),
    })]));
    stream.abort();
    let error = futures::StreamExt::next(&mut Box::pin(stream))
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.is_cancelled());
}
