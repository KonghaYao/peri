use super::*;
use crate::middleware::chain::MiddlewareChain;
use crate::session::subagent::{
    SessionFactory, SubagentChainAssembler, SubagentChainContext, SubagentResumeConfig,
    SubagentRunMode,
};
use crate::session::test_resources::mock::model::{fixture_source, text_events};

pub(super) struct HandoffGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
    fail: bool,
    compensations: AtomicUsize,
}

pub(super) struct GatedClaim {
    pub(super) inner: Box<dyn ChildResumeClaim>,
    pub(super) gate: Arc<HandoffGate>,
    pub(super) handed_off: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl ChildResumeClaim for GatedClaim {
    async fn mark_running(&self) -> SessionResourceResult<()> {
        self.inner.mark_running().await
    }
    async fn hand_off_to_background(&self) -> SessionResourceResult<()> {
        self.handed_off.store(true, Ordering::SeqCst);
        self.gate.entered.notify_one();
        self.gate.release.acquire().await.unwrap().forget();
        if self.gate.fail {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Unavailable {
                    detail: "handoff offline".into(),
                },
            ));
        }
        self.inner.hand_off_to_background().await
    }
    async fn mark_failed(&self) -> SessionResourceResult<()> {
        if self.handed_off.swap(false, Ordering::SeqCst) {
            self.gate.compensations.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.mark_failed().await
    }
    async fn mark_terminated(&self) -> SessionResourceResult<()> {
        if self.handed_off.load(Ordering::SeqCst) {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::InvalidInput {
                    detail: "child resume claim was handed off to background execution".into(),
                },
            ));
        }
        self.inner.mark_terminated().await
    }
}

struct EmptyChain;
impl SubagentChainAssembler for EmptyChain {
    fn assemble(&self, _: &SubagentChainContext) -> MiddlewareChain {
        MiddlewareChain::new()
    }
}

#[derive(Clone)]
struct QuickModel;
impl QuickModel {
    async fn respond(
        &self,
        _: peri_model::ModelRequest,
        _: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        text_events("quick completion")
    }
}
crate::fixture_model_impl!(QuickModel);

fn config(
    store: Arc<ClaimResources>,
    manager: Arc<crate::agent::async_tasks::TaskManager>,
) -> SubagentResumeConfig {
    SubagentResumeConfig {
        thread_id: CHILD.into(),
        prompt: Some("new run".into()),
        agent_name: None,
        run_mode: SubagentRunMode::Background,
        max_iterations: 2,
        llm: fixture_source(Arc::new(QuickModel), "fixture"),
        chain_assembler: Arc::new(EmptyChain),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: store,
        event_handler: None,
        bg_event_sender: None,
        task_manager: Some(manager),
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: None,
        parent_tool_call_id: None,
        cancel_token: None,
        cwd: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    }
}

async fn fixture(
    fail: bool,
) -> (
    Arc<ClaimResources>,
    Arc<HandoffGate>,
    Arc<crate::agent::async_tasks::TaskManager>,
) {
    let gate = Arc::new(HandoffGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
        fail,
        compensations: AtomicUsize::new(0),
    });
    let mut store = ClaimResources::new();
    Arc::get_mut(&mut store).unwrap().handoff_gate = Some(gate.clone());
    let mut meta = ThreadMeta::new_at("/tmp/work", peri_time::now_wall());
    meta.id = CHILD.into();
    meta.agent_status = AgentStatus::Done;
    store.inner.create_resumable_thread(meta).await.unwrap();
    (
        store,
        gate,
        Arc::new(crate::agent::async_tasks::TaskManager::new()),
    )
}

async fn idle(manager: &crate::agent::async_tasks::TaskManager) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !manager.is_execution_idle() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn observe_runtime(config: &mut SubagentResumeConfig) -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let registrations = Arc::new(AtomicUsize::new(0));
    let deregistrations = Arc::new(AtomicUsize::new(0));
    let registered = registrations.clone();
    config.register_runtime = Some(Arc::new(move |_, _, _| {
        registered.fetch_add(1, Ordering::SeqCst);
    }));
    let deregistered = deregistrations.clone();
    config.deregister_runtime = Some(Arc::new(move |_| {
        deregistered.fetch_add(1, Ordering::SeqCst);
    }));
    (registrations, deregistrations)
}

#[tokio::test]
async fn background_handoff_blocks_execution_until_active_write_finishes() {
    let (store, gate, manager) = fixture(false).await;
    let starts = Arc::new(AtomicUsize::new(0));
    let mut config = config(store.clone(), manager.clone());
    let (registrations, deregistrations) = observe_runtime(&mut config);
    let observed = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _, _| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    let resume = tokio::spawn(async move { SessionFactory::resume_subagent(None, config).await });
    gate.entered.notified().await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(registrations.load(Ordering::SeqCst), 0);
    assert_eq!(deregistrations.load(Ordering::SeqCst), 0);
    gate.release.add_permits(1);
    resume.await.unwrap().unwrap();
    idle(&manager).await;
    assert_eq!(registrations.load(Ordering::SeqCst), 1);
    assert_eq!(deregistrations.load(Ordering::SeqCst), 1);
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Done
    );
}

#[tokio::test]
async fn background_handoff_caller_drop_rolls_back_without_execution() {
    let (store, gate, manager) = fixture(false).await;
    let starts = Arc::new(AtomicUsize::new(0));
    let mut config = config(store.clone(), manager.clone());
    let (registrations, deregistrations) = observe_runtime(&mut config);
    let observed = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _, _| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    let resume = tokio::spawn(async move { SessionFactory::resume_subagent(None, config).await });
    gate.entered.notified().await;
    resume.abort();
    assert!(resume.await.err().unwrap().is_cancelled());
    gate.release.add_permits(1);
    idle(&manager).await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .agent_status
            != AgentStatus::Done
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(manager.active_count(), 0);
    assert_eq!(gate.compensations.load(Ordering::SeqCst), 1);
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(registrations.load(Ordering::SeqCst), 0);
    assert_eq!(deregistrations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn background_handoff_failure_rolls_back_without_execution() {
    let (store, gate, manager) = fixture(true).await;
    gate.release.add_permits(1);
    let starts = Arc::new(AtomicUsize::new(0));
    let mut config = config(store.clone(), manager.clone());
    let (registrations, deregistrations) = observe_runtime(&mut config);
    let observed = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _, _| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    let error = SessionFactory::resume_subagent(None, config)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("handoff offline"));
    idle(&manager).await;
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Done
    );
    assert_eq!(manager.active_count(), 0);
    assert_eq!(gate.compensations.load(Ordering::SeqCst), 1);
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(registrations.load(Ordering::SeqCst), 0);
    assert_eq!(deregistrations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn background_panic_persists_error_and_preserves_diagnostic() {
    let (store, gate, manager) = fixture(false).await;
    gate.release.add_permits(1);
    let result = Arc::new(Mutex::new(None));
    let captured = result.clone();
    let mut config = config(store.clone(), manager.clone());
    config.on_subagent_start = Some(Arc::new(|_, _, _| panic!("start-hook diagnostic anchor")));
    config.on_bg_complete = Some(Arc::new(move |terminal, _| {
        *captured.lock().unwrap() = Some(terminal.clone());
        Ok(())
    }));
    SessionFactory::resume_subagent(None, config).await.unwrap();
    idle(&manager).await;
    let result = result.lock().unwrap().clone().unwrap();
    assert!(!result.success);
    assert!(result.output.contains("start-hook diagnostic anchor"));
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Error
    );
}

#[tokio::test]
async fn background_terminal_store_failure_cannot_report_success() {
    let (mut store, gate, manager) = fixture(false).await;
    Arc::get_mut(&mut store).unwrap().terminal_error = true;
    gate.release.add_permits(1);
    let result = Arc::new(Mutex::new(None));
    let captured = result.clone();
    let mut config = config(store.clone(), manager.clone());
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    config.bg_event_sender = Some(event_tx);
    config.on_bg_complete = Some(Arc::new(move |terminal, _| {
        *captured.lock().unwrap() = Some(terminal.clone());
        Ok(())
    }));
    SessionFactory::resume_subagent(None, config).await.unwrap();
    idle(&manager).await;
    let result = result.lock().unwrap().clone().unwrap();
    assert!(!result.success);
    assert!(result.output.contains("terminal store offline"));
    let mut events = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        events.push(event);
    }
    assert!(matches!(
        events.last(),
        Some(crate::agent::events::ExecutorEvent::SubagentStopped { is_error: true, .. })
    ));
    assert!(!events.iter().any(|event| matches!(
        event,
        crate::agent::events::ExecutorEvent::BackgroundTaskCompleted(_)
    )));
}

#[tokio::test]
async fn background_stop_panic_retains_active_until_terminal_and_persists_error() {
    let (store, gate, manager) = fixture(false).await;
    gate.release.add_permits(1);
    let result = Arc::new(Mutex::new(None));
    let captured = result.clone();
    let mut config = config(store.clone(), manager.clone());
    let observed = store.clone();
    config.on_subagent_stop = Some(Arc::new(move |_, _, _, _, _| {
        assert_eq!(observed.inner.statuses().last().unwrap().1, "active");
        panic!("stop-hook diagnostic anchor");
    }));
    config.on_bg_complete = Some(Arc::new(move |terminal, _| {
        *captured.lock().unwrap() = Some(terminal.clone());
        Ok(())
    }));
    SessionFactory::resume_subagent(None, config).await.unwrap();
    idle(&manager).await;
    let result = result.lock().unwrap().clone().unwrap();
    assert!(!result.success);
    assert!(result.output.contains("stop-hook diagnostic anchor"));
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Error
    );
}
