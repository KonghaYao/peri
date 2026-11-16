use crate::host::requests::tests::MockTransport;
use crate::host::AcpServerConfig;
use crate::host::{PromptLocks, SharedSessions};
use crate::provider::LlmProvider;
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};

use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::tasks::BgTaskRegistration;
use peri_acp_types::thread::{ThreadId, WorkState};
use serde_json::json;
use serial_test::serial;
use std::collections::HashMap;
use std::sync::Arc;
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
        let mut provider = crate::host::requests::tests::make_provider_config(
            "local", "openai", "test", "gpt-test",
        );
        provider.base_url = base_url.to_string();
        let config = crate::host::requests::tests::make_peri_config_with_provider(provider);
        let provider = LlmProvider::from_config(&config).unwrap();
        let cfg = Arc::new(
            crate::host::requests::tests::make_server_config(config, provider, &directory).await,
        );
        let mut sessions = HashMap::new();
        let session_id = crate::host::requests::tests::register_session_with_history(
            &mut sessions,
            directory.path().to_str().unwrap(),
            &cfg,
        )
        .await;
        let frozen = crate::host::requests::tests::frozen_snapshot_bytes(&cfg, &session_id)
            .await
            .unwrap();
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

include!("activation_test_part2_test.rs");
