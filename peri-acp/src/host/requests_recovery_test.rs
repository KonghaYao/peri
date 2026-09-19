use super::*;
use peri_acp_types::workspace::{RecoveryRequiredDetails, ResetDirtyRequest, WorkspaceErrorData};
use peri_acp_types::PeriCaps;

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
        let mut fixture = Self {
            cfg,
            tmp,
            id,
            sessions,
            transport,
        };
        let target = fixture.observe_dirty().await;
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

    /// 通过 load 失败观测当前 dirty 代次（不改动存储）。
    async fn observe_dirty(&mut self) -> RecoveryRequiredDetails {
        let params = self.params();
        let error = self.load(&params).await.unwrap_err();
        let WorkspaceErrorData::RecoveryRequired(target) =
            serde_json::from_value(error.data.clone().unwrap()).unwrap();
        target
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
    let error = handle_request("session/load", &params, &cfg, &mut sessions, &transport)
        .await
        .unwrap_err();
    let WorkspaceErrorData::RecoveryRequired(target) =
        serde_json::from_value(error.data.clone().unwrap()).unwrap();
    assert_eq!(target.thread_id, id);
    assert_eq!(target.generation, 1);
    assert!(sessions.is_empty());
    // 参数校验先于能力门控需要显式协商；本用例聚焦载荷校验与不写库语义。
    cfg.session_manager
        .set_pending_caps(peri_acp_types::PeriCaps::all_enabled());
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
    let unchanged = handle_request("session/load", &params, &cfg, &mut sessions, &transport)
        .await
        .unwrap_err();
    assert_eq!(unchanged.data, error.data);
    let ack = serde_json::to_value(ResetDirtyRequest {
        target,
        accept_risk: true,
    })
    .unwrap();
    cfg.session_manager
        .set_pending_caps(peri_acp_types::PeriCaps::default());
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
        .set_pending_caps(peri_acp_types::PeriCaps::all_enabled());
    handle_request(
        "peri/session_reset_dirty",
        &ack,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    handle_request("session/load", &params, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert!(sessions.contains_key(&id));
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
    // 零存储副作用：dirty 记录与代次原样保留，普通 load 仍然按原样阻塞。
    let unchanged = fixture.observe_dirty().await;
    assert_eq!(unchanged, target);
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
