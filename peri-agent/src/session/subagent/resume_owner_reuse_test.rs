use super::*;

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
