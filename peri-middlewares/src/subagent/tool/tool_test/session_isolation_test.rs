use super::*;
use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::tasks::TaskManager as _;
use peri_agent::agent::async_tasks::TaskManager;
use peri_agent::session::subagent::{
    SubagentCancelPolicy, SubagentChainAssembler, SubagentHost, SubagentRunMode,
};
use peri_agent::session::{FrozenContext, Session};
use peri_agent::tools::ToolContext;

/// 记录子链收到的工具目录的假模型（H1 起子链消费 `peri_model::Model`，
/// 生产装配点自建 bridge）。
#[derive(Clone)]
struct ObservedToolsLlm(Arc<RwLock<Vec<String>>>);

impl ObservedToolsLlm {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        use crate::subagent::test_support::*;
        let _ = &cancellation;
        let defined = defined_tools(&request);
        let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

        *self.0.write() = tools.iter().map(|tool| tool.name().to_owned()).collect();
        text_events("nested-complete")
    }
}
crate::subagent::test_support::fixture_model_impl!(ObservedToolsLlm);

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
        Arc::new(move |_| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(ObservedToolsLlm(captured_tools.clone())),
                "fixture-scripted",
            )
        }),
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
        crate::subagent::test_support::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
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
    let mut nested_ctx = ToolContext::new(&[], &cwd);
    let nested_tool_call = format!("fixture-nested-tool-call:{}", uuid::Uuid::now_v7());
    nested_ctx.invocation_id = Some(nested_tool_call.clone());
    nested_ctx.tool_call_id = Some(nested_tool_call);
    let output = bound[0]
        .invoke(
            serde_json::json!({"prompt": "nested", "fork": true, "run_in_background": true}),
            nested_ctx,
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
    // 单一继承策略（与工具描述同一契约）：fork 子链不继承 `Agent`（防递归）
    // 与其它子链无持有者能力；工作区/元工具以外的父工具仍按策略过滤。
    assert!(
        !observed_tools.read().iter().any(|name| name == "Agent"),
        "fork 子链不得继承 Agent 工具: {:?}",
        observed_tools.read()
    );
    assert!(!observed_tools.read().iter().any(|name| name == "Probe"));

    drop(bound);
    drop(tool);
    drop(parent);
    let replacement_parent = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        Some(root_id.clone()),
    );
    replacement_parent.set_subagent_host(SubagentHost {
        session_resources: Some(store.facade()),
        task_manager: Some(Arc::new(TaskManager::new())),
        mcp_pool: Some(pool.clone()),
        ..Default::default()
    });
    let tool = make_subagent_tool(Vec::new()).with_parent_session(replacement_parent);
    // resume 携带本次工具调用身份（父侧 provenance 来源）。
    let resume_invocation = "fixture-isolation-nested-resume";

    let config = tool.resume_config_base(
        child.child_thread_id.clone(),
        Some("resume".into()),
        SubagentRunMode::Sync,
        20,
        crate::subagent::test_support::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        // 恢复必须重建保存的工具上限（spawn 时 ceiling = [Agent]）。
        vec![Arc::new(tool.clone())],
        Arc::new(|_| true),
        store.facade(),
        cwd,
        Some(resume_invocation.to_string()),
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
    pool.bind_agent_session(
        &child_id,
        SessionInbox::new(old_queue.clone()).handle(),
        old_manager.clone(),
    );
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
        crate::subagent::test_support::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
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
    config.on_subagent_stop = Some(Arc::new(move |_, _, _, _, _| {
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
        BaseMessage::human("closed runtime pending"),
    ));
    let config = tool.resume_config_base(
        child_id.clone(),
        Some("new conversation turn".into()),
        SubagentRunMode::Sync,
        20,
        crate::subagent::test_support::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
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
    assert!(pool.verify_shared_environment_close(&root_id).is_ok());
    assert!(old_manager.spawn_owned(Box::pin(async {})).is_err());
}

async fn spawn_completed_child(
    tool: &SubAgentTool,
    cwd: &str,
    prompt: &str,
) -> peri_agent::session::subagent::SubagentSpawned {
    let config = tool.spawn_config_base(
        "fork".into(),
        prompt.into(),
        Vec::new(),
        SubagentCancelPolicy::Cascade,
        20,
        None,
        SubagentRunMode::Sync,
        crate::subagent::test_support::fixture_source(Arc::new(EchoLLM), "fixture-scripted"),
        Vec::new(),
        Arc::new(|_| true),
        None,
        Vec::new(),
        cwd.into(),
        None,
    );
    tool.spawn(config).await.unwrap()
}

async fn wait_for_background_idle(manager: &TaskManager) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !manager.is_execution_idle() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn background_grandchild_resumes_route_to_direct_parent_with_unique_delivery() {
    use peri_agent::agent::events::ExecutorEvent;
    let dir = tempdir().unwrap();
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let root_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .unwrap();
    let root = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        Some(root_id.clone()),
    );
    root.set_subagent_host(SubagentHost {
        session_resources: Some(store.facade()),
        task_manager: Some(Arc::new(TaskManager::new())),
        ..Default::default()
    });
    let root_tool = make_subagent_tool(Vec::new()).with_parent_session(root.clone());
    let child = spawn_completed_child(&root_tool, &cwd, "child").await;
    let child_tool = make_subagent_tool(Vec::new()).with_parent_session(child.session.clone());
    let grandchild = spawn_completed_child(&child_tool, &cwd, "grandchild").await;
    assert_eq!(
        store
            .load_meta(&grandchild.child_thread_id)
            .await
            .unwrap()
            .parent_thread_id,
        Some(child.child_thread_id.clone())
    );
    let manager = child
        .session
        .subagent_host()
        .unwrap()
        .task_manager
        .clone()
        .unwrap();
    let mut task_ids = std::collections::HashSet::new();
    let mut delivery_ids = std::collections::HashSet::new();
    for index in 0..2 {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = child_tool.resume_config_base(
            grandchild.child_thread_id.clone(),
            Some(format!("explicit resume {index}")),
            SubagentRunMode::Background,
            20,
            crate::subagent::test_support::fixture_source(Arc::new(EchoLLM), "fixture-scripted"),
            Vec::new(),
            Arc::new(|_| true),
            store.facade(),
            cwd.clone(),
            Some(format!("resume-call-{index}")),
        );
        config.bg_event_sender = Some(event_tx);
        let resumed = child_tool.resume(config).await.unwrap();
        wait_for_background_idle(&manager).await;
        assert!(task_ids.insert(resumed.task_id.clone().unwrap()));
        assert_eq!(resumed.child_thread_id, grandchild.child_thread_id);
        assert!(root.queue().is_empty());
        assert!(resumed.session.queue().is_empty());
        let delivered = child.session.queue().drain_all();
        assert_eq!(delivered.len(), 1);
        assert!(delivery_ids.insert(delivered[0].delivery_id.unwrap()));
        let mut terminal = None;
        while let Ok(event) = event_rx.try_recv() {
            if let ExecutorEvent::BackgroundTaskCompleted(result) = event {
                terminal = Some(result);
            }
        }
        let result = terminal.unwrap();
        let router =
            peri_agent::session::async_router::AsyncRouter::for_queue(child.session.queue());
        let delivery: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |result, kind| {
            router.route_bg_result(result, kind);
            Ok(())
        });
        assert!(!manager
            .settle_completed(&result.task_id.clone(), result, delivery)
            .unwrap());
        assert!(child.session.queue().is_empty());
        assert!(root.queue().is_empty());
        assert_eq!(
            store
                .load_meta(&grandchild.child_thread_id)
                .await
                .unwrap()
                .agent_status,
            peri_agent::thread::AgentStatus::Done
        );
    }
}

#[tokio::test]
async fn configured_background_callback_overrides_live_parent_and_retries_original_target() {
    use std::sync::atomic::{AtomicBool, Ordering};
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
        Some(root_id),
    );
    let manager = Arc::new(TaskManager::new());
    parent.set_subagent_host(SubagentHost {
        session_resources: Some(store.facade()),
        task_manager: Some(manager.clone()),
        ..Default::default()
    });
    let tool = make_subagent_tool(Vec::new()).with_parent_session(parent.clone());
    let child = spawn_completed_child(&tool, &cwd, "child").await;
    let target = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        None,
    );
    let allowed = Arc::new(AtomicBool::new(false));
    let attempted = Arc::new(tokio::sync::Notify::new());
    let result = Arc::new(std::sync::Mutex::new(None));
    let configured = peri_agent::session::bg_complete::task_bg_complete_callback(
        peri_agent::session::bg_complete::queue_terminal_delivery(target.queue().clone()),
    );
    let delivery: peri_acp_types::tasks::OnBgCompleteFn = {
        let allowed = allowed.clone();
        let attempted = attempted.clone();
        let result = result.clone();
        Arc::new(move |terminal, kind| {
            *result.lock().unwrap() = Some(terminal.clone());
            attempted.notify_one();
            if !allowed.load(Ordering::SeqCst) {
                return Err("configured target unavailable".into());
            }
            configured(terminal, kind)
        })
    };
    let mut config = tool.resume_config_base(
        child.child_thread_id.clone(),
        Some("background resume".into()),
        SubagentRunMode::Background,
        20,
        crate::subagent::test_support::fixture_source(Arc::new(EchoLLM), "fixture-scripted"),
        Vec::new(),
        Arc::new(|_| true),
        store.facade(),
        cwd,
        None,
    );
    config.on_bg_complete = Some(delivery.clone());
    tool.resume(config).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), attempted.notified())
        .await
        .unwrap();
    assert!(parent.queue().is_empty());
    assert!(target.queue().is_empty());
    assert_eq!(manager.active_count(), 1);
    allowed.store(true, Ordering::SeqCst);
    manager.retry_pending_deliveries();
    wait_for_background_idle(&manager).await;
    assert!(parent.queue().is_empty());
    let delivered = target.queue().drain_all();
    assert_eq!(delivered.len(), 1);
    assert!(delivered[0].delivery_id.is_some());
    let result = result.lock().unwrap().clone().unwrap();
    delivery(&result, peri_acp_types::tasks::BgTaskKind::Agent).unwrap();
    assert!(target.queue().is_empty());
    assert!(parent.queue().is_empty());
}
