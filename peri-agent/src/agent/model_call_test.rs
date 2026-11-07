use std::sync::{Arc, Mutex};

use peri_acp_types::identity::AgentId;
use peri_model::{ModelMessage, ModelResponse, ModelStreamEvent, StopReason};
use tokio::sync::Barrier;

use super::*;
use crate::agent::events_v2::EventBus;
use crate::agent::model_bridge::AgentModelBridge;
use crate::agent::react::{ReactLLM, StreamingContext};
use crate::agent::stages::StageContext;
use crate::messages::BaseMessage;
use crate::session::store::FrozenContext;
use crate::session::turn::TurnId;
use crate::session::Session;

fn parent(tool_call: &str) -> ModelCallParent {
    ModelCallParent {
        owner: "parent-session".into(),
        execution: "parent-execution".into(),
        call: "reason:2".into(),
        tool_call: tool_call.into(),
        slot: None,
    }
}

fn context(owner: &str, purpose: &str) -> ModelCallContext {
    ModelCallContext {
        owner: owner.into(),
        execution: format!("{owner}-execution"),
        call: format!("{purpose}:0"),
        purpose: purpose.into(),
        parent: None,
    }
}

#[tokio::test]
async fn concurrent_tool_parent_scopes_do_not_cross_or_leak() {
    let barrier = Barrier::new(2);
    let barrier = &barrier;
    let first = parent("first-tool");
    let second = parent("second-tool");
    let read = |expected: ModelCallParent| async move {
        barrier.wait().await;
        tokio::task::yield_now().await;
        let captured = capture_tool_parent(Some(&expected.tool_call));
        assert_eq!(captured, Some(expected));
        assert!(capture_tool_parent(Some("unrelated-tool")).is_none());
        captured
    };

    let (left, right) = tokio::join!(
        scope_tool_parent(Some(first.clone()), read(first.clone())),
        scope_tool_parent(Some(second.clone()), read(second.clone())),
    );
    assert_eq!(left, Some(first));
    assert_eq!(right, Some(second));
    assert!(capture_tool_parent(Some("first-tool")).is_none());
    assert!(capture_tool_parent(Some("second-tool")).is_none());
}

#[tokio::test]
async fn child_task_requires_explicit_parent_capture_and_empty_scope_masks_outer() {
    let expected = parent("spawn-tool");
    scope_tool_parent(Some(expected.clone()), async {
        let auxiliary = tool_model_call_context("goal-verify").unwrap();
        assert_eq!(auxiliary.owner, expected.owner);
        assert_eq!(auxiliary.execution, expected.execution);
        assert_eq!(auxiliary.call, "goal-verify:reason:2:spawn-tool");
        assert!(auxiliary.parent.is_none());
        let copied = capture_tool_parent(Some("spawn-tool"));
        let inherited = tokio::spawn(async move {
            assert!(capture_tool_parent(Some("spawn-tool")).is_none());
            copied
        })
        .await
        .unwrap();
        assert_eq!(inherited, Some(expected.clone()));
        scope_tool_parent(None, async {
            assert!(capture_tool_parent(Some("spawn-tool")).is_none());
        })
        .await;
        assert_eq!(capture_tool_parent(Some("spawn-tool")), Some(expected));
    })
    .await;
    assert!(capture_tool_parent(Some("spawn-tool")).is_none());
    assert!(tool_model_call_context("goal-verify").is_none());
}

#[derive(Default)]
struct CaptureModel(Mutex<Vec<ModelRequest>>);

#[async_trait]
impl Model for CaptureModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        // Allow another owner to enter this same provider before capturing the request.
        tokio::task::yield_now().await;
        self.0.lock().unwrap().push(request);
        let response = ModelResponse::new(
            ModelMessage::assistant_text("done"),
            StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(ModelStream::with_parent_cancellation(
            futures::stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

fn streaming(call_context: ModelCallContext) -> StreamingContext {
    StreamingContext {
        event_bus: Arc::new(EventBus::new(Default::default()).0),
        turn_id: TurnId::new(),
        agent_id: AgentId::new(),
        cancel: CancellationToken::new(),
        call_context: Some(call_context),
    }
}

#[tokio::test]
async fn identical_concurrent_bridge_requests_keep_request_owned_contexts() {
    let model = Arc::new(CaptureModel::default());
    let bridge = AgentModelBridge::new(Arc::clone(&model) as Arc<dyn Model>);
    let messages = vec![BaseMessage::human("the same prompt")];
    let first = context("session-a", "reason");
    let second = context("session-b", "reason");
    let (left, right) = tokio::join!(
        bridge.generate_reasoning(&messages, &[], Some(streaming(first.clone()))),
        bridge.generate_reasoning(&messages, &[], Some(streaming(second.clone()))),
    );
    left.unwrap();
    right.unwrap();
    let requests = model.0.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages, requests[1].messages);
    let contexts: Vec<_> = requests.iter().map(|r| r.call_context.clone()).collect();
    assert!(contexts.contains(&Some(first)));
    assert!(contexts.contains(&Some(second)));
}

#[tokio::test]
async fn prepared_bridge_rejects_identity_supplied_after_freezing() {
    let bridge = AgentModelBridge::new(Arc::new(CaptureModel::default()));
    let prepared = PreparedModelCall::new(serde_json::json!({}), |_| {
        panic!("late attribution must be rejected before the prepared call starts")
    });
    let error = bridge
        .generate_prepared_reasoning(prepared, Some(streaming(context("session-a", "reason"))))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("bound before preparation"));
}

#[tokio::test]
async fn auxiliary_call_sequences_are_local_to_the_operation() {
    let provider = CaptureModel::default();
    let left = ScopedModel::new(&provider, Some(context("session-a", "compact")));
    let right = ScopedModel::new(&provider, Some(context("session-b", "compact")));
    let (a, b) = tokio::join!(
        left.complete(ModelRequest::default(), CancellationToken::new()),
        right.complete(ModelRequest::default(), CancellationToken::new()),
    );
    a.unwrap();
    b.unwrap();
    left.complete(ModelRequest::default(), CancellationToken::new())
        .await
        .unwrap();
    let contexts: Vec<_> = provider
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.call_context.clone().unwrap())
        .collect();
    let mut actual: Vec<_> = contexts
        .iter()
        .map(|context| (context.owner.as_str(), context.call.as_str()))
        .collect();
    actual.sort_unstable();
    assert_eq!(
        actual,
        vec![
            ("session-a", "compact:0:0"),
            ("session-a", "compact:0:1"),
            ("session-b", "compact:0:0"),
        ]
    );
}

#[tokio::test]
async fn scoped_model_preserves_complete_override_and_binds_identity_once() {
    #[derive(Default)]
    struct CompleteOnlyModel(Mutex<Vec<ModelRequest>>);

    #[async_trait]
    impl Model for CompleteOnlyModel {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::default()
        }

        async fn stream(
            &self,
            _request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> ModelResult<ModelStream> {
            panic!("the wrapper must preserve the provider's complete override")
        }

        async fn complete(
            &self,
            request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> ModelResult<ModelResponse> {
            self.0.lock().unwrap().push(request);
            ModelResponse::new(
                ModelMessage::assistant_text("complete override"),
                StopReason::EndTurn,
                None,
                None,
            )
        }
    }

    let provider = CompleteOnlyModel::default();
    let model = ScopedModel::new(&provider, Some(context("session-a", "compact")));
    let response = model
        .complete(ModelRequest::default(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        response.assistant_text().as_deref(),
        Some("complete override")
    );
    let requests = provider.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let identity = requests[0].call_context.as_ref().unwrap();
    assert_eq!(identity.owner, "session-a");
    assert_eq!(identity.call, "compact:0:0");
}

#[test]
fn reason_identity_uses_real_execution_and_requires_a_session_owner() {
    let session = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let mut ctx = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    assert!(ctx
        .model_call_context("reason", "reason:0".into())
        .is_none());
    ctx.session
        .session_context
        .write()
        .insert("session_id".into(), "session-a".into());
    let first = ctx.model_call_context("reason", "reason:0".into()).unwrap();
    assert_eq!(
        first.execution,
        ctx.session.turn.execution_binding().attempt_id.to_string()
    );
    assert_eq!(
        first,
        ctx.model_call_context("reason", "reason:0".into()).unwrap()
    );
    ctx.session.turn.advance_step();
    let continuation = ctx
        .model_call_context(
            "reason",
            format!("reason:{}", ctx.session.turn.current_step()),
        )
        .unwrap();
    assert_eq!(continuation.execution, first.execution);
    assert_ne!(continuation.call, first.call);
    ctx.session.turn = Arc::new(session.start_turn());
    let next = ctx.model_call_context("reason", "reason:0".into()).unwrap();
    assert_eq!(next.owner, first.owner);
    assert_ne!(next.execution, first.execution);
}
