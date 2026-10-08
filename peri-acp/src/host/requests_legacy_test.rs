use super::*;
use async_trait::async_trait;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, CloseSettlement, ForkSnapshot,
    FrozenSnapshotBytes, NewSession, NewSessionDraft, PersistenceRecovery, RewindBoundary,
    SessionAvailability, SessionInitialization, SessionMetaPatch, SessionResourceError,
    SessionResourceResult, SessionResources, SessionSnapshot,
};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};

/// legacy 原始事实：只有 thread 行与消息，**没有 binding、没有 frozen**。
///
/// 用裸句柄按原表构造——门面没有「无 binding 无 frozen」的创建入口；`bridge` 与 `cfg`
/// 的门面出自同一次打开（同一库句柄），后续经协议/门面读到的是同一份事实。
pub(super) async fn old_thread(bridge: &SqliteThreadStore, cwd: &Path) -> String {
    let id = bridge
        .create_thread(ThreadMeta::new_at(
            cwd.to_str().unwrap(),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    bridge
        .append_message(
            &id,
            peri_acp_types::messages::BaseMessage::human("legacy user message"),
        )
        .await
        .unwrap();
    id
}

#[tokio::test]
#[serial]
async fn legacy_history_context_then_load_restores_saved_cwd_and_frozen_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("saved-project");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("CLAUDE.md"), "LEGACY_PROJECT_INSTRUCTION").unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let id = old_thread(&bridge, &cwd).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let context = handle_request(
        "peri/session_context",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(context["workspace"]["cwd"], cwd.to_str().unwrap());
    assert!(context["binding"].is_null());
    assert!(bridge.load_session_binding(&id).await.unwrap().is_none());
    assert!(bridge.load_frozen_snapshot(&id).await.unwrap().is_none());
    handle_request(
        "session/load",
        &json!({"sessionId":id,"cwd":cwd}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(Path::new(&sessions[&id].cwd), cwd);
    assert_eq!(sessions[&id].history[0].content(), "legacy user message");
    // W5（J2 §3.1）：legacy 首次接纳发生在内容准入之前，没有执行环境 ⇒ 没有
    // workspace 资源面 ⇒ 项目指令不可得（`claude_md` 为空），且**不回落磁盘**
    // （同样的 X4/J5 口径见 W4b 的 legacy 空技能摘要先例）。后半段
    // 「load 恢复已保存 frozen 快照」的断言不受影响。
    assert!(
        sessions[&id].frozen.as_ref().unwrap().claude_md().is_none(),
        "legacy 首次接纳：无资源面 ⇒ 指令不可得（零磁盘兜底）"
    );
    assert!(
        std::path::Path::new(&cwd).join("CLAUDE.md").is_file()
            || std::path::Path::new(&cwd).join("AGENTS.md").is_file(),
        "零兜底前提：磁盘上的指令文件仍在"
    );
    let frozen = bridge.load_frozen_snapshot(&id).await.unwrap().unwrap();
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    std::fs::write(cwd.join("CLAUDE.md"), "CHANGED_LATER").unwrap();
    handle_request(
        "session/resume",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(
        bridge.load_frozen_snapshot(&id).await.unwrap().unwrap(),
        frozen
    );
    let fork = handle_request(
        "session/fork",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fork_id = fork["sessionId"].as_str().unwrap();
    assert_ne!(fork_id, id);
    assert_eq!(
        sessions[fork_id].history[0].content(),
        "legacy user message"
    );
    handle_request(
        "session/close",
        &json!({"sessionId":fork_id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert!(!cfg.session_resources.is_session_closing(&id).await.unwrap());
}

#[tokio::test]
#[serial]
async fn legacy_history_missing_directory_is_readable_without_adoption() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let saved = tmp.path().join("removed");
    let id = old_thread(&bridge, &saved).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let response = handle_request(
        "peri/session_history",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(response["payloads"].as_array().unwrap().len(), 1);
    assert!(response["binding"].is_null());
    for method in ["session/load", "session/resume"] {
        assert!(handle_request(
            method,
            &json!({"sessionId":id,"cwd":tmp.path()}),
            &cfg,
            &mut sessions,
            &transport
        )
        .await
        .is_err());
        assert!(sessions.is_empty());
        assert!(cfg.session_manager.get_session(&id).is_none());
    }
    assert!(bridge.load_session_binding(&id).await.unwrap().is_none());
    assert!(bridge.load_frozen_snapshot(&id).await.unwrap().is_none());
}

#[tokio::test]
#[serial]
async fn legacy_history_load_ignores_wrong_directory_and_preserves_saved_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let saved = root.join("saved");
    let other = root.join("other");
    std::fs::create_dir(&saved).unwrap();
    std::fs::create_dir(&other).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let id = old_thread(&bridge, &saved).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    handle_request(
        "session/load",
        &json!({"sessionId":id,"cwd":other}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(Path::new(&sessions[&id].cwd), saved);
    assert_eq!(sessions[&id].history[0].content(), "legacy user message");
    assert_eq!(Path::new(&bridge.load_meta(&id).await.unwrap().cwd), saved);
    assert!(bridge.load_session_binding(&id).await.unwrap().is_some());
    assert!(bridge.load_frozen_snapshot(&id).await.unwrap().is_some());
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert!(!sessions.contains_key(&id));
    assert!(!cfg.session_resources.is_session_closing(&id).await.unwrap());
}

#[tokio::test]
#[serial]
async fn legacy_history_rejects_bad_frozen_without_adoption() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let saved = std::fs::canonicalize(tmp.path()).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    for snapshot in ["broken", r#"{"version":999,"data":{}}"#] {
        let id = old_thread(&bridge, &saved).await;
        bridge
            .store_frozen_snapshot_if_absent(&id, snapshot)
            .await
            .unwrap();
        let error = handle_request(
            "session/load",
            &json!({"sessionId":id}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap_err();
        assert!(
            error.message.contains("frozen snapshot"),
            "{}",
            error.message
        );
        assert!(bridge.load_session_binding(&id).await.unwrap().is_none());
        assert_eq!(
            bridge.load_frozen_snapshot(&id).await.unwrap().as_deref(),
            Some(snapshot)
        );
        assert!(cfg.session_manager.get_session(&id).is_none());
    }
    assert!(sessions.is_empty());
}

#[tokio::test]
#[serial]
async fn legacy_history_fix_does_not_rebuild_missing_native_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    // 已绑定但**缺 frozen**：门面创建要求 frozen 成立，因此夹具按原表构造。
    let workspace = bridge.resolve_workspace(tmp.path()).await.unwrap();
    let id = bridge
        .create_bound_thread(
            ThreadMeta::new_at(workspace.cwd.to_str().unwrap(), peri_time::now_wall()),
            &workspace,
        )
        .await
        .unwrap();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let error = handle_request(
        "session/load",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.message, "Bound session has no frozen snapshot");
    assert!(bridge.load_frozen_snapshot(&id).await.unwrap().is_none());
    assert!(sessions.is_empty());
}

#[tokio::test]
#[serial]
async fn legacy_history_freezes_saved_workspace_configuration_and_plugins() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let startup = tmp.path().join("startup");
    let target = tmp.path().join("saved");
    std::fs::create_dir(&startup).unwrap();
    std::fs::create_dir_all(target.join(".peri/meta")).unwrap();
    let target = std::fs::canonicalize(&target).unwrap();
    std::fs::write(target.join(".peri/meta/01_intro.md"), "SAVED_META_HARNESS").unwrap();
    std::fs::write(
        target.join(".peri/settings.json"),
        r#"{"config":{"language":"zh-CN","meta_harness":{"01_intro":true,"WebMiddleware":false}}}"#,
    )
    .unwrap();
    seed_plugin_ecc(tmp.path());
    let skills = tmp.path().join(".claude/plugins/ecc/skills/legacy-skill");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: legacy-skill\ndescription: SAVED_PLUGIN_SKILL\n---\nLegacy plugin skill",
    )
    .unwrap();
    let mut config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    config.config.language = Some("en".into());
    let (mut cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    crate::provider::save_to(&config, cfg.config_source.global_path()).unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: startup.to_str().unwrap().to_owned(),
        bare: false,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    assert!(cfg.plugin_skill_roots.is_empty());
    let id = old_thread(&bridge, &target).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    handle_request(
        "session/load",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let frozen = sessions[&id].frozen.as_ref().unwrap().v2_frozen();
    assert_eq!(frozen.language.as_deref(), Some("zh-CN"));
    // M5：legacy 首次接纳先建立资源环境（builtin `workspace` 实例），覆盖文档
    // 经**资源面 provider** 读取后进入冻结——这是提供方的读取，不是宿主扫描
    // （宿主侧已无 `.peri/meta` 读点；X8 的「不可得即内置」只在资源面缺席时生效）。
    assert_eq!(
        frozen
            .meta_harness
            .section_overrides
            .get("01_intro")
            .map(|value| value.to_string()),
        Some("SAVED_META_HARNESS".to_string()),
        "覆盖文档必须经资源面进入冻结输入"
    );
    assert!(frozen
        .meta_harness
        .disabled_middlewares
        .contains("WebMiddleware"));
    // W4b（F3/J5）：技能摘要同样只在内容准入期（P4）从 system 来源取；
    // legacy 首次接纳先于执行环境 ⇒ 无资源面 ⇒ 摘要为空。插件技能根在盘上
    // 存在也不得被宿主读取（宿主已无技能扫描点）。
    // M5：技能目录同样在 bootstrap 后经资源面（builtin `workspace` 实例）读取：
    // 摘要必须包含该实例声明的技能，且**不得**出现宿主机扫盘才会发现的插件技能
    // （`plugin_skill_roots` 为空 ⇒ 插件根不进入资源面）。
    assert!(
        frozen.skill_summary.contains("mcp__workspace__cron"),
        "技能摘要必须来自资源面的技能目录：{}",
        frozen.skill_summary
    );
    assert!(
        !frozen.skill_summary.contains("SAVED_PLUGIN_SKILL"),
        "宿主不得从插件根扫描技能目录：{}",
        frozen.skill_summary
    );
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert!(!sessions.contains_key(&id));
    assert!(!cfg.session_resources.is_session_closing(&id).await.unwrap());
}

/// M5：legacy 首次接纳先做**仅资源 bootstrap**（builtin `workspace` 资源面），
/// 读到项目指引后再以「定稿 frozen + binding」一次性原子接纳并发布。
///
/// 旧行为（本组修复前）：接纳发生在内容准入之前、没有执行环境 ⇒ 指令面按
/// 「不可得」冻结为空，永不恢复。本用例断言新契约：资源面可得时，指引必须进入
/// 冻结字节，且 live 与持久化字节同源（不存在「先写空、后替换」的中间态）。
#[tokio::test]
#[serial]
async fn legacy_first_adoption_bootstraps_resources_before_atomic_adoption() {
    const LEGACY_FACE_INSTRUCTION: &str = "LEGACY_FACE_INSTRUCTION_FROM_RESOURCE_FACE";
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("legacy-face");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("CLAUDE.md"), LEGACY_FACE_INSTRUCTION).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (mut cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    // 生产形态：会话级 workspace 装配（builtin `workspace` 提供指令/技能资源面）。
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let id = old_thread(&bridge, &cwd).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    handle_request(
        "session/load",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("legacy 首次接纳必须成功");

    let live = sessions[&id].frozen.clone().expect("接纳必须发布冻结输入");
    assert_eq!(
        live.claude_md(),
        Some(LEGACY_FACE_INSTRUCTION),
        "资源面可得时，首次接纳必须捕获项目指引（不再冻结为空）"
    );
    let persisted = bridge.load_frozen_snapshot(&id).await.unwrap().unwrap();
    let decoded = crate::session::frozen_snapshot::decode_frozen_snapshot(&persisted).unwrap();
    assert_eq!(
        decoded.claude_md(),
        Some(LEGACY_FACE_INSTRUCTION),
        "持久化字节必须携带同一次 bootstrap 读到的指引"
    );
    assert_eq!(
        crate::session::frozen_snapshot::encode_frozen_snapshot(&live).unwrap(),
        persisted,
        "live 与持久化 frozen 必须逐字节同源（一次 write-once）"
    );
    assert!(
        bridge.load_session_binding(&id).await.unwrap().is_some(),
        "接纳必须同时写入不可变 binding"
    );
    if let Some(environment) = &sessions[&id].environment {
        assert!(environment.shutdown().await);
    }
}

/// M5：接纳 write-once 且竞争输家读取 winner——先到的字节不被后写覆盖，
/// 后续发布使用 winner 的持久字节。
#[tokio::test]
#[serial]
async fn legacy_adoption_is_write_once_and_loser_reads_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();

    // winner 字节：另一条真实创建路径产出的合法 frozen 快照。
    let winner_cwd = std::fs::canonicalize(tmp.path()).unwrap().join("winner");
    std::fs::create_dir(&winner_cwd).unwrap();
    let winner_response = handle_request(
        "session/new",
        &json!({"cwd": winner_cwd}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let winner_id = winner_response["sessionId"].as_str().unwrap().to_owned();
    let winner = bridge
        .load_frozen_snapshot(&winner_id)
        .await
        .unwrap()
        .unwrap();
    let loser_cwd = std::fs::canonicalize(tmp.path()).unwrap().join("loser");
    std::fs::create_dir(&loser_cwd).unwrap();
    let loser_response = handle_request(
        "session/new",
        &json!({"cwd": loser_cwd}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let loser_id = loser_response["sessionId"].as_str().unwrap().to_owned();
    let loser = bridge
        .load_frozen_snapshot(&loser_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(winner, loser, "夹具前提：两条快照字节不同");

    // legacy（无 binding、无 frozen）的保存 cwd 必须是本机可用执行目录。
    let legacy_cwd = std::fs::canonicalize(tmp.path()).unwrap().join("legacy");
    std::fs::create_dir(&legacy_cwd).unwrap();
    let legacy_id = old_thread(&bridge, &legacy_cwd).await;
    let workspace = cfg
        .session_resources
        .resolve_workspace(&legacy_cwd)
        .await
        .unwrap();

    // winner 先接纳；竞争输家随后用不同字节接纳：不覆盖既有事实。
    cfg.session_resources
        .adopt_legacy_session(
            &legacy_id,
            legacy_cwd.to_str().unwrap(),
            &workspace,
            &peri_acp_types::session_resources::FrozenSnapshotBytes::new(winner.clone()),
        )
        .await
        .unwrap();
    cfg.session_resources
        .adopt_legacy_session(
            &legacy_id,
            legacy_cwd.to_str().unwrap(),
            &workspace,
            &peri_acp_types::session_resources::FrozenSnapshotBytes::new(loser.clone()),
        )
        .await
        .expect("输家接纳不得因字节不同而失败（write-once）");
    assert_eq!(
        bridge
            .load_frozen_snapshot(&legacy_id)
            .await
            .unwrap()
            .unwrap(),
        winner,
        "既有字节（winner）不得被后来者覆盖"
    );

    // 发布面读取 winner：session/load 后的 live 冻结输入与 winner 字节同源。
    handle_request(
        "session/load",
        &json!({"sessionId":legacy_id.clone()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let live = sessions[&legacy_id].frozen.clone().unwrap();
    assert_eq!(
        crate::session::frozen_snapshot::encode_frozen_snapshot(&live).unwrap(),
        winner,
        "竞争输家路径必须读取 winner 字节，而不是本次候选"
    );
    if let Some(environment) = &sessions[&legacy_id].environment {
        assert!(environment.shutdown().await);
    }
}

/// M5：只读存储不做 bootstrap、不读本机同名路径、不接纳——历史保持可读。
///
/// 资格检查（`inspect_availability`）在更早的入口已拒绝只读存储；本用例直接在
/// legacy 准备入口验证第二道边界：即使被单独调用，也只读检查失败即返回，不对
/// 本机同名路径做资源读取，更不写 binding/frozen。
#[tokio::test]
#[serial]
async fn legacy_read_only_store_does_not_bootstrap_or_adopt() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("readonly-legacy");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("CLAUDE.md"), "READ_ONLY_MUST_NOT_BE_READ").unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let db_path = tmp.path().join("threads.db");
    let (writable_cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let id = old_thread(&bridge, &cwd).await;
    // 关掉写句柄后以只读方式重新打开同一库。
    drop(bridge);
    drop(writable_cfg);
    let read_only: Arc<dyn SessionResources> = Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open_existing_read_only(&db_path)
            .await
            .unwrap(),
    );
    let read_only_cfg = build_server_config(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
        read_only,
    )
    .await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let error = handle_request(
        "session/load",
        &json!({"sessionId":id}),
        &read_only_cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect_err("只读存储不得进入 legacy 接纳");
    assert!(
        error.message.contains("read-only"),
        "只读边界必须给出可解释错误: {error:?}"
    );
    assert!(sessions.is_empty(), "只读边界不得发布会话");
    // 只读边界不得 bootstrap：观察面里没有该会话的环境关闭（也没有过环境）。
    assert!(
        !crate::host::workspace::take_shutdown_observations()
            .iter()
            .any(|session| session == &id),
        "只读边界不得进入资源 bootstrap"
    );
    // 重新以可写方式打开：不得因拒绝接纳而写入任何 binding/frozen，历史仍可读。
    let reopened: Arc<dyn SessionResources> = Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open(&db_path)
            .await
            .unwrap(),
    );
    let snapshot = reopened.load_session_snapshot(&id).await.unwrap();
    assert!(
        !matches!(
            snapshot.binding,
            peri_acp_types::session_resources::BindingState::Bound(_)
        ),
        "只读边界不得写入 binding: {:?}",
        snapshot.binding
    );
    assert!(
        matches!(
            snapshot.frozen,
            peri_acp_types::session_resources::FrozenState::LegacyAbsent
        ),
        "只读边界不得写入 frozen: {:?}",
        snapshot.frozen
    );
    assert_eq!(
        reopened.load_session_history(&id).await.unwrap().len(),
        1,
        "历史读取不受接纳边界影响"
    );
}

/// M5 缺陷 1：legacy bootstrap 前必须解析有效 servers（持久 owner + 请求声明），
/// 池在装配时一次消费（OnceLock）——请求声明的 MCP server 必须真正进入环境。
///
/// 直接走恢复准备（不经 handle_request 的 reopen 后段）：声明的服务器用远端
/// `workspace` 身份，`workspace_source` 在装配后即同步可读——若 servers 仍像缺陷
/// 时那样在 bootstrap 之后才写入，OnceLock 会定格空集，来源判定拿不到声明。
#[tokio::test]
#[serial]
async fn legacy_bootstrap_applies_request_declared_mcp_servers() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("legacy-servers");
    std::fs::create_dir(&cwd).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (mut cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().to_owned(),
        // 非 bare：会话声明的 MCP server 才进入装配面（bare 只留 builtin workspace）。
        bare: false,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let id = old_thread(&bridge, &cwd).await;
    let mut sessions = HashMap::new();
    // mcpServers 的 wire 形状取自 SDK 类型序列化（形状漂移解析器必须跟着改）。
    // 远端 workspace 声明：`workspace_source` 是装配后同步可见的池事实
    // （不依赖后台 initialize 的连接结果）。
    let declared = serde_json::to_value(agent_client_protocol_schema::v1::McpServer::Http(
        agent_client_protocol_schema::v1::McpServerHttp::new("workspace", "http://127.0.0.1:9/mcp"),
    ))
    .unwrap();
    super::super::session_lifecycle::restore::prepare_existing(
        &json!({
            "sessionId": id,
            "mcpServers": [declared]
        }),
        &cfg,
        &mut sessions,
    )
    .await
    .expect("legacy 接纳必须应用请求声明的 servers");

    let environment = sessions[&id]
        .environment
        .clone()
        .expect("生产形态必须装配会话环境");
    let pool = environment
        .cfg
        .mcp_pool
        .as_ref()
        .expect("会话环境持有 MCP 池");
    let source = pool.workspace_source();
    assert!(
        matches!(
            source,
            Some(peri_acp_types::plugin::ConfigSource::WorkspaceRemote)
        ),
        "请求声明的 servers 必须在 bootstrap 装配时定格进池（actual: {source:?}）"
    );
    assert!(environment.shutdown().await);
}

/// M5 缺陷 2：bootstrap 之后（接纳失败）必须排空已激活环境，不留半成品。
///
/// 注入：把一个“有父线程的无绑定子会话”当作 legacy 接纳目标——资源环境先
/// bootstrap，随后 `adopt_legacy_session` 按真实规则拒绝（child 不可接纳）。
#[tokio::test]
#[serial]
async fn legacy_adopt_failure_drains_bootstrapped_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("legacy-child");
    std::fs::create_dir(&cwd).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (mut cfg, bridge) = make_server_config_with_bridge(
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
    // 有父线程 ⇒ 不是可接纳的 legacy root（adopt 会拒绝）；父线程必须真实存在。
    let parent_id = bridge
        .create_thread(peri_agent::thread::ThreadMeta::new_at(
            cwd.to_str().unwrap(),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    let mut meta =
        peri_agent::thread::ThreadMeta::new_at(cwd.to_str().unwrap(), peri_time::now_wall());
    meta.parent_thread_id = Some(parent_id);
    let id = bridge.create_thread(meta).await.unwrap();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let error = handle_request(
        "session/load",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect_err("有父线程的无绑定会话不得被 legacy 接纳");
    assert!(!error.message.is_empty(), "原始错误原因必须保留: {error:?}");
    assert!(sessions.is_empty(), "失败不得发布会话");
    assert!(bridge.load_session_binding(&id).await.unwrap().is_none());
    assert!(bridge.load_frozen_snapshot(&id).await.unwrap().is_none());
    // 环境已排空：shutdown 观察面记录了该会话的环境关闭。
    let shutdowns = crate::host::workspace::take_shutdown_observations();
    assert!(
        shutdowns.iter().any(|session| session == &id),
        "bootstrap 失败路径必须排空环境（观察到的 shutdown: {shutdowns:?}）"
    );
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
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (mut cfg, bridge) = make_server_config_with_bridge(
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
    let id = old_thread(&bridge, &cwd).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let error = handle_request(
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
// 注入，不能用内存替身伪造接纳事实。只有 `load_session_history` 返回可解释的冲突
// 错误，其余方法逐项转发同一库句柄的门面。

/// 单点故障：`load_session_history` 一律返回冲突错误。
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

            async fn load_session_history(
                &self,
                _id: &ThreadId,
            ) -> SessionResourceResult<Vec<PersistedPayload>> {
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
    rewind_history(id: &ThreadId, boundary: RewindBoundary) -> ();
    remove_history_entries(id: &ThreadId, ids: &[MessageId]) -> ();
    update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> ();
    delete_session_tree(id: &ThreadId) -> ();
    recover_session_persistence(id: &ThreadId) -> PersistenceRecovery;
    drain_persistence(id: &ThreadId) -> ();
}
