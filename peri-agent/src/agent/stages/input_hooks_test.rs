//! 输入批次准备与 before_agent hook 的调用生命周期。
use super::super::*;
use crate::messages::MessageContent;
use crate::middleware::capabilities as hook_state;
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;
use crate::session::Session;

/// 构造测试用 StageContext
fn make_stage_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder()
        .system_prompt("You are a test agent.")
        .build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new_best_effort_fixture(turn, session.transcript(), session.queue().clone())
}

/// Mock LLM：首轮返回 final_answer，无 tool_calls
struct FinalAnswerLLM {
    answer: &'static str,
}

#[async_trait::async_trait]
impl ReactLLM for FinalAnswerLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        Ok(crate::agent::react::Reasoning::with_answer(
            "thinking",
            self.answer,
        ))
    }
    fn model_name(&self) -> String {
        "mock-final-answer".to_string()
    }
}

struct InputBatchProbe {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    batches: Arc<parking_lot::Mutex<Vec<Vec<crate::messages::MessageId>>>>,
}

#[async_trait::async_trait]
impl crate::middleware::Middleware for InputBatchProbe {
    fn name(&self) -> &str {
        "InputBatchProbe"
    }

    async fn before_agent(
        &self,
        state: &mut dyn hook_state::BeforeAgentState,
    ) -> crate::error::AgentResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let ids = state
            .input_message_ids()
            .expect("生产 runner 必须明确传入输入批次")
            .to_vec();
        self.batches.lock().push(ids.clone());
        let inputs: Vec<_> = state
            .messages()
            .iter()
            .filter(|message| ids.contains(&message.id()))
            .cloned()
            .collect();
        for input in inputs {
            let content = MessageContent::text(format!("{} prepared", input.content()));
            assert!(state.replace_message(input.clone_with_content(content)));
        }
        Ok(())
    }
}

#[tokio::test]
async fn test_input_batch_reaches_single_before_agent_without_history_or_background() {
    let mut ctx = make_stage_context();
    ctx.runtime.llm = Arc::new(FinalAnswerLLM { answer: "done" });
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let batches = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(InputBatchProbe {
        calls: calls.clone(),
        batches: batches.clone(),
    }));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let historical = BaseMessage::human("history @image old.png");
    ctx.session.transcript.write().append(historical.clone());
    let first = BaseMessage::human("A @image current.png");
    let second = BaseMessage::human("B @src/current.rs");
    let background = BaseMessage::human("background @image ignored.png");
    ctx.session.queue.push_batch(vec![
        QueuedMessage::prompt(MessageSource::UserInput, first.clone()),
        QueuedMessage::prompt(MessageSource::UserInput, second.clone()),
        QueuedMessage::prompt(MessageSource::UserInput, BaseMessage::human("")),
        QueuedMessage::defer(MessageSource::SubAgentComplete, background.clone()),
    ]);

    assert!(matches!(
        run_react_loop(ctx.clone(), 1).await,
        LoopResult::Completed
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "整条 before_agent 链只能执行一次"
    );
    assert_eq!(
        *batches.lock(),
        vec![vec![first.id(), second.id()]],
        "批次仅包含本次非空用户输入且顺序不变"
    );
    let transcript = ctx.session.transcript.read();
    for input in [&first, &second] {
        assert_eq!(
            transcript.get(input.id()).unwrap().message().content(),
            format!("{} prepared", input.content()),
            "每条输入都经同一链准备且稳定 ID 写回"
        );
    }
    for untouched in [&historical, &background] {
        assert_eq!(
            transcript.get(untouched.id()).unwrap().message().content(),
            untouched.content(),
            "历史与后台 Human 不属于本批用户输入"
        );
    }
}

#[tokio::test]
async fn test_input_batch_empty_background_attempt_does_not_reprocess_history() {
    let mut ctx = make_stage_context();
    ctx.runtime.llm = Arc::new(FinalAnswerLLM { answer: "done" });
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let batches = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(InputBatchProbe {
        calls: calls.clone(),
        batches: batches.clone(),
    }));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let historical = BaseMessage::human("history @image old.png");
    ctx.session.transcript.write().append(historical.clone());
    ctx.session.queue.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human("background result"),
    ));

    assert!(matches!(
        run_react_loop(ctx.clone(), 1).await,
        LoopResult::Completed
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "后台 attempt 仍只调用一次原 hook 链"
    );
    assert_eq!(
        *batches.lock(),
        vec![Vec::<crate::messages::MessageId>::new()],
        "生产空批次明确为 Some(empty)，不能按最后 Human 回退"
    );
    assert_eq!(
        ctx.session
            .transcript
            .read()
            .get(historical.id())
            .unwrap()
            .message()
            .content(),
        historical.content()
    );
}
#[tokio::test]
async fn test_p0_2_before_agent_runs_once_after_tool_round_trip() {
    use std::collections::BTreeMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    struct ToolRoundTripLLM(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl ReactLLM for ToolRoundTripLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn crate::tools::BaseTool],
            _streaming: Option<crate::agent::react::StreamingContext>,
        ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
            match self.0.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(crate::agent::react::Reasoning::with_tools(
                    "use the local tool",
                    vec![crate::agent::react::ToolCall::new(
                        "p0-2-tool-call",
                        "p0_2_local_tool",
                        serde_json::json!({}),
                    )],
                )),
                1 => {
                    assert!(
                        messages
                            .iter()
                            .any(|message| message.content().contains("p0-2 tool result marker")),
                        "second LLM call must observe the local tool result"
                    );
                    Ok(crate::agent::react::Reasoning::with_answer("", "done"))
                }
                call => panic!("unexpected LLM call {call}"),
            }
        }
    }

    struct LocalTool(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl crate::tools::BaseTool for LocalTool {
        fn name(&self) -> &str {
            "p0_2_local_tool"
        }

        fn description(&self) -> &str {
            "deterministic local test tool"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("p0-2 tool result marker".to_string())
        }
    }

    struct BeforeAgentProbe {
        calls: Arc<AtomicUsize>,
        prompt_visible: Arc<Mutex<Vec<bool>>>,
    }

    #[async_trait::async_trait]
    impl crate::middleware::Middleware for BeforeAgentProbe {
        fn name(&self) -> &str {
            "BeforeAgentProbe"
        }

        async fn before_agent(
            &self,
            state: &mut dyn hook_state::BeforeAgentState,
        ) -> crate::error::AgentResult<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.prompt_visible.lock().unwrap().push(
                state
                    .messages()
                    .iter()
                    .any(|message| message.content().contains("p0-2 prompt marker")),
            );
            state.push_recall("p0-2 recall marker".to_string());
            Ok(())
        }
    }

    let before_agent_calls = Arc::new(AtomicUsize::new(0));
    let prompt_visible = Arc::new(Mutex::new(Vec::new()));
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(BeforeAgentProbe {
        calls: Arc::clone(&before_agent_calls),
        prompt_visible: Arc::clone(&prompt_visible),
    }));
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "p0_2_local_tool".to_string(),
        Arc::new(LocalTool(Arc::clone(&tool_calls))) as Arc<dyn crate::tools::BaseTool>,
    )])));

    let session = Session::new(
        Arc::from("/tmp/p0-2-before-agent-tool-round-trip"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let context = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(ToolRoundTripLLM(Arc::clone(&llm_calls))))
    .with_tools(tools)
    .with_middleware_chain(Arc::new(chain))
    .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("p0-2 prompt marker"),
    ));

    assert!(matches!(
        run_react_loop(context.clone(), 10).await,
        LoopResult::Completed
    ));
    assert_eq!(llm_calls.load(Ordering::SeqCst), 2);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(before_agent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*prompt_visible.lock().unwrap(), vec![true]);
    assert_eq!(
        context.recall_buffer.read().as_slice(),
        ["p0-2 recall marker"]
    );
}

#[tokio::test]
async fn test_p0_2_before_agent_runs_once_after_receive_and_skips_empty_or_cancelled_turns() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    struct CountingLLM(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl ReactLLM for CountingLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn crate::tools::BaseTool],
            _streaming: Option<crate::agent::react::StreamingContext>,
        ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(crate::agent::react::Reasoning::with_answer("", "done"))
        }
    }

    struct BeforeAgentProbe {
        calls: Arc<AtomicUsize>,
        prompt_visible: Arc<Mutex<Vec<bool>>>,
    }

    #[async_trait::async_trait]
    impl crate::middleware::Middleware for BeforeAgentProbe {
        fn name(&self) -> &str {
            "BeforeAgentProbe"
        }

        async fn before_agent(
            &self,
            state: &mut dyn hook_state::BeforeAgentState,
        ) -> crate::error::AgentResult<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.prompt_visible.lock().unwrap().push(
                state
                    .messages()
                    .iter()
                    .any(|message| message.content().contains("p0-2 prompt marker")),
            );
            state.push_recall("p0-2 recall marker".to_string());
            Ok(())
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let prompt_visible = Arc::new(Mutex::new(Vec::new()));
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(BeforeAgentProbe {
        calls: Arc::clone(&calls),
        prompt_visible: Arc::clone(&prompt_visible),
    }));

    let cwd: Arc<str> = Arc::from("/tmp/p0-2-before-agent");
    let session = Session::new(cwd, FrozenContext::builder().build(), None);
    let turn = session.start_turn();
    let context = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(CountingLLM(Arc::clone(&llm_calls))))
    .with_middleware_chain(Arc::new(chain))
    .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("p0-2 prompt marker"),
    ));

    assert!(matches!(
        run_react_loop(context.clone(), 10).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(*prompt_visible.lock().unwrap(), vec![true]);
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        context.recall_buffer.read().as_slice(),
        ["p0-2 recall marker"]
    );

    let empty_session = Session::new(
        Arc::from("/tmp/p0-2-before-agent-empty"),
        FrozenContext::builder().build(),
        None,
    );
    let empty_turn = empty_session.start_turn();
    let empty_context = StageContext::best_effort_fixture_builder(
        empty_turn,
        empty_session.transcript(),
        empty_session.queue().clone(),
    )
    .with_llm(Arc::new(CountingLLM(Arc::clone(&llm_calls))))
    .with_middleware_chain(context.runtime.middleware_chain.clone())
    .build();
    assert!(matches!(
        run_react_loop(empty_context.clone(), 10).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(empty_context.recall_buffer.read().is_empty());
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);

    let cancelled_session = Session::new(
        Arc::from("/tmp/p0-2-before-agent-cancelled"),
        FrozenContext::builder().build(),
        None,
    );
    let cancelled_turn = cancelled_session.start_turn();
    let cancelled_context = StageContext::best_effort_fixture_builder(
        cancelled_turn,
        cancelled_session.transcript(),
        cancelled_session.queue().clone(),
    )
    .with_llm(Arc::new(CountingLLM(Arc::clone(&llm_calls))))
    .with_middleware_chain(context.runtime.middleware_chain.clone())
    .build();
    cancelled_context.session.turn.cancel_token.cancel();
    assert!(matches!(
        run_react_loop(cancelled_context.clone(), 10).await,
        LoopResult::Interrupted
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(cancelled_context.recall_buffer.read().is_empty());
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
}
