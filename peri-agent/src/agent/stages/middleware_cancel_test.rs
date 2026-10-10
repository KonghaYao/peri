use super::*;
use crate::agent::react::{AgentOutput, ReactLLM, Reasoning};
use crate::agent::stages::{run_react_loop, LoopResult};
use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, MessageContent};
use crate::middleware::{Middleware, MiddlewareChain};
use crate::session::queue::{MessageSource, QueuedMessage};
use crate::session::store::FrozenContext;
use crate::session::Session;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Clone, Copy)]
enum PendingHook {
    BeforeAgent,
    Startup,
    BeforeModel,
    BeforeCompact,
    AfterCompact,
    ReasonCatalog,
    AfterModel,
    AfterAgent,
    BeforeTool,
    AfterTool,
    AfterToolsBatch,
    FirstReminder,
}

struct BlockingMiddleware {
    hook: PendingHook,
    entered: Arc<Notify>,
}

impl BlockingMiddleware {
    async fn block(&self) -> AgentResult<()> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[async_trait::async_trait]
impl Middleware for BlockingMiddleware {
    fn name(&self) -> &str {
        "blocking-cancel-regression"
    }

    async fn before_agent(&self, state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        if matches!(self.hook, PendingHook::BeforeAgent) {
            let original = state.messages()[0].clone();
            assert!(state.replace_message(
                original.clone_with_content(MessageContent::text("prepared input"))
            ));
            return self.block().await;
        }
        Ok(())
    }

    async fn before_react_start(
        &self,
        state: &mut dyn hook_state::StartupState,
    ) -> AgentResult<()> {
        if matches!(self.hook, PendingHook::Startup) {
            state.stage_startup_tools(StartupToolUpdate {
                tools: Vec::new(),
                required: Vec::new(),
            })?;
            return self.block().await;
        }
        Ok(())
    }

    async fn before_compact(&self, _state: &mut dyn hook_state::StateView) -> AgentResult<()> {
        if matches!(self.hook, PendingHook::BeforeCompact) {
            return self.block().await;
        }
        Ok(())
    }

    async fn after_compact(&self, _state: &mut dyn hook_state::StateView) -> AgentResult<()> {
        if matches!(self.hook, PendingHook::AfterCompact) {
            return self.block().await;
        }
        Ok(())
    }

    async fn before_reason_catalog(
        &self,
        _state: &mut dyn hook_state::CatalogState,
    ) -> AgentResult<()> {
        if matches!(self.hook, PendingHook::ReasonCatalog) {
            return self.block().await;
        }
        Ok(())
    }

    async fn after_model(
        &self,
        _state: &mut dyn hook_state::StateView,
        _reasoning: &Reasoning,
    ) -> AgentResult<()> {
        if matches!(self.hook, PendingHook::AfterModel) {
            return self.block().await;
        }
        Ok(())
    }

    async fn before_tool(
        &self,
        _state: &mut dyn hook_state::BeforeToolState,
        call: &crate::agent::react::ToolCall,
    ) -> AgentResult<crate::agent::react::ToolCall> {
        if matches!(self.hook, PendingHook::BeforeTool) {
            self.block().await?;
        }
        Ok(call.clone())
    }

    async fn first_turn_reminder(
        &self,
        _state: &mut dyn hook_state::QueueState,
    ) -> AgentResult<Option<String>> {
        if matches!(self.hook, PendingHook::FirstReminder) {
            self.block().await?;
        }
        Ok(None)
    }

    async fn after_tool(
        &self,
        _state: &mut dyn hook_state::AfterToolState,
        _call: &crate::agent::react::ToolCall,
        _result: &crate::agent::react::ToolResult,
    ) -> AgentResult<()> {
        if matches!(self.hook, PendingHook::AfterTool) {
            return self.block().await;
        }
        Ok(())
    }

    async fn after_tools_batch(
        &self,
        _state: &mut dyn hook_state::AfterToolsBatchState,
        _results: &[(
            crate::agent::react::ToolCall,
            crate::agent::react::ToolResult,
        )],
    ) -> AgentResult<()> {
        if matches!(self.hook, PendingHook::AfterToolsBatch) {
            return self.block().await;
        }
        Ok(())
    }

    async fn before_model(&self, _state: &mut dyn hook_state::BeforeModelState) -> AgentResult<()> {
        if matches!(self.hook, PendingHook::BeforeModel) {
            return self.block().await;
        }
        Ok(())
    }

    async fn after_agent(
        &self,
        _state: &mut dyn hook_state::AfterAgentState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        if matches!(self.hook, PendingHook::AfterAgent) {
            self.block().await?;
        }
        Ok(output.clone())
    }
}

struct FinalAnswer;

#[async_trait::async_trait]
impl ReactLLM for FinalAnswer {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> AgentResult<Reasoning> {
        Ok(Reasoning::with_answer("", "done"))
    }
}

async fn cancel_pending_hook(hook: PendingHook) {
    let session = Session::new(
        Arc::from("/tmp/cancel-hooks"),
        FrozenContext::builder().build(),
        None,
    );
    let mut context = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let entered = Arc::new(Notify::new());
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(BlockingMiddleware {
        hook,
        entered: entered.clone(),
    }));
    context.runtime.middleware_chain = Arc::new(chain);
    context.runtime.llm = Arc::new(FinalAnswer);
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("original input")),
    ));
    let cancel = context.session.turn.cancel_token.clone();
    let catalog = context.runtime.tool_catalog.clone();
    let before = catalog.snapshot();
    let mut execution = Box::pin(run_react_loop(context, 10));
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            _ = entered.notified() => {},
            result = execution.as_mut() => panic!("hook never entered: {result:?}"),
        }
    })
    .await
    .expect("hook must be entered");
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_millis(200), execution)
        .await
        .expect("cancel must interrupt a pending middleware hook");
    assert!(matches!(result, LoopResult::Interrupted), "{result:?}");
    if matches!(hook, PendingHook::BeforeAgent) {
        assert_eq!(
            session.transcript().read().visible_messages()[0]
                .content()
                .to_string(),
            "prepared input"
        );
    }
    if matches!(hook, PendingHook::Startup) {
        assert!(
            Arc::ptr_eq(&before, &catalog.snapshot()),
            "cancelled startup must discard staged tools"
        );
    }

    let next_turn = crate::session::turn::TurnContext::new(
        Arc::from("/tmp/cancel-hooks"),
        Arc::new(tokio_util::sync::CancellationToken::new()),
    );
    let mut next = StageContext::new(next_turn, session.transcript(), session.queue().clone());
    next.runtime.llm = Arc::new(FinalAnswer);
    next.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("next prompt")),
    ));
    let result = run_react_loop(next, 10).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
}

#[tokio::test]
async fn cancel_pending_before_agent_preserves_prepared_input() {
    cancel_pending_hook(PendingHook::BeforeAgent).await;
}

#[tokio::test]
async fn cancel_pending_startup_gate() {
    cancel_pending_hook(PendingHook::Startup).await;
}

#[tokio::test]
async fn cancel_pending_before_model() {
    cancel_pending_hook(PendingHook::BeforeModel).await;
}

#[tokio::test]
async fn cancel_pending_after_agent() {
    cancel_pending_hook(PendingHook::AfterAgent).await;
}

#[tokio::test]
async fn cancel_pending_before_compact() {
    cancel_pending_hook(PendingHook::BeforeCompact).await;
}

#[tokio::test]
async fn cancel_pending_after_compact() {
    cancel_pending_hook(PendingHook::AfterCompact).await;
}

#[tokio::test]
async fn cancel_pending_reason_catalog() {
    cancel_pending_hook(PendingHook::ReasonCatalog).await;
}

#[tokio::test]
async fn cancel_pending_after_model() {
    cancel_pending_hook(PendingHook::AfterModel).await;
}

#[tokio::test]
async fn cancel_pending_batch_approval_preserves_result_count() {
    let session = Session::new(
        Arc::from("/tmp/cancel-approval"),
        FrozenContext::builder().build(),
        None,
    );
    let mut context = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let entered = Arc::new(Notify::new());
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(BlockingMiddleware {
        hook: PendingHook::BeforeTool,
        entered: entered.clone(),
    }));
    context.runtime.middleware_chain = Arc::new(chain);
    let calls = vec![
        crate::agent::react::ToolCall::new("first", "Read", serde_json::json!({})),
        crate::agent::react::ToolCall::new("second", "Read", serde_json::json!({})),
    ];
    let mut approval = Box::pin(run_before_tools_batch(&context, &calls));
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            _ = entered.notified() => {},
            _ = approval.as_mut() => panic!("approval must wait"),
        }
    })
    .await
    .unwrap();
    context.session.turn.cancel_token.cancel();
    let results = tokio::time::timeout(Duration::from_millis(200), approval)
        .await
        .unwrap();
    assert_eq!(results.len(), calls.len());
    assert!(results
        .iter()
        .all(|result| matches!(result, Err(AgentError::Interrupted))));
}

#[tokio::test]
async fn cancel_pending_first_turn_reminder() {
    let session = Session::new(
        Arc::from("/tmp/cancel-reminder"),
        FrozenContext::builder().build(),
        None,
    );
    let mut context = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let entered = Arc::new(Notify::new());
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(BlockingMiddleware {
        hook: PendingHook::FirstReminder,
        entered: entered.clone(),
    }));
    context.runtime.middleware_chain = Arc::new(chain);
    let mut reminder = Box::pin(run_first_turn_reminders(&context));
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            _ = entered.notified() => {},
            _ = reminder.as_mut() => panic!("reminder must wait"),
        }
    })
    .await
    .unwrap();
    context.session.turn.cancel_token.cancel();
    let result = tokio::time::timeout(Duration::from_millis(200), reminder)
        .await
        .unwrap();
    assert!(matches!(result, Err(AgentError::Interrupted)));
}

struct CompletedTool;

#[async_trait::async_trait]
impl crate::tools::BaseTool for CompletedTool {
    fn name(&self) -> &str {
        "CompletedTool"
    }

    fn description(&self) -> &str {
        "cancel regression"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _context: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("completed tool output".into())
    }
}

async fn cancel_pending_tool_postprocessing(hook: PendingHook) {
    let session = Session::new(
        Arc::from("/tmp/cancel-tool-postprocessing"),
        FrozenContext::builder().build(),
        None,
    );
    let mut context = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let entered = Arc::new(Notify::new());
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(BlockingMiddleware {
        hook,
        entered: entered.clone(),
    }));
    context.runtime.middleware_chain = Arc::new(chain);
    context
        .runtime
        .tools
        .write()
        .insert("CompletedTool".into(), Arc::new(CompletedTool));
    let catalog = context
        .runtime
        .tool_catalog
        .pin_working_tools(&context.runtime.tools.read())
        .unwrap();
    let reasoning = Reasoning::with_tools(
        "",
        vec![crate::agent::react::ToolCall::new(
            "completed-call",
            "CompletedTool",
            serde_json::json!({}),
        )],
    );
    let mut dispatch = Box::pin(crate::agent::stages::tool_dispatch::dispatch_tools(
        &context,
        &reasoning,
        &catalog,
        &context.session.turn.cancel_token,
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            _ = entered.notified() => {},
            result = dispatch.as_mut() => panic!("postprocessing must wait: {}", result.is_ok()),
        }
    })
    .await
    .unwrap();
    context.session.turn.cancel_token.cancel();
    let result = tokio::time::timeout(Duration::from_millis(200), dispatch)
        .await
        .unwrap();
    assert!(matches!(result, Err(AgentError::Interrupted)));
    assert!(
        session
            .transcript()
            .read()
            .visible_messages()
            .iter()
            .any(|message| {
                matches!(message, BaseMessage::Tool { .. })
                    && message.content() == "completed tool output"
            }),
        "cancel must preserve the tool result completed before the hook"
    );
}

#[tokio::test]
async fn cancel_pending_after_tool_preserves_completed_result() {
    cancel_pending_tool_postprocessing(PendingHook::AfterTool).await;
}

#[tokio::test]
async fn cancel_pending_after_tools_batch_preserves_completed_result() {
    cancel_pending_tool_postprocessing(PendingHook::AfterToolsBatch).await;
}

#[tokio::test]
async fn cancelled_startup_does_not_poll_or_commit_candidate() {
    let session = Session::new(
        Arc::from("/tmp/cancel-startup"),
        FrozenContext::builder().build(),
        None,
    );
    let context = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let before = context.runtime.tool_catalog.snapshot();
    context.session.turn.cancel_token.cancel();
    let result = run_before_react_start(&context).await;
    assert!(matches!(result, Err(AgentError::Interrupted)));
    assert!(Arc::ptr_eq(
        &before,
        &context.runtime.tool_catalog.snapshot()
    ));
}
