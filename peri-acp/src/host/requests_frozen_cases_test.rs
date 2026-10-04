use super::*;

/// 生产形态宿主：bare workspace 装配（W5 后项目指令的唯一来源是 builtin
/// `workspace` 实例的资源面，宿主本地已无扫描点；没有本装配就只剩 X4/J5 的空指令面）。
///
/// `startup_cwd` 取 `tmp` 的**规范化**形态：与准备面的「同目录复用配置」判据同口径
/// （`resolve_configuration` 对 startup_cwd 做 canonicalize 后与会话 cwd 比较），
/// 命中复用路径即不触发第二次只读配置加载。
async fn frozen_case_config(
    peri_config: crate::provider::PeriConfig,
    provider: crate::provider::LlmProvider,
    tmp: &tempfile::TempDir,
) -> AcpServerConfig {
    let mut cfg = make_server_config(peri_config, provider, tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: std::fs::canonicalize(tmp.path())
            .expect("测试工作区必须可规范化")
            .to_string_lossy()
            .into_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    cfg
}

/// 创建期指令快照经 builtin `workspace` 资源面采集 ⇒ 本用例依赖注入**默认态**
/// （`PERI_MCP_BUILTIN` 非 `off`）。该 env 是进程级全局，由同进程的开关组用例
/// 在 `#[serial]` 临界区内改写（TEST-HERMETIC-001）⇒ 依赖它的读侧必须同键互斥，
/// 否则并行窗口内会读到 `off` 态：池里没有 `workspace` 句柄且 `initPhase` 已收口，
/// 资源面按 X4 整体缺席、快照退化为空（不是被测语义）。
#[tokio::test]
#[serial]
async fn test_session_load_cold_host_restores_original_frozen_prompt() {
    // Arrange
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "FROZEN_PROMPT_V1").unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = frozen_case_config(peri_config.clone(), provider.clone(), &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let created = handle_request(
        "session/new",
        &json!({
            "cwd": tmp.path().to_str().unwrap(),
            "_meta": { "peri.instructions": "CUSTOM_AGENT_INSTRUCTIONS_V1" },
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    let original_prompt = sessions[&session_id]
        .frozen
        .as_ref()
        .unwrap()
        .system_prompt()
        .to_string();
    assert!(original_prompt.contains("CUSTOM_AGENT_INSTRUCTIONS_V1"));
    // W5：项目指令经 builtin `workspace` 资源面在创建期（P4）采集——夹具带生产形态
    // workspace 装配，指令快照必须非空且为创建时磁盘上的内容。
    let original_claude_md = sessions[&session_id]
        .frozen
        .as_ref()
        .unwrap()
        .claude_md()
        .expect("创建期必须采集到项目指令（workspace 资源面已装配）")
        .to_string();
    assert!(
        original_claude_md.contains("FROZEN_PROMPT_V1"),
        "创建期指令快照必须为磁盘上的 V1: {original_claude_md:?}"
    );
    append_human_message(&cfg, &session_id, "existing history").await;
    handle_request(
        "session/close",
        &json!({"sessionId": session_id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    drop(cfg);
    drop(sessions);
    std::fs::write(tmp.path().join("CLAUDE.md"), "FROZEN_PROMPT_V2").unwrap();
    let restarted = frozen_case_config(peri_config, provider, &tmp).await;
    let mut restored_sessions = HashMap::new();
    // Act
    handle_request(
        "session/load",
        &json!({
            "sessionId": session_id,
            "cwd": tmp.path().to_str().unwrap(),
        }),
        &restarted,
        &mut restored_sessions,
        &transport,
    )
    .await
    .unwrap();
    // Assert
    let restored_prompt = restored_sessions[&session_id]
        .frozen
        .as_ref()
        .unwrap()
        .system_prompt();
    let restored_claude_md = restored_sessions[&session_id]
        .frozen
        .as_ref()
        .unwrap()
        .claude_md()
        .expect("冷恢复必须带出创建期的指令快照（持久化 blob 消费者）")
        .to_string();
    assert_eq!(
        restored_prompt, original_prompt,
        "冷恢复逐字复用 frozen prompt"
    );
    assert_eq!(
        restored_claude_md, original_claude_md,
        "ARC-FROZEN-001：冷恢复的指令快照与创建期逐字一致"
    );
    assert!(
        restored_claude_md.contains("FROZEN_PROMPT_V1"),
        "冷恢复必须带出创建期的 V1: {restored_claude_md:?}"
    );
    assert!(
        !restored_claude_md.contains("FROZEN_PROMPT_V2"),
        "冷恢复不得重读磁盘（改写的 V2 不得进入快照）: {restored_claude_md:?}"
    );
}

/// 驻留空会话恢复后，fork 必须读到同一份 canonical 历史，而非只补展示缓存。
#[tokio::test]
async fn test_session_resume_existing_empty_history_is_available_to_fork() {
    use peri_acp_types::messages::BaseMessage;

    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp.path().to_str().unwrap();
    let created = handle_request(
        "session/new",
        &json!({ "cwd": cwd }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    let original = BaseMessage::human("persisted while the resident session is empty");
    cfg.session_resources
        .append_history(&session_id, &[PersistedPayload::Message(original.clone())])
        .await
        .unwrap();

    handle_request(
        "session/resume",
        &json!({ "sessionId": session_id, "cwd": cwd }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let forked = handle_request(
        "session/fork",
        &json!({ "sessionId": session_id, "cwd": cwd }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fork_id = forked["sessionId"].as_str().unwrap().to_string();
    let persisted_fork = own_payloads(&cfg, &fork_id).await;
    assert_eq!(
        persisted_fork.len(),
        1,
        "resume 后的真实 fork 不得丢失已持久化历史"
    );
    let copied = persisted_fork[0].as_message().unwrap();
    assert_eq!(copied.content(), original.content());
    assert_ne!(copied.id(), original.id(), "fork 保持独立 payload identity");
    assert_eq!(
        sessions[&session_id].history_payloads[0].id(),
        original.id()
    );
    assert_eq!(own_payloads(&cfg, &session_id).await[0].id(), original.id());
    assert_eq!(
        sessions[&fork_id].frozen.as_ref().unwrap().system_prompt(),
        sessions[&session_id]
            .frozen
            .as_ref()
            .unwrap()
            .system_prompt(),
        "恢复与 fork 不得重建 frozen 前缀"
    );
}

#[tokio::test]
async fn test_session_load_future_frozen_snapshot_fails_without_overwrite() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    // 未来版本快照是「本构建读不懂的既有事实」：夹具用裸句柄按原样落库，
    // 门面不给这种字节提供写入口（生产创建路径不接受写不懂的快照）。
    let (cfg, bridge) =
        make_server_config_with_bridge(peri_config.clone(), provider.clone(), &tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let workspace = bridge.resolve_workspace(tmp.path()).await.unwrap();
    let session_id = bridge
        .create_bound_thread(
            ThreadMeta::new_at(tmp.path().to_str().unwrap(), peri_time::now_wall()),
            &workspace,
        )
        .await
        .unwrap();
    let owner = bridge.acquire_execution_lease(&session_id).await.unwrap();
    let future_snapshot = r#"{"version":999,"data":{"must":"remain"}}"#;
    bridge
        .store_frozen_snapshot_if_absent(&session_id, future_snapshot)
        .await
        .unwrap();
    owner.mark_clean().await.unwrap();
    drop(owner);
    drop(cfg);
    drop(bridge);

    let (restarted, restarted_bridge) =
        make_server_config_with_bridge(peri_config, provider, &tmp).await;
    let mut restored_sessions = HashMap::new();
    let error = handle_request(
        "session/load",
        &json!({
            "sessionId": session_id,
            "cwd": tmp.path().to_str().unwrap(),
        }),
        &restarted,
        &mut restored_sessions,
        &transport,
    )
    .await
    .unwrap_err();

    assert!(error
        .message
        .contains("unsupported frozen snapshot version"));
    assert!(restored_sessions.is_empty());
    assert!(restarted.session_manager.get_session(&session_id).is_none());
    assert_eq!(
        restarted_bridge
            .load_frozen_snapshot(&session_id)
            .await
            .unwrap()
            .unwrap(),
        future_snapshot,
        "future snapshot must be preserved for a newer binary"
    );
}
