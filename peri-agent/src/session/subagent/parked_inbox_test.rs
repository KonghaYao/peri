use super::*;
use crate::session::queue::{MessageSource, QueuedMessage};
use crate::session::subagent::SubagentHost;
use crate::session::MessageQueue;
use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport};
use peri_acp_types::session::MqFilter;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::tasks::{
    BgTaskKind, BgTaskRegistration, OnBgCompleteFn, TaskManager as TaskManagerPort,
};
use std::any::Any;
use std::time::Duration;

/// 记录 bind/park 事件；park 后从注册集合移除，模拟真实 broadcast 收件人。
#[derive(Default)]
struct RecordingPool {
    events: parking_lot::Mutex<Vec<(&'static str, String)>>,
    registered: parking_lot::Mutex<std::collections::HashSet<String>>,
}

impl RecordingPool {
    fn is_registered(&self, session: &str) -> bool {
        self.registered.lock().contains(session)
    }

    fn count(&self, event: &str, session: &str) -> usize {
        self.events
            .lock()
            .iter()
            .filter(|(kind, id)| *kind == event && id == session)
            .count()
    }
}

#[async_trait::async_trait]
impl McpPoolPort for RecordingPool {
    fn bind_agent_session(
        &self,
        session_id: &str,
        _inbox: peri_acp_types::session::InboxHandle,
        _queue: MessageQueue,
        _manager: Arc<dyn TaskManagerPort>,
    ) {
        self.events.lock().push(("bind", session_id.to_owned()));
        self.registered.lock().insert(session_id.to_owned());
    }

    fn park_agent_session(&self, session_id: &str) {
        self.events.lock().push(("park", session_id.to_owned()));
        self.registered.lock().remove(session_id);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn shutdown(&self) -> McpPoolShutdownReport {
        panic!("subagent parking must not shutdown the shared pool")
    }

    fn snapshot(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

fn parent_session(
    store: &Arc<MockSessionResources>,
    manager: &Arc<TaskManager>,
    pool: &Arc<RecordingPool>,
) -> Arc<Session> {
    store.register_bound_session("parent-thread", "/tmp/work");
    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(manager.clone()),
        session_resources: Some(store.clone()),
        mcp_pool: Some(pool.clone() as Arc<dyn McpPoolPort>),
        ..Default::default()
    });
    parent
}

fn spawn_config(
    store: &Arc<MockSessionResources>,
    manager: &Arc<TaskManager>,
    run_mode: SubagentRunMode,
    on_bg_complete: Option<OnBgCompleteFn>,
    deregister: Option<crate::session::factory::DeregisterRuntimeFn>,
) -> SubagentSpawnConfig {
    SubagentSpawnConfig {
        agent_name: "parked-agent".to_string(),
        prompt: "finish".to_string(),
        parent_messages: Vec::new(),
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 50,
        fork_directive_kind: None,
        run_mode,
        skill_names: Vec::new(),
        llm: crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(store.clone()),
        event_handler: None,
        bg_event_sender: None,
        task_manager: Some(manager.clone()),
        on_bg_complete,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: deregister,
        parent_agent_id: None,
        parent_tool_call_id: None,
        cancel_token: None,
        cwd: None,
        parent_thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    }
}

/// 正常结束的 sync child 停驻：注销广播收件人但保留 owner/binding；resume
/// 重新注册并消费停驻期间积压的 MQ 行（写历史 + 删行）。
#[tokio::test]
async fn sync_terminal_parks_then_resume_consumes_parked_backlog() {
    let store = MockSessionResources::new();
    let manager = Arc::new(TaskManager::new());
    let pool = Arc::new(RecordingPool::default());
    let parent = parent_session(&store, &manager, &pool);

    let spawned = SessionFactory::spawn_subagent(
        Some(&parent),
        spawn_config(&store, &manager, SubagentRunMode::Sync, None, None),
    )
    .await
    .expect("spawn must complete");
    let child_id = spawned.child_thread_id;
    assert!(!pool.is_registered(&child_id), "终态停驻后不再接收广播");
    assert_eq!(pool.count("bind", &child_id), 1);
    assert_eq!(pool.count("park", &child_id), 1);

    let owner = manager
        .child_owner(&child_id)
        .expect("parked owner must survive inbox unregistration");
    assert!(owner.check_resume().is_ok(), "parked owner 仍可 resume");

    let backlog = QueuedMessage::info(
        MessageSource::McpNotification,
        BaseMessage::human("parked backlog"),
    );
    let backlog_id = backlog.id();
    owner.queue.push_sync(backlog);
    assert_eq!(owner.queue.len_sync(), 1);

    let resumed = SessionFactory::resume_subagent(
        Some(&parent),
        resume_config(Arc::clone(&store), child_id.clone()),
    )
    .await
    .expect("parked owner must resume");
    assert_eq!(resumed.child_thread_id, child_id);
    assert!(
        owner.check_resume().is_ok(),
        "resume 完成后的新停驻仍保留 owner"
    );
    assert_eq!(owner.queue.len_sync(), 0, "resume 消费停驻积压");
    assert!(
        store
            .mq_history_contains(&ThreadId::from(child_id.as_str()), backlog_id)
            .await
            .unwrap(),
        "Receive 将积压行写入历史"
    );
    assert!(
        store
            .mq_query(&ThreadId::from(child_id.as_str()), MqFilter::default())
            .await
            .unwrap()
            .is_empty(),
        "Receive 删除 MQ 行"
    );
    assert_eq!(pool.count("bind", &child_id), 2, "resume 重新注册 inbox");
    assert_eq!(pool.count("park", &child_id), 2, "resume 结束再次停驻");
    assert!(!pool.is_registered(&child_id));
}

/// background 正常结束同样停驻：注销 inbox，owner 保留。
#[tokio::test]
async fn background_terminal_parks_inbox_and_keeps_owner() {
    let store = MockSessionResources::new();
    let manager = Arc::new(TaskManager::new());
    let pool = Arc::new(RecordingPool::default());
    let parent = parent_session(&store, &manager, &pool);

    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let finished_tx = parking_lot::Mutex::new(Some(finished_tx));
    let deregister: crate::session::factory::DeregisterRuntimeFn = Arc::new(move |_| {
        if let Some(sender) = finished_tx.lock().take() {
            let _ = sender.send(());
        }
    });
    let complete: OnBgCompleteFn = Arc::new(|_, _| Ok(()));
    let spawned = SessionFactory::spawn_subagent(
        Some(&parent),
        spawn_config(
            &store,
            &manager,
            SubagentRunMode::Background,
            Some(complete),
            Some(deregister),
        ),
    )
    .await
    .expect("background spawn must start");
    let child_id = spawned.child_thread_id;
    tokio::time::timeout(Duration::from_secs(10), finished_rx)
        .await
        .expect("background child must finish")
        .unwrap();

    assert!(!pool.is_registered(&child_id), "终态停驻后不再接收广播");
    assert_eq!(pool.count("bind", &child_id), 1);
    assert_eq!(pool.count("park", &child_id), 1);
    assert!(
        manager.child_owner(&child_id).is_some(),
        "parked owner 必须保留"
    );
}

/// HandedOff 仍可能由交接 wrapper 消费，期间必须保持 inbox 注册。
#[tokio::test(start_paused = true)]
async fn handed_off_resume_keeps_inbox_registered() {
    let store = MockSessionResources::new();
    let manager = Arc::new(TaskManager::new());
    let pool = Arc::new(RecordingPool::default());
    let parent = parent_session(&store, &manager, &pool);

    let spawned = SessionFactory::spawn_subagent(
        Some(&parent),
        spawn_config(&store, &manager, SubagentRunMode::Sync, None, None),
    )
    .await
    .expect("first run must complete");
    let child_id = spawned.child_thread_id;
    assert_eq!(pool.count("park", &child_id), 1);

    // 永久 shell 任务让本轮无法 Settled，进入 HandedOff；任务目录属于 parked owner。
    let owner = manager
        .child_owner(&child_id)
        .expect("parked owner must exist after terminal");
    owner
        .manager
        .register(BgTaskRegistration {
            task_id: "parked-shell".into(),
            kind: BgTaskKind::Shell,
            summary: "sleep 300".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    let resumed = SessionFactory::resume_subagent(
        Some(&parent),
        resume_config(Arc::clone(&store), child_id.clone()),
    )
    .await
    .expect("handoff resume must return pending");
    assert_eq!(resumed.child_thread_id, child_id);
    assert!(
        !resumed.pending_tasks.is_empty() || resumed.handoff.is_some(),
        "永久 shell 应产生 HandedOff"
    );
    assert!(
        pool.is_registered(&child_id),
        "HandedOff 阶段不得注销 inbox"
    );
    assert_eq!(
        pool.count("park", &child_id),
        1,
        "只有首次正常停驻注销，HandedOff 不注销"
    );
    assert_eq!(pool.count("bind", &child_id), 2, "resume 已重新注册");
}
