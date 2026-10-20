use super::*;

fn owner(parent: &str, child: &str) -> Arc<ParkedChildOwner> {
    ParkedChildOwner::new(
        child.into(),
        parent.into(),
        Arc::new(TaskManager::new()),
        MessageQueue::new(),
        crate::session::test_resources::mock::MockSessionResources::new(),
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
    let root = TaskManager::new();
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
    let root = TaskManager::new();
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
async fn test_close_retains_unprocessed_required_input() {
    let child = owner("root", "child");
    child.queue.push(crate::session::QueuedMessage::prompt(
        crate::session::MessageSource::UserInput,
        crate::messages::BaseMessage::human("追加输入"),
    ));
    assert!(child
        .close()
        .await
        .unwrap_err()
        .contains("messages require canonical settlement"));
    assert!(child.queue.has_required(), "尚未结算的输入不得清空");
}
