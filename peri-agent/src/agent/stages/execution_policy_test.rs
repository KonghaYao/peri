use super::*;
use crate::agent::react::{Reasoning, StreamingContext};
use crate::error::AgentResult;
use crate::messages::BaseMessage;
use crate::session::{FrozenContext, MessageKind, MessageSource, Session};
use peri_acp_types::session::{ExecutionBinding, MessageDisposition, MessagePolicy};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct PolicyLlm {
    calls: Arc<AtomicUsize>,
    queue: MessageQueue,
    tail: Option<MessagePolicy>,
}

#[async_trait::async_trait]
impl ReactLLM for PolicyLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            if let Some(policy) = &self.tail {
                self.queue.push(
                    QueuedMessage::new(
                        MessageKind::Info,
                        MessageSource::ChannelMessage,
                        BaseMessage::human("tail"),
                    )
                    .with_policy(policy.clone()),
                );
            }
        }
        Ok(Reasoning::with_answer("", "done"))
    }
}

fn context() -> StageContext {
    let session = Session::new(
        Arc::from("/tmp/execution-policy"),
        FrozenContext::builder().build(),
        None,
    );
    StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .build()
}

#[test]
fn recovery_instructions_bind_both_turn_and_attempt() {
    let context = context();
    enqueue_truncation_continuation(&context);
    enqueue_stream_interruption_continuation(&context);
    let execution = context.session.turn.execution_binding();
    let other_attempt = ExecutionBinding {
        turn_id: execution.turn_id,
        attempt_id: peri_acp_types::identity::AttemptId::new(),
    };
    assert!(context.session.queue.has_required_for_run(&execution));
    assert!(!context.session.queue.has_required_for_run(&other_attempt));
    assert!(!context.session.queue.has_ensure_processing());
    for message in context.session.queue.drain_all() {
        assert_eq!(
            message.policy,
            MessagePolicy::continue_current_run(execution.clone())
        );
        assert_eq!(
            message.policy.disposition(&other_attempt),
            MessageDisposition::Suppressed
        );
    }
}

#[tokio::test]
async fn passive_tail_projects_without_another_reason() {
    let mut context = context();
    let calls = Arc::new(AtomicUsize::new(0));
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("start"),
    ));
    context.runtime.llm = Arc::new(PolicyLlm {
        calls: calls.clone(),
        queue: context.session.queue.clone(),
        tail: Some(MessagePolicy::passive()),
    });
    let queue = context.session.queue.clone();
    assert!(matches!(
        run_react_loop(context, 3).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(queue.is_empty());
    assert!(!queue.has_ensure_processing());
}

#[tokio::test]
async fn required_message_behind_multiple_passive_batches_is_not_starved() {
    let mut context = context();
    let calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..130 {
        context.session.queue.push(QueuedMessage::info(
            MessageSource::SystemInjected,
            BaseMessage::human("progress"),
        ));
    }
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("start"),
    ));
    context.runtime.llm = Arc::new(PolicyLlm {
        calls: calls.clone(),
        queue: context.session.queue.clone(),
        tail: None,
    });
    assert!(matches!(
        run_react_loop(context, 1).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn required_tail_continues_current_run_without_source_dispatch() {
    let mut context = context();
    let execution = context.session.turn.execution_binding();
    let calls = Arc::new(AtomicUsize::new(0));
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("start"),
    ));
    context.runtime.llm = Arc::new(PolicyLlm {
        calls: calls.clone(),
        queue: context.session.queue.clone(),
        tail: Some(MessagePolicy::continue_current_run(execution)),
    });
    assert!(matches!(
        run_react_loop(context, 3).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn stale_attempt_steering_is_suppressed_without_reason() {
    let context = context();
    let execution = context.session.turn.execution_binding();
    context.session.queue.push(
        QueuedMessage::defer(MessageSource::TodoSteering, BaseMessage::human("stale")).with_policy(
            MessagePolicy::continue_current_run(ExecutionBinding {
                turn_id: execution.turn_id,
                attempt_id: peri_acp_types::identity::AttemptId::new(),
            }),
        ),
    );
    let queue = context.session.queue.clone();
    assert!(matches!(
        run_react_loop(context, 0).await,
        LoopResult::Completed
    ));
    assert_eq!(queue.suppressed_messages().len(), 1);
    assert!(queue.is_empty());
}

#[tokio::test]
async fn idle_run_wakes_for_its_bound_steering() {
    let mut context = context();
    let calls = Arc::new(AtomicUsize::new(0));
    let suspended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let execution = context.session.turn.execution_binding();
    let queue = context.session.queue.clone();
    context.runtime.llm = Arc::new(PolicyLlm {
        calls: calls.clone(),
        queue: queue.clone(),
        tail: None,
    });
    context.async_ctx.idle_wait_enabled = true;
    context.async_ctx.idle_suspended_flag = Some(suspended.clone());
    context.async_ctx.idle_should_wait = Some({
        let calls = calls.clone();
        Arc::new(move || calls.load(Ordering::SeqCst) == 0)
    });
    let task = tokio::spawn(run_react_loop(context, 3));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !suspended.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("run must enter idle before steering");
    queue.push(
        QueuedMessage::defer(MessageSource::TodoSteering, BaseMessage::human("continue"))
            .with_policy(MessagePolicy::continue_current_run(execution)),
    );
    assert!(!queue.has_ensure_processing());
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("bound steering must wake its idle run")
        .expect("loop must not panic");
    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!suspended.load(Ordering::Acquire));
}
