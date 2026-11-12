use super::*;
use crate::host::{PromptLocks, SharedSessions};
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
use peri_acp_types::tasks::BgTaskRegistration;
use peri_acp_types::thread::{ThreadId, WorkState};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 最小 tracing 捕获（与 `host/diagnostics_test.rs` 同形）：断言重查 warn 可观察。
#[derive(Clone, Default)]
struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    fn warn_subscriber(&self) -> impl tracing::Subscriber + Send + Sync {
        let buffer = self.clone();
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || buffer.clone())
            .finish()
    }
}

/// 注入真实 SQLite 瞬时故障：查询期间模板表不存在。
async fn rename_mq_table(path: &std::path::Path, from: &str, to: &str) {
    use sqlx::Connection as _;

    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(path),
    )
    .await
    .unwrap();
    // 表名来自本文件常量，非外部输入。
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE {from} RENAME TO {to}"
    )))
    .execute(&mut connection)
    .await
    .unwrap();
    drop(connection);
}

struct ActivationHarness {
    recorded: Arc<MockTransport>,
    cfg: Arc<AcpServerConfig>,
    sessions: SharedSessions,
    session_id: String,
    transport: Arc<dyn crate::transport::AcpTransport>,
    locks: PromptLocks,
    sender: Arc<mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>>,
    receiver: Option<mpsc::UnboundedReceiver<crate::session::executor::ContinuationRequest>>,
    task_tickets: std::sync::Mutex<HashMap<String, peri_acp_types::tasks::TaskRegistrationTicket>>,
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
        let base_url = server.url();
        Self::assemble(&base_url, server, mock).await
    }

    /// 由测试自控模型端点的 harness：首个请求何时响应由调用方决定。
    async fn with_base_url(base_url: &str) -> Self {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .create_async()
            .await;
        Self::assemble(base_url, server, mock).await
    }

    async fn assemble(base_url: &str, server: mockito::ServerGuard, mock: mockito::Mock) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut provider = make_provider_config("local", "openai", "test", "gpt-test");
        provider.base_url = base_url.to_string();
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
            task_tickets: std::sync::Mutex::new(HashMap::new()),
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

    /// 夹具 SQLite 文件：用于注入真实的瞬时存储故障。
    fn db_path(&self) -> std::path::PathBuf {
        self._directory.path().join("threads.db")
    }

    fn complete_child(&self, task_id: &str) {
        self.complete_task(task_id, BgTaskKind::Agent, true);
    }

    fn complete_task(&self, task_id: &str, kind: BgTaskKind, success: bool) {
        let tasks = self
            .cfg
            .session_manager
            .get_session(&self.session_id)
            .unwrap()
            .task_manager
            .clone();
        let ticket = self
            .task_tickets
            .lock()
            .unwrap()
            .entry(task_id.into())
            .or_insert_with(|| {
                tasks
                    .register(BgTaskRegistration {
                        task_id: task_id.into(),
                        kind,
                        summary: "child".into(),
                        pid: None,
                        kill: None,
                    })
                    .unwrap()
            })
            .clone();
        tasks.confirm_external_execution_stopped(&ticket);
        let callback = peri_agent::session::bg_complete::session_bg_complete_callback(
            Arc::new(self.cfg.session_manager.clone()),
            self.session_id.clone(),
        );
        tasks
            .settle_completed(
                &ticket,
                peri_acp_types::event::BackgroundTaskResult {
                    task_id: task_id.into(),
                    agent_name: "child".into(),
                    prompt_summary: "child".into(),
                    success,
                    output: if success {
                        "late child result".into()
                    } else {
                        "late task error".into()
                    },
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

#[path = "error_activation_test.rs"]
mod error_activation_tests;

#[path = "budget_activation_test.rs"]
mod budget_activation_tests;

#[path = "admission_activation_test.rs"]
mod admission_activation_tests;

#[path = "stop_activation_test.rs"]
mod stop_activation_tests;

#[path = "steer_activation_test.rs"]
mod steer_activation_tests;

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
    let ticket = harness
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
    harness
        .task_tickets
        .lock()
        .unwrap()
        .insert("late-child".into(), ticket);
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
    queue
        .push_async(QueuedMessage::new(
            MessageKind::Info,
            MessageSource::SystemInjected,
            BaseMessage::human("passive"),
        ))
        .await;
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
        pending_result: false,
        recovery: false,
    };
    let epoch = {
        let mut sessions = harness.sessions.lock().await;
        crate::host::continuation::take_continuation_for_request(
            sessions.get_mut(&harness.session_id).unwrap(),
            &request,
        )
        .unwrap()
    };
    assert_eq!(queue.drain_all_async().await.len(), 1);
    let params =
        json!({"sessionId": harness.session_id, "message": {"role": "user", "content": []}});
    let empty = crate::host::dispatch_prompt_turn(
        params.clone(),
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
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
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
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
    assert!(queue.is_empty().await.expect("test MQ len"));
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
        let _ = crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": harness.session_id}),
            &mut sessions,
            &harness.cfg,
        )
        .await;
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
    queue.drain_all_async().await;
    {
        let mut sessions = harness.sessions.lock().await;
        let state = sessions.get_mut(&harness.session_id).unwrap();
        assert!(
            crate::host::continuation::take_continuation_for_request(state, &request).is_some()
        );
        state.continuation_in_flight = true;
        state.cancel_token = Some(CancellationToken::new());
        let _ = crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": harness.session_id}),
            &mut sessions,
            &harness.cfg,
        )
        .await;
        let state = sessions.get_mut(&harness.session_id).unwrap();
        state.continuation_in_flight = false;
        state.cancel_token = None;
    }
    harness.complete_child("after-continuation-stop");
    tokio::task::yield_now().await;
    assert!(harness.receiver.as_mut().unwrap().try_recv().is_err());
    assert!(queue
        .has_pending_defer(&MessageSource::SubAgentComplete)
        .await
        .expect("test MQ defer"));
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// 一次查询错误不能耗尽唯一唤醒：listener 必须重查并最终处理 Workflow 提醒。
#[tokio::test]
#[serial]
async fn activation_query_error_is_retried_until_the_reminder_is_processed() {
    use sqlx::Connection as _;

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
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::WorkflowComplete,
            BaseMessage::human("workflow finished"),
        ))
        .await;

    // 先让 listener 消费唤醒，再停在 sessions 锁上：故障发生在查询之前，且不是新唤醒。
    let sessions = harness.sessions.clone();
    let held = sessions.lock().await;
    harness.listen();
    tokio::task::yield_now().await;

    let db_path = harness.db_path();
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&db_path),
    )
    .await
    .unwrap();
    sqlx::query("ALTER TABLE thread_mq RENAME TO thread_mq_offline")
        .execute(&mut connection)
        .await
        .unwrap();
    drop(connection);
    drop(held);

    // 故障窗口内不得把错误当成「没有工作」而发出 continuation。
    tokio::time::sleep(Duration::from_millis(160)).await;
    assert!(
        harness.receiver.as_mut().unwrap().try_recv().is_err(),
        "存储不可用时不得发出 continuation"
    );

    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&db_path),
    )
    .await
    .unwrap();
    sqlx::query("ALTER TABLE thread_mq_offline RENAME TO thread_mq")
        .execute(&mut connection)
        .await
        .unwrap();
    drop(connection);

    let request = tokio::time::timeout(
        Duration::from_secs(5),
        harness.receiver.as_mut().unwrap().recv(),
    )
    .await
    .expect("listener 必须在故障恢复后重查")
    .unwrap();
    assert!(request.mq_steering, "Workflow 提醒必须作为 MQ 工作被处理");
    assert_eq!(
        queue
            .query(peri_acp_types::session::MqFilter::default())
            .await
            .unwrap()
            .len(),
        1,
        "调度只产生 continuation，提醒仍留给 turn 消费"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// dispatch 二次资格检查遇到瞬时查询错误：预算内重查恢复后同一轮处理提醒，
/// 不把已消费的唤醒当作「无工作」。
#[tokio::test]
#[serial]
async fn dispatch_query_error_is_retried_within_budget_and_reminder_is_processed() {
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
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::WorkflowComplete,
            BaseMessage::human("workflow finished"),
        ))
        .await;
    let epoch = harness
        .sessions
        .lock()
        .await
        .get(&harness.session_id)
        .unwrap()
        .continuation_epoch;

    // 停在准入闸门：此前的调度查询已经成功，故障只落在 dispatch 的二次检查上。
    let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{}:admitted",
        harness.session_id
    ));
    let logs = LogBuffer::default();
    let _subscriber = tracing::subscriber::set_default(logs.warn_subscriber());
    let params = json!({
        "sessionId": harness.session_id,
        "message": {"role": "user", "content": []},
    });
    let dispatch = crate::host::dispatch_prompt_turn(
        params,
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
        Some(epoch),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    );
    let db_path = harness.db_path();
    let logs_for_fault = logs.clone();
    let fault = async {
        gate.entered.notified().await;
        rename_mq_table(&db_path, "thread_mq", "thread_mq_offline").await;
        gate.release.notify_one();
        // 等 dispatch 至少失败一次并进入退避后再恢复存储，不依赖固定睡眠竞态。
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if logs_for_fault
                    .text()
                    .contains("continuation dispatch query failed")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dispatch 必须进入查询重查");
        rename_mq_table(&db_path, "thread_mq_offline", "thread_mq").await;
    };
    let (response, ()) = tokio::join!(dispatch, fault);
    let response = response.unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    assert!(queue.is_empty().await.expect("test MQ len"));
    let logs = logs.text();
    assert!(
        logs.contains("retry_after_ms"),
        "缺少重查退避的可观察 warn：{logs}"
    );
    assert!(
        logs.contains("store is unavailable"),
        "缺少失败原因：{logs}"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// 重查预算耗尽：本轮不得静默当作「无工作」完成，必须唤醒 listener 重查，
/// 且提醒仍留在 MQ 等待处理。
#[tokio::test]
#[serial]
async fn dispatch_query_error_after_budget_requests_listener_recheck() {
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
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::WorkflowComplete,
            BaseMessage::human("workflow finished"),
        ))
        .await;
    let epoch = harness
        .sessions
        .lock()
        .await
        .get(&harness.session_id)
        .unwrap()
        .continuation_epoch;
    let mut wake = queue.subscribe_wake();
    wake.borrow_and_update();

    let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{}:admitted",
        harness.session_id
    ));
    let logs = LogBuffer::default();
    let _subscriber = tracing::subscriber::set_default(logs.warn_subscriber());
    let params = json!({
        "sessionId": harness.session_id,
        "message": {"role": "user", "content": []},
    });
    let dispatch = crate::host::dispatch_prompt_turn(
        params,
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
        Some(epoch),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    );
    let db_path = harness.db_path();
    let logs_for_fault = logs.clone();
    let fault = async {
        gate.entered.notified().await;
        rename_mq_table(&db_path, "thread_mq", "thread_mq_offline").await;
        gate.release.notify_one();
        // 故障保持到 dispatch 用完重查预算并交还 listener，避免与恢复竞态。
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if logs_for_fault
                    .text()
                    .contains("requesting listener recheck")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dispatch 必须耗尽预算并请求重查");
        rename_mq_table(&db_path, "thread_mq_offline", "thread_mq").await;
    };
    let (response, ()) = tokio::join!(dispatch, fault);
    assert!(response.unwrap().is_null(), "查询失败不得当作空工作完成");
    let logs = logs.text();
    assert!(
        logs.contains("retry_after_ms") && logs.contains("requesting listener recheck"),
        "预算耗尽必须留下可观察 warn：{logs}"
    );
    assert!(
        wake.has_changed().expect("test wake receiver"),
        "必须请求 listener 重查"
    );
    assert_eq!(
        queue
            .query(peri_acp_types::session::MqFilter::default())
            .await
            .unwrap()
            .len(),
        1,
        "提醒必须留在 MQ 等待重查"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// P1 回归：取消后的唯一一次 armed 续跑，若 dispatch 二次查询耗尽预算，
/// 必须把资格还给 listener；不能等用户下一次输入才消费滞留的 Defer。
#[tokio::test]
#[serial]
async fn armed_continuation_query_failure_restores_eligibility_for_listener() {
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
    // 用户取消前台 prompt：epoch++、suppress、arm（与 Stop 同路径）。
    {
        let mut sessions = harness.sessions.lock().await;
        sessions.get_mut(&harness.session_id).unwrap().cancel_token =
            Some(CancellationToken::new());
        let _ = crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": harness.session_id}),
            &mut sessions,
            &harness.cfg,
        )
        .await;
        sessions.get_mut(&harness.session_id).unwrap().cancel_token = None;
    }
    harness.complete_child("armed-child");
    harness.listen();
    let first = tokio::time::timeout(
        Duration::from_secs(2),
        harness.receiver.as_mut().unwrap().recv(),
    )
    .await
    .expect("listener 必须发出 armed 续跑请求")
    .expect("continuation channel");
    assert!(!first.mq_steering, "armed 路径不得走 mq steering");
    assert!(!first.pending_result);
    let epoch = {
        let mut sessions = harness.sessions.lock().await;
        crate::host::continuation::take_continuation_for_request(
            sessions.get_mut(&harness.session_id).unwrap(),
            &first,
        )
        .expect("armed 请求必须消费一次性资格")
    };

    let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{}:admitted",
        harness.session_id
    ));
    let logs = LogBuffer::default();
    let _subscriber = tracing::subscriber::set_default(logs.warn_subscriber());
    let params = json!({
        "sessionId": harness.session_id,
        "message": {"role": "user", "content": []},
    });
    let origin = crate::host::PromptOrigin::Continuation {
        mq_steering: false,
        pending_result: false,
        recovery: false,
    };
    let dispatch = crate::host::dispatch_prompt_turn(
        params.clone(),
        origin,
        Some(epoch),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    );
    let db_path = harness.db_path();
    let logs_for_fault = logs.clone();
    let fault = async {
        gate.entered.notified().await;
        rename_mq_table(&db_path, "thread_mq", "thread_mq_offline").await;
        gate.release.notify_one();
        // 恢复 armed 的 warn 发生在 request_recheck 之前：见到它即可恢复存储。
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if logs_for_fault.text().contains("restored=true") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("查询失败必须恢复 armed 资格");
        rename_mq_table(&db_path, "thread_mq_offline", "thread_mq").await;
    };
    let (response, ()) = tokio::join!(dispatch, fault);
    assert!(response.unwrap().is_null(), "查询失败不得当作空工作完成");
    let logs = logs.text();
    assert!(
        logs.contains("restored=true") && logs.contains("requesting listener recheck"),
        "预算耗尽必须恢复 armed 并请求 listener 重查：{logs}"
    );

    // 无需用户输入：listener 重查同一 Defer，必须再次发出 armed 请求。
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        harness.receiver.as_mut().unwrap().recv(),
    )
    .await
    .expect("恢复资格后 listener 必须自动重查")
    .expect("continuation channel");
    assert!(!second.mq_steering);
    assert!(!second.pending_result);
    let retaken = {
        let mut sessions = harness.sessions.lock().await;
        crate::host::continuation::take_continuation_for_request(
            sessions.get_mut(&harness.session_id).unwrap(),
            &second,
        )
    };
    assert_eq!(retaken, Some(epoch), "重查必须拿回同一代际的资格");

    let response = crate::host::dispatch_prompt_turn(
        params,
        origin,
        retaken,
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    assert!(
        queue.is_empty().await.expect("test MQ len"),
        "续跑必须消费滞留的子任务结果"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// 恢复的 armed 资格属于当前代际：用户新 prompt 后 listener 不得再发旧代际请求。
#[tokio::test]
#[serial]
async fn restored_armed_is_not_reactivated_after_new_prompt() {
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
        .suppress();
    {
        let mut sessions = harness.sessions.lock().await;
        let state = sessions.get_mut(&harness.session_id).unwrap();
        state.continuation_epoch = 3;
        assert!(crate::host::continuation::restore_continuation_armed(
            state, 3
        ));
        // 用户新 prompt：清除 armed 并递增代际。
        state.continuation_armed = false;
        state.continuation_epoch += 1;
    }
    harness.complete_child("stale-generation-child");
    harness.listen();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        harness.receiver.as_mut().unwrap().try_recv().is_err(),
        "新 prompt 后不得复活旧代际的 armed 请求"
    );
    assert_eq!(
        queue
            .query(peri_acp_types::session::MqFilter::default())
            .await
            .unwrap()
            .len(),
        1,
        "Defer 留在 MQ 等新 prompt 消费"
    );
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
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human("late"),
        ))
        .await;
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
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::CronTrigger,
            BaseMessage::human("approved schedule"),
        ))
        .await;
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
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
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
    assert!(queue.has_ensure_processing().await.expect("test MQ policy"));
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
    assert!(queue.is_empty().await.expect("test MQ len"));
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

/// 模型配置变化会让旧 token 快照失效（设计 §2.2）。
#[tokio::test]
#[serial]
async fn model_config_update_invalidates_saved_context_usage() {
    let harness = ActivationHarness::new().await;
    let alias = harness.cfg.peri_config.read().config.active_alias.clone();
    let mut sessions = harness.sessions.lock().await;
    let outcome = crate::host::notify::handle_notification(
        "session/config_update",
        &json!({
            "sessionId": harness.session_id,
            "configId": "model",
            "value": alias,
        }),
        &mut sessions,
        &harness.cfg,
    )
    .await;
    assert_eq!(
        outcome.context_usage_invalidations,
        vec![ThreadId::from(harness.session_id.clone())]
    );
    drop(sessions);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// 定时调度是已批准的调度，但被动回调不能解除用户显式 Stop（设计 §2.1/§5.2）。
#[tokio::test]
#[serial]
async fn scheduled_continuation_does_not_override_user_stop() {
    let harness = ActivationHarness::new().await;
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::CronTrigger,
            BaseMessage::human("approved schedule"),
        ))
        .await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .suppress();
    let thread = ThreadId::from(harness.session_id.clone());
    harness
        .cfg
        .session_manager
        .session_resources()
        .set_work_state(&thread, WorkState::Stopped)
        .await
        .unwrap();
    harness
        .sessions
        .lock()
        .await
        .get_mut(&harness.session_id)
        .unwrap()
        .continuation_mq_steering_pending = true;
    let params =
        json!({"sessionId": harness.session_id, "message": {"role": "user", "content": []}});
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
    assert!(response.is_null(), "用户 Stop 后不得自动运行定时调度");
    assert!(queue.has_ensure_processing().await.expect("test MQ policy"));
    assert_eq!(
        harness
            .cfg
            .session_manager
            .session_resources()
            .load_session_meta(&thread)
            .await
            .unwrap()
            .work_state,
        WorkState::Stopped,
        "定时调度不得把 stopped 改回 working"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
