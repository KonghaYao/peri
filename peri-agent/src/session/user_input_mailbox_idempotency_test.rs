//! 幂等入队的已移交事实：ACP v2 稳定身份的重试应答来源。

use super::{make_dispatch, make_input};
use crate::session::user_input_mailbox::UserInputMailbox;
use crate::session::user_input_mailbox::UserInputQueueError;
use peri_acp_types::messages::MessageContent;
use peri_acp_types::session::EnqueueUserInputRequest;
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session::SessionInbox;
use peri_acp_types::session::UserInputState;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// 重来一遍的调用方（新邮箱、同存储）用同一个 input_id 入队，必须得到已移交事实。
///
/// ACP v2 的幂等键派生稳定 input_id：重连/进程重启后重试同一提交时，邮箱没有
/// 运行期记录，只有存储的 `messages` 行。`Delivered` 是唯一能让适配器立即应答
/// 而不等待永远不会到来的交付事件的结论；`Inserted`/`Existing` 保持原状态。
#[tokio::test]
async fn test_enqueue_of_handed_off_input_reports_delivered() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = Arc::new(SessionInbox::new(Arc::clone(&queue)));
    let first = UserInputMailbox::new("session".into(), inbox.clone(), Arc::new(|_| {}));
    first
        .attach_external_attempt(CancellationToken::new(), false)
        .await
        .expect("attach fixture");
    let input = make_input(&first, "幂等提交");
    let receipt = first.enqueue(&input).await.expect("enqueue fixture");
    assert_eq!(
        receipt.results[0].state,
        UserInputState::Queued,
        "首次接纳仍是待发"
    );
    first
        .dispatch(&make_dispatch(&first, &[&input]))
        .await
        .expect("dispatch fixture");
    let received = inbox.queue_for_test().drain_all_sync();
    assert_eq!(received.len(), 1, "移交写入历史");
    drop(first);

    // 新邮箱 = 重连/重启后的调用方：没有任何运行期记录。
    let restarted = UserInputMailbox::new("session".into(), inbox.clone(), Arc::new(|_| {}));
    restarted
        .attach_external_attempt(CancellationToken::new(), false)
        .await
        .expect("attach fixture");
    let retry = EnqueueUserInputRequest {
        generation: restarted.generation().into(),
        command_id: uuid::Uuid::now_v7().to_string(),
        ..input
    };
    let receipt = restarted.enqueue(&retry).await.expect("retry fixture");
    assert_eq!(
        receipt.results[0].state,
        UserInputState::Delivered,
        "已移交事实必须如实报告，不能停留在 Unknown"
    );

    // 同一 ID 的同一正文仍是重试；`Existing` 与 `Inserted` 都不改状态。
    let pending = make_input(&restarted, "另一个提交");
    let queued = restarted.enqueue(&pending).await.expect("enqueue fixture");
    assert_eq!(queued.results[0].state, UserInputState::Queued);
    let existing = EnqueueUserInputRequest {
        command_id: uuid::Uuid::now_v7().to_string(),
        ..pending.clone()
    };
    let again = restarted
        .enqueue(&existing)
        .await
        .expect("existing fixture");
    assert_eq!(
        again.results[0].state,
        UserInputState::Queued,
        "仍在 MQ 的同 ID 同正文不改变状态"
    );

    // 同一 ID 不同内容复用稳定身份是冲突，不是一次静默的去重。
    let different = EnqueueUserInputRequest {
        command_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text("改过的正文"),
        original_draft: "改过的正文".into(),
        ..retry
    };
    let error = restarted
        .enqueue(&different)
        .await
        .expect_err("同一 ID 的正文必须一致");
    assert!(
        matches!(error, UserInputQueueError::Store(ref message)
            if message.contains("a different message already uses this id")),
        "{error:?}"
    );
}
