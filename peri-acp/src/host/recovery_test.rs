//! 进入会话时的恢复（设计 §3/§4）：准备、去重、Stop、实例复用、启动门与错误映射。
//!
//! 夹具与 `activation_test.rs` 同形：真实 `AcpServerConfig` + 临时 SQLite + mock 模型端点；
//! `ServerLoop` 用例走真实请求分发，断言可观察的 wire 顺序与错误码。

use super::*;
use crate::host::{PromptLocks, SharedSessions};
use peri_acp_types::recovery::{RecoverySkipReason, SessionRecovery};
use peri_acp_types::session::{MessageSource, MqFilter};
use peri_acp_types::thread::WorkState;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::sync::mpsc;

const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

// ── 记录型 transport ─────────────────────────────────────────────────────────

#[derive(Default)]
struct RecordingTransport {
    incoming: StdMutex<VecDeque<IncomingMessage>>,
    responses: StdMutex<Vec<(RequestId, std::result::Result<Value, String>)>>,
    errors: StdMutex<Vec<(RequestId, AcpError)>>,
    notifications: StdMutex<Vec<(String, Value)>>,
    /// 响应与恢复启动的相对顺序：`response` 由这里写入，`continuation` 由转发任务写入。
    order: Arc<StdMutex<Vec<&'static str>>>,
}

impl RecordingTransport {
    fn new(order: Arc<StdMutex<Vec<&'static str>>>, incoming: Vec<IncomingMessage>) -> Self {
        Self {
            incoming: StdMutex::new(incoming.into()),
            order,
            ..Default::default()
        }
    }

    fn notifications(&self) -> Vec<(String, Value)> {
        self.notifications.lock().unwrap().clone()
    }

    fn responses(&self) -> Vec<(RequestId, std::result::Result<Value, String>)> {
        self.responses.lock().unwrap().clone()
    }

    fn error_for(&self, id: RequestId) -> Option<AcpError> {
        self.errors
            .lock()
            .unwrap()
            .iter()
            .find(|(request_id, _)| *request_id == id)
            .map(|(_, error)| error.clone())
    }
}

#[async_trait]
impl crate::transport::AcpTransport for RecordingTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.notifications
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming.lock().unwrap().pop_front()
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: std::result::Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        self.order.lock().unwrap().push("response");
        let errors = result
            .as_ref()
            .err()
            .map(|error| (id.clone(), error.clone()));
        self.responses
            .lock()
            .unwrap()
            .push((id.clone(), result.map_err(|error| error.to_string())));
        if let Some(error) = errors {
            self.errors.lock().unwrap().push(error);
        }
        Ok(())
    }
}

// ── 夹具 ─────────────────────────────────────────────────────────────────────

struct RecoveryHarness {
    cfg: Arc<AcpServerConfig>,
    sessions: SharedSessions,
    locks: PromptLocks,
    sender: Arc<mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>>,
    receiver: Option<mpsc::UnboundedReceiver<crate::session::executor::ContinuationRequest>>,
    recorded: Arc<RecordingTransport>,
    transport: Arc<dyn crate::transport::AcpTransport>,
    thread_id: String,
    cwd: String,
    db_path: std::path::PathBuf,
    order: Arc<StdMutex<Vec<&'static str>>>,
    _server: mockito::ServerGuard,
    _mock: mockito::Mock,
    _directory: tempfile::TempDir,
}

impl RecoveryHarness {
    /// `session/new` 建会话：恢复前置条件（已发布 `SessionState` + 冻结上下文）走生产路径。
    async fn new() -> Self {
        Self::new_with_workspace(false).await
    }

    /// 需要真实会话环境（交互 router / endpoint）的用例显式打开工作区装配。
    async fn new_with_workspace(with_workspace: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("threads.db");
        let cwd = directory.path().to_str().unwrap().to_owned();
        let mut harness = Self::assemble(
            directory,
            db_path.clone(),
            cwd,
            open_facade(&db_path).await,
            with_workspace,
        )
        .await;
        harness.create_session().await;
        harness
    }

    /// 用例保管工作区与库文件的「进程」夹具：退出后目录与 `threads.db` 仍在。
    ///
    /// 同一个 `workspace` 先后构造两次就是「旧进程退出、新进程再进入」：它们只共享
    /// 库文件与 cwd，运行期状态（门面、`SessionManager`、MQ 实例）各自独立。
    async fn durable(workspace: &std::path::Path) -> Self {
        let db_path = workspace.join("threads.db");
        let facade = open_facade(&db_path).await;
        let cwd = workspace.to_string_lossy().into_owned();
        Self::assemble(tempfile::tempdir().unwrap(), db_path, cwd, facade, false).await
    }

    /// 用给定门面装配夹具：只读旧库、跨进程复用同一路径都走这里。
    ///
    /// `directory` 只承载宿主级临时文件（配置源、插件目录），不参与库定位。
    async fn assemble(
        directory: tempfile::TempDir,
        db_path: std::path::PathBuf,
        cwd: String,
        resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
        with_workspace: bool,
    ) -> Self {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(
                "data: {\"id\":\"test-response\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"recovered\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
            .expect(1)
            .create_async()
            .await;
        let mut provider = make_provider_config("local", "openai", "test", "gpt-test");
        provider.base_url = server.url();
        let config = make_peri_config_with_provider(provider);
        let provider = LlmProvider::from_config(&config).unwrap();
        let mut cfg = build_server_config(config, provider, &directory, resources).await;
        if with_workspace {
            cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
                startup_cwd: cwd.clone(),
                bare: true,
                drive_cron_tick: false,
                mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
                capabilities: Default::default(),
            });
        }
        let cfg = Arc::new(cfg);
        let order = Arc::new(StdMutex::new(Vec::<&'static str>::new()));
        let recorded = Arc::new(RecordingTransport::new(Arc::clone(&order), Vec::new()));
        let transport: Arc<dyn crate::transport::AcpTransport> = recorded.clone();
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            cfg,
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            sender: Arc::new(sender),
            receiver: Some(receiver),
            recorded,
            transport,
            thread_id: String::new(),
            cwd,
            db_path,
            order,
            _server: server,
            _mock: mock,
            _directory: directory,
        }
    }

    /// 生产同款 `session/new`：恢复前置条件（已发布 `SessionState` + 冻结上下文）随之成立。
    async fn create_session(&mut self) {
        let cwd = self.cwd.clone();
        let mut states = self.sessions.lock().await;
        let created = handle_request(
            "session/new",
            &json!({"cwd": cwd}),
            &self.cfg,
            &mut states,
            &self.transport,
        )
        .await
        .expect("session/new 必须成功");
        self.thread_id = created["sessionId"].as_str().unwrap().to_owned();
    }

    /// 生产同款 `session/load`：让已持久化的 thread 在本进程内取得运行实例。
    async fn load_session(&mut self, thread_id: &str) {
        let cwd = self.cwd.clone();
        let mut states = self.sessions.lock().await;
        handle_request(
            "session/load",
            &json!({"sessionId": thread_id, "cwd": cwd}),
            &self.cfg,
            &mut states,
            &self.transport,
        )
        .await
        .expect("session/load 必须成功");
        self.thread_id = thread_id.to_owned();
    }

    fn recovery(&self) -> crate::host::recovery::HostSessionRecovery {
        recovery_for(&self.cfg, &self.sessions, &self.sender)
    }

    async fn set_working(&self, thread_id: &str) {
        self.cfg
            .session_resources
            .set_work_state(&thread_id.to_owned(), WorkState::Working)
            .await
            .unwrap();
    }

    /// 以 `ServerLoop::dispatch_notification` 同序处理 Stop：先锁内捕获目标，再锁外
    /// 持久化。返回捕获的目标与持久化任务，测试负责在需要的屏障处等待/放行。
    async fn capture_and_spawn_stop(
        &self,
        thread_id: &str,
    ) -> (
        Vec<crate::host::notify::IntentTarget>,
        tokio::task::JoinHandle<()>,
    ) {
        let targets = {
            let mut states = self.sessions.lock().await;
            crate::host::notify::handle_notification(
                "session/cancel",
                &json!({"sessionId": thread_id}),
                &mut states,
                &self.cfg,
            )
            .await
            .stop_targets
        };
        let cfg = Arc::clone(&self.cfg);
        let sessions = Arc::clone(&self.sessions);
        let locks = Arc::clone(&self.locks);
        let persist_targets = targets.clone();
        let persist = tokio::spawn(async move {
            crate::host::notify::persist_notification_effects(
                "session/cancel",
                &cfg,
                &sessions,
                &locks,
                persist_targets,
                Vec::new(),
            )
            .await;
        });
        (targets, persist)
    }

    fn queue(&self, thread_id: &str) -> peri_acp_types::session::MessageQueue {
        self.cfg
            .session_manager
            .v2_queue_for(thread_id)
            .unwrap_or_else(|| {
                peri_acp_types::session::MessageQueue::bind(
                    Arc::clone(&self.cfg.session_resources),
                    thread_id.to_owned(),
                )
            })
    }

    async fn recovery_rows(&self, thread_id: &str) -> Vec<peri_acp_types::session::MqEntry> {
        self.queue(thread_id)
            .query(MqFilter {
                sources: vec![MessageSource::SessionRecovery],
                ..Default::default()
            })
            .await
            .unwrap()
    }

    /// 在 `parent` 之下建 child（roots 取父链的根，frozen 取自根的已保存快照）。
    async fn add_child(&self, parent: &str, child: &str, root: &str) {
        let workspace = self
            .cfg
            .session_resources
            .resolve_workspace(std::path::Path::new(&self.cwd))
            .await
            .unwrap();
        let root_frozen = match self
            .cfg
            .session_resources
            .load_session_snapshot(&root.to_owned())
            .await
            .unwrap()
            .frozen
        {
            peri_acp_types::session_resources::FrozenState::Present(bytes) => bytes,
            other => panic!("root frozen snapshot is missing: {other:?}"),
        };
        let snapshot = peri_acp_types::session_resources::ChildSnapshot {
            target: peri_acp_types::session_resources::NewSession {
                thread_id: child.to_owned(),
                created_at: "2026-09-26T00:00:01Z".to_owned(),
                meta: peri_acp_types::session_resources::NewSessionMeta {
                    title: Some(child.to_owned()),
                    cwd: workspace.cwd.to_string_lossy().into_owned(),
                    parent_thread_id: Some(parent.to_owned()),
                    hidden: true,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                binding: peri_acp_types::workspace::SessionBinding::from_workspace(&workspace),
                frozen: root_frozen,
            },
            parent_id: parent.to_owned(),
            root_id: root.to_owned(),
            inherited: peri_acp_types::store::InheritedContext {
                payloads: Vec::new(),
                flags: Default::default(),
            },
        };
        self.cfg
            .session_resources
            .save_child(&snapshot)
            .await
            .unwrap();
        self.cfg
            .session_resources
            .update_session_meta(
                &child.to_owned(),
                &peri_acp_types::session_resources::SessionMetaPatch {
                    config: Some(Some(
                        peri_acp_types::thread::with_completion_target(None, Some(parent)).unwrap(),
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    /// 生产同款 continuation scheduler；通道前面挂一层记录，观察启动门顺序。
    fn spawn_continuation_scheduler(&mut self) {
        let receiver = self.receiver.take().expect("scheduler starts once");
        let (forward_tx, forward_rx) = mpsc::unbounded_channel();
        let order = Arc::clone(&self.order);
        tokio::spawn(async move {
            let mut receiver = receiver;
            while let Some(request) = receiver.recv().await {
                order.lock().unwrap().push("continuation");
                if forward_tx.send(request).is_err() {
                    break;
                }
            }
        });
        assert!(
            self.cfg
                .host_task_spawner
                .spawn(
                    crate::host::task_scope::HostTaskOwnerKind::Host,
                    crate::host::task_scope::HostTaskKind::ContinuationScheduler,
                    crate::host::continuation::run_continuation_scheduler(
                        forward_rx,
                        self.sessions.clone(),
                        self.locks.clone(),
                        Arc::clone(&self.cfg),
                        self.transport.clone(),
                        Arc::downgrade(&self.sender),
                        self.cfg.host_task_spawner.clone(),
                        self.cfg.host_task_spawner.shutdown_token(),
                    ),
                )
                .is_ok(),
            "continuation scheduler 必须被 task owner 接受"
        );
    }

    fn activation_allowed(&self, session_id: &str) -> bool {
        self.cfg
            .session_manager
            .get_session(session_id)
            .is_some_and(|session| session.activation.is_allowed())
    }

    fn listener_started(&self, session_id: &str) -> bool {
        self.cfg
            .session_manager
            .get_session(session_id)
            .is_some_and(|session| {
                session
                    .activation
                    .listener_started
                    .load(std::sync::atomic::Ordering::Acquire)
            })
    }
}

/// 读写门面：按显式路径打开，库文件由用例保管（跨进程用例共享同一文件）。
async fn open_facade(
    db_path: &std::path::Path,
) -> Arc<dyn peri_acp_types::session_resources::SessionResources> {
    peri_agent::resources::open_session_resources_with(Some(db_path.to_path_buf()))
        .await
        .unwrap()
}

async fn wait_for<F, Fut>(label: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    loop {
        if condition().await {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "等待超时：{label}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn reminder_source(entry: &peri_acp_types::session::MqEntry) -> String {
    match &entry.payload {
        peri_acp_types::store::PersistedPayload::SystemReminder { reminder, .. } => {
            reminder.as_reminder().source.0.clone()
        }
        peri_acp_types::store::PersistedPayload::Message(_) => panic!("不是提醒行"),
    }
}

fn reminder_body(entry: &peri_acp_types::session::MqEntry) -> String {
    match &entry.payload {
        peri_acp_types::store::PersistedPayload::SystemReminder { reminder, .. } => {
            reminder.as_reminder().body.clone()
        }
        peri_acp_types::store::PersistedPayload::Message(_) => panic!("不是提醒行"),
    }
}

#[path = "recovery_behavior_test.rs"]
mod behavior_tests;

#[path = "recovery_gate_test.rs"]
mod gate_tests;

#[path = "recovery_child_test.rs"]
mod recovery_child;

#[path = "recovery_parent_binding_test.rs"]
mod parent_binding_tests;

#[path = "recovery_wire_test.rs"]
mod wire_tests;

#[path = "recovery_durability_test.rs"]
mod durability_tests;

#[path = "recovery_legacy_store_test.rs"]
mod legacy_store_tests;

#[path = "prompt_admission_test.rs"]
mod prompt_admission_tests;
