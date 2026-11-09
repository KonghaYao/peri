use super::*;
use peri_acp_types::identity::AttemptId;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::{
    ExecutionBinding, MessageQueue, MqReceivePurpose, QueuedMessage, SessionInbox, TurnId,
};

fn manager() -> Arc<dyn TaskManager> {
    Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
}

/// parked 只影响 Agent binding：根会话保持注册，binding 与 queue 不丢失，
/// resume 重新注册后可消费停驻前积压的 MQ 行。
#[tokio::test]
async fn park_agent_keeps_binding_and_root_registration() {
    let mut bindings = SessionBindings::default();
    let root_queue = Arc::new(MessageQueue::new());
    bindings.register_initial_inbox("root", SessionInbox::new(root_queue).handle());
    let child_queue = Arc::new(MessageQueue::new());
    let child_inbox = SessionInbox::new(Arc::clone(&child_queue)).handle();
    bindings.bind_agent(
        "child",
        child_inbox.clone(),
        child_queue.as_ref().clone(),
        manager(),
    );
    assert_eq!(bindings.registered_owners().len(), 2);

    bindings.park_agent("child");
    assert!(bindings.inbox("child").is_none(), "parked child 不再广播");
    assert_eq!(bindings.registered_owners(), vec!["root".to_string()]);
    assert!(
        bindings.binding("child").is_some(),
        "parked 必须保留 owner 复用所需的 binding/queue/manager"
    );

    // 根会话与非 Agent binding 不受 park 影响；重复 park 幂等。
    bindings.park_agent("root");
    bindings.park_agent("child");
    assert!(
        bindings.inbox("root").is_some(),
        "Initial binding 不参与 Agent park"
    );
    assert!(bindings.inbox("child").is_none());

    // resume 装配等价于重新 bind_agent；停驻期间积压的行仍可正常 Receive。
    let backlog = QueuedMessage::prompt(
        peri_acp_types::session::MessageSource::McpNotification,
        BaseMessage::human("parked backlog"),
    );
    child_queue.push_async(backlog.clone()).await;
    assert_eq!(child_queue.len().await.unwrap(), 1);

    bindings.bind_agent(
        "child",
        child_inbox,
        child_queue.as_ref().clone(),
        manager(),
    );
    assert!(bindings.inbox("child").is_some(), "resume 重新注册 inbox");

    let execution = ExecutionBinding {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let owner = child_queue.owner();
    let permit = owner.begin_receive(MqReceivePurpose::ModelLoop, execution);
    let items = owner.select_for_receive(&permit).await.unwrap();
    let outcome = owner.receive(permit, items).await.unwrap();
    assert_eq!(outcome.items.len(), 1);
    assert!(child_queue.len().await.unwrap() == 0, "Receive 删除 MQ 行");
}
