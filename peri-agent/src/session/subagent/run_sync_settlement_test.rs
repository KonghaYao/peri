//! sync 交接后的终态结算：终态、工作意图与父 MQ 结果必须一次收敛。

use peri_acp_types::tasks::TaskManager as _;
use std::future::Future as _;

use crate::session::subagent::run_sync::SyncCompletion;
use crate::session::subagent::run_sync::SyncHandoffSignal;
use crate::session::subagent::terminal_settlement::TerminalMqRoute;
use crate::session::subagent::types::ChildHandoffReason;
use crate::session::subagent::types::ChildHandoffReceipt;
use crate::session::subagent::types::ChildRunOutcome;
use crate::session::Session;

use crate::agent::async_tasks::TaskManager;
use crate::agent::events::BackgroundTaskResult;
use crate::session::async_result_mailbox::AsyncResultMailbox;
use crate::session::async_router::AsyncRouter;
use crate::session::queue::{MessageSource, QueuedMessage};

use crate::session::subagent::factory::ResumeClaim;
use crate::session::subagent::SubagentCloseState;
use crate::session::subagent::SubagentHost;
use crate::session::test_resources::mock::MockSessionResources;
use crate::session::test_resources::TestSession;
use crate::session::FrozenContext;
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration};
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};
use std::sync::Arc;
use std::time::Duration;

struct Fixture {
    session: Arc<Session>,
    store: Arc<MockSessionResources>,
    route: TerminalMqRoute,
    parent_queue: MessageQueue,
    parent_manager: Arc<TaskManager>,
    child_id: String,
    task_id: String,
}

fn fixture() -> Fixture {
    let (session, _owner, store) =
        crate::session::subagent::run_sync::tests::bound_child_with_store(Arc::new(
            TaskManager::new(),
        ));
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    let target = uuid::Uuid::now_v7().to_string();
    store.register_bound_session(&child_id, "/tmp");
    store.register_bound_session(&target, "/tmp");
    let resources = Arc::clone(&store) as Arc<dyn SessionResources>;
    Fixture {
        route: TerminalMqRoute {
            store: Arc::clone(&resources),
            target: target.clone(),
            mailbox: None,
        },
        parent_queue: MessageQueue::bind(resources, target.clone()),
        session,
        store,
        parent_manager: Arc::new(TaskManager::new()),
        child_id,
        task_id: crate::session::subagent::run_sync::new_sync_task_id(),
    }
}

fn receipt(session: &Arc<Session>, child_id: &str) -> ChildHandoffReceipt {
    ChildHandoffReceipt {
        child_thread_id: child_id.to_string(),
        execution: session.start_turn().execution_binding(),
        owner_session_id: child_id.to_string(),
        reason: ChildHandoffReason::SettlementDeadline,
        tasks: Vec::new(),
        message_ids: Vec::new(),
        message_sequences: Vec::new(),
        unprocessed_tool_calls: Vec::new(),
        canonical_message_id: peri_acp_types::messages::MessageId::new(),
    }
}

async fn wait_for_settlement(
    fixture: &Fixture,
    delivered: &Arc<parking_lot::Mutex<Option<BackgroundTaskResult>>>,
) -> BackgroundTaskResult {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let captured = delivered.lock().clone();
            if let Some(result) = captured {
                if fixture
                    .parent_queue
                    .snapshot()
                    .await
                    .expect("the snapshot fixture result is required for this assertion")
                    .len()
                    == 1
                {
                    break result;
                }
            }
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("handoff worker must settle the terminal")
}

async fn run_to_pending(
    fixture: &Fixture,
    outcome: Result<ChildRunOutcome, Box<dyn std::error::Error + Send + Sync>>,
) -> (
    SyncCompletion,
    Arc<parking_lot::Mutex<Option<BackgroundTaskResult>>>,
) {
    let delivered = Arc::new(parking_lot::Mutex::new(None));
    let observed = Arc::clone(&delivered);
    let receipt = receipt(&fixture.session, &fixture.child_id);
    let completion = crate::session::subagent::run_sync::run_sync_with_registered_handoff(
        Arc::clone(&fixture.session),
        fixture.child_id.clone(),
        "fixture".into(),
        Some(Arc::clone(&fixture.parent_manager)),
        Some(fixture.task_id.clone()),
        Some(Arc::new(move |result, _| {
            *observed.lock() = Some(result.clone());
            Ok(())
        })),
        Some(fixture.route.clone()),
        move |handoff, _| async move {
            handoff
                .sender
                .send(receipt)
                .expect("receipt receiver alive");
            outcome
        },
    )
    .await
    .expect("the run sync with registered handoff fixture result is required for this assertion");
    (completion, delivered)
}

/// 正常完成：finalize 把 Done + Idle 与唯一结果一次写定；重放不产生第二条。
#[tokio::test(start_paused = true)]
async fn handoff_completion_settles_done_idle_and_single_result() {
    let fixture = fixture();
    let (completion, delivered) = run_to_pending(&fixture, Ok(ChildRunOutcome::Settled)).await;
    assert!(matches!(completion, SyncCompletion::Pending(_)));
    let result = wait_for_settlement(&fixture, &delivered).await;

    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Done);
    assert_eq!(meta.work_state, WorkState::Idle, "不得残留 Done + working");
    assert_eq!(
        fixture
            .parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        1
    );

    // 同一 task/id 的重复结算必须是 no-op。
    fixture
        .route
        .settle(&fixture.child_id, AgentStatus::Done, &result)
        .await
        .expect("the settle fixture result is required for this assertion");
    assert_eq!(
        fixture
            .parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        1
    );
    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Done);
    assert_eq!(meta.work_state, WorkState::Idle);
}

/// 交接后真实取消：Cancelled + Stopped，结果仍进入父 MQ。
#[tokio::test(start_paused = true)]
async fn handoff_cancel_maps_to_cancelled_stopped() {
    let fixture = fixture();
    let delivered = Arc::new(parking_lot::Mutex::new(None));
    let observed = delivered.clone();
    let receipt = receipt(&fixture.session, &fixture.child_id);
    let cancel = fixture.session.config().cancel_token.clone();
    let completion = crate::session::subagent::run_sync::run_sync_with_registered_handoff(
        fixture.session.clone(),
        fixture.child_id.clone(),
        "fixture".into(),
        Some(fixture.parent_manager.clone()),
        Some(fixture.task_id.clone()),
        Some(Arc::new(move |result, _| {
            *observed.lock() = Some(result.clone());
            Ok(())
        })),
        Some(fixture.route.clone()),
        move |handoff, _| async move {
            handoff
                .sender
                .send(receipt)
                .expect("handoff sender test receiver must remain available for this delivery");
            cancel.cancelled().await;
            Ok(ChildRunOutcome::Interrupted)
        },
    )
    .await
    .expect("the run sync with registered handoff fixture result is required for this assertion");
    assert!(matches!(completion, SyncCompletion::Pending(_)));
    fixture.session.config().cancel_token.cancel();
    wait_for_settlement(&fixture, &delivered).await;

    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Cancelled);
    assert_eq!(meta.work_state, WorkState::Stopped);
    assert_eq!(
        fixture
            .parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        1
    );
}

/// 未取消的中断不是用户 Stop：按 Error + Idle 收口，不得伪报 Cancelled。
#[tokio::test(start_paused = true)]
async fn handoff_interrupted_without_cancel_maps_to_error_idle() {
    let fixture = fixture();
    let (completion, delivered) = run_to_pending(&fixture, Ok(ChildRunOutcome::Interrupted)).await;
    assert!(matches!(completion, SyncCompletion::Pending(_)));
    wait_for_settlement(&fixture, &delivered).await;

    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(
        fixture
            .parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        1
    );
}

/// 交接后失败：Error + Idle，结果仍进入父 MQ。
#[tokio::test(start_paused = true)]
async fn handoff_failure_maps_to_error_idle() {
    let fixture = fixture();
    let outcome = Ok(ChildRunOutcome::Failed {
        error: crate::error::AgentError::Other(anyhow::anyhow!("boom")),
        residual: None,
    });
    let (completion, delivered) = run_to_pending(&fixture, outcome).await;
    assert!(matches!(completion, SyncCompletion::Pending(_)));
    wait_for_settlement(&fixture, &delivered).await;

    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(
        fixture
            .parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        1
    );
}

/// 没有终态路由时不 detach：原同步收尾继续写终态，也不留下 Working。
#[tokio::test(start_paused = true)]
async fn handoff_without_route_stays_synchronous_and_never_marks_working() {
    let (session, _owner, store) =
        crate::session::subagent::run_sync::tests::bound_child_with_store(Arc::new(
            TaskManager::new(),
        ));
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    store.register_bound_session(&child_id, "/tmp");
    let manager = session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .task_manager
        .clone()
        .expect("the clone fixture result is required for this assertion");
    crate::session::subagent::run_sync::tests::shell_task(&manager, "shell-task");
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = crate::session::subagent::run_sync::tests::context(&session, "child answer");
    let parent_manager = Arc::new(TaskManager::new());
    let deliveries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&deliveries);
    let run_store = Arc::clone(&store) as Arc<dyn SessionResources>;
    let run_session = Arc::clone(&session);
    let run_child_id = child_id.clone();
    let completion = crate::session::subagent::run_sync::run_sync_with_registered_handoff(
        Arc::clone(&session),
        child_id.clone(),
        "fixture".into(),
        Some(Arc::clone(&parent_manager)),
        Some(crate::session::subagent::run_sync::new_sync_task_id()),
        Some(Arc::new(move |_, _| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })),
        None,
        move |handoff, _| async move {
            crate::session::subagent::run_sync::run_sync_subagent(
                &run_child_id,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                Some(run_store),
                None,
                None,
                None,
                None,
                None,
                built,
                run_session,
                None,
                Some(handoff),
            )
            .await
        },
    )
    .await
    .expect("the run sync with registered handoff fixture result is required for this assertion");
    let SyncCompletion::Finished(ChildRunOutcome::HandedOff { .. }) = completion else {
        panic!("no route must finish synchronously with the typed outcome");
    };
    assert_eq!(deliveries.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(parent_manager.snapshot().tasks.is_empty());

    let meta = store
        .load_session_meta(&child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(meta.work_state, WorkState::Idle, "无路由时不得写 Working");
}

/// [回归] resume + sync handoff：认领不得提前写回旧状态；child 保持 active
/// 直到 finalize 一次性写入终态与结果（远端/带 active 守卫的资源）。
#[tokio::test(start_paused = true)]
async fn resumed_handoff_finalize_writes_terminal_under_active_guard() {
    let fixture = fixture();
    fixture.store.require_active_for_finalize();
    fixture
        .store
        .update_thread_status(
            &peri_acp_types::thread::ThreadId::from(fixture.child_id.clone()),
            "done",
        )
        .await
        .expect("the update thread status fixture result is required for this assertion");
    let (_, claim) = ResumeClaim::acquire(
        Arc::clone(&fixture.store) as Arc<dyn SessionResources>,
        fixture.child_id.clone(),
        fixture.child_id.clone(),
        None,
    )
    .await
    .expect("the acquire fixture result is required for this assertion");

    let manager = fixture
        .session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .task_manager
        .clone()
        .expect("the clone fixture result is required for this assertion");
    crate::session::subagent::run_sync::tests::shell_task(&manager, "shell-task");
    fixture.session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built =
        crate::session::subagent::run_sync::tests::context(&fixture.session, "child answer");
    let delivered = Arc::new(parking_lot::Mutex::new(None));
    let observed = Arc::clone(&delivered);
    let run_session = Arc::clone(&fixture.session);
    let run_child = fixture.child_id.clone();
    let run_store = Arc::clone(&fixture.store) as Arc<dyn SessionResources>;
    let completion = crate::session::subagent::run_sync::run_sync_with_registered_handoff(
        Arc::clone(&fixture.session),
        fixture.child_id.clone(),
        "fixture".into(),
        Some(Arc::clone(&fixture.parent_manager)),
        Some(fixture.task_id.clone()),
        Some(Arc::new(move |result: &BackgroundTaskResult, _| {
            *observed.lock() = Some(result.clone());
            Ok(())
        })),
        Some(fixture.route.clone()),
        move |handoff, _| async move {
            crate::session::subagent::run_sync::run_sync_subagent(
                &run_child,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                Some(run_store),
                None,
                None,
                None,
                None,
                None,
                built,
                run_session,
                Some(claim),
                Some(handoff),
            )
            .await
        },
    )
    .await
    .expect("the run sync with registered handoff fixture result is required for this assertion");
    assert!(matches!(completion, SyncCompletion::Pending(_)));
    wait_for_settlement(&fixture, &delivered).await;

    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    // HandedOff 是「委派未完成」，按 A3 映射 Error + Idle；关键是 finalize 仍能写入。
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(
        fixture
            .parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        1
    );
}

/// [回归] finalize 注入一次失败：同 ID/正文重试成功，期间保持 Active + Working，
/// 不把失败改写成另一条结果。
#[tokio::test(start_paused = true)]
async fn handoff_finalize_failure_keeps_working_and_retries() {
    let fixture = fixture();
    fixture.store.fail_next_finalizes(1);
    let (completion, delivered) = run_to_pending(&fixture, Ok(ChildRunOutcome::Settled)).await;
    assert!(matches!(completion, SyncCompletion::Pending(_)));

    for _ in 0..10_000 {
        if fixture.store.finalize_attempts() >= 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(fixture.store.finalize_attempts() >= 1);
    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(
        meta.agent_status,
        AgentStatus::Active,
        "失败期间必须保留 Active"
    );
    assert_eq!(
        meta.work_state,
        WorkState::Working,
        "失败期间必须保留续跑意图"
    );

    wait_for_settlement(&fixture, &delivered).await;
    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Done);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert!(fixture.store.finalize_attempts() >= 2, "必须用同一身份重试");
    let ids = fixture.store.finalize_message_ids();
    assert!(ids.len() >= 2);
    assert!(
        ids.windows(2).all(|pair| pair[0] == pair[1]),
        "重试必须复用同一消息身份: {ids:?}"
    );
    assert_eq!(
        fixture
            .parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        1
    );
}

/// [回归] finalize 连续耗尽：child 保持 Active + Working，父 MQ 无结果，close 报
/// Incomplete；三次尝试必须复用同一消息身份，便于重试幂等。
#[tokio::test(start_paused = true)]
async fn handoff_finalize_retry_exhaustion_keeps_active_working_and_close_incomplete() {
    let fixture = fixture();
    fixture.store.fail_next_finalizes(3);
    let (completion, delivered) = run_to_pending(&fixture, Ok(ChildRunOutcome::Settled)).await;
    assert!(matches!(completion, SyncCompletion::Pending(_)));

    let sync_state = fixture
        .session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while sync_state.sync_delivery_errors().is_empty() {
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("finalize retry exhaustion must be recorded");

    assert_eq!(fixture.store.finalize_attempts(), 3);
    let ids = fixture.store.finalize_message_ids();
    assert_eq!(ids.len(), 3);
    assert!(
        ids.windows(2).all(|pair| pair[0] == pair[1]),
        "耗尽重试也必须复用同一消息身份: {ids:?}"
    );

    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Active, "未结算不得伪报终态");
    assert_eq!(
        meta.work_state,
        WorkState::Working,
        "未结算必须保留续跑意图"
    );
    assert_eq!(
        fixture
            .parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        0,
        "finalize 未成功时父 MQ 不得出现结果"
    );
    assert!(delivered.lock().is_none());

    let error =
        crate::session::subagent::close_subagent_session_scope(Arc::clone(&fixture.session))
            .await
            .unwrap_err();
    assert!(error.contains("Incomplete"), "{error}");
    assert!(
        error.contains("terminal settlement failed after retries"),
        "{error}"
    );
}

/// [回归] worker 准入被拒：同一轮同步收尾写终态，不制造父结果或幽灵任务。
#[tokio::test(start_paused = true)]
async fn handoff_worker_admission_rejected_settles_synchronously() {
    let fixture = fixture();
    fixture
        .parent_manager
        .register(BgTaskRegistration {
            task_id: fixture.task_id.clone(),
            kind: BgTaskKind::Agent,
            summary: "duplicate admission".into(),
            kill: None,
            pid: None,
        })
        .expect("the register fixture result is required for this assertion");
    let receipt = receipt(&fixture.session, &fixture.child_id);
    let delivered = Arc::new(parking_lot::Mutex::new(None));
    let observed = Arc::clone(&delivered);
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let call = tokio::spawn(
        crate::session::subagent::run_sync::run_sync_with_registered_handoff(
            Arc::clone(&fixture.session),
            fixture.child_id.clone(),
            "fixture".into(),
            Some(Arc::clone(&fixture.parent_manager)),
            Some(fixture.task_id.clone()),
            Some(Arc::new(move |result: &BackgroundTaskResult, _| {
                *observed.lock() = Some(result.clone());
                Ok(())
            })),
            Some(fixture.route.clone()),
            move |handoff, _| async move {
                handoff
                    .sender
                    .send(receipt)
                    .expect("receipt receiver alive");
                let _ = release_rx.await;
                Ok(ChildRunOutcome::Settled)
            },
        ),
    );
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    release_tx
        .send(())
        .expect("release tx send test receiver must remain available for this delivery");
    let completion = call.await.expect("the call fixture result is required for this assertion").expect("handoff worker admission rejected settles synchronously: the completed fixture operation must provide its result payload");
    assert!(matches!(
        completion,
        SyncCompletion::Finished(ChildRunOutcome::Settled)
    ));

    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Done);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert!(delivered.lock().is_none(), "准入被拒不得走后台结果交付");
    assert_eq!(
        fixture
            .parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        0,
        "准入被拒时结果由同步调用返回，不进入父 MQ"
    );
}

/// [回归] 父 mailbox 接纳先于结果可见：接纳冲突时 finalize 不得让结果出现。
#[tokio::test(start_paused = true)]
async fn handoff_mailbox_accept_precedes_result_visibility() {
    let mut fixture = fixture();
    let mailbox = Arc::new(AsyncResultMailbox::new(
        "parent",
        fixture.route.target.clone(),
    ));
    fixture.route.mailbox = Some(Arc::clone(&mailbox));
    let conflicting = BackgroundTaskResult {
        task_id: fixture.task_id.clone(),
        agent_name: "fixture".into(),
        prompt_summary: "synchronous sub-agent handoff".into(),
        success: true,
        output: "different result".into(),
        tool_calls_count: 0,
        duration_ms: 0,
        timed_out: false,
        child_thread_id: Some(fixture.child_id.clone()),
        subagent_failure: None,
        shell_output: None,
    };
    let message =
        AsyncRouter::background_result_message(&conflicting, BgTaskKind::Agent).into_mq_input();
    let PersistedPayload::SystemReminder { id, reminder } = &message.payload else {
        panic!("background result must use a system reminder");
    };
    mailbox
        .accept(*id, reminder, message.source.clone())
        .expect("the accept fixture result is required for this assertion");

    let (completion, delivered) = run_to_pending(&fixture, Ok(ChildRunOutcome::Settled)).await;
    assert!(matches!(completion, SyncCompletion::Pending(_)));
    let sync_state = fixture
        .session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while sync_state.sync_delivery_errors().is_empty() {
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("mailbox acceptance conflict must be recorded");

    let meta = fixture
        .store
        .load_session_meta(&fixture.child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Active);
    assert_eq!(meta.work_state, WorkState::Working);
    assert_eq!(
        fixture
            .parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        0,
        "接纳未成立时结果不得在父 MQ 可见"
    );
    assert!(delivered.lock().is_none());
    assert_eq!(mailbox.pending_count(), 1);
}

/// [回归] 真实 SQLite：handoff 完成后再次进入，恢复候选为空。
#[tokio::test]
async fn completed_handoff_child_is_not_recovered_on_next_entry() {
    let fixture = TestSession::open().await;
    let root = fixture.thread_id();
    let child_id = fixture.create_child(&root).await;
    let resources = fixture.resources();
    let cwd = resources
        .load_session_meta(&child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion")
        .cwd;
    let session = Session::new_with_cancel_and_queue(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
        Arc::new(tokio_util::sync::CancellationToken::new()),
        MessageQueue::bind(Arc::clone(&resources), child_id.clone()),
    );
    let manager = Arc::new(TaskManager::new());
    session.set_subagent_host(SubagentHost {
        close_state: Arc::new(SubagentCloseState::default()),
        session_resources: Some(Arc::clone(&resources)),
        task_manager: Some(manager),
        ..Default::default()
    });
    let route = TerminalMqRoute {
        store: Arc::clone(&resources),
        target: root.clone(),
        mailbox: None,
    };
    let receipt = receipt(&session, &child_id);
    let parent_manager = Arc::new(TaskManager::new());
    let task_id = crate::session::subagent::run_sync::new_sync_task_id();
    let run_child = child_id.clone();
    let run_resources = Arc::clone(&resources);
    let completion = crate::session::subagent::run_sync::run_sync_with_registered_handoff(
        session,
        child_id.clone(),
        "fixture".into(),
        Some(parent_manager),
        Some(task_id.clone()),
        Some(Arc::new(|_, _| Ok(()))),
        Some(route),
        move |handoff, _| async move {
            run_resources
                .set_work_state(
                    &peri_acp_types::thread::ThreadId::from(run_child.clone()),
                    WorkState::Working,
                )
                .await
                .expect("the set work state fixture result is required for this assertion");
            handoff
                .sender
                .send(receipt)
                .expect("receipt receiver alive");
            Ok(ChildRunOutcome::Settled)
        },
    )
    .await
    .expect("the run sync with registered handoff fixture result is required for this assertion");
    assert!(matches!(completion, SyncCompletion::Pending(_)));

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let meta = resources
                .load_session_meta(&child_id)
                .await
                .expect("the load session meta fixture result is required for this assertion");
            if meta.agent_status == AgentStatus::Done {
                break;
            }
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("handoff worker must finalize the child");
    let meta = resources
        .load_session_meta(&child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.work_state, WorkState::Idle);

    let scope = resources
        .load_recovery_scope(&root)
        .await
        .expect("the load recovery scope fixture result is required for this assertion");
    assert!(
        scope
            .candidates
            .iter()
            .all(|candidate| candidate.thread_id != child_id),
        "completed handoff child must not be a recovery candidate"
    );
    let parent_queue = MessageQueue::bind(resources, root);
    assert_eq!(
        parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        1
    );
}
include!("run_sync_handoff_commit_order_test.rs");
