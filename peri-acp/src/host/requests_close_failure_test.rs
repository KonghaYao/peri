//! #196 回归：关闭失败不得把会话永久置为不可用。
//!
//! 真实 `ServerLoop` + 真实存储门面：`session/close` 在取得生命周期锁之前只能中断在飞
//! turn（根令牌 + Cascade 子 agent）；`SessionState.closing` 闸门与 `pre_close_session`
//! 必须等到持久化关闭意图成立之后。锁超时 / 存储读取失败 / 关闭意图写入被拒时客户端
//! 收到关闭失败，会话必须保持开放、运行期未被取消，后续 `session/prompt` 仍能跑完整
//! 一轮（旧行为：先置 `closing` 并预关闭，失败后会话被永久拒绝）。

use super::*;
use crate::host::{PromptLocks, SharedSessions};
use crate::transport::AcpTransport;
use peri_acp_types::history_page::{
    HistoryCursor, HistoryForwardCursor, HistoryPage, HistoryPageBounds, HistorySegment,
};
use peri_acp_types::history_window::{
    HistoryBodyChunk, HistoryCommittedTail, HistoryReadTicket, HistorySource,
};
use peri_acp_types::messages::{MessageContent, MessageId};
use peri_acp_types::session::{
    EnqueueUserInputRequest, MqCancelOutcome, MqEnqueueOutcome, MqEntry, MqFilter, MqInput,
    MqReceiveOutcome, MqReceiveSelection,
};
use peri_acp_types::session_resources::{
    ActiveSessionSnapshot, BindingRecheck, ChildResumeClaim, ChildSnapshot, CloseSettlement,
    ForkSnapshot, FrozenSnapshotBytes, HistoryWindowPagePlan, HistoryWindowRows, NewSessionDraft,
    PersistenceRecovery, RewindBoundary, RewindCandidate, RewindOutcome, SessionAvailability,
    SessionHistoryWindowBounds, SessionInitialization, SessionMetaPatch, SessionResourceError,
    SessionResourceErrorKind, SessionResourceResult, SessionSnapshot, TerminalSettlementOutcome,
};
use peri_acp_types::store::{CompactionChange, MessageFlags};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{AgentStatus, ThreadId, ThreadMeta, WorkState};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};

// ── 故障注入门面：真实 SQLite 门面 + 单点失败 ────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum CloseFault {
    /// 无注入：成功路径不回归。
    None,
    /// `load_session_meta` 失败：关闭还没读到会话事实。
    ReadMeta,
    /// `mark_session_closing` 被拒且读回确认未落盘：关闭意图没有成立。
    WriteIntent,
}

/// 故障开关在会话建立**之后**才武装：`session/new` 自己也读会话事实。
struct FaultSwitch {
    kind: CloseFault,
    armed: std::sync::atomic::AtomicBool,
}

struct FlakyResources {
    inner: Arc<dyn SessionResources>,
    switch: Arc<FaultSwitch>,
}

macro_rules! forward_session_resources_except_close {
    ($( $method:ident ( $( $argument:ident : $kind:ty ),* ) -> $output:ty; )*) => {
        #[async_trait]
        impl SessionResources for FlakyResources {
            fn oauth_credentials(
                &self,
            ) -> Option<Arc<dyn peri_acp_types::oauth_credentials::OAuthCredentialPort>> {
                self.inner.oauth_credentials()
            }

            fn oauth_credentials_for_workspace(
                &self,
                workspace_id: peri_acp_types::workspace::WorkspaceId,
            ) -> Option<Arc<dyn peri_acp_types::oauth_credentials::OAuthCredentialPort>> {
                self.inner.oauth_credentials_for_workspace(workspace_id)
            }

            fn supports_history_window(&self) -> bool {
                self.inner.supports_history_window()
            }

            fn install_receive_gate(&self, gate: peri_acp_types::session::ReceiveGate) {
                self.inner.install_receive_gate(gate)
            }

            $(async fn $method(&self, $( $argument: $kind ),*) -> SessionResourceResult<$output> {
                self.inner.$method($( $argument ),*).await
            })*

            async fn load_session_meta(
                &self,
                id: &ThreadId,
            ) -> SessionResourceResult<ThreadMeta> {
                if self.switch.kind == CloseFault::ReadMeta
                    && self.switch.armed.load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err(SessionResourceError::new(
                        SessionResourceErrorKind::Unavailable {
                            detail: "injected: session meta read failed".to_owned(),
                        },
                    ));
                }
                self.inner.load_session_meta(id).await
            }

            async fn mark_session_closing(&self, id: &ThreadId) -> SessionResourceResult<()> {
                if self.switch.kind == CloseFault::WriteIntent
                    && self.switch.armed.load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err(SessionResourceError::new(
                        SessionResourceErrorKind::Conflict {
                            detail: "injected: closing intent write refused".to_owned(),
                        },
                    ));
                }
                self.inner.mark_session_closing(id).await
            }
        }
    };
}

forward_session_resources_except_close! {
    inspect_availability(session: Option<&ThreadId>) -> SessionAvailability;
    resolve_workspace(cwd: &std::path::Path) -> ResolvedWorkspace;
    validate_session(id: &ThreadId, workspace: &ResolvedWorkspace) -> ();
    finish_close(id: &ThreadId) -> ();
    close_settlement(id: &ThreadId) -> CloseSettlement;
    create_session(input: &NewSession) -> ();
    abandon_initialization(id: &ThreadId) -> ();
    begin_initialization(draft: &NewSessionDraft) -> Arc<dyn SessionInitialization>;
    discard_incomplete_initialization(id: &ThreadId) -> ();
    adopt_legacy_session(id: &ThreadId, saved_cwd: &str, workspace: &ResolvedWorkspace, frozen: &FrozenSnapshotBytes) -> ();
    relink_unbound_session(id: &ThreadId) -> ResolvedWorkspace;
    load_session_snapshot(id: &ThreadId) -> SessionSnapshot;
    load_fork_snapshot(id: &ThreadId) -> SessionSnapshot;
    load_active_snapshot(id: &ThreadId) -> ActiveSessionSnapshot;
    load_frozen_state(id: &ThreadId) -> FrozenState;
    load_session_binding(id: &ThreadId) -> BindingState;
    validate_bound_workspace(id: &ThreadId, check: BindingRecheck) -> ResolvedWorkspace;
    load_session_history(id: &ThreadId) -> Vec<PersistedPayload>;
    load_rewind_candidates(id: &ThreadId, limit: usize) -> Vec<RewindCandidate>;
    load_history_page(id: &ThreadId, cursor: Option<&HistoryCursor>, limit: usize, byte_budget: usize) -> HistoryPage;
    load_history_segment(id: &ThreadId, cursor: Option<&HistoryForwardCursor>, limit: usize, byte_budget: usize) -> HistorySegment;
    history_page_bounds(id: &ThreadId) -> HistoryPageBounds;
    history_window_bounds(id: &ThreadId) -> SessionHistoryWindowBounds;
    load_history_window_page(id: &ThreadId, plan: &HistoryWindowPagePlan) -> HistoryWindowRows;
    seek_history_window(id: &ThreadId, source: HistorySource, anchor: MessageId, before_limit: usize, after_limit: usize, byte_budget: usize, committed_tail: &HistoryCommittedTail) -> HistoryWindowRows;
    load_history_body_chunk(id: &ThreadId, source: HistorySource, entry_id: MessageId, offset: u64, max_bytes: u32, committed_tail: Option<&HistoryCommittedTail>, read_ticket: &HistoryReadTicket) -> HistoryBodyChunk;
    load_entry_positions(id: &ThreadId) -> HashMap<MessageId, u64>;
    inherited_archive_root(id: &ThreadId, source: &ThreadId, anchor: MessageId) -> peri_acp_types::context_archive::ArchiveRoot;
    read_archive_metadata(id: &ThreadId, query: &peri_acp_types::context_archive::ArchiveMetadataQuery) -> peri_acp_types::context_archive::ArchiveMetadataPage;
    read_archive_chunk(id: &ThreadId, query: &peri_acp_types::context_archive::ArchiveChunkQuery) -> peri_acp_types::context_archive::ArchiveChunk;
    read_archive_chunks(id: &ThreadId, batch: &peri_acp_types::context_archive::ArchiveChunkBatch) -> Vec<peri_acp_types::context_archive::ArchiveChunk>;
    load_message_flags(id: &ThreadId, ids: &[MessageId]) -> HashMap<MessageId, MessageFlags>;
    load_recovery_scope(id: &ThreadId) -> peri_acp_types::recovery::RecoveryScope;
    load_interrupted_threads(id: &ThreadId) -> Vec<peri_acp_types::recovery::InterruptedThread>;
    session_environment_id(id: &ThreadId) -> Option<String>;
    session_workspace_id(id: &ThreadId) -> Option<peri_acp_types::workspace::WorkspaceId>;
    list_machines() -> Vec<peri_acp_types::workspace::MachineInfo>;
    list_workspaces(machine_id: &str) -> Vec<peri_acp_types::workspace::WorkspaceInfo>;
    rename_machine(machine_id: &str, name: &str) -> ();
    list_sessions(query: &ScopedThreadQuery) -> ScopedThreadPage;
    list_archived_sessions(query: &ScopedThreadQuery) -> ScopedThreadPage;
    set_session_archived(id: &ThreadId, archived: bool) -> ();
    list_children(parent: &ThreadId) -> Vec<ThreadMeta>;
    list_session_tree(root: &ThreadId) -> Vec<ThreadMeta>;
    append_history(id: &ThreadId, payloads: &[PersistedPayload]) -> ();
    append_reminder_if_absent(id: &ThreadId, message_id: MessageId, reminder: &TrustedSystemReminder) -> bool;
    is_session_closing(id: &ThreadId) -> bool;
    save_fork(fork: &ForkSnapshot) -> ();
    save_child(child: &ChildSnapshot) -> ();
    claim_child_resume(child: &ThreadId, root: &ThreadId) -> Box<dyn ChildResumeClaim>;
    apply_compaction(id: &ThreadId, change: &CompactionChange) -> ();
    apply_message_projections(id: &ThreadId, updates: &[(MessageId, MessageFlags)]) -> ();
    rewind_history(id: &ThreadId, boundary: RewindBoundary, clear_excluded: &[MessageId]) -> RewindOutcome;
    remove_history_entries(id: &ThreadId, ids: &[MessageId]) -> ();
    update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> ();
    set_work_state(id: &ThreadId, state: WorkState) -> ();
    settle_work_state_after_turn(id: &ThreadId) -> bool;
    set_context_usage(id: &ThreadId, tokens: Option<u64>) -> ();
    delete_session_tree(id: &ThreadId) -> ();
    recover_session_persistence(id: &ThreadId) -> PersistenceRecovery;
    drain_persistence(id: &ThreadId) -> ();
    mq_enqueue(thread_id: &ThreadId, input: MqInput) -> MqEnqueueOutcome;
    mq_query(thread_id: &ThreadId, filter: MqFilter) -> Vec<MqEntry>;
    mq_cancel(thread_id: &ThreadId, id: MessageId) -> MqCancelOutcome;
    mq_receive(thread_id: &ThreadId, selection: MqReceiveSelection) -> MqReceiveOutcome;
    mq_clear(thread_id: &ThreadId) -> ();
    mq_history_contains(thread_id: &ThreadId, id: MessageId) -> bool;
    finalize_child_terminal(child: &ThreadId, status: AgentStatus, work_state: WorkState, target: &ThreadId, message: MqInput) -> MqEnqueueOutcome;
    settle_terminal_without_result(child: &ThreadId, status: AgentStatus, work_state: WorkState) -> TerminalSettlementOutcome;
    settle_retained_mq(thread_id: &ThreadId, reminders: &[peri_acp_types::session::RetainedReminder], rows: &[MqEntry]) -> ();
}

// ── 真实 ServerLoop 夹具 ────────────────────────────────────────────────────

struct CloseHarness {
    cfg: Arc<AcpServerConfig>,
    sessions: SharedSessions,
    locks: PromptLocks,
    client: Arc<crate::transport::mpsc::MpscClientTransport>,
    sid: String,
    switch: Arc<FaultSwitch>,
    server: tokio::task::JoinHandle<()>,
    _mock_server: mockito::ServerGuard,
    _mock: mockito::Mock,
    _directory: tempfile::TempDir,
}

impl CloseHarness {
    async fn new(fault: CloseFault) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_str().unwrap().to_owned();
        let mut mock_server = mockito::Server::new_async().await;
        let mock = mock_server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(
                "data: {\"id\":\"close-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
            .create_async()
            .await;
        let mut provider = make_provider_config("local", "openai", "test", "gpt-test");
        provider.base_url = mock_server.url();
        let config = make_peri_config_with_provider(provider);
        let provider = LlmProvider::from_config(&config).unwrap();
        let inner = peri_agent::resources::open_session_resources_with(Some(
            directory.path().join("threads.db"),
        ))
        .await
        .unwrap();
        let switch = Arc::new(FaultSwitch {
            kind: fault,
            armed: std::sync::atomic::AtomicBool::new(false),
        });
        let resources: Arc<dyn SessionResources> = Arc::new(FlakyResources {
            inner,
            switch: Arc::clone(&switch),
        });
        let cfg = Arc::new(build_server_config(config, provider, &directory, resources).await);

        // 生产同款 `session/new`：发布 `SessionState` + 冻结上下文 + 运行期登记。
        let mut sessions = HashMap::new();
        let recorder: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
        let created = handle_request(
            "session/new",
            &json!({ "cwd": cwd }),
            &cfg,
            &mut sessions,
            &recorder,
        )
        .await
        .expect("session/new 必须成功");
        let sid = created["sessionId"]
            .as_str()
            .expect("session/new 必须返回会话 id")
            .to_owned();
        let sessions: SharedSessions = Arc::new(tokio::sync::Mutex::new(sessions));
        let locks: PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

        let (client, server_transport) = crate::transport::mpsc::mpsc_transport_pair();
        let client = Arc::new(client);
        let server_transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(server_transport);
        let server = {
            let cfg = Arc::clone(&cfg);
            let sessions = Arc::clone(&sessions);
            let locks = Arc::clone(&locks);
            tokio::spawn(async move {
                let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
                let cont_tx = Arc::new(cont_tx);
                let connection = Arc::new(tokio::sync::Mutex::new(
                    crate::host::connection::ConnectionContext::new(false),
                ));
                let cancellation = tokio_util::sync::CancellationToken::new();
                let recovery = recovery_for(&cfg, &sessions, &cont_tx);
                crate::host::server_loop::ServerLoop {
                    transport: &server_transport,
                    cfg: &cfg,
                    sessions: &sessions,
                    prompt_locks: &locks,
                    cont_tx: &cont_tx,
                    connection: &connection,
                    connection_cancellation: &cancellation,
                    recovery: &recovery,
                }
                .run()
                .await;
            })
        };

        Self {
            cfg,
            sessions,
            locks,
            client,
            sid,
            switch,
            server,
            _mock_server: mock_server,
            _mock: mock,
            _directory: directory,
        }
    }

    /// 会话已建立：从这里开始注入关闭路径的存储故障。
    fn arm(&self) {
        self.switch
            .armed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    async fn close(&self) -> Result<Value, AcpError> {
        self.client
            .send_request("session/close", json!({ "sessionId": self.sid }))
            .await
    }

    async fn prompt(&self) -> Result<Value, AcpError> {
        self.client
            .send_request(
                "session/prompt",
                json!({
                    "sessionId": self.sid,
                    "prompt": [{ "type": "text", "text": "继续" }],
                }),
            )
            .await
    }

    /// 会话必须仍是可续聊的热态：准入闸门未置位、运行期令牌未被取消
    /// （`pre_close_session` 的不可逆指纹），且会话仍在 `sessions` 里。
    async fn assert_still_open(&self) {
        {
            let states = self.sessions.lock().await;
            let state = states.get(&self.sid).expect("关闭失败不得移除会话");
            assert!(
                !state.closing,
                "关闭失败且无持久化意图时会话必须保持开放，实际 closing=true"
            );
        }
        let runtime = self
            .cfg
            .session_manager
            .get_session(&self.sid)
            .expect("关闭失败不得移除会话运行期");
        assert!(
            !runtime.cancel_token.is_cancelled(),
            "关闭失败路径不得执行 pre_close_session（会话运行期令牌已被永久取消）"
        );
    }

    /// 失败关闭之后同一会话仍能跑完整一轮。
    async fn assert_prompt_succeeds(&self) {
        let reply = self
            .prompt()
            .await
            .unwrap_or_else(|error| panic!("关闭失败后会话必须仍可续聊，实际拒绝：{error:?}"));
        assert_eq!(
            reply["stopReason"], "end_turn",
            "prompt 必须完整收尾：{reply}"
        );
    }
}

impl Drop for CloseHarness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// 锁超时：客户端收到关闭失败；会话保持开放且可续聊。
#[tokio::test]
async fn close_lock_timeout_keeps_session_open_and_promptable() {
    let harness = CloseHarness::new(CloseFault::None).await;
    // 模拟在飞 turn：同一把 per-session 生命周期锁被占住超过关闭预算。
    let lock = {
        let mut locks = harness.locks.lock().await;
        Arc::clone(
            locks
                .entry(harness.sid.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    };
    let guard = lock.lock().await;
    let error = harness.close().await.expect_err("锁被占住时关闭必须失败");
    assert_eq!(error.code, -32010, "{error:?}");
    assert!(
        error.message.contains("active execution has not stopped"),
        "失败原因必须保留：{error:?}"
    );
    drop(guard);

    harness.assert_still_open().await;
    harness.assert_prompt_succeeds().await;
}

/// 存储读取失败：关闭在读到会话事实之前失败，会话保持开放且可续聊。
#[tokio::test]
async fn close_meta_read_failure_keeps_session_open_and_promptable() {
    let harness = CloseHarness::new(CloseFault::ReadMeta).await;
    harness.arm();
    let error = harness
        .close()
        .await
        .expect_err("会话事实读取失败时关闭必须失败");
    assert!(
        error.message.contains("injected: session meta read failed"),
        "原始读取错误必须保留：{error:?}"
    );

    harness.assert_still_open().await;
    harness.assert_prompt_succeeds().await;
}

/// 关闭意图写入被拒（读回确认未落盘）：会话保持开放且可续聊。
#[tokio::test]
async fn close_intent_write_refusal_keeps_session_open_and_promptable() {
    let harness = CloseHarness::new(CloseFault::WriteIntent).await;
    harness.arm();
    let error = harness
        .close()
        .await
        .expect_err("关闭意图写入被拒时关闭必须失败");
    assert!(
        error
            .message
            .contains("injected: closing intent write refused"),
        "原始写入错误必须保留：{error:?}"
    );

    harness.assert_still_open().await;
    harness.assert_prompt_succeeds().await;
}

/// 成功路径不回归：关闭成功即移除内存会话，后续 prompt 被拒（session not found）。
#[tokio::test]
async fn close_success_still_removes_session() {
    let harness = CloseHarness::new(CloseFault::None).await;
    harness.close().await.expect("空闲会话关闭必须成功");
    assert!(
        !harness.sessions.lock().await.contains_key(&harness.sid),
        "关闭成功后内存会话必须移除"
    );
    let error = harness
        .prompt()
        .await
        .expect_err("已关闭会话不得再接受 prompt");
    assert!(
        error.message.contains("session not found") || error.message.contains("Session is closing"),
        "已关闭会话的拒绝原因必须明确：{error:?}"
    );
}

/// 关闭等锁期间，已排队的 prompt 不得抢锁先开一轮：否则 close 会被一个与本请求
/// 无关的新 turn 拖到超时，而 close 明明已经中断了到达时在飞的那一轮。
///
/// 判据：prompt 被准入闸门拒绝（不得成功开轮），close 成功完成。
#[tokio::test]
async fn queued_prompt_cannot_overtake_a_waiting_close() {
    let harness = CloseHarness::new(CloseFault::None).await;
    let lock = {
        let mut locks = harness.locks.lock().await;
        Arc::clone(
            locks
                .entry(harness.sid.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    };
    let guard = lock.lock().await;

    // prompt 先到达并通过准入门，排在生命周期锁上（用户 prompt 会先推进代际）。
    let initial_epoch = {
        let states = harness.sessions.lock().await;
        states[&harness.sid].continuation_epoch
    };
    let prompt = {
        let client = Arc::clone(&harness.client);
        let sid = harness.sid.clone();
        tokio::spawn(async move {
            client
                .send_request(
                    "session/prompt",
                    json!({
                        "sessionId": sid,
                        "prompt": [{ "type": "text", "text": "排队中" }],
                    }),
                )
                .await
        })
    };
    wait_until(&harness, "prompt 进入准入门并排队", |state| {
        state.continuation_epoch > initial_epoch
    })
    .await;

    // close 随后到达：置等锁闸门后阻塞在同一把锁上。
    let close = {
        let client = Arc::clone(&harness.client);
        let sid = harness.sid.clone();
        tokio::spawn(async move {
            client
                .send_request("session/close", json!({ "sessionId": sid }))
                .await
        })
    };
    wait_until(&harness, "close 置等锁闸门", |state| {
        state.close_in_flight > 0
    })
    .await;

    // 在飞 turn 结束：排队的 prompt 先拿到锁，但必须被闸门拒绝。
    drop(guard);
    let prompt_result = prompt.await.expect("prompt 任务不得 panic");
    let close_result = close.await.expect("close 任务不得 panic");
    assert!(
        prompt_result.is_err(),
        "关闭等锁期间排队的 prompt 不得抢跑成一轮，实际成功：{prompt_result:?}"
    );
    assert!(
        close_result.is_ok(),
        "close 不得被排队的 prompt 拖到超时：{close_result:?}"
    );
    assert!(
        !harness.sessions.lock().await.contains_key(&harness.sid),
        "关闭成功后内存会话必须移除"
    );
}

/// 已通过准入、但尚未开跑的轮次必须能被 close 取消：令牌在准入段与关闭闸门
/// 同锁发布，close 取消后由执行前复核终止本轮（#196 P2）。
///
/// 判据：close 不超时且成功移除会话；prompt 被拒绝，不得开跑第二轮。
#[tokio::test]
async fn admitted_prompt_is_cancelled_by_close_before_it_runs() {
    let harness = CloseHarness::new(CloseFault::None).await;
    // 先跑完一轮，令牌在收尾时清除；第二轮才暴露「准入后、开跑前」的窗口。
    let first = harness.prompt().await.expect("第一轮必须成功");
    assert_eq!(first["stopReason"], "end_turn", "{first}");
    assert!(
        harness.sessions.lock().await[&harness.sid]
            .cancel_token
            .is_none(),
        "正常收尾必须清除本轮令牌"
    );

    let gate =
        crate::host::prompt_dispatch::admission_gate::install(&format!("{}:admitted", harness.sid));
    let prompt = {
        let client = Arc::clone(&harness.client);
        let sid = harness.sid.clone();
        tokio::spawn(async move {
            client
                .send_request(
                    "session/prompt",
                    json!({
                        "sessionId": sid,
                        "prompt": [{ "type": "text", "text": "第二轮" }],
                    }),
                )
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.entered.notified())
        .await
        .expect("第二轮 prompt 必须到达准入后的暂停点");

    let close = {
        let client = Arc::clone(&harness.client);
        let sid = harness.sid.clone();
        tokio::spawn(async move {
            client
                .send_request("session/close", json!({ "sessionId": sid }))
                .await
        })
    };
    // close 置闸门并取消本轮令牌后，再放行暂停的 prompt。
    wait_until(&harness, "close 置等锁闸门", |state| {
        state.close_in_flight > 0
    })
    .await;
    gate.release.notify_one();

    let prompt_result = tokio::time::timeout(std::time::Duration::from_secs(10), prompt)
        .await
        .expect("prompt 任务不得挂起")
        .expect("prompt 任务不得 panic");
    let close_result = tokio::time::timeout(std::time::Duration::from_secs(10), close)
        .await
        .expect("close 不得等满 5s 预算")
        .expect("close 任务不得 panic");
    let error = prompt_result.expect_err("已准入但未开跑的轮次必须被 close 取消");
    assert_eq!(error.code, -32010, "{error:?}");
    assert!(
        error.message.contains("Session is closing"),
        "拒绝原因必须指明关闭：{error:?}"
    );
    close_result.expect("close 不得被已准入的 prompt 拖到超时");
    assert!(
        !harness.sessions.lock().await.contains_key(&harness.sid),
        "关闭成功后内存会话必须移除"
    );
}

/// P2 复审：已准入、尚未 attach 的队列输入被 Stop 撤销后，`run_prompt` 在 attach
/// 前返回 `Ok(Null)`。这是成功空操作，但同样没有执行器接管准入段发布的令牌；
/// 若不回滚，close 会读到残留令牌并报 "prompt is still active"（#196）。
///
/// 判据：Stop 后放行到 `Ok(Null)`，`cancel_token` 必须清除，随后 close 成功。
#[tokio::test]
async fn stop_revoked_queued_input_rolls_back_admission_token() {
    let harness = CloseHarness::new(CloseFault::None).await;
    let transport: Arc<dyn AcpTransport> = Arc::new(MockTransport::default());
    let mailbox = crate::host::user_input::ensure_mailbox(&harness.sid, &harness.cfg, &transport)
        .await
        .expect("队列会话必须能建立 mailbox");
    let generation = mailbox.generation().to_owned();
    mailbox
        .enqueue(&EnqueueUserInputRequest {
            session_id: harness.sid.clone(),
            generation,
            command_id: "enqueue-before-stop".into(),
            input_id: "00000000-0000-0000-0000-000000000181".into(),
            content: MessageContent::text("被 Stop 撤销的队列输入"),
            original_draft: "被 Stop 撤销的队列输入".into(),
        })
        .await
        .expect("队列输入应入队");

    // 暂停在准入段发布令牌之后、attach 之前。
    let gate =
        crate::host::prompt_dispatch::admission_gate::install(&format!("{}:admitted", harness.sid));
    let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::host::user_input::schedule_mailbox(
        &harness.sid,
        &harness.sessions,
        &harness.locks,
        &harness.cfg,
        &transport,
        &Arc::new(cont_tx),
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.entered.notified())
        .await
        .expect("队列输入必须到达准入后的暂停点");

    // Stop 撤销尚未 attach 的预约：active 没有令牌，直接移除。
    harness
        .client
        .send_notification("session/cancel", json!({ "sessionId": harness.sid }))
        .await
        .expect("session/cancel 通知必须送达");
    wait_until(&harness, "Stop 取消准入令牌", |state| {
        state
            .cancel_token
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
    })
    .await;

    gate.release.notify_one();
    // attach 返回 false 后 run_prompt 返回 Ok(Null)：空轮次不得留下在飞令牌。
    wait_until(&harness, "空轮次清理准入令牌", |state| {
        state.cancel_token.is_none()
    })
    .await;

    let close_result = harness.close().await;
    if let Err(error) = &close_result {
        assert!(
            !error.message.contains("prompt is still active"),
            "空轮次不得阻塞 close：{error:?}"
        );
    }
    close_result.expect("空轮次清理后 close 必须成功");
    assert!(
        !harness.sessions.lock().await.contains_key(&harness.sid),
        "关闭成功后内存会话必须移除"
    );
}

/// 轮询会话态直到条件成立（有界等待，避免用例自身无限阻塞）。
async fn wait_until(
    harness: &CloseHarness,
    label: &str,
    mut ready: impl FnMut(&SessionState) -> bool,
) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let matched = {
            let states = harness.sessions.lock().await;
            states.get(&harness.sid).is_some_and(&mut ready)
        };
        if matched {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "等待超时：{label}");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
