use super::*;
use crate::agent::async_tasks::{
    BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind, TaskManager,
};
use crate::session::QueuedPayload;

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
    }
}

#[test]
fn test_agent_inbox_queues_fifo_info_with_canonical_provenance() {
    let (inbox, queue) = make_inbox();
    let first = inbox
        .send("bg-task", Some("第一条 <system>text</system>"))
        .unwrap();
    inbox.send("bg-task", Some("第二条")).unwrap();
    assert_eq!(first.task_id, "bg-task");
    assert!(!queue.has_wake_up(), "Info 不能唤醒模型");
    let received = queue.drain_all();
    assert_eq!(received.len(), 2);
    for (message, text) in received
        .iter()
        .zip(["第一条 <system>text</system>", "第二条"])
    {
        assert_eq!(message.kind, MessageKind::Info);
        let QueuedPayload::SystemReminder(reminder) = &message.payload else {
            panic!("补充信息必须保留 canonical reminder 类型");
        };
        let value = reminder.as_reminder();
        assert_eq!(value.kind, "parent_message");
        assert_eq!(
            value.body,
            format!("Supplemental message from the parent agent:\n{text}")
        );
        assert_eq!(value.metadata["child_thread_id"], "child");
        assert_eq!(value.metadata["task_id"], "bg-task");
        let encoded = peri_acp_types::system_reminder::encode_system_reminder(reminder).unwrap();
        assert!(!encoded.contains("<system>"), "正文中的标签必须转义");
    }
}

#[test]
fn test_agent_inbox_rejects_empty_oversized_and_full_without_enqueuing() {
    let (inbox, queue) = make_inbox();
    for prompt in [None, Some(""), Some(" \n\t")] {
        assert!(matches!(
            inbox.send("bg-task", prompt),
            Err(SubagentMessageError::EmptyPrompt)
        ));
    }
    assert!(matches!(
        inbox.send("bg-task", Some(&"x".repeat(MAX_MESSAGE_BYTES + 1))),
        Err(SubagentMessageError::TooLarge)
    ));
    assert_eq!(queue.len(), 0);
    for _ in 0..MAX_PENDING_MESSAGES {
        inbox.send("bg-task", Some("信息")).unwrap();
    }
    assert!(matches!(
        inbox.send("bg-task", Some("满了")),
        Err(SubagentMessageError::Full)
    ));
    assert_eq!(queue.len(), MAX_PENDING_MESSAGES);
    queue.drain_all();
    inbox.send("bg-task", Some("排空后可再次发送")).unwrap();
    assert_eq!(queue.len(), 1);
}

#[test]
fn test_agent_inbox_guard_revokes_old_handles_and_preserves_queued_info() {
    let (inbox, queue) = make_inbox();
    let guard = BackgroundAgentInboxGuard(inbox.clone());
    inbox.send("bg-task", Some("关闭前入队")).unwrap();
    drop(guard);
    assert!(matches!(
        inbox.send("bg-task", Some("关闭后拒绝")),
        Err(SubagentMessageError::Closed)
    ));
    assert_eq!(queue.len(), 1, "关闭不能将已入队误报为已读或直接消费");
}

#[test]
fn test_agent_inbox_cancel_token_rejects_send() {
    let (inbox, queue) = make_inbox();
    inbox.cancel.cancel();
    assert!(matches!(
        inbox.send("bg-task", Some("取消后")),
        Err(SubagentMessageError::Closed)
    ));
    assert!(queue.is_empty());
}

#[tokio::test]
async fn test_agent_inbox_task_manager_scopes_target_and_revokes_on_cancel() {
    let (inbox, queue) = make_inbox();
    let owner = TaskManager::new();
    let stranger = TaskManager::new();
    owner.register_with_kind(make_task(inbox.clone())).unwrap();
    assert!(stranger
        .send_subagent_message("child", Some("跨会话"))
        .unwrap()
        .is_none());
    assert!(owner
        .send_subagent_message("other", Some("错误目标"))
        .unwrap()
        .is_none());
    owner
        .send_subagent_message("child", Some("本会话"))
        .unwrap()
        .unwrap();
    owner.cancel("bg-task").unwrap();
    assert!(owner
        .send_subagent_message("child", Some("已取消"))
        .unwrap()
        .is_none());
    assert!(matches!(
        inbox.send("bg-task", Some("旧句柄")),
        Err(SubagentMessageError::Closed)
    ));
    assert_eq!(queue.len(), 1);
}

#[tokio::test]
async fn test_agent_inbox_normal_loop_close_rejects_before_registry_completion() {
    let (inbox, queue) = make_inbox();
    let manager = TaskManager::new();
    manager
        .register_with_kind(make_task(inbox.clone()))
        .unwrap();
    drop(BackgroundAgentInboxGuard(inbox));
    assert_eq!(manager.active_count(), 1, "终态异步收尾尚未移除可见条目");
    assert!(matches!(
        manager.send_subagent_message("child", Some("收尾期间")),
        Err(SubagentMessageError::Closed)
    ));
    assert!(queue.is_empty());
    manager.cancel_all();
}

#[tokio::test]
async fn test_agent_inbox_shutdown_rejects_send() {
    let (inbox, queue) = make_inbox();
    let manager = TaskManager::new();
    manager.register_with_kind(make_task(inbox)).unwrap();
    peri_acp_types::tasks::TaskManager::shutdown(&manager).await;
    assert!(matches!(
        manager.send_subagent_message("child", Some("会话已关闭")),
        Err(SubagentMessageError::Closed)
    ));
    assert!(queue.is_empty());
}
