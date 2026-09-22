use super::*;
use peri_acp_types::workspace::{
    ReadOnlyAdmission, RecoveryRequiredDetails, ResetDirtyRequest, WorkspaceError,
    WorkspaceErrorData,
};
use peri_acp_types::PeriCaps;

/// 读取本次准入的只读原因；`None` 表示本次取得了执行所有权。
fn read_only(response: &Value) -> Option<ReadOnlyAdmission> {
    response
        .pointer("/_meta/peri.sessionWorkspaceV1/read_only")
        .map(|value| serde_json::from_value(value.clone()).expect("read-only marker is typed"))
}

/// 只读准入必须是 dirty 原因，并复述精确代际。
fn read_only_dirty(response: &Value) -> RecoveryRequiredDetails {
    match read_only(response) {
        Some(ReadOnlyAdmission::RecoveryRequired(target)) => target,
        other => panic!("expected recovery-required read-only admission, got {other:?}"),
    }
}

struct Fixture {
    cfg: AcpServerConfig,
    tmp: tempfile::TempDir,
    id: String,
    sessions: HashMap<String, SessionState>,
    transport: Arc<dyn crate::transport::AcpTransport>,
}

impl Fixture {
    /// 建好一个绑定会话并只释放 owner（不伪造正常收尾），使其进入 dirty。
    async fn dirty() -> (Self, RecoveryRequiredDetails) {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = PeriConfig::default();
        config.config.active_alias = "sonnet".into();
        config.config.providers = vec![ProviderConfig {
            id: "local".into(),
            provider_type: "openai".into(),
            api_key: uuid::Uuid::new_v4().to_string(),
            base_url: "http://127.0.0.1:1".into(),
            models: ProviderModels {
                sonnet: "fixture".into(),
                ..Default::default()
            },
            ..Default::default()
        }];
        let provider = LlmProvider::from_config(&config).unwrap();
        let cfg = make_server_config(config, provider, &tmp).await;
        let mut sessions = HashMap::new();
        let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
        let created = handle_request(
            "session/new",
            &json!({"cwd":tmp.path()}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        let id = created["sessionId"].as_str().unwrap().to_owned();
        sessions.clear();
        let fixture = Self {
            cfg,
            tmp,
            id,
            sessions,
            transport,
        };
        let target = fixture.dirty_target().await;
        (fixture, target)
    }

    fn params(&self) -> Value {
        json!({"sessionId":self.id,"cwd":self.tmp.path()})
    }

    async fn load(&mut self, params: &Value) -> Result<Value, AcpError> {
        handle_request(
            "session/load",
            params,
            &self.cfg,
            &mut self.sessions,
            &self.transport,
        )
        .await
    }

    async fn reset(&mut self, target: &RecoveryRequiredDetails) -> Result<Value, AcpError> {
        let ack = serde_json::to_value(ResetDirtyRequest {
            target: target.clone(),
            accept_risk: true,
        })
        .unwrap();
        handle_request(
            "peri/session_reset_dirty",
            &ack,
            &self.cfg,
            &mut self.sessions,
            &self.transport,
        )
        .await
    }

    /// 经存储直读当前 dirty 代次：不经 ACP 准入，也就不会改动存储。
    ///
    /// 准入路径对未协商恢复能力的连接会直接解除 dirty（见
    /// `test_unnegotiated_client_load_clears_dirty_generation_and_admits_owned`），
    /// 因此观测 dirty 不能借道 load。
    async fn dirty_target(&self) -> RecoveryRequiredDetails {
        let error = match self
            .cfg
            .controller
            .sessions()
            .acquire_execution_lease(&self.id)
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("dirty generation refuses lease acquisition"),
        };
        match error.downcast_ref::<WorkspaceError>() {
            Some(WorkspaceError::RecoveryRequired(target)) => target.clone(),
            other => panic!("expected recovery-required lease error, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn test_workspace_dirty_recovery_original_load_and_frozen() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = PeriConfig::default();
    config.config.active_alias = "sonnet".into();
    config.config.providers = vec![ProviderConfig {
        id: "local".into(),
        provider_type: "openai".into(),
        api_key: uuid::Uuid::new_v4().to_string(),
        base_url: "http://127.0.0.1:1".into(),
        models: ProviderModels {
            sonnet: "fixture".into(),
            ..Default::default()
        },
        ..Default::default()
    }];
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let created = handle_request(
        "session/new",
        &json!({"cwd":tmp.path()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let binding = cfg.thread_store.load_session_binding(&id).await.unwrap();
    let frozen = cfg.thread_store.load_frozen_snapshot(&id).await.unwrap();
    // 只释放 owner，不伪造正常收尾；夹具不启动外部任务。
    sessions.clear();
    let params = json!({"sessionId":id,"cwd":tmp.path()});
    // 协商了 `peri.sessionRecoveryV1` 的客户端自己确认：准入不失败，但停在只读，
    // 由它读走原因并决定是否调用 `peri/session_reset_dirty`。
    cfg.session_manager
        .set_pending_caps(PeriCaps::all_enabled());
    let admitted = handle_request("session/load", &params, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    let target = read_only_dirty(&admitted);
    assert_eq!(target.thread_id, id);
    assert_eq!(target.generation, 1);
    assert!(sessions[&id].execution_owner.is_none());
    assert!(sessions[&id].frozen.is_none());
    let cancel = ResetDirtyRequest {
        target: target.clone(),
        accept_risk: false,
    };
    assert_eq!(
        handle_request(
            "peri/session_reset_dirty",
            &serde_json::to_value(cancel).unwrap(),
            &cfg,
            &mut sessions,
            &transport
        )
        .await
        .unwrap_err()
        .code,
        -32602
    );
    // 取消确认不触碰存储：可解除的代际仍是同一个。
    let unchanged = handle_request("session/load", &params, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert_eq!(read_only_dirty(&unchanged), target);
    let ack = serde_json::to_value(ResetDirtyRequest {
        target,
        accept_risk: true,
    })
    .unwrap();
    cfg.session_manager.set_pending_caps(PeriCaps::default());
    assert_eq!(
        handle_request(
            "peri/session_reset_dirty",
            &ack,
            &cfg,
            &mut sessions,
            &transport
        )
        .await
        .unwrap_err()
        .code,
        -32601
    );
    cfg.session_manager
        .set_pending_caps(PeriCaps::all_enabled());
    handle_request(
        "peri/session_reset_dirty",
        &ack,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    // 解除 dirty 后重新准入取得所有权：上一轮的只读状态必须整体重建。
    let owned = handle_request("session/load", &params, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert!(read_only(&owned).is_none());
    assert!(sessions.contains_key(&id));
    assert!(sessions[&id].execution_owner.is_some());
    assert!(sessions[&id].frozen.is_some());
    assert_eq!(
        cfg.thread_store.load_session_binding(&id).await.unwrap(),
        binding
    );
    assert_eq!(
        cfg.thread_store.load_frozen_snapshot(&id).await.unwrap(),
        frozen
    );
    assert_eq!(
        Path::new(&sessions[&id].cwd),
        tmp.path().canonicalize().unwrap()
    );
    let busy = handle_request(
        "peri/session_reset_dirty",
        &ack,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(busy.data.is_none());
    handle_request("session/close", &params, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
}

/// 未经 initialize 协商时，reset 必须拒绝且不触碰存储。
#[tokio::test]
async fn test_dirty_reset_without_initialize_is_rejected_without_store_effect() {
    let (mut fixture, target) = Fixture::dirty().await;
    assert!(!fixture.cfg.session_manager.pending_caps_was_set());
    let error = fixture.reset(&target).await.unwrap_err();
    assert_eq!(error.code, -32601);
    assert!(error.data.is_none());
    // 零存储副作用：被拒的这条 RPC 没有解除任何代际——同一精确目标仍能被存储 CAS
    // 命中（存储层解除成功），说明 dirty 记录与代次原样保留。
    fixture
        .cfg
        .thread_store
        .reset_dirty_execution(&target)
        .await
        .unwrap();
}

/// 未协商 `peri.sessionRecoveryV1` 的连接没有确认交互：dirty 不再把它挡在只读，
/// host 直接解除精确代际并取得所有权。
#[tokio::test]
async fn test_unnegotiated_client_load_clears_dirty_generation_and_admits_owned() {
    let (mut fixture, target) = Fixture::dirty().await;
    assert_eq!(target.generation, 1);
    // initialize 声明了别的事件能力、没有声明恢复能力（RCS/acp-link 的形态）。
    fixture.cfg.session_manager.set_pending_caps(PeriCaps {
        token_stats: true,
        agent_event: true,
        unstable_event: true,
        ..Default::default()
    });
    let params = fixture.params();
    let loaded = fixture.load(&params).await.unwrap();
    // 未协商身份载荷，响应里没有只读标记；本次准入也不是只读准入。
    assert!(loaded.pointer("/_meta/peri.sessionWorkspaceV1").is_none());
    assert!(fixture.sessions[&fixture.id].execution_owner.is_some());
    assert!(fixture.sessions[&fixture.id].frozen.is_some());
    // 解除的是观测到的那一代：按正常路径收尾（close 标 clean 并释放所有权）后，
    // 原目标再解除只会是代次/clean 错配——存储层 CAS 已不再命中。
    handle_request(
        "session/close",
        &params,
        &fixture.cfg,
        &mut fixture.sessions,
        &fixture.transport,
    )
    .await
    .unwrap();
    let again = fixture
        .cfg
        .thread_store
        .reset_dirty_execution(&target)
        .await
        .unwrap_err();
    assert!(
        again.to_string().contains("dirty generation changed"),
        "unexpected error: {again}"
    );
}

/// `session/fork` 共用 `prepare_existing` 的准入，但随后还要 `reacquire_for_load`：
/// 未协商能力的连接同样能 fork dirty 源会话——源会话的精确代际在准入时已被解除，
/// 二次取得看到的是活 owner，而不是只读降级。
#[tokio::test]
async fn test_unnegotiated_client_fork_clears_source_dirty_generation_and_admits_owned() {
    let (mut fixture, target) = Fixture::dirty().await;
    assert_eq!(target.generation, 1);
    // initialize 声明了别的事件能力、没有声明恢复能力（RCS/acp-link 的形态）。
    fixture.cfg.session_manager.set_pending_caps(PeriCaps {
        token_stats: true,
        agent_event: true,
        unstable_event: true,
        ..Default::default()
    });
    let params = fixture.params();
    let forked = handle_request(
        "session/fork",
        &params,
        &fixture.cfg,
        &mut fixture.sessions,
        &fixture.transport,
    )
    .await
    .unwrap();
    let fork_id = forked["sessionId"].as_str().unwrap().to_owned();
    assert_ne!(fork_id, fixture.id);
    // 源会话在本进程内持有执行所有权，fork 出去的会话独立存在。
    assert!(fixture.sessions[&fixture.id].execution_owner.is_some());
    assert!(fixture.sessions[&fork_id].execution_owner.is_some());
    // 解除的是观测到的那一代：按正常路径收尾（close 标 clean 并释放所有权）后，
    // 原目标再解除只会是代次/clean 错配——存储层 CAS 已不再命中。
    handle_request(
        "session/close",
        &params,
        &fixture.cfg,
        &mut fixture.sessions,
        &fixture.transport,
    )
    .await
    .unwrap();
    let again = fixture
        .cfg
        .thread_store
        .reset_dirty_execution(&target)
        .await
        .unwrap_err();
    assert!(
        again.to_string().contains("dirty generation changed"),
        "unexpected error: {again}"
    );
}

/// 协商了 `peri.sessionRecoveryV1` 的连接 fork dirty 源会话：dirty 原因正是它要看的
/// 信号，host 不替它解除，`reacquire_for_load` 也不接受降级——按原语义上报 `-32010`
/// 与精确代际的类型化载荷，且不触碰存储。
#[tokio::test]
async fn test_negotiated_client_fork_on_dirty_source_reports_typed_recovery_required() {
    let (mut fixture, target) = Fixture::dirty().await;
    fixture
        .cfg
        .session_manager
        .set_pending_caps(PeriCaps::all_enabled());
    let params = fixture.params();
    let error = handle_request(
        "session/fork",
        &params,
        &fixture.cfg,
        &mut fixture.sessions,
        &fixture.transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32010);
    let data: WorkspaceErrorData = serde_json::from_value(
        error
            .data
            .clone()
            .expect("recovery rejection carries typed data"),
    )
    .expect("recovery rejection data is typed");
    match data {
        WorkspaceErrorData::RecoveryRequired(details) => assert_eq!(details, target),
    }
    // 未取得所有权：源会话即便已只读进入内存，也没有 owner；被拒的这条 fork 没有解除
    // 任何代际——同一精确目标仍能被存储 CAS 命中。
    assert!(fixture.sessions[&fixture.id].execution_owner.is_none());
    fixture
        .cfg
        .thread_store
        .reset_dirty_execution(&target)
        .await
        .unwrap();
}

/// reset 成功但随后的 load 失败：存储保持 clean 且代次不变，原 ID 之后仍可恢复。
#[tokio::test]
async fn test_dirty_reset_then_failing_reload_keeps_store_state_and_original_id() {
    let (mut fixture, target) = Fixture::dirty().await;
    fixture
        .cfg
        .session_manager
        .set_pending_caps(PeriCaps::all_enabled());
    assert_eq!(target.generation, 1);
    fixture.reset(&target).await.unwrap();

    // 用不匹配的 cwd 触发一次失败的重新 load（等价于客户端 reset 后重载失败）。
    let other = tempfile::tempdir().unwrap();
    let failed = fixture
        .load(&json!({"sessionId":fixture.id,"cwd":other.path()}))
        .await
        .unwrap_err();
    assert_eq!(failed.code, -32010);
    assert!(failed.data.is_none());
    assert!(!fixture.sessions.contains_key(&fixture.id));

    // 存储事实：clean 已置位且代次未推进（同代次再 reset 只能报错配）。
    let again = fixture.reset(&target).await.unwrap_err();
    assert!(again.message.contains("dirty generation changed"));
    let binding = fixture
        .cfg
        .thread_store
        .load_session_binding(&fixture.id)
        .await
        .unwrap();
    let frozen = fixture
        .cfg
        .thread_store
        .load_frozen_snapshot(&fixture.id)
        .await
        .unwrap();

    // 原 ID 用原目录仍可正常 load，并保持 binding/frozen。
    let params = fixture.params();
    fixture.load(&params).await.unwrap();
    assert!(fixture.sessions.contains_key(&fixture.id));
    assert_eq!(
        fixture
            .cfg
            .thread_store
            .load_session_binding(&fixture.id)
            .await
            .unwrap(),
        binding
    );
    assert_eq!(
        fixture
            .cfg
            .thread_store
            .load_frozen_snapshot(&fixture.id)
            .await
            .unwrap(),
        frozen
    );
    let updated = fixture.reset(&target).await.unwrap_err();
    assert!(
        updated.data.is_none(),
        "活 owner 上再次 reset 只能是锁定/代次类错误，不得解掉新代"
    );
}
