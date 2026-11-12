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
    async fn finish_with_status(&self, status: AgentStatus) -> SessionResourceResult<bool> {
        if self.handed_off.load(Ordering::SeqCst) {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::InvalidInput {
                    detail: "child resume claim was handed off to background execution".into(),
                },
            ));
        }
        self.inner.finish_with_status(status).await
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
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
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
        events.last().map(|event| event.payload()),
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

fn blocked_child(
    store: Arc<ClaimResources>,
    manager: Arc<crate::agent::async_tasks::TaskManager>,
) -> (
    Arc<crate::session::Session>,
    Arc<crate::session::subagent::ParkedChildOwner>,
    MessageId,
) {
    use crate::session::MessageSource;
    use peri_acp_types::system_reminder::{ReminderCategory, ReminderDelivery, ReminderSeverity};

    let owner = crate::session::subagent::ParkedChildOwner::new(
        CHILD.into(),
        "parent-thread".into(),
        Arc::new(crate::agent::async_tasks::TaskManager::new()),
        crate::session::MessageQueue::new(),
        Some(store),
        None,
    );
    let id = MessageId::new();
    let reminder = crate::session::producer_reminders::trusted_reminder(
        ReminderCategory::Task,
        "shell",
        "completed",
        ReminderSeverity::Info,
        ReminderDelivery::Required,
        "测试中尚未覆盖的结果".into(),
        None,
        serde_json::json!({"task_id": "blocked-task"}),
    );
    owner
        .mailbox
        .accept(id, &reminder, MessageSource::ShellComplete)
        .unwrap();
    owner
        .mailbox
        .mark_canonical_ready(id, &reminder, MessageSource::ShellComplete, None)
        .unwrap();
    owner.mailbox.block_uncovered("reason", "模型失败");
    manager.register_child_owner(owner.clone()).unwrap();
    let parent = crate::session::Session::new(
        Arc::from("/tmp"),
        crate::session::FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    parent.set_subagent_host(crate::session::subagent::SubagentHost {
        task_manager: Some(manager),
        ..Default::default()
    });
    (parent, owner, id)
}

/// [回归测试] 注册放行不能在认领接收及启动确认前恢复 mailbox。
#[tokio::test]
async fn background_registration_waits_for_handoff_confirmation_before_resume() {
    use crate::session::async_result_mailbox::AsyncResultState;
    use crate::session::subagent::background::{confirm_background_start, BackgroundStartGate};

    let (store, _handoff_gate, manager) = fixture(false).await;
    let (_parent, owner, id) = blocked_child(store, manager);
    let alive = Arc::new(parking_lot::Mutex::new(true));
    let (confirmed_tx, confirmed_rx) = tokio::sync::oneshot::channel();
    let mut gate = BackgroundStartGate {
        alive: alive.clone(),
        confirmed: confirmed_rx,
    };
    let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel();
    let mailbox = owner.mailbox.clone();
    let execution = tokio::spawn(async move {
        registered_rx.await.unwrap().unwrap();
        waiting_tx.send(()).unwrap();
        gate.wait().await.unwrap();
        mailbox.resume_scope();
    });
    let (accept_tx, accept_rx) = tokio::sync::oneshot::channel();
    let handoff = super::super::BackgroundHandoff(accept_tx);
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let caller_alive = alive.clone();
    let caller = std::thread::spawn(move || {
        confirm_background_start(&caller_alive, registered_tx, confirmed_tx, || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            handoff.accept()
        })
    });
    entered_rx.await.unwrap();
    waiting_rx.await.unwrap();
    assert_eq!(owner.mailbox.state_of(&id), Some(AsyncResultState::Blocked));
    release_tx.send(()).unwrap();
    accept_rx.await.unwrap();
    caller.join().unwrap().unwrap();
    execution.await.unwrap();
    assert_eq!(owner.mailbox.state_of(&id), Some(AsyncResultState::Ready));
    assert!(!*alive.lock(), "后台启动门退出后必须撤销存活标记");
}

/// [回归测试] 已注册但 handoff 接收失败时，后台门必须失败且 mailbox 保持 Blocked。
#[tokio::test]
async fn background_handoff_accept_failure_keeps_mailbox_blocked() {
    use crate::session::async_result_mailbox::AsyncResultState;
    use crate::session::subagent::background::{confirm_background_start, BackgroundStartGate};

    let (store, _handoff_gate, manager) = fixture(false).await;
    let (_parent, owner, id) = blocked_child(store, manager);
    let alive = Arc::new(parking_lot::Mutex::new(true));
    let (confirmed_tx, confirmed_rx) = tokio::sync::oneshot::channel();
    let mut gate = BackgroundStartGate {
        alive: alive.clone(),
        confirmed: confirmed_rx,
    };
    let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    let mailbox = owner.mailbox.clone();
    let execution = tokio::spawn(async move {
        registered_rx.await.unwrap().unwrap();
        let result = gate.wait().await;
        if result.is_ok() {
            mailbox.resume_scope();
        }
        result
    });
    let (accept_tx, accept_rx) = tokio::sync::oneshot::channel();
    drop(accept_rx);
    let handoff = super::super::BackgroundHandoff(accept_tx);
    let error = confirm_background_start(&alive, registered_tx, confirmed_tx, || handoff.accept())
        .unwrap_err();
    assert!(error.contains("hand-off worker stopped"));
    assert!(execution
        .await
        .unwrap()
        .unwrap_err()
        .contains("hand-off worker stopped"));
    assert_eq!(owner.mailbox.state_of(&id), Some(AsyncResultState::Blocked));
}

/// [回归测试] 后台接收方已退出时，不得调用 handoff.accept 消费 claim。
#[tokio::test]
async fn background_start_receiver_gone_does_not_accept_claim() {
    use crate::session::subagent::background::{confirm_background_start, BackgroundStartGate};

    let alive = Arc::new(parking_lot::Mutex::new(true));
    let (confirmed_tx, confirmed_rx) = tokio::sync::oneshot::channel();
    drop(BackgroundStartGate {
        alive: alive.clone(),
        confirmed: confirmed_rx,
    });
    let (registered_tx, registered_rx) = tokio::sync::oneshot::channel();
    let (accept_tx, accept_rx) = tokio::sync::oneshot::channel();
    let handoff = super::super::BackgroundHandoff(accept_tx);
    let error = confirm_background_start(&alive, registered_tx, confirmed_tx, || handoff.accept())
        .unwrap_err();
    assert!(error.contains("startup acceptance"));
    assert!(registered_rx.await.is_err());
    assert!(accept_rx.await.is_err(), "未启动时必须丢弃而非接收 handoff");
}

struct CloseScopeDuringAssembly(Arc<crate::agent::async_tasks::TaskManager>);

impl SubagentChainAssembler for CloseScopeDuringAssembly {
    fn assemble(&self, _: &SubagentChainContext) -> MiddlewareChain {
        self.0.begin_session_close();
        MiddlewareChain::new()
    }
}

/// [回归测试] owner acquire 后 spawn 注册失败，claim 回滚且 mailbox 不解阻塞。
#[tokio::test]
async fn background_registration_failure_after_acquire_preserves_claim_and_mailbox() {
    use crate::session::async_result_mailbox::AsyncResultState;

    let (store, gate, manager) = fixture(false).await;
    let (parent, owner, id) = blocked_child(store.clone(), manager.clone());
    let mut config = config(store.clone(), manager.clone());
    config.chain_assembler = Arc::new(CloseScopeDuringAssembly(manager.clone()));
    let error = match SessionFactory::resume_subagent(Some(&parent), config).await {
        Ok(_) => panic!("关闭中的 scope 不得注册后台执行"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("closing"), "{error}");
    assert_eq!(gate.compensations.load(Ordering::SeqCst), 0);
    assert_eq!(owner.mailbox.state_of(&id), Some(AsyncResultState::Blocked));
    assert!(owner.check_resume().is_ok(), "未启动时必须释放执行 permit");
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

/// [回归测试] owner 已 acquire、后台已注册但 handoff 写入失败时，准备回滚不能毒化 owner。
#[tokio::test]
async fn background_handoff_failure_after_acquire_keeps_blocked_mailbox() {
    use crate::session::async_result_mailbox::AsyncResultState;

    let (store, gate, manager) = fixture(true).await;
    let (parent, owner, id) = blocked_child(store.clone(), manager.clone());
    let config = config(store.clone(), manager.clone());
    let mut resume =
        tokio::spawn(async move { SessionFactory::resume_subagent(Some(&parent), config).await });
    tokio::select! {
        _ = gate.entered.notified() => {}
        result = &mut resume => match result {
            Ok(Err(error)) => panic!("进入移交阶段前失败: {error}"),
            Ok(Ok(_)) => panic!("未进入移交阶段就启动成功"),
            Err(error) => panic!("恢复任务异常退出: {error}"),
        },
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
            panic!("恢复未能进入移交阶段");
        }
    }
    assert!(owner.check_resume().unwrap_err().contains("active"));
    assert_eq!(owner.mailbox.state_of(&id), Some(AsyncResultState::Blocked));
    gate.release.add_permits(1);
    let error = match resume.await.unwrap() {
        Ok(_) => panic!("handoff 写失败必须拒绝启动"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("handoff offline"));
    idle(&manager).await;
    assert_eq!(gate.compensations.load(Ordering::SeqCst), 1);
    assert_eq!(owner.mailbox.state_of(&id), Some(AsyncResultState::Blocked));
    assert!(
        owner.check_resume().is_ok(),
        "未启动且回滚成功不得毒化 owner"
    );
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

/// [回归测试] 认领移交等待时强制取消，claim 回滚但资源未确认仍须拒绝恢复。
#[tokio::test(start_paused = true)]
async fn background_cancel_during_handoff_after_acquire_rolls_back_claim() {
    use crate::session::async_result_mailbox::AsyncResultState;

    let (store, gate, manager) = fixture(false).await;
    let (parent, owner, id) = blocked_child(store.clone(), manager.clone());
    let config = config(store.clone(), manager.clone());
    let mut resume =
        tokio::spawn(async move { SessionFactory::resume_subagent(Some(&parent), config).await });
    tokio::select! {
        _ = gate.entered.notified() => {}
        result = &mut resume => match result {
            Ok(Err(error)) => panic!("进入移交阶段前失败: {error}"),
            Ok(Ok(_)) => panic!("未进入移交阶段就启动成功"),
            Err(error) => panic!("恢复任务异常退出: {error}"),
        },
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
            panic!("恢复未能进入移交阶段");
        }
    }
    let tasks = manager.registry().list_tasks_full();
    assert_eq!(tasks.len(), 1, "后台任务必须已经注册");
    manager.cancel(&tasks[0].task_id).unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(4)).await;
    gate.release.add_permits(1);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), resume)
        .await
        .expect("取消后的恢复调用必须终结")
        .unwrap();
    let error = match result {
        Ok(_) => panic!("被取消的后台启动不能成功"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("startup acceptance"), "{error}");
    assert_eq!(gate.compensations.load(Ordering::SeqCst), 1);
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Done
    );
    assert_eq!(owner.mailbox.state_of(&id), Some(AsyncResultState::Blocked));
    assert!(
        owner.check_resume().unwrap_err().contains("unconfirmed"),
        "强制中止后的资源结算未确认，不得放行 owner"
    );
}

/// 显式 resume 前的真实持久状态：`Done + Stopped`，与旧 bug 留下的
/// `Active + Stopped` 可区分。
async fn stopped_fixture(
    fail_work_state: bool,
) -> (
    Arc<ClaimResources>,
    Arc<WorkStateGate>,
    Arc<crate::agent::async_tasks::TaskManager>,
) {
    let work_gate = Arc::new(WorkStateGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
        fail: fail_work_state,
        writes: AtomicUsize::new(0),
        pause_after_commit: std::sync::atomic::AtomicBool::new(false),
        committed: tokio::sync::Notify::new(),
        post_release: tokio::sync::Semaphore::new(0),
    });
    let mut store = ClaimResources::new();
    Arc::get_mut(&mut store).unwrap().work_state_gate = Some(work_gate.clone());
    let mut meta = ThreadMeta::new_at("/tmp/work", peri_time::now_wall());
    meta.id = CHILD.into();
    meta.agent_status = AgentStatus::Done;
    meta.work_state = peri_acp_types::thread::WorkState::Stopped;
    store.inner.create_resumable_thread(meta).await.unwrap();
    (
        store,
        work_gate,
        Arc::new(crate::agent::async_tasks::TaskManager::new()),
    )
}

/// Working 写未完成前不得报告后台启动成功；成功返回时 Working 已落库。
#[tokio::test]
async fn resume_persists_working_before_reporting_background_success() {
    let (store, work_gate, manager) = stopped_fixture(false).await;
    let mut config = config(store.clone(), manager.clone());
    // 模型挂起，避免后台 turn 在断言前收尾把 Working 改回终态。
    config.llm = fixture_source(Arc::new(HangingModel), "fixture");
    let resume = tokio::spawn(async move { SessionFactory::resume_subagent(None, config).await });
    work_gate.entered.notified().await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(!resume.is_finished(), "后台启动成功必须等待 Working 写完成");
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .work_state,
        peri_acp_types::thread::WorkState::Stopped,
        "写未放行前磁盘仍是认领前的 Stopped"
    );
    work_gate.release.add_permits(1);
    resume.await.unwrap().unwrap();
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .work_state,
        peri_acp_types::thread::WorkState::Working,
        "成功返回时 Working 已提交"
    );
    assert_eq!(
        work_gate.writes.load(Ordering::SeqCst),
        1,
        "resume 只写一次 Working，不再依赖 worker 补写"
    );
    assert_eq!(manager.active_count(), 1);
}

/// Working 写失败必须拒绝启动：返回错误、认领状态回滚、不留下可恢复的 Working。
#[tokio::test]
async fn resume_working_write_failure_rejects_background_start() {
    let (store, work_gate, manager) = stopped_fixture(true).await;
    let starts = Arc::new(AtomicUsize::new(0));
    let mut config = config(store.clone(), manager.clone());
    let observed = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _, _| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    let resume = tokio::spawn(async move { SessionFactory::resume_subagent(None, config).await });
    work_gate.entered.notified().await;
    work_gate.release.add_permits(1);
    let error = resume
        .await
        .unwrap()
        .err()
        .expect("Working 写失败不得报告后台启动成功");
    assert!(
        error.to_string().contains("resume work state write failed"),
        "{error}"
    );
    let meta = store.inner.load_session_meta(&CHILD.into()).await.unwrap();
    assert_eq!(
        meta.agent_status,
        AgentStatus::Done,
        "认领补偿必须恢复到认领前的状态"
    );
    assert_eq!(
        meta.work_state,
        peri_acp_types::thread::WorkState::Stopped,
        "失败不得留下 Working"
    );
    idle(&manager).await;
    assert_eq!(manager.active_count(), 0);
    assert_eq!(starts.load(Ordering::SeqCst), 0, "不得启动执行");
}

/// [回归] Working 已提交、调用在返回前被取消：认领 worker 必须同时恢复工作意图，
/// 不能留下 `Done + Working` 让下一次进入把已停止的 child 当成待继续。
#[tokio::test]
async fn cancel_after_working_commit_restores_work_state() {
    let (store, work_gate, manager) = stopped_fixture(false).await;
    work_gate
        .pause_after_commit
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let starts = Arc::new(AtomicUsize::new(0));
    let mut config = config(store.clone(), manager.clone());
    let observed = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _, _| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    // 后台 turn 若被错误放行会挂起，不自行收尾掩盖残留。
    config.llm = fixture_source(Arc::new(HangingModel), "fixture");
    let resume = tokio::spawn(async move { SessionFactory::resume_subagent(None, config).await });
    work_gate.entered.notified().await;
    work_gate.release.add_permits(1);
    work_gate.committed.notified().await;
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .work_state,
        peri_acp_types::thread::WorkState::Working,
        "前置条件：Working 已提交"
    );
    resume.abort();
    assert!(resume.await.err().unwrap().is_cancelled());
    idle(&manager).await;
    let settled = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let meta = store.inner.load_session_meta(&CHILD.into()).await.unwrap();
            if meta.agent_status == AgentStatus::Done
                && meta.work_state == peri_acp_types::thread::WorkState::Stopped
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if settled.is_err() {
        let meta = store.inner.load_session_meta(&CHILD.into()).await.unwrap();
        panic!("取消补偿必须恢复工作意图，实际状态为 {meta:?}");
    }
    assert_eq!(starts.load(Ordering::SeqCst), 0, "后台不得启动执行");
    assert_eq!(manager.active_count(), 0);
}

/// [回归] 旧认领的迟到补偿不得覆盖新 resume 的 `Active + Working`。
///
/// 旧补偿被延迟到新 run 已经进入 `Active + claimed + Working` 之后才结清。这个状态
/// 与旧认领写入的中间态完全相同，按记录值判断归属会把新 run 误认为自己的记录，
/// 从而回滚成 `Done + Stopped`；归属只能由进程内认领世代区分。
#[tokio::test]
async fn stale_resume_rollback_preserves_new_working_run() {
    use peri_acp_types::thread::WorkState;
    let (mut store, work_gate, manager) = stopped_fixture(false).await;
    let rollback_gate = Arc::new(WorkRollbackGate::default());
    Arc::get_mut(&mut store).unwrap().rollback_gate = Some(rollback_gate.clone());

    // 旧认领：旧 run 已工作并写入终态（状态部分已结清），工作意图回滚仍被延迟。
    let old = store
        .claim_child_resume(&CHILD.into(), &CHILD.into())
        .await
        .unwrap();
    old.hand_off_to_background().await.unwrap();
    store
        .inner
        .set_work_state(&CHILD.into(), WorkState::Working)
        .await
        .unwrap();
    store
        .update_session_meta(
            &CHILD.into(),
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let stale = store.last_claim();
    let stale_settle = tokio::spawn(async move { stale.mark_failed().await });
    rollback_gate.entered.notified().await;

    // 旧记录已是非 active：新 run 正常认领并进入模型。
    work_gate.release.add_permits(1);
    let requests = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let mut new_config = config(store.clone(), manager.clone());
    new_config.llm = fixture_source(
        Arc::new(ObservedHangingModel {
            requests: requests.clone(),
            started: started.clone(),
        }),
        "fixture",
    );
    let new_resume = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        SessionFactory::resume_subagent(None, new_config),
    )
    .await
    .unwrap()
    .expect("new resume should be accepted after the old run reached a terminal state");
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    let before = store.inner.load_session_meta(&CHILD.into()).await.unwrap();
    assert_eq!(before.agent_status, AgentStatus::Active);
    assert_eq!(before.work_state, WorkState::Working);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "new run reached its model"
    );
    assert_eq!(manager.active_count(), 1);

    // 释放迟到的旧补偿：归属校验必须看到新认领世代并放弃回滚。
    rollback_gate.release.add_permits(1);
    stale_settle.await.unwrap().unwrap();
    let after = store.inner.load_session_meta(&CHILD.into()).await.unwrap();
    assert_eq!(after.agent_status, AgentStatus::Active);
    assert_eq!(
        after.work_state,
        WorkState::Working,
        "a stale rollback must not persist Stopped while a newer accepted run is executing"
    );
    new_resume.cancel_token.cancel();
    manager.cancel_all();
    rollback_gate.release.add_permits(10);
}

/// 记录请求数并在第一次请求上挂起的模型：断言新 run 已真正进入执行。
#[derive(Clone)]
struct ObservedHangingModel {
    requests: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
}

impl ObservedHangingModel {
    async fn respond(
        &self,
        _: peri_model::ModelRequest,
        _: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        std::future::pending::<()>().await;
        Vec::new()
    }
}
crate::fixture_model_impl!(ObservedHangingModel);

/// 用于阻挡后台 turn 立即收尾：只在运行时被丢弃时结束。
#[derive(Clone)]
struct HangingModel;

impl HangingModel {
    async fn respond(
        &self,
        _: peri_model::ModelRequest,
        _: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        std::future::pending::<()>().await;
        Vec::new()
    }
}
crate::fixture_model_impl!(HangingModel);
