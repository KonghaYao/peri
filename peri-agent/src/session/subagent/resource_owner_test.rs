use super::*;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::system_reminder::{ReminderCategory, ReminderDelivery, ReminderSeverity};

fn owner(parent: &str, child: &str) -> Arc<ParkedChildOwner> {
    ParkedChildOwner::new(
        child.into(),
        parent.into(),
        Arc::new(TaskManager::new()),
        MessageQueue::new(),
        Some(crate::session::test_resources::mock::MockSessionResources::new()),
        None,
    )
}

fn execution() -> ExecutionBinding {
    ExecutionBinding {
        turn_id: Default::default(),
        attempt_id: Default::default(),
    }
}

#[tokio::test]
async fn test_owner_retains_resources_without_computation() {
    let root = Arc::new(TaskManager::new());
    let child = owner("root", "child");
    let weak = Arc::downgrade(&child);
    root.register_child_owner(child.clone()).unwrap();
    let permit = child.acquire(execution()).unwrap();
    assert!(child.check_resume().unwrap_err().contains("active=true"));
    permit.finish(Ok(()));
    drop(child);
    let retained = root.child_owner("child").unwrap();
    assert!(weak.upgrade().is_some(), "资源不能随计算释放");
    assert!(
        root.child_owner("unrelated").is_none(),
        "不能越过可信子树寻址"
    );
    retained.check_resume().unwrap();
    retained.close().await.unwrap();
    assert!(retained
        .acquire(execution())
        .err()
        .unwrap()
        .contains("closing"));
}

#[tokio::test]
async fn test_abandoned_execution_blocks_resume_without_cancelling_resources() {
    let child = owner("root", "child");
    let permit = child.acquire(execution()).unwrap();
    drop(permit);
    assert!(child
        .failure()
        .unwrap()
        .contains("without terminal settlement"));
    assert!(child
        .acquire(execution())
        .err()
        .unwrap()
        .contains("unconfirmed"));
    assert!(
        !child.manager.session_close_settled(),
        "异常退出不得隐式关闭任务 scope"
    );
}

#[tokio::test]
async fn test_nested_owner_lookup_and_closing_host_rejection() {
    let root = Arc::new(TaskManager::new());
    let child = owner("root", "child");
    let nested = owner("child", "nested");
    child.manager.register_child_owner(nested.clone()).unwrap();
    root.register_child_owner(child).unwrap();
    assert!(Arc::ptr_eq(&root.child_owner("nested").unwrap(), &nested));
    root.begin_session_close();
    assert!(root
        .register_child_owner(owner("root", "new"))
        .unwrap_err()
        .contains("closing"));
}

#[tokio::test]
async fn test_handoff_commit_reuses_frozen_execution_snapshot() {
    let resources = crate::session::test_resources::mock::MockSessionResources::new();
    let session = crate::session::Session::new(
        Arc::from("/tmp"),
        crate::session::FrozenContext::builder().build(),
        Some("child".into()),
    );
    *session.transcript().write() = crate::session::MessageTranscript::new()
        .with_persistence(resources.clone(), "child".into());
    let owner = ParkedChildOwner::new(
        "child".into(),
        "root".into(),
        Arc::new(TaskManager::new()),
        session.queue().clone(),
        Some(resources.clone()),
        None,
    );
    let context = crate::agent::stages::StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .build();
    let permit = owner
        .acquire(context.session.turn.execution_binding())
        .unwrap();
    let first = super::super::commit_child_handoff(
        &context.session,
        &owner,
        super::super::ChildHandoffReason::SettlementDeadline,
        vec![],
    )
    .await
    .unwrap();
    let second = super::super::commit_child_handoff(
        &context.session,
        &owner,
        super::super::ChildHandoffReason::SettlementDeadline,
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(first.canonical_message_id, second.canonical_message_id);
    assert_eq!(resources.payloads().len(), 1, "重复提交不得产生第二条交接");
    permit.finish(Ok(()));
}

#[tokio::test]
async fn test_close_settles_terminal_and_records_retained_input() {
    let resources = crate::session::test_resources::mock::MockSessionResources::new();
    let child = ParkedChildOwner::new(
        "child".into(),
        "root".into(),
        Arc::new(TaskManager::new()),
        MessageQueue::new(),
        Some(resources.clone()),
        None,
    );
    let terminal_id = peri_acp_types::messages::MessageId::new();
    let terminal = crate::session::producer_reminders::trusted_reminder(
        ReminderCategory::Task,
        "subagent_task_lifecycle",
        "terminal_result",
        ReminderSeverity::Info,
        ReminderDelivery::Required,
        "terminal result".into(),
        None,
        serde_json::json!({}),
    );
    child.queue.push(
        crate::session::QueuedMessage::system_reminder_with_delivery_id(
            crate::session::MessageKind::Defer,
            crate::session::MessageSource::SystemInjected,
            terminal,
            terminal_id,
        ),
    );
    child.queue.push(crate::session::QueuedMessage::prompt(
        crate::session::MessageSource::UserInput,
        crate::messages::BaseMessage::human("追加输入"),
    ));
    child.close().await.unwrap();
    assert!(!child.queue.has_required(), "已结算输入不得残留 Required");
    let payloads = resources.payloads();
    assert_eq!(payloads.len(), 2, "终态按原 ID 落库，未处理输入单独记录");
    assert!(payloads.iter().any(|payload| {
        matches!(payload, PersistedPayload::SystemReminder { reminder, .. }
            if reminder.as_reminder().kind == "terminal_result")
    }));
    assert!(payloads.iter().any(|payload| {
        matches!(payload, PersistedPayload::SystemReminder { reminder, .. }
            if reminder.as_reminder().kind == "child_retained_input_unprocessed")
    }));
}

#[tokio::test]
async fn test_close_without_canonical_port_refuses_to_drop_required_input() {
    let child = ParkedChildOwner::new(
        "child".into(),
        "root".into(),
        Arc::new(TaskManager::new()),
        MessageQueue::new(),
        None,
        None,
    );
    child.queue.push(crate::session::QueuedMessage::prompt(
        crate::session::MessageSource::UserInput,
        crate::messages::BaseMessage::human("追加输入"),
    ));
    assert!(child.close().await.unwrap_err().contains("canonical"));
    assert!(child.queue.has_required(), "缺 canonical 能力时不得清空");
}

/// root 可在真实三层 owner 树中查询、取消、关闭任意后代资源。
#[tokio::test]
async fn test_root_queries_cancels_and_closes_nested_owner_tree() {
    let root = Arc::new(TaskManager::new());
    let b_manager = Arc::new(TaskManager::new());
    let c_manager = Arc::new(TaskManager::new());
    let b = ParkedChildOwner::new(
        "b".into(),
        "root".into(),
        b_manager.clone(),
        MessageQueue::new(),
        Some(crate::session::test_resources::mock::MockSessionResources::new()),
        None,
    );
    let c = ParkedChildOwner::new(
        "c".into(),
        "b".into(),
        c_manager.clone(),
        MessageQueue::new(),
        Some(crate::session::test_resources::mock::MockSessionResources::new()),
        None,
    );
    b_manager.register_child_owner(c.clone()).unwrap();
    root.register_child_owner(b.clone()).unwrap();
    c_manager
        .register(peri_acp_types::tasks::BgTaskRegistration {
            task_id: "c-shell".into(),
            kind: peri_acp_types::tasks::BgTaskKind::Shell,
            summary: "nested shell".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    assert!(Arc::ptr_eq(&root.child_owner("c").unwrap(), &c));
    let c_from_root = root.child_task_manager("c").unwrap();
    assert_eq!(c_from_root.active_count(), 1);
    c_from_root.cancel_async("c-shell").await.unwrap();
    assert_eq!(c_manager.active_count(), 0);
    // 模拟执行环境确认 kill 已生效；真实 ShellExecutor 由完成回调确认。
    c_manager.registry().confirm_external_stopped("c-shell");
    root.close_child_resources().await.unwrap();
    assert!(root.child_owner("b").is_none());
    assert!(root.child_owner("c").is_none());
}

use crate::messages::BaseMessage;
use crate::session::{FrozenContext, MessageTranscript, Session};

#[tokio::test]
async fn test_unconfirmed_failure_and_pending_receipt_count_as_obligations() {
    let child = owner("root", "child");
    let permit = child.acquire(execution()).unwrap();
    permit.finish(Err("handoff persistence failed".into()));
    assert!(child.has_unsettled_obligations());
    assert!(child.check_resume().is_err());
    assert!(child
        .acquire(execution())
        .err()
        .unwrap()
        .contains("unconfirmed"));

    let child = owner("root", "child");
    let binding = execution();
    let permit = child.acquire(binding.clone()).unwrap();
    let receipt = super::super::ChildHandoffReceipt {
        child_thread_id: "child".into(),
        execution: binding,
        owner_session_id: "child".into(),
        reason: super::super::ChildHandoffReason::SettlementDeadline,
        tasks: Vec::new(),
        message_ids: Vec::new(),
        message_sequences: Vec::new(),
        canonical_message_id: peri_acp_types::messages::MessageId::new(),
    };
    child.freeze_receipt(receipt).unwrap();
    permit.finish(Ok(()));
    assert!(child.has_unsettled_obligations());
    let error = child.acquire(execution()).err().unwrap();
    assert!(
        error.contains("unconfirmed") || error.contains("pending"),
        "{error}"
    );
    assert!(child.close().await.unwrap_err().contains("unconfirmed"));
}

#[tokio::test]
async fn test_close_with_unconfirmed_history_flush_keeps_owner() {
    let manager = Arc::new(TaskManager::new());
    let parent = Arc::new(TaskManager::new());
    let session = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("child".into()),
    );
    let store = crate::session::test_resources::mock::MockSessionResources::new();
    *session.transcript().write() =
        MessageTranscript::new().with_persistence(store.clone(), "child".into());
    let owner = ParkedChildOwner::new(
        "child".into(),
        "root".into(),
        manager.clone(),
        session.queue().clone(),
        Some(store.clone()),
        None,
    );
    owner.set_writer(session.transcript().read().persist_tx_handle());
    parent.register_child_owner(owner.clone()).unwrap();
    store.restrict_to_history_read_only();
    session
        .transcript()
        .write()
        .append(BaseMessage::human("unflushed history"));
    let error = owner.close().await.unwrap_err();
    assert!(
        error.contains("persistence unconfirmed"),
        "真实 barrier 失败必须 Incomplete: {error}"
    );
    assert!(owner.failure().is_some());
    assert!(
        parent.child_owner("child").is_some(),
        "失败时不得释放 owner/binding"
    );
}

#[tokio::test]
async fn test_close_rejects_active_execution_until_it_ends() {
    let child = owner("root", "child");
    let permit = child.acquire(execution()).unwrap();
    assert!(child.close().await.unwrap_err().contains("active"));
    permit.finish(Ok(()));
    child.close().await.unwrap();
}

#[test]
fn test_missing_handoff_capability_is_not_classified_as_persistence_failure() {
    use crate::agent::async_tasks::handoff::HandoffCommitError;
    use crate::session::subagent::{ChildHandoffFailure, SubagentFailure};

    let missing = ChildHandoffFailure {
        error: HandoffCommitError::MissingCapability,
        receipt: None,
    };
    let failure = SubagentFailure::handoff_failed("child", "agent", None, &missing);
    assert_eq!(
        failure.error().category_name(),
        "child_handoff_capability_unavailable"
    );

    let persistence = ChildHandoffFailure {
        error: HandoffCommitError::Persistence(anyhow::anyhow!("append failed")),
        receipt: None,
    };
    let failure = SubagentFailure::handoff_failed("child", "agent", None, &persistence);
    assert_eq!(
        failure.error().category_name(),
        "child_handoff_persistence_failed"
    );
}
