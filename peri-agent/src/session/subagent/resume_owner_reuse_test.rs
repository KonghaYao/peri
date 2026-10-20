use super::*;

/// 旧执行的 Stop 留在 owner 队列：真实 resume 复用队列后必须抑制它，新 run 仍推理。
#[tokio::test]
async fn test_resume_suppresses_stop_intent_from_previous_execution() {
    use crate::agent::async_tasks::TaskManager;
    use crate::messages::BaseMessage;
    use crate::session::queue::{MessageSource, QueuedMessage};
    use crate::session::subagent::{ParkedChildOwner, SubagentHost};
    use crate::session::{FrozenContext, MessageQueue, Session};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct CountingAnswerLlm(Arc<AtomicUsize>);

    impl CountingAnswerLlm {
        async fn respond(
            &self,
            _request: peri_model::ModelRequest,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            crate::session::test_resources::mock::model::text_events("resumed answer")
        }
    }
    crate::fixture_model_impl!(CountingAnswerLlm);

    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = thread_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store.create_resumable_thread(meta).await.unwrap();
    store
        .update_thread_status(&thread_id, "error")
        .await
        .unwrap();

    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    let child_manager = Arc::new(TaskManager::new());
    let owner = ParkedChildOwner::new(
        thread_id.clone(),
        "parent-thread".into(),
        child_manager,
        MessageQueue::new(),
        Some(store.clone()),
        None,
    );
    let previous_execution =
        Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None)
            .start_turn()
            .execution_binding();
    let reminder = crate::session::producer_reminders::trusted_reminder(
        peri_acp_types::system_reminder::ReminderCategory::Lifecycle,
        "hook",
        "post_tool_batch_stop",
        peri_acp_types::system_reminder::ReminderSeverity::Info,
        peri_acp_types::system_reminder::ReminderDelivery::Configurable,
        "old hook stop".into(),
        None,
        serde_json::json!({}),
    );
    owner.queue.push(
        QueuedMessage::system_reminder(
            crate::session::MessageKind::Info,
            MessageSource::HookStopIntent,
            reminder,
        )
        .with_policy(peri_acp_types::session::MessagePolicy::stop_current_run(
            previous_execution,
        )),
    );
    owner.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("resume without old stop"),
    ));
    let parent_manager = Arc::new(TaskManager::new());
    parent_manager.register_child_owner(owner.clone()).unwrap();
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager),
        session_resources: Some(store.clone()),
        ..Default::default()
    });

    let calls = Arc::new(AtomicUsize::new(0));
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            Arc::new(CountingAnswerLlm(calls.clone())),
            "fixture-scripted",
        ),
        SubagentRunMode::Sync,
        None,
        None,
    );
    SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("resume must accept the handed-off owner");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "旧 Stop 不得阻止 resume 新执行推理"
    );
    assert_eq!(owner.queue.len(), 0);
    assert_eq!(
        owner.queue.suppressed_messages().len(),
        1,
        "旧 Stop 必须作为过期控制消息显式记录"
    );
}

/// 真实 resume factory/claim 复用 handoff owner 的原队列与 manager。
#[tokio::test]
async fn test_resume_factory_reuses_handoff_owner_queue_and_manager() {
    use crate::agent::async_tasks::TaskManager;
    use crate::messages::BaseMessage;
    use crate::session::queue::{MessageSource, QueuedMessage};
    use crate::session::subagent::{ParkedChildOwner, SubagentHost};
    use crate::session::{FrozenContext, MessageQueue, Session};

    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = thread_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store.create_resumable_thread(meta).await.unwrap();
    store
        .update_thread_status(&thread_id, "error")
        .await
        .unwrap();

    let parent_manager = Arc::new(TaskManager::new());
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    let child_manager = Arc::new(TaskManager::new());
    let owner = ParkedChildOwner::new(
        thread_id.clone(),
        "parent-thread".into(),
        child_manager.clone(),
        MessageQueue::new(),
        Some(store.clone()),
        None,
    );
    owner.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("late handoff context"),
    ));
    parent_manager.register_child_owner(owner.clone()).unwrap();
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager),
        session_resources: Some(store.clone()),
        ..Default::default()
    });

    let spawned = SessionFactory::resume_subagent(
        Some(&parent),
        resume_config(store.clone(), thread_id.clone()),
    )
    .await
    .expect("factory resume must accept the handed-off owner");
    assert_eq!(spawned.child_thread_id, thread_id);
    assert_eq!(owner.queue.len(), 0, "恢复必须复用并消费原 owner 队列");
    assert!(
        owner.check_resume().is_ok(),
        "恢复完成后 owner 必须释放执行准入"
    );
}
