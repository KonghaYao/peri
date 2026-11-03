//! 终态投递：内存去重只挡重复投递，不能替存储回答「是否已经落库」。

use std::sync::Arc;

use peri_acp_types::session::MessageSource;
use peri_acp_types::tasks::BgTaskKind;

use super::*;
use crate::agent::async_tasks::delivery::terminal_delivery_id;
use crate::session::async_router::background_result_reminder;
use crate::session::test_resources::TestSession;

fn result(task_id: &str) -> crate::agent::events::BackgroundTaskResult {
    crate::agent::events::BackgroundTaskResult {
        task_id: task_id.into(),
        agent_name: "fixture".into(),
        prompt_summary: "task".into(),
        success: true,
        output: "done".into(),
        tool_calls_count: 0,
        duration_ms: 0,
        child_thread_id: Some("child".into()),
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

/// [回归] 首次落库失败 → 重试命中 mailbox 去重：结果必须真的进入 MQ，且只有一条。
///
/// 失败用只读门面注入（真实 `SessionResources`，不是假替身）；重试复用同一 mailbox，
/// 因此走的正是「Duplicate」分支。
#[tokio::test]
async fn retry_after_failed_enqueue_still_lands_the_result_once() {
    let session = TestSession::open().await;
    let mailbox = Arc::new(
        crate::session::async_result_mailbox::AsyncResultMailbox::new("session", "thread"),
    );
    let reminder = background_result_reminder(&result("task-retry"), BgTaskKind::Agent);
    let delivery_id = terminal_delivery_id("task-retry", "terminal");

    let failing = SessionTerminalDelivery::for_queue_with_mailbox(
        MessageQueue::bind(session.read_only_resources().await, session.thread_id()),
        Arc::clone(&mailbox),
    );
    let error = failing
        .deliver(delivery_id, &reminder, MessageSource::SubAgentComplete)
        .await
        .expect_err("只读门面必须如实拒绝入队");
    assert!(!error.is_empty());

    let writable =
        SessionTerminalDelivery::for_queue_with_mailbox(session.queue(), Arc::clone(&mailbox));
    writable
        .deliver(delivery_id, &reminder, MessageSource::SubAgentComplete)
        .await
        .expect("重试必须以存储事实为准完成落库");

    let entries = session.queue().snapshot().await.unwrap();
    assert_eq!(entries.len(), 1, "结果最终必须落库且只落一条");
    assert_eq!(entries[0].delivery_id, Some(delivery_id));

    // 再次投递仍是幂等的：存储已有同一身份，不新增第二行。
    writable
        .deliver(delivery_id, &reminder, MessageSource::SubAgentComplete)
        .await
        .unwrap();
    assert_eq!(session.queue().snapshot().await.unwrap().len(), 1);
}
