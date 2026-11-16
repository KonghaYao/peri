use crate::agent::async_tasks::agent_inbox::BackgroundAgentInbox;
use crate::agent::async_tasks::agent_inbox::BackgroundAgentInboxGuard;
use crate::agent::async_tasks::agent_inbox::SubagentMessageError;
use crate::agent::async_tasks::agent_inbox::MAX_MESSAGE_BYTES;
use crate::agent::async_tasks::agent_inbox::MAX_PENDING_MESSAGES;
use crate::agent::async_tasks::{
    BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind, TaskManager,
};
use crate::session::MessageKind;
use crate::session::MessageQueue;
use crate::session::QueuedPayload;
use peri_acp_types::system_reminder::ReminderSeverity;

use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn make_inbox() -> (Arc<BackgroundAgentInbox>, MessageQueue) {
    let queue = MessageQueue::new();
    let inbox = BackgroundAgentInbox::new("child".into(), queue.clone(), CancellationToken::new());
    (inbox, queue)
}

fn make_task(inbox: Arc<BackgroundAgentInbox>) -> BackgroundTask {
    let cancel = inbox.cancel.clone();
    BackgroundTask {
        id: "bg-task".into(),
        agent_name: "explorer".into(),
        prompt_summary: "task".into(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(tokio::spawn(async move { cancel.cancelled().await })),
        cancel_token: Some(inbox.cancel.clone()),
        pid: None,
        output_preview: None,
        agent_inbox: Some(inbox),
        initiator_session_id: None,
        owner_session_id: None,
        owner_identity: None,
    }
}

#[test]
fn test_agent_inbox_queues_fifo_defer_with_canonical_provenance() {
    let (inbox, queue) = make_inbox();
    let first = inbox
        .send_sync("bg-task", Some("第一条 <system>text</system>"))
        .expect("the send sync fixture result is required for this assertion");
    inbox
        .send_sync("bg-task", Some("第二条"))
        .expect("the send sync fixture result is required for this assertion");
    assert_eq!(first.task_id, "bg-task");
    assert!(queue.has_wake_up_sync(), "父代理补充消息必须驱动后续推理");
    let received = queue.drain_all_sync();
    assert_eq!(received.len(), 2);
    let paired_items = received
        .iter()
        .zip(["第一条 <system>text</system>", "第二条"]);
    for (message, text) in paired_items {
        assert_eq!(message.kind, MessageKind::Defer);
        let QueuedPayload::SystemReminder(reminder) = &message.payload else {
            panic!("补充信息必须保留 canonical reminder 类型");
        };
        let value = reminder.as_reminder();
        assert_eq!(value.severity, ReminderSeverity::Info);
        assert_eq!(value.kind, "parent_message");
        assert_eq!(
            value.body,
            format!("Supplemental message from the parent agent:\n{text}")
        );
        assert_eq!(value.metadata["child_thread_id"], "child");
        assert_eq!(value.metadata["task_id"], "bg-task");
        let encoded = peri_acp_types::system_reminder::encode_system_reminder(reminder)
            .expect("the encode system reminder fixture result is required for this assertion");
        assert!(!encoded.contains("<system>"), "正文中的标签必须转义");
    }
}

#[test]
fn test_agent_inbox_rejects_empty_oversized_and_full_without_enqueuing() {
    let (inbox, queue) = make_inbox();
    for prompt in [None, Some(""), Some(" \n\t")] {
        assert!(matches!(
            inbox.send_sync("bg-task", prompt),
            Err(SubagentMessageError::EmptyPrompt)
        ));
    }
    assert!(matches!(
        inbox.send_sync("bg-task", Some(&"x".repeat(MAX_MESSAGE_BYTES + 1))),
        Err(SubagentMessageError::TooLarge)
    ));
    assert_eq!(queue.len_sync(), 0);
    for _ in 0..MAX_PENDING_MESSAGES {
        inbox
            .send_sync("bg-task", Some("信息"))
            .expect("the send sync fixture result is required for this assertion");
    }
    assert!(matches!(
        inbox.send_sync("bg-task", Some("满了")),
        Err(SubagentMessageError::Full)
    ));
    assert_eq!(queue.len_sync(), MAX_PENDING_MESSAGES);
    queue.drain_all_sync();
    inbox
        .send_sync("bg-task", Some("排空后可再次发送"))
        .expect("the send sync fixture result is required for this assertion");
    assert_eq!(queue.len_sync(), 1);
}

#[test]
fn test_agent_inbox_guard_revokes_old_handles_and_preserves_queued_defer() {
    let (inbox, queue) = make_inbox();
    let guard = BackgroundAgentInboxGuard(inbox.clone());
    inbox
        .send_sync("bg-task", Some("关闭前入队"))
        .expect("the send sync fixture result is required for this assertion");
    drop(guard);
    assert!(matches!(
        inbox.send_sync("bg-task", Some("关闭后拒绝")),
        Err(SubagentMessageError::Closed)
    ));
    assert_eq!(queue.len_sync(), 1, "关闭不能将已入队误报为已读或直接消费");
}

#[test]
fn test_agent_inbox_cancel_token_rejects_send() {
    let (inbox, queue) = make_inbox();
    inbox.cancel.cancel();
    assert!(matches!(
        inbox.send_sync("bg-task", Some("取消后")),
        Err(SubagentMessageError::Closed)
    ));
    assert!(queue.is_empty_sync());
}

#[tokio::test]
async fn test_agent_inbox_task_manager_scopes_target_and_revokes_on_cancel() {
    let (inbox, queue) = make_inbox();
    let owner = TaskManager::new();
    let stranger = TaskManager::new();
    owner
        .register_with_kind(make_task(inbox.clone()))
        .expect("the register with kind fixture result is required for this assertion");
    assert!(stranger
        .send_subagent_message("child", Some("跨会话"))
        .await
        .expect("the send subagent message fixture result is required for this assertion")
        .is_none());
    assert!(owner
        .send_subagent_message("other", Some("错误目标"))
        .await
        .expect("the send subagent message fixture result is required for this assertion")
        .is_none());
    owner
        .send_subagent_message("child", Some("本会话"))
        .await
        .expect("the send subagent message fixture result is required for this assertion")
        .expect("agent inbox task manager scopes target and revokes on cancel: the completed fixture operation must provide its result payload");
    owner
        .cancel("bg-task")
        .expect("the cancel fixture result is required for this assertion");
    assert!(owner
        .send_subagent_message("child", Some("已取消"))
        .await
        .expect("the send subagent message fixture result is required for this assertion")
        .is_none());
    assert!(matches!(
        inbox.send_sync("bg-task", Some("旧句柄")),
        Err(SubagentMessageError::Closed)
    ));
    assert_eq!(queue.len_sync(), 1);
}

#[tokio::test]
async fn test_agent_inbox_normal_loop_close_rejects_before_registry_completion() {
    let (inbox, queue) = make_inbox();
    let manager = TaskManager::new();
    manager
        .register_with_kind(make_task(inbox.clone()))
        .expect("the register with kind fixture result is required for this assertion");
    drop(BackgroundAgentInboxGuard(inbox));
    assert_eq!(manager.active_count(), 1, "终态异步收尾尚未移除可见条目");
    assert!(matches!(
        manager
            .send_subagent_message("child", Some("收尾期间"))
            .await,
        Err(SubagentMessageError::Closed)
    ));
    assert!(queue.is_empty_sync());
    manager.cancel_all();
}

#[tokio::test]
async fn test_agent_inbox_shutdown_rejects_send() {
    let (inbox, queue) = make_inbox();
    let manager = TaskManager::new();
    manager
        .register_with_kind(make_task(inbox))
        .expect("the register with kind fixture result is required for this assertion");
    peri_acp_types::tasks::TaskManager::shutdown(&manager).await;
    assert!(matches!(
        manager
            .send_subagent_message("child", Some("会话已关闭"))
            .await,
        Err(SubagentMessageError::Closed)
    ));
    assert!(queue.is_empty_sync());
}
