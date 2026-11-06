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
use peri_acp_types::messages::MessageId;
use peri_acp_types::session::{
    MqCancelOutcome, MqEnqueueOutcome, MqEntry, MqFilter, MqInput, MqReceiveOutcome,
    MqReceiveSelection,
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
