//! 从 inbox.rs 分离的测试模块

use super::*;
use crate::messages::MessageContent;
use std::time::Duration;

fn make_msg(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

#[test]
fn test_inbox_handle_push_prompt_wakes() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();

    handle.push_prompt(MessageSource::UserInput, make_msg("hello"));
    assert!(inbox.queue().has_wake_up());
    assert_eq!(inbox.queue().len(), 1);
}

#[test]
fn test_inbox_handle_push_defer_wakes() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();

    handle.push_defer(MessageSource::SubAgentComplete, make_msg("done"));
    assert!(inbox.queue().has_wake_up());
}

#[test]
fn test_inbox_handle_push_info_does_not_wake() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();

    handle.push_info(MessageSource::SystemInjected, make_msg("info"));
    assert!(!inbox.queue().has_wake_up());
    // Info is still in the queue
    assert_eq!(inbox.queue().len(), 1);
}

#[test]
fn test_inbox_handle_push_arbitrary_conditional_wake() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();

    // Info via push() — no wake
    handle.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        make_msg("info"),
    ));
    assert!(!inbox.queue().has_wake_up());

    // Prompt via push() — wakes
    handle.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        make_msg("prompt"),
    ));
    assert!(inbox.queue().has_wake_up());
}

#[test]
fn test_inbox_handle_batch_wakes_on_any_prompt_or_defer() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();

    // Batch of only Info — no wake
    handle.push_batch(vec![QueuedMessage::info(
        MessageSource::SystemInjected,
        make_msg("info1"),
    )]);
    assert!(!inbox.queue().has_wake_up());

    // Batch with one Prompt — wakes
    handle.push_batch(vec![
        QueuedMessage::info(MessageSource::SystemInjected, make_msg("info2")),
        QueuedMessage::prompt(MessageSource::UserInput, make_msg("prompt")),
    ]);
    assert!(inbox.queue().has_wake_up());
}

#[test]
fn test_inbox_handle_batch_empty_no_op() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();

    handle.push_batch(vec![]);
    assert!(inbox.queue().is_empty());
}

#[test]
fn test_inbox_handle_clone_independence() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle1 = inbox.handle();
    let handle2 = inbox.handle();

    handle1.push_prompt(MessageSource::UserInput, make_msg("from h1"));
    handle2.push_defer(MessageSource::CronTrigger, make_msg("from h2"));

    // Both handles write to the same underlying queue
    assert_eq!(inbox.queue().len(), 2);
}

#[tokio::test]
async fn test_await_wake_returns_immediately_when_pending() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();

    // Push before await — should return immediately
    handle.push_prompt(MessageSource::UserInput, make_msg("already here"));

    // Should not hang
    tokio::time::timeout(Duration::from_millis(100), inbox.await_wake())
        .await
        .expect("await_wake should return immediately when pending");
}

#[tokio::test]
async fn test_await_wake_blocks_until_prompt() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();

    let inbox_clone = inbox; // move into async block

    let handle_async = handle.clone();
    let h = tokio::spawn(async move {
        // Wait a bit then push
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle_async.push_prompt(MessageSource::UserInput, make_msg("wake me"));
    });

    // await_wake should block until the push
    tokio::time::timeout(Duration::from_secs(1), inbox_clone.await_wake())
        .await
        .expect("await_wake should return after push");

    h.await.unwrap();
}

#[tokio::test]
async fn test_await_wake_ignores_info_only() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();

    let inbox_clone = inbox;
    let handle_async = handle.clone();

    let h = tokio::spawn(async move {
        // Push Info (should NOT wake)
        handle_async.push_info(MessageSource::SystemInjected, make_msg("info"));
        // Wait then push Prompt (should wake)
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle_async.push_prompt(MessageSource::UserInput, make_msg("now wake"));
    });

    // await_wake should NOT return on Info, only on Prompt
    tokio::time::timeout(Duration::from_secs(1), inbox_clone.await_wake())
        .await
        .expect("await_wake should return after Prompt, not Info");

    h.await.unwrap();
}

#[tokio::test]
async fn test_await_wake_non_destructive() {
    // await_wake should NOT consume messages
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();

    handle.push_prompt(MessageSource::UserInput, make_msg("preserve me"));
    inbox.await_wake().await;

    // Message should still be in the queue
    assert_eq!(inbox.queue().len(), 1, "await_wake should not drain");
}
