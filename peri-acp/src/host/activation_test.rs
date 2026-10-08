use super::*;
use crate::host::{PromptLocks, SharedSessions};
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
use peri_acp_types::tasks::BgTaskRegistration;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct ActivationHarness {
    recorded: Arc<MockTransport>,
    cfg: Arc<AcpServerConfig>,
    sessions: SharedSessions,
    session_id: String,
    transport: Arc<dyn crate::transport::AcpTransport>,
    locks: PromptLocks,
    sender: Arc<mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>>,
    receiver: Option<mpsc::UnboundedReceiver<crate::session::executor::ContinuationRequest>>,
    _server: mockito::ServerGuard,
    _mock: mockito::Mock,
    _directory: tempfile::TempDir,
}

impl ActivationHarness {
    async fn new() -> Self {
        let mut server = mockito::Server::new_async().await;
        let mock = server.mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("data: {\"id\":\"test-response\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
            .create_async().await;
        let directory = tempfile::tempdir().unwrap();
        let mut provider = make_provider_config("local", "openai", "test", "gpt-test");
        provider.base_url = server.url();
        let config = make_peri_config_with_provider(provider);
        let provider = LlmProvider::from_config(&config).unwrap();
        let cfg = Arc::new(make_server_config(config, provider, &directory).await);
        let mut sessions = HashMap::new();
        let session_id =
            register_session_with_history(&mut sessions, directory.path().to_str().unwrap(), &cfg)
                .await;
        let frozen = frozen_snapshot_bytes(&cfg, &session_id).await.unwrap();
        sessions.get_mut(&session_id).unwrap().frozen =
            Some(crate::session::frozen_snapshot::decode_frozen_snapshot(&frozen).unwrap());
        cfg.session_manager
            .ensure_session(&session_id, directory.path().to_str().unwrap());
        cfg.session_manager.ensure_session_caps(&session_id);
        let (sender, receiver) = mpsc::unbounded_channel();
        let recorded = Arc::new(MockTransport::default());
        Self {
            cfg,
            sessions: Arc::new(tokio::sync::Mutex::new(sessions)),
            session_id,
            transport: recorded.clone(),
            recorded,
            locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            sender: Arc::new(sender),
            receiver: Some(receiver),
            _server: server,
            _mock: mock,
            _directory: directory,
        }
    }

    fn listen(&self) {
        crate::host::activation::ensure_listener(
            &self.session_id,
            &self.sessions,
            &self.cfg,
            &self.sender,
        )
        .unwrap();
    }

    fn complete_child(&self, task_id: &str) {
        let tasks = self
            .cfg
            .session_manager
            .get_session(&self.session_id)
            .unwrap()
            .task_manager
            .clone();
        if !tasks
            .snapshot()
            .tasks
            .iter()
            .any(|task| task.task_id == task_id)
        {
            tasks
                .register(BgTaskRegistration {
                    task_id: task_id.into(),
                    kind: BgTaskKind::Agent,
                    summary: "child".into(),
                    pid: None,
                    kill: None,
                })
                .unwrap();
        }
        let callback = peri_agent::session::bg_complete::session_bg_complete_callback(
            Arc::new(self.cfg.session_manager.clone()),
            self.session_id.clone(),
        );
        tasks
            .settle_completed(
                task_id,
                peri_acp_types::event::BackgroundTaskResult {
                    task_id: task_id.into(),
                    agent_name: "child".into(),
                    prompt_summary: "child".into(),
                    success: true,
                    output: "late child result".into(),
                    tool_calls_count: 0,
                    duration_ms: 1,
                    child_thread_id: None,
                    timed_out: false,
                    subagent_failure: None,
                    shell_output: None,
                },
                callback,
            )
            .unwrap();
        assert_eq!(tasks.active_count(), 0);
    }
}

#[path = "execution_test.rs"]
mod execution_tests;

#[tokio::test]
#[serial]
async fn late_child_result_restarts_completed_host_run_and_commits_result() {
    let mut harness = ActivationHarness::new().await;
    let mut events = harness.cfg.controller.subscribe();
    let receiver = harness.receiver.take().unwrap();
    harness
        .cfg
        .host_task_spawner
        .spawn(
            crate::host::task_scope::HostTaskOwnerKind::Host,
            crate::host::task_scope::HostTaskKind::ContinuationScheduler,
            crate::host::continuation::run_continuation_scheduler(
                receiver,
                harness.sessions.clone(),
                harness.locks.clone(),
                harness.cfg.clone(),
                harness.transport.clone(),
                Arc::downgrade(&harness.sender),
                harness.cfg.host_task_spawner.clone(),
                harness.cfg.host_task_spawner.shutdown_token(),
            ),
        )
        .unwrap();
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .task_manager
        .register(BgTaskRegistration {
            task_id: "late-child".into(),
            kind: BgTaskKind::Agent,
            summary: "pending child".into(),
            pid: None,
            kill: None,
        })
        .unwrap();
    let prompt_cfg = harness.cfg.clone();
    let prompt_sessions = harness.sessions.clone();
    let prompt_locks = harness.locks.clone();
    let prompt_transport = harness.transport.clone();
    let prompt_sender = harness.sender.clone();
    let prompt_session_id = harness.session_id.clone();
    let prompt = tokio::spawn(async move {
        crate::host::dispatch_prompt_turn(
            json!({"sessionId": prompt_session_id, "message": {"role": "user", "content": "continue"}}),
            crate::host::PromptOrigin::User, None, &prompt_sessions, &prompt_locks, &prompt_transport, &prompt_cfg, &prompt_sender,
        ).await.unwrap()
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while !harness
            .cfg
            .session_manager
            .is_idle_suspended(&harness.session_id)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("main run must enter its bounded wait while child is active");
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(121)).await;
    tokio::time::resume();
    let response = tokio::time::timeout(Duration::from_secs(10), prompt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    assert!(harness.sessions.lock().await[&harness.session_id]
        .cancel_token
        .is_none());
    harness.complete_child("late-child");
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut starts = 0;
        while starts < 2 {
            let event = events.recv().await.unwrap();
            if event.envelope.session_id == harness.session_id
                && matches!(event.event, Some(ExecutorEvent::TurnStarted { .. }))
            {
                starts += 1;
            }
        }
        loop {
            let history = harness
                .cfg
                .session_resources
                .load_session_history(&harness.session_id)
                .await
                .unwrap();
            if history.iter().any(|payload| {
                peri_acp_types::store::serialize_persisted_payload(payload)
                    .unwrap()
                    .contains("late child result")
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("late completion must autonomously start another turn and commit its reminder");
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn listener_rechecks_existing_work_and_does_not_activate_info() {
    let mut harness = ActivationHarness::new().await;
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    queue.push(QueuedMessage::new(
        MessageKind::Info,
        MessageSource::SystemInjected,
        BaseMessage::human("passive"),
    ));
    harness.listen();
    tokio::task::yield_now().await;
    assert!(harness.receiver.as_mut().unwrap().try_recv().is_err());
    harness.complete_child("already-pending");
    let request = tokio::time::timeout(
        Duration::from_secs(2),
        harness.receiver.as_mut().unwrap().recv(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(request.mq_steering);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn empty_queued_continuation_does_not_block_the_next_child_result() {
    let harness = ActivationHarness::new().await;
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    harness.complete_child("first-child");
    let request = crate::session::executor::ContinuationRequest {
        session_id: harness.session_id.clone(),
        kind: BgTaskKind::Agent,
        mq_steering: true,
    };
    let epoch = {
        let mut sessions = harness.sessions.lock().await;
        crate::host::continuation::take_continuation_for_request(
            sessions.get_mut(&harness.session_id).unwrap(),
            &request,
        )
        .unwrap()
    };
    assert_eq!(queue.drain_all().len(), 1);
    let params =
        json!({"sessionId": harness.session_id, "message": {"role": "user", "content": []}});
    let empty = crate::host::dispatch_prompt_turn(
        params.clone(),
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        Some(epoch),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert!(empty.is_null());
    harness.complete_child("next-child");
    let next_epoch = {
        let mut sessions = harness.sessions.lock().await;
        crate::host::continuation::take_continuation_for_request(
            sessions.get_mut(&harness.session_id).unwrap(),
            &request,
        )
    };
    assert_eq!(next_epoch, Some(epoch), "空跑不能永久占用下一次续跑准入");
    let response = crate::host::dispatch_prompt_turn(
        params,
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        next_epoch,
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    assert!(queue.is_empty());
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn cancellation_allows_one_child_continuation_but_never_restarts_cancelled_continuation() {
    let mut harness = ActivationHarness::new().await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    {
        let mut sessions = harness.sessions.lock().await;
        sessions.get_mut(&harness.session_id).unwrap().cancel_token =
            Some(CancellationToken::new());
        crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": harness.session_id}),
            &mut sessions,
            &harness.cfg,
        );
        sessions.get_mut(&harness.session_id).unwrap().cancel_token = None;
    }
    harness.complete_child("cancelled-parent-child");
    harness.listen();
    let request = tokio::time::timeout(
        Duration::from_secs(2),
        harness.receiver.as_mut().unwrap().recv(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!request.mq_steering);
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    queue.drain_all();
    {
        let mut sessions = harness.sessions.lock().await;
        let state = sessions.get_mut(&harness.session_id).unwrap();
        assert!(
            crate::host::continuation::take_continuation_for_request(state, &request).is_some()
        );
        state.continuation_in_flight = true;
        state.cancel_token = Some(CancellationToken::new());
        crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": harness.session_id}),
            &mut sessions,
            &harness.cfg,
        );
        let state = sessions.get_mut(&harness.session_id).unwrap();
        state.continuation_in_flight = false;
        state.cancel_token = None;
    }
    harness.complete_child("after-continuation-stop");
    tokio::task::yield_now().await;
    assert!(harness.receiver.as_mut().unwrap().try_recv().is_err());
    assert!(queue.has_pending_defer(&MessageSource::SubAgentComplete));
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn closing_session_does_not_activate_late_work() {
    let mut harness = ActivationHarness::new().await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    let sessions = harness.sessions.clone();
    let _held_session_lock = sessions.lock().await;
    harness.listen();
    tokio::task::yield_now().await;
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    queue.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human("late"),
    ));
    tokio::task::yield_now().await;
    assert!(harness.receiver.as_mut().unwrap().try_recv().is_err());
    let owner = Arc::get_mut(&mut harness.cfg)
        .unwrap()
        .host_task_owner
        .as_mut()
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), owner.shutdown())
            .await
            .unwrap(),
        crate::host::task_scope::HostShutdownReport::Complete,
    );
}

#[tokio::test]
#[serial]
async fn approved_schedule_runs_without_reenabling_suppressed_passive_results() {
    let harness = ActivationHarness::new().await;
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    queue.push(QueuedMessage::defer(
        MessageSource::CronTrigger,
        BaseMessage::human("approved schedule"),
    ));
    harness
        .sessions
        .lock()
        .await
        .get_mut(&harness.session_id)
        .unwrap()
        .continuation_mq_steering_pending = true;
    let params =
        json!({"sessionId": harness.session_id, "message": {"role": "user", "content": []}});
    let suppressed = crate::host::dispatch_prompt_turn(
        params.clone(),
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        Some(0),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert!(suppressed.is_null());
    assert!(queue.has_ensure_processing());
    let response = crate::host::dispatch_prompt_turn(
        params,
        crate::host::PromptOrigin::Scheduled,
        Some(0),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    assert!(queue.is_empty());
    assert!(!harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .is_allowed());
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
