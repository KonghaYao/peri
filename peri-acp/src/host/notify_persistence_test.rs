//! Stop 持久化移出收包循环后的回归：同连接响应性与迟到意图的代次守卫。
//!
//! 全部走真实 `ServerLoop` / 真实 SQLite 门面；只有 `set_work_state(Stopped)` 被
//! 包装层阻塞，用来证明写入等待不再占住收包循环。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::session::{
    MqCancelOutcome, MqEnqueueOutcome, MqEntry, MqFilter, MqInput, MqReceiveOutcome,
    MqReceiveSelection,
};
use peri_acp_types::session_resources::{
    BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, CloseSettlement, ForkSnapshot,
    FrozenSnapshotBytes, FrozenState, NewSession, NewSessionDraft, NewSessionMeta,
    PersistenceRecovery, RewindBoundary, RewindOutcome, SessionAvailability, SessionInitialization,
    SessionResourceResult, SessionResources, SessionSnapshot,
};
use peri_acp_types::thread::{ThreadId, ThreadMeta, WorkState};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::provider::{LlmProvider, PeriConfig, ProviderConfig, ProviderModels};
use crate::transport::stdio::StdioTransport;
use crate::transport::AcpTransport;

/// 只阻塞 `set_work_state(Stopped)`，其余方法逐项转发真实门面。
///
/// 门面方法约三十个，注入点只有一个，故用宏转发而不是手写全部方法。
macro_rules! forward_session_resources_except_stop_write {
    (
        $name:ident
        $( $method:ident ( $( $argument:ident : $kind:ty ),* ) -> $output:ty; )*
    ) => {
        #[async_trait]
        impl SessionResources for $name {
            $(async fn $method(&self, $( $argument: $kind ),*) -> SessionResourceResult<$output> {
                self.inner.$method($( $argument ),*).await
            })*

            async fn set_work_state(
                &self,
                id: &ThreadId,
                state: WorkState,
            ) -> SessionResourceResult<()> {
                if state == WorkState::Stopped {
                    self.started.notify_one();
                    self.release.notified().await;
                }
                self.inner.set_work_state(id, state).await
            }

            async fn set_context_usage(
                &self,
                id: &ThreadId,
                tokens: Option<u64>,
            ) -> SessionResourceResult<()> {
                self.inner.set_context_usage(id, tokens).await
            }
        }
    };
}

struct BlockingResources {
    inner: Arc<dyn SessionResources>,
    started: Arc<Notify>,
    release: Arc<Notify>,
}

forward_session_resources_except_stop_write! {
    BlockingResources
    inspect_availability(session: Option<&ThreadId>) -> SessionAvailability;
    resolve_workspace(cwd: &Path) -> ResolvedWorkspace;
    validate_session(id: &ThreadId, workspace: &ResolvedWorkspace) -> ();
    finish_close(id: &ThreadId) -> ();
    close_settlement(id: &ThreadId) -> CloseSettlement;
    create_session(input: &NewSession) -> ();
    abandon_initialization(id: &ThreadId) -> ();
    begin_initialization(draft: &NewSessionDraft) -> Arc<dyn SessionInitialization>;
    discard_incomplete_initialization(id: &ThreadId) -> ();
    adopt_legacy_session(id: &ThreadId, saved_cwd: &str, workspace: &ResolvedWorkspace, frozen: &FrozenSnapshotBytes) -> ();
    load_session_snapshot(id: &ThreadId) -> SessionSnapshot;
    load_frozen_state(id: &ThreadId) -> FrozenState;
    load_session_binding(id: &ThreadId) -> BindingState;
    validate_bound_workspace(id: &ThreadId, check: BindingRecheck) -> ResolvedWorkspace;
    load_session_history(id: &ThreadId) -> Vec<peri_acp_types::store::PersistedPayload>;
    load_session_meta(id: &ThreadId) -> ThreadMeta;
    list_sessions(query: &ScopedThreadQuery) -> ScopedThreadPage;
    list_children(parent: &ThreadId) -> Vec<ThreadMeta>;
    list_session_tree(root: &ThreadId) -> Vec<ThreadMeta>;
    append_history(id: &ThreadId, payloads: &[peri_acp_types::store::PersistedPayload]) -> ();
    append_reminder_if_absent(id: &ThreadId, message_id: peri_acp_types::messages::MessageId, reminder: &peri_acp_types::system_reminder::TrustedSystemReminder) -> bool;
    mark_session_closing(id: &ThreadId) -> ();
    is_session_closing(id: &ThreadId) -> bool;
    save_fork(fork: &ForkSnapshot) -> ();
    save_child(child: &ChildSnapshot) -> ();
    claim_child_resume(child: &ThreadId, root: &ThreadId) -> Box<dyn ChildResumeClaim>;
    apply_compaction(id: &ThreadId, change: &peri_acp_types::store::CompactionChange) -> ();
    apply_message_projections(id: &ThreadId, updates: &[(peri_acp_types::messages::MessageId, peri_acp_types::store::MessageFlags)]) -> ();
    rewind_history(id: &ThreadId, boundary: RewindBoundary, clear_excluded: &[peri_acp_types::messages::MessageId]) -> RewindOutcome;
    remove_history_entries(id: &ThreadId, ids: &[peri_acp_types::messages::MessageId]) -> ();
    update_session_meta(id: &ThreadId, patch: &peri_acp_types::session_resources::SessionMetaPatch) -> ();
    delete_session_tree(id: &ThreadId) -> ();
    recover_session_persistence(id: &ThreadId) -> PersistenceRecovery;
    drain_persistence(id: &ThreadId) -> ();
    mq_enqueue(thread_id: &ThreadId, input: MqInput) -> MqEnqueueOutcome;
    mq_query(thread_id: &ThreadId, filter: MqFilter) -> Vec<MqEntry>;
    mq_cancel(thread_id: &ThreadId, id: peri_acp_types::messages::MessageId) -> MqCancelOutcome;
    mq_receive(thread_id: &ThreadId, selection: MqReceiveSelection) -> MqReceiveOutcome;
    mq_clear(thread_id: &ThreadId) -> ();
    mq_history_contains(thread_id: &ThreadId, id: peri_acp_types::messages::MessageId) -> bool;
}

/// 最小 provider 配置：夹具只需要可解析的模型别名，不发起真实请求。
fn test_peri_config() -> PeriConfig {
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "unused-test-key".to_string(),
        models: ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }];
    peri_config
}

/// 生产同形宿主配置：注入给定的资源门面，使 Stop 持久化走真实调用链。
async fn build_config(
    tmp: &tempfile::TempDir,
    session_resources: Arc<dyn SessionResources>,
) -> Arc<AcpServerConfig> {
    Arc::new(make_server_config(tmp, session_resources).await)
}

async fn make_server_config(
    tmp: &tempfile::TempDir,
    session_resources: Arc<dyn SessionResources>,
) -> AcpServerConfig {
    let peri_config = test_peri_config();
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    crate::host::requests::tests::build_server_config(peri_config, provider, tmp, session_resources)
        .await
}

/// 生产同形配置 + 可注入 shutdown 时限的 host task owner（P1-2 用短时限）。
async fn build_config_with_owner(
    tmp: &tempfile::TempDir,
    session_resources: Arc<dyn SessionResources>,
    cooperative_grace: Duration,
    abort_drain_guard: Duration,
) -> (Arc<AcpServerConfig>, crate::host::task_scope::HostTaskOwner) {
    let mut cfg = make_server_config(tmp, session_resources).await;
    let (owner, spawner) = crate::host::task_scope::HostTaskOwner::with_policy_for_tests(
        cooperative_grace,
        abort_drain_guard,
    );
    cfg.host_task_owner = Some(owner);
    cfg.host_task_spawner = spawner;
    let owner = cfg.host_task_owner.take().expect("host task owner");
    (Arc::new(cfg), owner)
}

/// 与生产 wrapper 同形，但保留 owner 在 config 内、使用短 shutdown 时限。
async fn build_wrapper_config(
    tmp: &tempfile::TempDir,
    session_resources: Arc<dyn SessionResources>,
    cooperative_grace: Duration,
    abort_drain_guard: Duration,
) -> (AcpServerConfig, Arc<dyn SessionResources>) {
    let mut cfg = make_server_config(tmp, session_resources).await;
    let (owner, spawner) = crate::host::task_scope::HostTaskOwner::with_policy_for_tests(
        cooperative_grace,
        abort_drain_guard,
    );
    cfg.host_task_owner = Some(owner);
    cfg.host_task_spawner = spawner;
    let resources = Arc::clone(&cfg.session_resources);
    (cfg, resources)
}

/// 建一条已绑定会话（与生产创建路径同形），使 `set_work_state` 有真实行可写。
async fn create_bound_session(cfg: &AcpServerConfig, cwd: &str, sid: &str) {
    let workspace = cfg
        .session_resources
        .resolve_workspace(Path::new(cwd))
        .await
        .unwrap();
    let frozen = cfg
        .session_manager
        .build_frozen_data(workspace.cwd.to_str().unwrap());
    let encoded = crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap();
    cfg.session_resources
        .create_session(&NewSession {
            thread_id: sid.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: peri_acp_types::workspace::SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(encoded),
        })
        .await
        .unwrap();
}

/// 只读夹具：不装配 environment/frozen，只保留 sessions 映射所需的身份与代次。
fn session_state(sid: &str, cwd: &str) -> SessionState {
    SessionState {
        session_id: sid.to_string(),
        thread_id: sid.to_string(),
        cwd: cwd.to_string(),
        environment: None,
        closing: false,
        close_in_flight: 0,
        context_ownership: crate::host::ContextOwnership::Resident,
        history_payloads: Vec::new(),
        structure_version: 0,
        session_generation: crate::host::next_session_generation(),
        history_window: Default::default(),
        event_health: Default::default(),
        cancel_token: None,
        interaction_gate: Arc::new(tokio::sync::Mutex::new(())),
        host_request_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        continuation_armed: false,
        continuation_epoch: 0,
        work_intent_epoch: 0,
        user_intent_epoch: 0,
        continuation_in_flight: false,
        continuation_mq_steering_pending: false,
        recovery_settlement_pending: false,
        view_only: false,
        frozen: None,
        recall_items: Vec::new(),
        agent_pool: crate::session::agent_pool::AgentPool::new(),
        title: None,
        tags: Vec::new(),
    }
}

/// 直接读存储中的 durable 工作意图，不依赖 Host 内存态。
async fn work_state(cfg: &AcpServerConfig, sid: &str) -> WorkState {
    cfg.session_resources
        .load_session_meta(&sid.to_owned())
        .await
        .unwrap()
        .work_state
}

/// 有界轮询等待异步持久化收敛（写入已移出收包循环）。
async fn wait_for_work_state(cfg: &AcpServerConfig, sid: &str, expected: WorkState) {
    for _ in 0..100 {
        if work_state(cfg, sid).await == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "work_state did not settle to {expected:?}; last = {:?}",
        work_state(cfg, sid).await
    );
}

/// 验收 1：Stop 写库被门控期间，同连接下一条请求仍在 1s 内得到响应。
#[tokio::test]
async fn stop_persistence_does_not_block_the_next_frame_on_same_connection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (_bridge, facade) =
        peri_resources::sessions::open_store_and_facade_for_tests(tmp.path().join("threads.db"))
            .await
            .unwrap();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let blocking = Arc::new(BlockingResources {
        inner: Arc::new(facade),
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    });
    let cfg = build_config(&tmp, Arc::clone(&blocking) as Arc<dyn SessionResources>).await;
    let cwd = tmp.path().to_str().unwrap();
    let sid = "stop-persist-wire-session".to_string();
    create_bound_session(&cfg, cwd, &sid).await;
    cfg.session_manager.ensure_session(&sid, cwd);
    cfg.session_manager.ensure_session_caps(&sid);

    let states: SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
        sid.clone(),
        session_state(&sid, cwd),
    )])));
    let locks: PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let client = Arc::new(client);
    let server: Arc<dyn AcpTransport> = Arc::new(server);
    let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
    let cont_tx = Arc::new(cont_tx);
    let connection = Arc::new(tokio::sync::Mutex::new(
        crate::host::connection::ConnectionContext::new(false),
    ));
    let cancellation = CancellationToken::new();
    let recovery = crate::host::requests::tests::recovery_for(&cfg, &states, &cont_tx);
    let loop_cfg = Arc::clone(&cfg);
    let loop_states = Arc::clone(&states);
    let loop_locks = Arc::clone(&locks);
    let loop_server = Arc::clone(&server);
    let loop_cont_tx = Arc::clone(&cont_tx);
    let task = tokio::spawn(async move {
        crate::host::server_loop::ServerLoop {
            transport: &loop_server,
            cfg: &loop_cfg,
            sessions: &loop_states,
            prompt_locks: &loop_locks,
            cont_tx: &loop_cont_tx,
            connection: &connection,
            connection_cancellation: &cancellation,
            recovery: &recovery,
        }
        .run()
        .await;
    });

    client
        .send_notification("session/cancel", json!({"sessionId": sid}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("Stop 持久化必须走到被阻塞的 set_work_state");

    let reply = tokio::time::timeout(
        Duration::from_secs(1),
        client.send_request("session/input/snapshot", json!({"sessionId": sid})),
    )
    .await
    .expect("Stop 写库阻塞时，同连接下一条请求不能等待")
    .expect("snapshot 请求必须成功");
    assert!(
        reply.get("generation").is_some(),
        "snapshot 响应必须携带 generation: {reply}"
    );

    release.notify_one();
    wait_for_work_state(&cfg, &sid, WorkState::Stopped).await;
    client.close();
    task.abort();
}

/// 验收 2：迟到 Stop 目标（代次已过期）不得覆盖新准入写入的 `working`。
#[tokio::test]
async fn stale_stop_intent_does_not_override_new_admission() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (_bridge, facade) =
        peri_resources::sessions::open_store_and_facade_for_tests(tmp.path().join("threads.db"))
            .await
            .unwrap();
    let cfg = build_config(&tmp, Arc::new(facade)).await;
    let cwd = tmp.path().to_str().unwrap();
    let sid = "stop-persist-epoch-session".to_string();
    create_bound_session(&cfg, cwd, &sid).await;
    cfg.session_resources
        .set_work_state(&sid, WorkState::Working)
        .await
        .unwrap();

    let states: SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
        sid.clone(),
        session_state(&sid, cwd),
    )])));
    let locks: PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    // 捕获 Stop 目标：此时代次为 0。
    let stale = {
        let mut states = states.lock().await;
        crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": sid}),
            &mut states,
            &cfg,
        )
        .await
        .stop_targets
    };
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].epoch, 0);

    // 模拟新 turn 准入：`working` 落库后推进代次（与 prompt_dispatch 同序）。
    states.lock().await.get_mut(&sid).unwrap().work_intent_epoch += 1;
    cfg.session_resources
        .set_work_state(&sid, WorkState::Working)
        .await
        .unwrap();

    crate::host::notify::persist_notification_effects(
        "session/cancel",
        &cfg,
        &states,
        &locks,
        stale,
        Vec::new(),
    )
    .await;
    assert_eq!(
        work_state(&cfg, &sid).await,
        WorkState::Working,
        "过期 Stop 目标不得覆盖新准入的 working"
    );

    // 无新准入时，同代次目标仍须正常落为 stopped。
    let fresh = {
        let mut states = states.lock().await;
        crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": sid}),
            &mut states,
            &cfg,
        )
        .await
        .stop_targets
    };
    assert_eq!(fresh[0].epoch, 1);
    crate::host::notify::persist_notification_effects(
        "session/cancel",
        &cfg,
        &states,
        &locks,
        fresh,
        Vec::new(),
    )
    .await;
    assert_eq!(work_state(&cfg, &sid).await, WorkState::Stopped);
}

/// P1：正常 shutdown 的 abort 阶段不得丢弃已接受但未落库的 Stop。
///
/// 存储写入被固定阻塞时，shutdown 必须在有界等待后报告 `Incomplete`，不能因为
/// abort 后任务排空就声称 `Complete`；任务本身继续存活，存储恢复后 Stop 仍落库。
#[tokio::test]
async fn review_shutdown_preserves_pending_stop_intent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (_bridge, facade) =
        peri_resources::sessions::open_store_and_facade_for_tests(tmp.path().join("threads.db"))
            .await
            .unwrap();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let blocking = Arc::new(BlockingResources {
        inner: Arc::new(facade),
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    });
    let (cfg, owner) = build_config_with_owner(
        &tmp,
        Arc::clone(&blocking) as Arc<dyn SessionResources>,
        Duration::from_millis(1),
        Duration::from_millis(200),
    )
    .await;
    let cwd = tmp.path().to_str().unwrap();
    let sid = "shutdown-stop-pending-session".to_string();
    create_bound_session(&cfg, cwd, &sid).await;
    cfg.session_resources
        .set_work_state(&sid, WorkState::Working)
        .await
        .unwrap();
    cfg.session_manager.ensure_session(&sid, cwd);
    cfg.session_manager.ensure_session_caps(&sid);

    let states: SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
        sid.clone(),
        session_state(&sid, cwd),
    )])));
    let locks: PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    // 真实通知分派捕获 Stop 目标。
    let targets = {
        let mut states = states.lock().await;
        crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": sid}),
            &mut states,
            &cfg,
        )
        .await
        .stop_targets
    };
    assert_eq!(targets.len(), 1);

    // 与 `server_loop/notifications.rs` 相同的 host task 边界。
    {
        let spawner = cfg.host_task_spawner.clone();
        let spawn_cfg = Arc::clone(&cfg);
        let spawn_states = Arc::clone(&states);
        let spawn_locks = Arc::clone(&locks);
        spawner
            .spawn(
                crate::host::task_scope::HostTaskOwnerKind::Session,
                crate::host::task_scope::HostTaskKind::NotificationEffect,
                async move {
                    crate::host::notify::persist_notification_effects(
                        "session/cancel",
                        &spawn_cfg,
                        &spawn_states,
                        &spawn_locks,
                        targets,
                        Vec::new(),
                    )
                    .await;
                },
            )
            .expect("host task admission must stay open");
    }
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("Stop 持久化必须走到被阻塞的 set_work_state");

    // 正常关闭入口的用户：先停止会话准入，再收口 host task。
    cfg.session_manager.pre_close_session(&sid);
    let owner = Arc::new(tokio::sync::Mutex::new(owner));
    let shutdown_owner = Arc::clone(&owner);
    let shutdown = tokio::spawn(async move { shutdown_owner.lock().await.shutdown().await });
    let report = tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .expect("shutdown 必须有界返回")
        .expect("shutdown 任务不得 panic");
    assert!(
        matches!(
            report,
            crate::host::task_scope::HostShutdownReport::Incomplete { .. }
        ),
        "Stop 尚未落库时 shutdown 不得声称 Complete: {report:?}"
    );

    // 存储恢复：承载 Stop 的 host task 未被 abort，仍能完成 durable 写入。
    release.notify_one();
    wait_for_work_state(&cfg, &sid, WorkState::Stopped).await;

    assert!(
        cfg.session_manager.drain_session_tasks(&sid).await,
        "会话任务必须可收口"
    );
    if let Some(session) = cfg.session_manager.take_for_close(&sid) {
        assert!(
            session.close_resources().await,
            "Stop 落库后会话资源必须可关闭"
        );
    }
    drop(owner);
}

async fn write_json_line(stream: &mut DuplexStream, value: serde_json::Value) {
    stream
        .write_all(value.to_string().as_bytes())
        .await
        .unwrap();
    stream.write_all(b"\n").await.unwrap();
}

async fn read_json_line(reader: &mut BufReader<DuplexStream>) -> serde_json::Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .expect("stdio 输出超时")
        .unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

async fn send_stdio_request(
    input: &mut DuplexStream,
    output: &mut BufReader<DuplexStream>,
    id: i64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    write_json_line(
        input,
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
    )
    .await;
    loop {
        let value = read_json_line(output).await;
        if value.get("id").and_then(serde_json::Value::as_i64) == Some(id) {
            assert!(value.get("error").is_none(), "{method} 失败: {value}");
            return value;
        }
    }
}

/// P1：正常 stdio EOF 走真实 wrapper。第一次 shutdown 因 Stop 持久化阻塞报
/// `Incomplete` 后，wrapper 必须保留 context 重入 finish，而不是直接 Drop owner；
/// 存储恢复后旧 Stop 仍落库，最终报告 Complete。
#[tokio::test]
async fn review_stdio_eof_preserves_pending_stop() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (_bridge, facade) =
        peri_resources::sessions::open_store_and_facade_for_tests(tmp.path().join("threads.db"))
            .await
            .unwrap();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let blocking = Arc::new(BlockingResources {
        inner: Arc::new(facade),
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    });
    let (cfg, resources) = build_wrapper_config(
        &tmp,
        Arc::clone(&blocking) as Arc<dyn SessionResources>,
        Duration::from_millis(1),
        Duration::from_millis(100),
    )
    .await;
    let cwd = tmp.path().to_str().unwrap().to_string();

    let (mut input, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, output_read) = tokio::io::duplex(64 * 1024);
    let transport = Arc::new(StdioTransport::from_reader_writer(
        transport_read,
        transport_write,
    ));
    let server = tokio::spawn(crate::host::run_acp_server(transport, cfg));
    let mut output = BufReader::new(output_read);

    send_stdio_request(
        &mut input,
        &mut output,
        1,
        "initialize",
        json!({"protocolVersion": 1}),
    )
    .await;
    let created = send_stdio_request(
        &mut input,
        &mut output,
        2,
        "session/new",
        json!({"cwd": cwd}),
    )
    .await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_string();
    resources
        .set_work_state(&sid, WorkState::Working)
        .await
        .unwrap();

    write_json_line(
        &mut input,
        json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": sid},
        }),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("Stop 持久化必须走到被阻塞的 set_work_state");

    // stdin EOF：生产 wrapper 开始关闭；首轮 shutdown 会在短时限后报 Incomplete。
    drop(input);
    tokio::time::sleep(Duration::from_millis(400)).await;
    release.notify_one();

    let report = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("wrapper 必须在有界窗口内返回")
        .expect("server task 不得 panic");
    assert!(
        report.is_complete(),
        "存储恢复后 wrapper 必须让待持久化 Stop 完成: {report:?}"
    );
    assert_eq!(
        resources.load_session_meta(&sid).await.unwrap().work_state,
        WorkState::Stopped,
        "stdio EOF 关闭不得丢弃已接受的 Stop"
    );
}
