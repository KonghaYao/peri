//! G5：`session/load` 生产路径不得在 `SharedSessions` 全局锁内做资源/DB I/O。
//!
//! 用一个在 `inspect_availability` 处阻塞的资源实现：load 阻塞期间主线程仍能
//! `try_lock` 成功，证明 load 没有持有全局 sessions 锁。

use super::*;
use std::sync::Arc;

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
            None,
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
            None,
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
