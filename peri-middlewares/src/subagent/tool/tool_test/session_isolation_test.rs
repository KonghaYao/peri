use super::*;
use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::tasks::TaskManager as _;
use peri_agent::agent::async_tasks::TaskManager;
use peri_agent::session::subagent::{
    SubagentCancelPolicy, SubagentChainAssembler, SubagentHost, SubagentRunMode,
};
use peri_agent::session::{FrozenContext, Session};
use peri_agent::tools::ToolContext;

struct ObservedToolsLlm(Arc<RwLock<Vec<String>>>);

#[async_trait::async_trait]
impl ReactLLM for ObservedToolsLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        *self.0.write() = tools.iter().map(|tool| tool.name().to_owned()).collect();
        Ok(Reasoning::with_answer("", "nested-complete"))
    }
}

#[tokio::test]
async fn nested_delegation_uses_direct_parent_catalog_and_inbox() {
    let dir = tempdir().unwrap();
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let root_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .unwrap();
    let parent = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        Some(root_id.clone()),
    );
    let parent_manager = Arc::new(TaskManager::new());
    let pool = Arc::new(crate::mcp::McpClientPool::new_pending());
    parent.set_subagent_host(SubagentHost {
        session_resources: Some(store.facade()),
        task_manager: Some(parent_manager.clone()),
        mcp_pool: Some(pool.clone()),
        ..Default::default()
    });
    let observed_tools = Arc::new(RwLock::new(Vec::new()));
    let captured_tools = observed_tools.clone();
    let tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(move |_| Box::new(ObservedToolsLlm(captured_tools.clone()))),
        cwd.clone(),
    )
    .with_parent_session(parent.clone());
    let config = tool.spawn_config_base(
        "fork".into(),
        "outer".into(),
        Vec::new(),
        SubagentCancelPolicy::Cascade,
        20,
        None,
        SubagentRunMode::Sync,
        Box::new(EchoLLM),
        vec![Arc::new(tool.clone())],
        Arc::new(|_| true),
        None,
        Vec::new(),
        cwd.clone(),
        None,
    );
    let child = tool.spawn(config).await.unwrap();
    let child_manager = child
        .session
        .subagent_host()
        .unwrap()
        .task_manager
        .clone()
        .unwrap();
    assert!(!Arc::ptr_eq(&parent_manager, &child_manager));
    assert!(pool
        .session_bindings
        .read()
        .inbox(&child.child_thread_id)
        .is_some());
    let bound = super::super::session_binding::SessionBoundAssembler::new(&tool).bind_tools(
        &child.session,
        vec![Arc::new(tool.clone()), make_tool("Probe")],
        Arc::new(|tool| tool.name() != "Probe"),
    );
    let output = bound[0]
        .invoke(
            serde_json::json!({"prompt": "nested", "fork": true, "run_in_background": true}),
            ToolContext::new(&[], &cwd),
        )
        .await
        .unwrap();
    let nested_id = output
        .split("(thread: ")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap()
        .to_string();
    assert_eq!(
        store.load_meta(&nested_id).await.unwrap().parent_thread_id,
        Some(child.child_thread_id.clone())
    );
    assert!(parent_manager.list_tasks_full().is_empty());
    assert_eq!(child_manager.list_tasks_full().len(), 1);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !child.session.queue().has_wake_up() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(parent.queue().is_empty());
    assert!(child.session.queue().drain_all()[0].delivery_id.is_some());
    assert!(observed_tools.read().iter().any(|name| name == "Agent"));
    assert!(!observed_tools.read().iter().any(|name| name == "Probe"));

    drop(bound);
    drop(tool);
    drop(parent);
    let replacement_parent = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        Some(root_id),
    );
    replacement_parent.set_subagent_host(SubagentHost {
        session_resources: Some(store.facade()),
        task_manager: Some(Arc::new(TaskManager::new())),
        mcp_pool: Some(pool.clone()),
        ..Default::default()
    });
    let tool = make_subagent_tool(Vec::new()).with_parent_session(replacement_parent);

    let config = tool.resume_config_base(
        child.child_thread_id.clone(),
        Some("resume".into()),
        SubagentRunMode::Sync,
        20,
        Box::new(EchoLLM),
        Vec::new(),
        Arc::new(|_| true),
        store.facade(),
        cwd,
        None,
    );
    let resumed = tool.resume(config).await.unwrap();
    assert!(Arc::ptr_eq(
        &child_manager,
        resumed
            .session
            .subagent_host()
            .unwrap()
            .task_manager
            .as_ref()
            .unwrap()
    ));
    child
        .session
        .queue()
        .push(peri_agent::session::QueuedMessage::defer(
            peri_agent::session::MessageSource::SystemInjected,
            BaseMessage::human("after-rebind"),
        ));
    assert_eq!(
        resumed.session.queue().drain_all()[0]
            .message()
            .unwrap()
            .content(),
        "after-rebind"
    );
    pool.unregister_inbox(&nested_id);
    pool.unregister_inbox(&child.child_thread_id);
}

#[tokio::test]
async fn agent_binding_retains_task_directory_and_routes_only_to_child() {
    use peri_acp_types::ports::McpPoolPort;
    use peri_acp_types::session::{MessageSource, SessionInbox};
    use peri_acp_types::tasks::TaskManager as TaskManagerPort;

    let pool = crate::mcp::McpClientPool::new_pending();
    let parent_queue = Arc::new(peri_agent::session::MessageQueue::new());
    let child_queue = Arc::new(peri_agent::session::MessageQueue::new());
    pool.register_inbox("parent", SessionInbox::new(parent_queue.clone()).handle());
    let manager: Arc<dyn TaskManagerPort> = Arc::new(TaskManager::new());
    let manager_weak = Arc::downgrade(&manager);
    pool.bind_agent_session(
        "child",
        SessionInbox::new(child_queue.clone()).handle(),
        manager,
    );
    assert!(manager_weak.upgrade().is_some());
    let inbox = pool.session_bindings.read().inbox("child").unwrap();
    inbox.push_info(MessageSource::SystemInjected, BaseMessage::human("passive"));
    assert!(!child_queue.has_wake_up());
    inbox.push_defer(MessageSource::SystemInjected, BaseMessage::human("result"));
    tokio::time::timeout(std::time::Duration::from_secs(1), child_queue.await_wake())
        .await
        .unwrap();
    assert!(parent_queue.is_empty());
    assert_eq!(child_queue.len(), 2);
    assert!(pool
        .begin_external_task_execution("child", "remote")
        .is_ok());
    assert!(pool
        .begin_external_task_execution("parent", "remote")
        .is_err());
    pool.unregister_inbox("child");
    assert!(manager_weak.upgrade().is_some());
    assert!(pool.session_bindings.read().inbox("child").is_none());
    assert_eq!(child_queue.len(), 2);
}

#[tokio::test]
async fn completed_child_shell_blocks_shared_close_and_reopen_rebuilds_binding() {
    use peri_acp_types::ports::McpPoolPort;
    use peri_acp_types::session::{MessageSource, SessionInbox};
    use peri_acp_types::session_resources::{
        ControlAction, ControlCommand, ControlDecision, ControlStatus,
    };
    let dir = tempdir().unwrap();
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let root_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .unwrap();
    let mut meta = ThreadMeta::new_at(cwd.clone(), peri_time::now_wall());
    meta.parent_thread_id = Some(root_id.clone());
    let child_id = store.create_thread(meta).await.unwrap();
    store.update_thread_status(&child_id, "done").await.unwrap();
    let pool = Arc::new(crate::mcp::McpClientPool::new_empty());
    pool.initialized
        .store(true, std::sync::atomic::Ordering::Release);
    let old_manager = Arc::new(peri_mcp_common::create_local_task_manager());
    let old_queue = Arc::new(peri_agent::session::MessageQueue::new());
    pool.bind_agent_session_for_lifecycle(
        &child_id,
        1,
        SessionInbox::new(old_queue.clone()).handle(),
        old_manager.clone(),
    )
    .unwrap();
    let parent = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        Some(root_id.clone()),
    );
    parent.set_subagent_host(SubagentHost {
        session_resources: Some(store.facade()),
        mcp_pool: Some(pool.clone()),
        ..Default::default()
    });
    let tool = make_subagent_tool(Vec::new()).with_parent_session(parent);
    let mut config = tool.resume_config_base(
        child_id.clone(),
        Some("complete naturally".into()),
        SubagentRunMode::Sync,
        20,
        Box::new(EchoLLM),
        Vec::new(),
        Arc::new(|_| true),
        store.facade(),
        cwd.clone(),
        None,
    );
    let shell = Arc::new(std::sync::Mutex::new(None));
    let captured_shell = shell.clone();
    let captured_manager = old_manager.clone();
    let shell_cwd = cwd.clone();
    config.on_subagent_stop = Some(Arc::new(move |_, _, _, _| {
        let handle = captured_manager
            .spawn_shell("sleep 30".into(), shell_cwd.clone(), None, None)
            .unwrap();
        *captured_shell.lock().unwrap() = Some(handle.task_id);
    }));
    let child = tool.resume(config).await.unwrap();
    assert!(!child.interrupted);
    assert_eq!(
        store.load_meta(&child_id).await.unwrap().agent_status,
        peri_agent::thread::AgentStatus::Done
    );
    assert_eq!(
        store
            .resources
            .load_session_control(&child_id)
            .await
            .unwrap()
            .status,
        ControlStatus::Active
    );
    assert!(pool
        .verify_shared_environment_close(&root_id)
        .unwrap_err()
        .contains("Incomplete"));
    assert!(!child.session.config().cancel_token.is_cancelled());
    let shell_id = shell.lock().unwrap().clone().unwrap();
    assert!(old_manager
        .list_tasks_full()
        .iter()
        .any(|task| task.task_id == shell_id
            && matches!(
                task.status,
                peri_agent::agent::async_tasks::BackgroundTaskStatus::Running
            )));
    peri_agent::session::subagent::close_subagent_session_scope(child.session.clone())
        .await
        .unwrap();
    assert!(old_manager.session_close_settled());
    old_queue.push(peri_agent::session::QueuedMessage::defer(
        MessageSource::SystemInjected,
        BaseMessage::human("old lifecycle pending"),
    ));
    let closed = store
        .resources
        .load_session_control(&child_id)
        .await
        .unwrap();
    assert_eq!(closed.status, ControlStatus::Closed);
    let receipt = store
        .resources
        .apply_session_control(&ControlCommand {
            session_id: child_id.clone(),
            command_id: "fixture-explicit-lifecycle-reopen".into(),
            expected_lifecycle: closed.lifecycle,
            expected_revision: closed.revision,
            expected_control_generation: closed.control_generation,
            action: ControlAction::Reopen,
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, ControlDecision::Accepted);
    let config = tool.resume_config_base(
        child_id.clone(),
        Some("new lifecycle".into()),
        SubagentRunMode::Sync,
        20,
        Box::new(EchoLLM),
        Vec::new(),
        Arc::new(|_| true),
        store.facade(),
        cwd,
        None,
    );
    let reopened = tool.resume(config).await.unwrap();
    let new_manager = reopened
        .session
        .subagent_host()
        .unwrap()
        .task_manager
        .clone()
        .unwrap();
    assert!(!Arc::ptr_eq(&old_manager, &new_manager));
    assert_eq!(old_queue.len(), 1);
    assert!(reopened.session.queue().is_empty());
    assert!(pool.verify_shared_environment_close(&root_id).is_err());
    assert!(old_manager.spawn_owned(Box::pin(async {})).is_err());
}
