//! Hook 显式停止意图的执行绑定与跨 run 抑制。

use super::*;
use crate::agent::async_tasks::handoff::ChildSettlementWindow;
use crate::agent::react::{ReactLLM, Reasoning, StreamingContext};
use crate::messages::BaseMessage;
use crate::session::queue::{MessageSource, QueuedMessage};
use crate::session::{FrozenContext, MessageKind, Session, TurnContext};
use crate::tools::BaseTool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct AnswerLlm {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ReactLLM for AnswerLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Reasoning::with_answer("", "answer"))
    }
}

fn stop_message(execution: peri_acp_types::session::ExecutionBinding) -> QueuedMessage {
    use peri_acp_types::system_reminder::{ReminderCategory, ReminderDelivery, ReminderSeverity};
    let reminder = crate::session::producer_reminders::trusted_reminder(
        ReminderCategory::Lifecycle,
        "hook",
        "post_tool_batch_stop",
        ReminderSeverity::Info,
        ReminderDelivery::Configurable,
        "hook requested stop".into(),
        None,
        serde_json::json!({}),
    );
    QueuedMessage::system_reminder(MessageKind::Info, MessageSource::HookStopIntent, reminder)
        .with_policy(peri_acp_types::session::MessagePolicy::stop_current_run(
            execution,
        ))
}

fn context_with_turn(
    session: &Session,
    turn: TurnContext,
    calls: Arc<AtomicUsize>,
) -> StageContext {
    StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(AnswerLlm { calls }))
        .build()
}

fn new_session() -> Arc<Session> {
    Session::new(
        Arc::from("/tmp/stop-intent"),
        FrozenContext::builder().build(),
        None,
    )
}

/// 同一执行的 Stop 仍生效：不发起模型请求，返回 HookStopped。
#[tokio::test]
async fn stop_intent_for_current_execution_returns_hook_stopped() {
    let session = new_session();
    let turn = session.start_turn();
    session.queue().push(stop_message(turn.execution_binding()));
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_react_loop(context_with_turn(&session, turn, calls.clone()), 10).await;

    assert!(matches!(
        result,
        LoopResult::HookStopped { ref unprocessed_tool_calls } if unprocessed_tool_calls.is_empty()
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// 旧执行的 Stop 在新执行中被抑制；新执行照常推理并正常结束。
#[tokio::test]
async fn stale_stop_intent_does_not_stop_new_execution() {
    let stale = new_session().start_turn().execution_binding();
    let session = new_session();
    session.queue().push(stop_message(stale));
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("resume work"),
    ));
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_react_loop(
        context_with_turn(&session, session.start_turn(), calls.clone()),
        10,
    )
    .await;

    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "旧 Stop 不得跳过新执行推理"
    );
    assert_eq!(session.queue().suppressed_messages().len(), 1);
}

/// D 先到期不消费 Stop；resume 新执行后旧 Stop 被抑制，同 run 的 Stop 仍生效。
#[tokio::test(start_paused = true)]
async fn deadline_left_stop_intent_does_not_stop_resumed_execution() {
    let session = new_session();
    let first_turn = session.start_turn();
    session
        .queue()
        .push(stop_message(first_turn.execution_binding()));
    let window = Arc::new(ChildSettlementWindow::default());
    window.start();
    tokio::time::advance(Duration::from_secs(121)).await;
    let calls = Arc::new(AtomicUsize::new(0));

    let mut first = context_with_turn(&session, first_turn, calls.clone());
    first.async_ctx.child_settlement = Some(window);
    let first_result = run_react_loop(first, 10).await;
    assert!(matches!(first_result, LoopResult::SettlementDue { .. }));
    assert_eq!(session.queue().len(), 1, "D 到期不得消费旧 stop intent");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("resume after handoff"),
    ));
    let resumed = run_react_loop(
        context_with_turn(&session, session.start_turn(), calls.clone()),
        10,
    )
    .await;
    assert!(matches!(resumed, LoopResult::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(session.queue().suppressed_messages().len(), 1);

    let stop_turn = session.start_turn();
    session
        .queue()
        .push(stop_message(stop_turn.execution_binding()));
    let stopped = run_react_loop(context_with_turn(&session, stop_turn, calls.clone()), 10).await;
    assert!(
        matches!(stopped, LoopResult::HookStopped { .. }),
        "同一执行内消费到的 Stop 仍必须生效"
    );
}
