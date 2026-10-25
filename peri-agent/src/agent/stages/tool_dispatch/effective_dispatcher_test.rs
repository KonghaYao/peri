use super::*;
use crate::agent::stages::{ExecutionEntryOrigin, InvocationContext, StageContext};
use crate::middleware::{capabilities as hook_state, Middleware, MiddlewareChain};
use crate::session::{queue::MessageQueue, transcript::MessageTranscript, turn::TurnContext};
use crate::tools::BaseTool;
use crate::tools::InvocationOrigin;
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
        fn mcp_server_name(&self) -> Option<&str> {
            Some(self.0)
        }
        fn mcp_tool_name(&self) -> Option<&str> {
            Some("wire_target")
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
        async fn before_tool(
            &self,
            state: &mut dyn hook_state::BeforeToolState,
            call: &ToolCall,
        ) -> crate::error::AgentResult<ToolCall> {
            let origin = state
                .tool_origin(&call.id)
                .expect("bound identity reaches approval");
            assert_eq!(origin.mcp_server_name.as_deref(), Some("pinned"));
            assert_eq!(origin.mcp_tool_name.as_deref(), Some("wire_target"));
            assert_eq!(origin.builtin_mcp_instance, None);
            Ok(call.clone())
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
            state: &mut dyn hook_state::AfterToolsBatchState,
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
    let invocation = InvocationContext::from_stage(
        &ctx,
        ExecutionEntryOrigin::ModelBatch,
        ctx.session.turn.cancel_token.as_ref().clone(),
        None,
    );
    let dispatcher = StageEffectiveToolDispatcher::new(invocation.clone(), catalog);
    assert_eq!(
        dispatcher.admitted_mcp_tool_name("pinned", "wire_target"),
        Some("Target".into())
    );
    assert_eq!(
        dispatcher.admitted_mcp_tool_name("replacement", "wire_target"),
        None
    );
    assert_eq!(dispatcher.admitted_mcp_tool_name("pinned", "Target"), None);
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
                origin: InvocationOrigin::Nested {
                    parent_invocation_id: "outer".into(),
                },
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(output.text, "pinned");
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
    let mut started_origin = None;
    while let Some(event) = handles.try_render() {
        match event {
            crate::agent::events_v2::RenderEvent::ToolStarted {
                tool_call_id,
                origin_message_id,
                origin_readable,
                ..
            } => {
                started_origin = Some((origin_message_id, origin_readable));
                ids.push(tool_call_id);
            }
            crate::agent::events_v2::RenderEvent::ToolEnded { tool_call_id, .. } => {
                ids.push(tool_call_id)
            }
            _ => {}
        }
    }
    assert_eq!(ids, vec!["outer/inner", "outer/inner"]);
    assert_eq!(
        started_origin,
        Some((None, Some(false))),
        "嵌套执行没有持久 origin：必须显式标记不可回读，禁止消费方回退"
    );
}

#[tokio::test]
async fn reported_in_band_result_keeps_raw_fact_instead_of_protocol_error() {
    struct ReportingTool;
    #[async_trait::async_trait]
    impl BaseTool for ReportingTool {
        fn name(&self) -> &str {
            "Reporting"
        }
        fn description(&self) -> &str {
            "reports an in-band failure with a raw payload"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            unreachable!("typed tools must be reached through invoke_output")
        }
        async fn invoke_output(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<crate::tools::ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
            let raw = serde_json::json!({
                "content": [{"type": "text", "text": "remote failure"}],
                "isError": true,
            });
            Ok(crate::tools::ToolOutput::from_raw_result(
                "remote failure",
                peri_acp_types::tool_outcome::RawToolResult::new(raw),
            ))
        }
    }
    let turn = TurnContext::new(Arc::from("/tmp"), Arc::new(CancellationToken::new()));
    let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let ctx = StageContext::builder(turn, transcript, MessageQueue::new()).build();
    ctx.runtime
        .tools
        .write()
        .insert("Reporting".into(), Arc::new(ReportingTool));
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .unwrap();
    let invocation = InvocationContext::from_stage(
        &ctx,
        ExecutionEntryOrigin::ModelBatch,
        ctx.session.turn.cancel_token.as_ref().clone(),
        None,
    );
    let dispatcher = StageEffectiveToolDispatcher::new(invocation, catalog);

    let output = dispatcher
        .dispatch(
            EffectiveToolCall {
                invocation_id: "app-call".into(),
                tool_name: "Reporting".into(),
                input: json!({}),
                origin: InvocationOrigin::App {
                    app_id: "app-1".into(),
                    attachment_id: Some(peri_acp_types::interaction::AttachmentId::new("conn-1")),
                    connection_epoch: Some(peri_acp_types::interaction::ConnectionEpoch::new(1)),
                    server_generation: 1,
                },
            },
            CancellationToken::new(),
        )
        .await
        .expect("已报告结果事实的带内失败不是协议层失败");

    assert_eq!(output.is_error(), Some(true));
    match output.outcome() {
        crate::tools::ToolOutcome::Reported { is_error, raw } => {
            assert!(is_error);
            let raw = raw.as_ref().expect("raw 载荷必须保真返回");
            assert!(raw.is_error());
            assert_eq!(raw.content().len(), 1);
        }
        other => panic!("已报告结果必须保留 typed outcome，实际: {other:?}"),
    }
}

/// nested / App / Host 的 synthetic AI 与轮次入口绑定的批次尾部不同：分派必须
/// 回退为「历史 + synthetic AI」，不得复用带外层批次 AI 的快照。
#[tokio::test]
async fn nested_synthetic_ai_uses_owned_fallback_not_outer_batch_tail() {
    type Seen = Arc<parking_lot::Mutex<Vec<Vec<crate::messages::MessageId>>>>;
    struct Probe(Seen);
    #[async_trait::async_trait]
    impl BaseTool for Probe {
        fn name(&self) -> &str {
            "Probe"
        }
        fn description(&self) -> &str {
            "snapshot probe"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            context: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.0
                .lock()
                .push(context.messages.iter().map(BaseMessage::id).collect());
            Ok("ok".into())
        }
    }

    let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let history_id = transcript.write().append(BaseMessage::human("history"));
    let turn = TurnContext::new(Arc::from("/tmp"), Arc::new(CancellationToken::new()));
    let ctx = StageContext::builder(turn, Arc::clone(&transcript), MessageQueue::new()).build();
    let seen: Seen = Arc::default();
    ctx.runtime
        .tools
        .write()
        .insert("Probe".into(), Arc::new(Probe(seen.clone())));
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .unwrap();
    let outer_batch = BaseMessage::ai("outer batch");
    let invocation = InvocationContext::from_stage(
        &ctx,
        ExecutionEntryOrigin::ModelBatch,
        ctx.session.turn.cancel_token.as_ref().clone(),
        Some(&outer_batch),
    );
    let dispatcher = StageEffectiveToolDispatcher::new(invocation, catalog);

    dispatcher
        .dispatch(
            EffectiveToolCall {
                invocation_id: "inner".into(),
                tool_name: "Probe".into(),
                input: json!({}),
                origin: InvocationOrigin::Nested {
                    parent_invocation_id: "outer".into(),
                },
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let seen = seen.lock();
    assert_eq!(seen.len(), 1);
    let ids = &seen[0];
    assert_eq!(ids.len(), 2, "分派快照 = 历史 + synthetic AI");
    assert_eq!(ids[0], history_id);
    assert_ne!(ids[1], history_id);
    assert_ne!(
        ids[1],
        outer_batch.id(),
        "不得复用外层批次绑定的 AI 尾部快照"
    );
}
