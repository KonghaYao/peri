use crate::agent::react::ReactLLM;

use crate::agent::stages::context::StageContext;
use crate::agent::stages::LoopResult;
use crate::messages::BaseMessage;
use crate::session::config::DEFAULT_AGENT_MAX_ITERATIONS;
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;

use crate::session::QueuedMessage;
use crate::session::Session;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct RepeatingLlm {
    calls: Arc<AtomicUsize>,
    queue: crate::session::MessageQueue,
}

#[async_trait::async_trait]
impl ReactLLM for RepeatingLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if calls <= 500 {
            self.queue.push_sync(QueuedMessage::defer(
                MessageSource::SystemInjected,
                BaseMessage::human("continue"),
            ));
        }
        Ok(crate::agent::react::Reasoning::with_answer("", "done"))
    }
}

/// [回归测试] DeepSWE 3194：主 Agent 默认迭代预算不应在第 500 轮截断。
#[tokio::test]
async fn default_iteration_budget_allows_more_than_500_steps() {
    let session = Session::new(
        Arc::from("/tmp/default-iteration-budget"),
        FrozenContext::builder().build(),
        None,
    );
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("start"),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(RepeatingLlm {
        calls: calls.clone(),
        queue: session.queue().clone(),
    }))
    .build();
    let result = crate::agent::stages::run_react_loop(context, DEFAULT_AGENT_MAX_ITERATIONS).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 501);
}
