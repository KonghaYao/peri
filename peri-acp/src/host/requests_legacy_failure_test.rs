use crate::host::requests::tests::HomeDirGuard;
use crate::host::requests::tests::MockTransport;
use crate::provider::LlmProvider;
use async_trait::async_trait;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::BindingRecheck;
use peri_acp_types::session_resources::BindingState;
use peri_acp_types::session_resources::ChildResumeClaim;
use peri_acp_types::session_resources::ChildSnapshot;
use peri_acp_types::session_resources::CloseSettlement;
use peri_acp_types::session_resources::ForkSnapshot;
use peri_acp_types::session_resources::FrozenSnapshotBytes;
use peri_acp_types::session_resources::FrozenState;
use peri_acp_types::session_resources::NewSession;
use peri_acp_types::session_resources::NewSessionDraft;
use peri_acp_types::session_resources::PersistenceRecovery;
use peri_acp_types::session_resources::RewindBoundary;
use peri_acp_types::session_resources::RewindOutcome;
use peri_acp_types::session_resources::SessionAvailability;
use peri_acp_types::session_resources::SessionInitialization;
use peri_acp_types::session_resources::SessionMetaPatch;
use peri_acp_types::session_resources::SessionResourceError;
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::session_resources::SessionSnapshot;
use peri_acp_types::store::CompactionChange;
use peri_acp_types::store::MessageFlags;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::store::ThreadStore;
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::thread::ThreadMeta;
use peri_acp_types::workspace::ResolvedWorkspace;
use peri_acp_types::workspace::ScopedThreadPage;
use peri_acp_types::workspace::ScopedThreadQuery;
use serde_json::json;
use serial_test::serial;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
#[serial]
async fn bound_restore_preparation_does_not_request_active_history() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let config = crate::host::requests::tests::make_peri_config_with_provider(
        crate::host::requests::tests::make_provider_config("test", "openai", "test", "model"),
    );
    let (mut cfg, _bridge) = crate::host::requests::tests::make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let mut sessions = HashMap::new();
    let sid = crate::host::requests::tests::register_session_with_history(
        &mut sessions,
        tmp.path().to_str().unwrap(),
        &cfg,
    )
    .await;
    // 生效的历史读取走 `cfg.controller` 持有的 resource port，不是 `cfg.session_resources`；
    // 只替换后者时断言会空转。两个入口装同一份注入：任一历史读取都会失败。
    let failing: Arc<dyn SessionResources> = Arc::new(FailingHistoryLoad {
        inner: cfg.session_resources.clone(),
    });
    cfg.session_resources = Arc::clone(&failing);
    cfg.controller = Arc::new(peri_controller::Controller::new(failing));
    // 正控：同一 port 的 active history 读取确实失败，否则「未读历史」没有区分力。
    assert!(
        crate::dispatch::load_active_session_payloads(cfg.controller.as_ref(), &sid)
            .await
            .is_err()
    );
    crate::host::requests::session_lifecycle::restore::prepare_existing(
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
    )
    .await
    .expect("preparing a bound session should only need its binding");
}

/// M5 缺陷 2：接纳成功但后续 load（history 读取）失败时，也必须排空已激活环境；
/// 写一次接纳本身不回滚。
#[tokio::test]
#[serial]
async fn legacy_post_adoption_load_failure_drains_environment_without_rollback() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("legacy-post-adoption");
    std::fs::create_dir(&cwd).unwrap();
    let config = crate::host::requests::tests::make_peri_config_with_provider(
        crate::host::requests::tests::make_provider_config("test", "openai", "test", "model"),
    );
    let (mut cfg, bridge) = crate::host::requests::tests::make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    // 注入：controller 的历史读取失败——接纳（binding/frozen write-once）照常完成，
    // 失败落在发布前的 payload 读取；失败路径必须排空已激活的 bootstrap 环境。
    let failing = Arc::new(FailingHistoryLoad {
        inner: Arc::clone(&cfg.session_resources),
    });
    cfg.controller = Arc::new(peri_controller::Controller::new(failing));
    let id = crate::host::requests::tests::legacy_tests::old_thread(&bridge, &cwd).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let error = crate::host::requests::handle_request(
        "session/load",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect_err("history 读取失败不得发布会话");
    assert!(
        error.message.contains("session history load failed"),
        "原始错误原因必须保留: {error:?}"
    );
    assert!(sessions.is_empty(), "失败不得发布会话");
    // 接纳是 write-once 的原子事实：不回滚；环境必须已排空。
    assert!(
        bridge.load_frozen_snapshot(&id).await.unwrap().is_some(),
        "write-once 接纳事实保留（不回滚已提交的 frozen）"
    );
    let shutdowns = crate::host::workspace::take_shutdown_observations();
    assert!(
        shutdowns.iter().any(|session| session == &id),
        "接纳后的 load 失败必须排空已激活环境（观察到的 shutdown: {shutdowns:?}）"
    );
}

// ── M5 注入夹具：历史读取失败（其余逐项转发真实门面）──────────────────────────
//
// 与 `host/session_io_test.rs` 的 `BlockedWorkQuery` 同模式：真实门面 + 单点故障
// 注入，不能用内存替身伪造接纳事实。只有 `load_active_snapshot` 返回可解释的冲突
// 错误，其余方法逐项转发同一库句柄的门面。
//
// 注入面必须是**发布路径真正调用的读取**：接纳前的 `prepare_for_restore` /
// `load_frozen_bytes` 走 `cfg.session_resources`（真实门面），发布前的有效历史读取
// 走 `cfg.controller`（本夹具）——单点正落在接纳完成与发布之间。

/// 单点故障：`load_active_snapshot` 一律返回冲突错误（发布前的有效历史读取）。
struct FailingHistoryLoad {
    inner: Arc<dyn SessionResources>,
}

macro_rules! forward_session_resources_except_history {
    ($( $method:ident ( $( $argument:ident : $kind:ty ),* ) -> $output:ty; )*) => {
        #[async_trait]
        impl SessionResources for FailingHistoryLoad {
            $(async fn $method(&self, $( $argument: $kind ),*) -> SessionResourceResult<$output> {
                self.inner.$method($( $argument ),*).await
            })*

            async fn load_active_snapshot(
                &self,
                _id: &ThreadId,
            ) -> SessionResourceResult<peri_acp_types::session_resources::ActiveSessionSnapshot>
            {
                Err(SessionResourceError::conflict(
                    "injected: session history load failed",
                ))
            }
        }
    };
}

forward_session_resources_except_history! {
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
    load_session_snapshot(id: &ThreadId) -> SessionSnapshot;
    load_frozen_state(id: &ThreadId) -> FrozenState;
    load_session_history(id: &ThreadId) -> Vec<PersistedPayload>;
    load_session_binding(id: &ThreadId) -> BindingState;
    validate_bound_workspace(id: &ThreadId, check: BindingRecheck) -> ResolvedWorkspace;
    load_session_meta(id: &ThreadId) -> ThreadMeta;
    list_sessions(query: &ScopedThreadQuery) -> ScopedThreadPage;
    list_children(parent: &ThreadId) -> Vec<ThreadMeta>;
    list_session_tree(root: &ThreadId) -> Vec<ThreadMeta>;
    append_history(id: &ThreadId, payloads: &[PersistedPayload]) -> ();
    append_reminder_if_absent(id: &ThreadId, message_id: MessageId, reminder: &TrustedSystemReminder) -> bool;
    mark_session_closing(id: &ThreadId) -> ();
    is_session_closing(id: &ThreadId) -> bool;
    save_fork(fork: &ForkSnapshot) -> ();
    save_child(child: &ChildSnapshot) -> ();
    claim_child_resume(child: &ThreadId, root: &ThreadId) -> Box<dyn ChildResumeClaim>;
    apply_compaction(id: &ThreadId, change: &CompactionChange) -> ();
    apply_message_projections(id: &ThreadId, updates: &[(MessageId, MessageFlags)]) -> ();
    rewind_history(id: &ThreadId, boundary: RewindBoundary, clear_excluded: &[MessageId]) -> RewindOutcome;
    remove_history_entries(id: &ThreadId, ids: &[MessageId]) -> ();
    update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> ();
    delete_session_tree(id: &ThreadId) -> ();
    recover_session_persistence(id: &ThreadId) -> PersistenceRecovery;
    drain_persistence(id: &ThreadId) -> ();
}
