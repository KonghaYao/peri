use super::*;
use crate::middleware::{capabilities as hook_state, Middleware, MiddlewareChain};
use crate::session::{queue::MessageQueue, transcript::MessageTranscript, turn::TurnContext};
use crate::tools::BaseTool;
use serde_json::json;

#[tokio::test]
async fn nested_dispatch_keeps_pinned_target_and_does_not_commit_outer_batch() {
    struct Target(&'static str);
    #[async_trait::async_trait]
    impl BaseTool for Target {
        fn name(&self) -> &str {
            "Target"
        }
        fn description(&self) -> &str {
            self.0
        }
        fn parameters(&self) -> serde_json::Value {
            json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.0.into())
        }
    }
    type HookTrace = Arc<parking_lot::Mutex<Vec<(&'static str, usize)>>>;
    struct Hooks(HookTrace);
    #[async_trait::async_trait]
    impl Middleware for Hooks {
        fn name(&self) -> &str {
            "NestedHooks"
        }
        async fn after_tool(
            &self,
            state: &mut dyn hook_state::AfterToolState,
            _call: &ToolCall,
            _result: &crate::agent::react::ToolResult,
        ) -> crate::error::AgentResult<()> {
            self.0.lock().push(("after_tool", state.messages().len()));
            Ok(())
        }
        async fn after_tools_batch(
            &self,
            state: &mut dyn hook_state::StateView,
            _results: &[(ToolCall, crate::agent::react::ToolResult)],
        ) -> crate::error::AgentResult<()> {
            self.0.lock().push(("after_batch", state.messages().len()));
            Ok(())
        }
    }
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let turn = TurnContext::new(Arc::from("/tmp"), Arc::new(CancellationToken::new()));
    let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let original_id = transcript
        .write()
        .append(BaseMessage::human("parent history"));
    let mut ctx = StageContext::builder(turn, Arc::clone(&transcript), MessageQueue::new())
        .with_event_bus(Arc::new(bus))
        .build();
    let trace: HookTrace = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(Hooks(Arc::clone(&trace))));
    ctx.runtime.middleware_chain = Arc::new(chain);
    ctx.runtime
        .tools
        .write()
        .insert("Target".into(), Arc::new(Target("pinned")));
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .unwrap();
    let dispatcher = StageEffectiveToolDispatcher::new(ctx.clone(), catalog);
    // A later working-map change must not replace the target of this invocation.
    ctx.runtime
        .tools
        .write()
        .insert("Target".into(), Arc::new(Target("replacement")));
    ctx.compact
        .consecutive_failures
        .store(4, std::sync::atomic::Ordering::Relaxed);
    let output = dispatcher
        .dispatch(
            EffectiveToolCall {
                invocation_id: "inner".into(),
                tool_name: "Target".into(),
                input: json!({}),
                parent_invocation_id: Some("outer-ptc".into()),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(output, "pinned");
    assert_eq!(*trace.lock(), vec![("after_tool", 1)]);
    assert_eq!(
        transcript
            .read()
            .entries()
            .iter()
            .map(|entry| entry.id())
            .collect::<Vec<_>>(),
        vec![original_id]
    );
    assert_eq!(
        ctx.compact
            .consecutive_failures
            .load(std::sync::atomic::Ordering::Relaxed),
        4
    );
    let mut ids = Vec::new();
    while let Some(event) = handles.try_render() {
        if let crate::agent::events_v2::RenderEvent::ToolStarted { tool_call_id, .. }
        | crate::agent::events_v2::RenderEvent::ToolEnded { tool_call_id, .. } = event
        {
            ids.push(tool_call_id);
        }
    }
    assert_eq!(ids, vec!["outer-ptc/inner", "outer-ptc/inner"]);
}
