//! 通过生产 runner 验证窄接口的消息、目录和队列能力。
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage, SessionInbox};

use crate::middleware::{capabilities as hook_state, Middleware, MiddlewareChain};
use crate::{
    agent::{
        react::{AgentOutput, Reasoning, ToolCall, ToolResult},
        stages::{middleware_runner, SharedToolMap, StageContext},
    },
    error::AgentResult,
    messages::{BaseMessage, MessageContent},
    session::{FrozenContext, Session},
    tools::{BaseTool, ToolContext},
};

fn context_with(middleware: impl Middleware + 'static) -> StageContext {
    let session = Session::new(
        Arc::from("/tmp/capabilities"),
        FrozenContext::builder().build(),
        None,
    );
    let mut ctx = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(middleware));
    ctx.runtime.middleware_chain = Arc::new(chain);
    ctx
}

struct ModelProbe(Arc<AtomicBool>);

#[async_trait]
impl Middleware for ModelProbe {
    fn name(&self) -> &str {
        "ModelProbe"
    }

    async fn before_model(&self, state: &mut dyn hook_state::BeforeModelState) -> AgentResult<()> {
        state.add_message(BaseMessage::human(MessageContent::text("model marker")));
        Ok(())
    }

    async fn after_model(
        &self,
        state: &mut dyn hook_state::StateView,
        _: &Reasoning,
    ) -> AgentResult<()> {
        assert!(state
            .messages()
            .iter()
            .any(|message| message.content() == "model marker"));
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn model_append_is_visible_to_the_next_read_only_hook() {
    let seen = Arc::new(AtomicBool::new(false));
    let ctx = context_with(ModelProbe(Arc::clone(&seen)));
    middleware_runner::run_before_model(&ctx).await.unwrap();
    let reasoning = Reasoning {
        thought: String::new(),
        final_answer: None,
        tool_calls: vec![],
        source_message: None,
        usage: None,
        request_id: None,
        model: String::new(),
        streamed: false,
        stop_reason: peri_model::StopReason::EndTurn,
    };
    middleware_runner::run_after_model(&ctx, &reasoning)
        .await
        .unwrap();
    assert!(seen.load(Ordering::SeqCst));
    assert_eq!(ctx.session.transcript.read().len(), 1);
}

struct CatalogTool;

#[async_trait]
impl BaseTool for CatalogTool {
    fn name(&self) -> &str {
        "catalog_marker"
    }
    fn description(&self) -> &str {
        "catalog marker"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        unreachable!("catalog probe never invokes tools")
    }
}

struct CatalogProbe(SharedToolMap);

#[async_trait]
impl Middleware for CatalogProbe {
    fn name(&self) -> &str {
        "CatalogProbe"
    }
    async fn before_reason_catalog(
        &self,
        state: &mut dyn hook_state::CatalogState,
    ) -> AgentResult<()> {
        let tools = state
            .local_tools()
            .expect("production context has local tools");
        assert!(Arc::ptr_eq(tools, &self.0));
        tools
            .write()
            .insert("catalog_marker".to_string(), Arc::new(CatalogTool));
        state.push_recall("catalog changed".to_string());
        Ok(())
    }
}

#[tokio::test]
async fn catalog_capability_rebinds_the_working_map_and_drains_recall() {
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(Default::default()));
    let mut ctx = context_with(CatalogProbe(Arc::clone(&tools)));
    ctx.runtime.tools = Arc::clone(&tools);
    middleware_runner::run_before_reason_catalog(&ctx)
        .await
        .unwrap();
    assert!(tools.read().contains_key("catalog_marker"));
    assert!(
        !tools.read()["catalog_marker"].is_direct(),
        "capability migration must not change deferred visibility"
    );
    assert_eq!(*ctx.recall_buffer.read(), vec!["catalog changed"]);
}

struct QueueProbe;

fn enqueue(state: &dyn hook_state::QueueState, text: &str) {
    assert!(state.inbox_handle().is_some());
    crate::middleware::enqueue_v2_message(
        state,
        QueuedMessage::new(
            MessageKind::Defer,
            MessageSource::GoalSteering,
            BaseMessage::human(MessageContent::text(text)),
        ),
    );
}

#[async_trait]
impl Middleware for QueueProbe {
    fn name(&self) -> &str {
        "QueueProbe"
    }
    async fn before_tool(
        &self,
        state: &mut dyn hook_state::BeforeToolState,
        call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        assert_eq!(state.cwd(), "/tmp/capabilities");
        assert_eq!(state.current_step(), 0);
        let mut modified = call.clone();
        modified.input = serde_json::json!({ "approved": true });
        Ok(modified)
    }
    async fn after_tool(
        &self,
        state: &mut dyn hook_state::AfterToolState,
        _: &ToolCall,
        _: &ToolResult,
    ) -> AgentResult<()> {
        enqueue(state, "tool feedback");
        Ok(())
    }
    async fn after_agent(
        &self,
        state: &mut dyn hook_state::AfterAgentState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        enqueue(state, "agent feedback");
        let mut output = output.clone();
        output.block_continue = Some("queued feedback".to_string());
        Ok(output)
    }
}

#[tokio::test]
async fn tool_and_agent_feedback_use_queue_capability_without_writing_history() {
    let mut ctx = context_with(QueueProbe);
    let inbox = SessionInbox::new(Arc::new(ctx.session.queue.clone()));
    ctx.async_ctx.inbox_handle = Some(inbox.handle());
    let call = ToolCall::new("call", "probe", serde_json::json!({}));
    let mut approved = middleware_runner::run_before_tools_batch(&ctx, &[call]).await;
    let approved = approved.remove(0).unwrap();
    assert_eq!(approved.input, serde_json::json!({ "approved": true }));
    let result = ToolResult::success("call", "probe", "done");
    middleware_runner::run_after_tool(&ctx, &approved, &result)
        .await
        .unwrap();
    let output = middleware_runner::run_after_agent(&ctx, AgentOutput::new("done", 1))
        .await
        .unwrap();
    assert_eq!(output.block_continue.as_deref(), Some("queued feedback"));
    assert!(ctx.session.queue.has_wake_up());
    assert_eq!(ctx.session.queue.len(), 2);
    assert!(ctx.session.transcript.read().is_empty());
}
