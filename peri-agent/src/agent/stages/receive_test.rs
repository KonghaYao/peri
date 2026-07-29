//! 从 receive.rs 分离的测试模块
use super::*;
use crate::agent::stages::StageContext;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;
use crate::session::{QueuedMessage, Session};
use std::sync::Arc;

fn make_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

#[tokio::test]
async fn test_receive_empty_queue() {
    let ctx = make_context();
    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    assert_eq!(output.consumed_count, 0);
    assert!(ctx.session.transcript.read().is_empty());
}

#[tokio::test]
async fn test_receive_consumes_prompt() {
    let ctx = make_context();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("hello")),
    ));

    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    assert_eq!(output.consumed_count, 1);
    assert_eq!(ctx.session.transcript.read().len(), 1);
}

#[tokio::test]
async fn test_receive_consumes_info_wrapped_in_reminder() {
    let ctx = make_context();
    ctx.session.queue.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        BaseMessage::human(MessageContent::text("system info")),
    ));

    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    assert_eq!(output.consumed_count, 1);

    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    let content = transcript.entries()[0].message.content();
    assert!(
        content.contains("<system-reminder>"),
        "Info 应被 reminder 包裹"
    );
    assert!(content.contains("system info"));
}

#[tokio::test]
async fn test_receive_consumes_defer() {
    // RCRA：Receive 消费 Defer（不再保留）
    let ctx = make_context();
    ctx.session.queue.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human(MessageContent::text("deferred")),
    ));
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("prompt")),
    ));

    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    // 消费全部（Prompt + Defer）
    assert_eq!(output.consumed_count, 2);
    assert!(ctx.session.queue.is_empty(), "队列应完全排空");
    assert_eq!(ctx.session.transcript.read().len(), 2);
}

#[tokio::test]
async fn test_receive_consumes_prompt_defer_and_info_together() {
    // RCRA：混合队列应全部消费
    let ctx = make_context();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("p")),
    ));
    ctx.session.queue.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human(MessageContent::text("d")),
    ));
    ctx.session.queue.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        BaseMessage::human(MessageContent::text("i")),
    ));

    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    assert_eq!(output.consumed_count, 3);
    assert!(ctx.session.queue.is_empty());
}

#[tokio::test]
async fn test_receive_exit_on_empty_queue() {
    // RCRA：空队列 → consumed=0 → 退出判断触发
    let ctx = make_context();
    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    assert_eq!(output.consumed_count, 0);
}
