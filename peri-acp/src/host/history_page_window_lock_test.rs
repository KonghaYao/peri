//! G5：`session/load` 生产路径不得在 `SharedSessions` 全局锁内做资源/DB I/O。
//!
//! 用一个在 `inspect_availability` 处阻塞的资源实现：load 阻塞期间主线程仍能
//! `try_lock` 成功，证明 load 没有持有全局 sessions 锁。

use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use peri_acp_types::session_resources::{
    BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, CloseSettlement, ForkSnapshot,
    FrozenSnapshotBytes, FrozenState, NewSession, NewSessionDraft, PersistenceRecovery,
    RewindBoundary, RewindOutcome, SessionAvailability, SessionInitialization,
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult, SessionResources,
    SessionSnapshot,
};
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use tokio::sync::Notify;

struct RetryShutdownPool {
    settled: AtomicBool,
}

#[async_trait]
impl peri_acp_types::ports::McpPoolPort for RetryShutdownPool {
    async fn verify_shared_environment_close(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }

    async fn shutdown(&self) -> peri_acp_types::ports::McpPoolShutdownReport {
        if self.settled.load(Ordering::Acquire) {
            peri_acp_types::ports::McpPoolShutdownReport::Complete {
                settled_services: 1,
                failed_services: 0,
            }
        } else {
            peri_acp_types::ports::McpPoolShutdownReport::Incomplete {
                settled_services: 0,
                unfinished_services: 1,
                failed_services: 0,
            }
        }
    }

    fn snapshot(&self) -> Value {
        json!({})
    }
}

struct BlockingResources {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl SessionResources for BlockingResources {
    async fn inspect_availability(
        &self,
        _session: Option<&ThreadId>,
    ) -> SessionResourceResult<SessionAvailability> {
        self.started.notify_one();
        self.release.notified().await;
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }
    async fn is_session_closing(&self, _id: &ThreadId) -> SessionResourceResult<bool> {
        Ok(false)
    }

    async fn resolve_workspace(
        &self,
        _cwd: &std::path::Path,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        unimplemented!()
    }
    async fn validate_session(
        &self,
        _id: &ThreadId,
        _workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn finish_close(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn close_settlement(&self, _id: &ThreadId) -> SessionResourceResult<CloseSettlement> {
        unimplemented!()
    }
    async fn create_session(&self, _input: &NewSession) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn abandon_initialization(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn begin_initialization(
        &self,
        _draft: &NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionInitialization>> {
        unimplemented!()
    }
    async fn discard_incomplete_initialization(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn adopt_legacy_session(
        &self,
        _id: &ThreadId,
        _saved_cwd: &str,
        _workspace: &ResolvedWorkspace,
        _frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn load_session_snapshot(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<SessionSnapshot> {
        unimplemented!()
    }
    async fn load_frozen_state(&self, _id: &ThreadId) -> SessionResourceResult<FrozenState> {
        unimplemented!()
    }
    async fn load_session_binding(&self, _id: &ThreadId) -> SessionResourceResult<BindingState> {
        unimplemented!()
    }
    async fn validate_bound_workspace(
        &self,
        _id: &ThreadId,
        _check: BindingRecheck,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        unimplemented!()
    }
    async fn load_session_history(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<Vec<peri_acp_types::store::PersistedPayload>> {
        unimplemented!()
    }
    async fn load_session_meta(&self, _id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        unimplemented!()
    }
    async fn list_sessions(
        &self,
        _query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        unimplemented!()
    }
    async fn list_children(&self, _parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        unimplemented!()
    }
    async fn list_session_tree(&self, _root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        unimplemented!()
    }
    async fn append_history(
        &self,
        _id: &ThreadId,
        _payloads: &[peri_acp_types::store::PersistedPayload],
    ) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn append_reminder_if_absent(
        &self,
        _id: &ThreadId,
        _message_id: peri_acp_types::messages::MessageId,
        _reminder: &peri_acp_types::system_reminder::TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        unimplemented!()
    }
    async fn mark_session_closing(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn save_fork(&self, _fork: &ForkSnapshot) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn save_child(&self, _child: &ChildSnapshot) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn claim_child_resume(
        &self,
        _child: &ThreadId,
        _root: &ThreadId,
    ) -> SessionResourceResult<Box<dyn ChildResumeClaim>> {
        unimplemented!()
    }
    async fn apply_compaction(
        &self,
        _id: &ThreadId,
        _change: &peri_acp_types::store::CompactionChange,
    ) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn apply_message_projections(
        &self,
        _id: &ThreadId,
        _updates: &[(
            peri_acp_types::messages::MessageId,
            peri_acp_types::store::MessageFlags,
        )],
    ) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn rewind_history(
        &self,
        _id: &ThreadId,
        _boundary: RewindBoundary,
        _clear_excluded: &[peri_acp_types::messages::MessageId],
    ) -> SessionResourceResult<RewindOutcome> {
        unimplemented!()
    }
    async fn remove_history_entries(
        &self,
        _id: &ThreadId,
        _ids: &[peri_acp_types::messages::MessageId],
    ) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn update_session_meta(
        &self,
        _id: &ThreadId,
        _patch: &peri_acp_types::session_resources::SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn delete_session_tree(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!()
    }
    async fn recover_session_persistence(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        unimplemented!()
    }
    async fn drain_persistence(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!()
    }
}

async fn fixture() -> (
    AcpServerConfig,
    String,
    crate::host::SharedSessions,
    Arc<Notify>,
    Arc<Notify>,
) {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "unused-test-key",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    cfg.session_resources = Arc::new(BlockingResources {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    });
    (
        cfg,
        sid,
        Arc::new(tokio::sync::Mutex::new(sessions)),
        started,
        release,
    )
}

#[tokio::test]
#[serial]
async fn live_load_does_not_hold_the_global_sessions_lock_across_io() {
    let (cfg, sid, sessions, started, release) = fixture().await;
    let transport: Arc<dyn AcpTransport> = Arc::new(MockTransport::default());
    let params = json!({ "sessionId": sid });
    let cfg = Arc::new(cfg);
    let sessions_for_task = Arc::clone(&sessions);
    let task = tokio::spawn(async move {
        crate::host::requests::session_lifecycle::handle_load_shared(
            &params,
            &cfg,
            &sessions_for_task,
            &transport,
            crate::host::connection::ConnectionFacts::unnegotiated(),
        )
        .await
    });
    started.notified().await;
    assert!(
        sessions.try_lock().is_ok(),
        "load must release the global sessions lock before resource I/O"
    );
    release.notify_one();
    let result = task.await.unwrap();
    assert!(result.is_err(), "fake resource terminates the load");
}

#[tokio::test]
#[serial]
async fn cold_load_does_not_hold_the_global_sessions_lock_across_io() {
    let (cfg, sid, sessions, started, release) = fixture().await;
    {
        let mut states = sessions.lock().await;
        states.remove(&sid);
    }
    let transport: Arc<dyn AcpTransport> = Arc::new(MockTransport::default());
    let params = json!({ "sessionId": sid });
    let cfg = Arc::new(cfg);
    let sessions_for_task = Arc::clone(&sessions);
    let task = tokio::spawn(async move {
        crate::host::requests::session_lifecycle::handle_load_shared(
            &params,
            &cfg,
            &sessions_for_task,
            &transport,
            crate::host::connection::ConnectionFacts::unnegotiated(),
        )
        .await
    });
    started.notified().await;
    assert!(
        sessions.try_lock().is_ok(),
        "cold load must not hold the global sessions lock during preflight I/O"
    );
    release.notify_one();
    let result = task.await.unwrap();
    assert!(result.is_err(), "fake resource terminates the load");
}

#[tokio::test]
#[serial]
async fn resume_does_not_hold_the_global_sessions_lock_across_io() {
    let (cfg, sid, sessions, started, release) = fixture().await;
    let transport: Arc<dyn AcpTransport> = Arc::new(MockTransport::default());
    let params = json!({ "sessionId": sid });
    let cfg = Arc::new(cfg);
    let sessions_for_task = Arc::clone(&sessions);
    let task = tokio::spawn(async move {
        crate::host::requests::session_lifecycle::handle_resume_shared(
            &params,
            &cfg,
            &sessions_for_task,
            &transport,
        )
        .await
    });
    started.notified().await;
    assert!(
        sessions.try_lock().is_ok(),
        "resume must release the global sessions lock before resource I/O"
    );
    release.notify_one();
    let result = task.await.unwrap();
    assert!(result.is_err(), "fake resource terminates the resume");
}

#[tokio::test]
#[serial]
async fn fork_does_not_hold_the_global_sessions_lock_across_io() {
    let (cfg, sid, sessions, started, release) = fixture().await;
    let transport: Arc<dyn AcpTransport> = Arc::new(MockTransport::default());
    let params = json!({ "sessionId": sid });
    let cfg = Arc::new(cfg);
    let sessions_for_task = Arc::clone(&sessions);
    let task = tokio::spawn(async move {
        crate::host::requests::session_lifecycle::handle_fork_shared(
            &params,
            &cfg,
            &sessions_for_task,
            &transport,
        )
        .await
    });
    started.notified().await;
    assert!(
        sessions.try_lock().is_ok(),
        "fork must release the global sessions lock before resource I/O"
    );
    release.notify_one();
    let result = task.await.unwrap();
    assert!(result.is_err(), "fake resource terminates the fork");
}

#[tokio::test]
#[serial]
async fn unpublished_restore_cleanup_removes_manager_registration() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cwd = {
        let states = fixture.sessions.lock().await;
        states.get(&sid).unwrap().cwd.clone()
    };
    fixture
        .cfg
        .session_manager
        .ensure_session_with_task_manager(&sid, &cwd, None);
    assert!(fixture.cfg.session_manager.get_session(&sid).is_some());
    crate::host::requests::session_lifecycle::cleanup_unpublished_restore(&fixture.cfg, &sid, None)
        .await;
    assert!(
        fixture.cfg.session_manager.get_session(&sid).is_none(),
        "unpublished restore cleanup must remove the manager registration"
    );
}

/// [回归测试] 旧 publication generation 不得删除或关闭后来替换的新 state。
#[tokio::test]
async fn restore_publication_generation_mismatch_keeps_new_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let (cwd, old_generation) = {
        let states = fixture.sessions.lock().await;
        let state = states.get(&sid).unwrap();
        (state.cwd.clone(), state.session_generation)
    };
    fixture
        .cfg
        .session_manager
        .ensure_session_with_task_manager(&sid, &cwd, None);
    {
        let mut states = fixture.sessions.lock().await;
        states.get_mut(&sid).unwrap().session_generation = crate::host::next_session_generation();
    }
    let publication = crate::host::requests::session_lifecycle::RestorePublication {
        session_id: sid.clone(),
        session_generation: old_generation,
    };
    crate::host::requests::session_lifecycle::rollback_restore_publication(
        &fixture.cfg,
        &fixture.sessions,
        &publication,
    )
    .await
    .expect("stale publication 应为无操作");
    let states = fixture.sessions.lock().await;
    let state = states.get(&sid).expect("新 generation 必须保留");
    assert!(!state.closing, "旧 rollback 不得关闭新 state");
    drop(states);
    assert!(
        fixture.cfg.session_manager.get_session(&sid).is_some(),
        "旧 rollback 不得关闭新 manager"
    );
    fixture
        .cfg
        .session_manager
        .close_session(&sid)
        .await
        .unwrap();
}

/// [回归测试] shutdown 未确认时保留同一 owner 并拒绝 reload；显式 close 可重试收口。
#[tokio::test]
#[serial]
async fn restore_cleanup_incomplete_retains_owner_until_explicit_close() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "unused-test-key",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut raw_cfg = make_server_config(config, provider, &tmp).await;
    let cwd = tmp.path().canonicalize().unwrap();
    raw_cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_string_lossy().into_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let cfg = Arc::new(raw_cfg);
    let mut state_map = HashMap::new();
    let sid = register_session_with_history(&mut state_map, cwd.to_str().unwrap(), &cfg).await;
    let sessions = Arc::new(tokio::sync::Mutex::new(state_map));
    let mut environment =
        crate::host::workspace::SessionEnvironment::assemble(&cfg, cwd.to_str().unwrap(), &sid)
            .await
            .unwrap()
            .expect("fixture 应装配 environment");
    let pool = Arc::new(RetryShutdownPool {
        settled: AtomicBool::new(false),
    });
    Arc::get_mut(&mut environment).unwrap().cfg.mcp_pool = Some(pool.clone());
    let generation = {
        let mut states = sessions.lock().await;
        let state = states.get_mut(&sid).unwrap();
        state.environment = Some(environment.clone());
        state.session_generation
    };
    cfg.session_manager.ensure_session_with_task_manager(
        &sid,
        cwd.to_str().unwrap(),
        Some(environment.task_manager()),
    );
    let publication = crate::host::requests::session_lifecycle::RestorePublication {
        session_id: sid.clone(),
        session_generation: generation,
    };
    let error = crate::host::requests::session_lifecycle::rollback_restore_publication(
        &cfg,
        &sessions,
        &publication,
    )
    .await
    .expect_err("未确认 shutdown 必须显式失败");
    assert_eq!(error.code, -32010, "{error:?}");
    assert_eq!(
        error.data.as_ref().unwrap()["reason"],
        "session_cleanup_incomplete"
    );
    assert_eq!(error.data.as_ref().unwrap()["sessionId"], sid);
    {
        let states = sessions.lock().await;
        let state = states.get(&sid).expect("incomplete 必须保留 retry owner");
        assert!(state.closing, "incomplete state 必须拒绝再次 load");
        assert!(
            state
                .environment
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &environment)),
            "必须保留同一 environment owner"
        );
    }
    let transport: Arc<dyn AcpTransport> = Arc::new(MockTransport::default());
    let reload = crate::host::requests::session_lifecycle::handle_load_shared(
        &json!({ "sessionId": sid }),
        &cfg,
        &sessions,
        &transport,
        crate::host::connection::ConnectionFacts::unnegotiated(),
    )
    .await
    .expect_err("pending cleanup 不得再次 load");
    assert_eq!(reload.code, -32010, "{reload:?}");

    pool.settled.store(true, Ordering::Release);
    let mut states = sessions.lock().await;
    handle_request(
        "session/close",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut states,
        &transport,
    )
    .await
    .expect("显式 close 必须重试并完成 cleanup");
    assert!(!states.contains_key(&sid));
}
