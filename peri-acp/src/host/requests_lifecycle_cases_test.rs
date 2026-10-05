use super::*;

/// 删除后：响应为空对象、线程从 store 移除（load_meta 报错）、活跃会话从
/// sessions 表清理。

#[tokio::test]
async fn test_delete_removes_thread_and_active_session() {
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

    // 真实创建线程（id 即 session id）
    let thread_id = create_bound_fixture(&cfg, cwd, None).await;
    let sid = thread_id.clone();

    // 活跃会话登记（与 session/new 后的内存态一致）
    register_session_with_workflow(&mut sessions, &sid, cwd, &cfg).await;

    let resp = handle_request(
        "session/delete",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/delete 应成功");

    // 标准响应为空对象
    assert_eq!(
        resp,
        serde_json::json!({}),
        "标准 session/delete 响应为 {{}}"
    );

    // 活跃会话已清理
    assert!(
        !sessions.contains_key(&sid),
        "删除后活跃会话应从 sessions 表移除"
    );

    // 线程已从 store 持久化删除（元数据不存在 + 列表不再包含）
    assert!(
        cfg.session_resources.load_session_meta(&sid).await.is_err(),
        "删除后线程元数据不应存在"
    );
    let remaining = cfg
        .session_resources
        .list_sessions(&peri_acp_types::workspace::ScopedThreadQuery {
            scope: peri_acp_types::workspace::ThreadScope::All,
            cursor: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert!(
        !remaining.entries.iter().any(|entry| entry.thread.id == sid),
        "删除后 session/list 不应再包含该线程"
    );
}

/// 删除不存在的线程：幂等成功（存储层不报错，历史不存在视为已删除）。
#[tokio::test]
async fn test_delete_unknown_session_is_idempotent() {
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

    let resp = handle_request(
        "session/delete",
        &json!({ "sessionId": "never-existed" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("删除不存在的会话应幂等成功");
    assert_eq!(resp, serde_json::json!({}));
}

/// 缺失 sessionId → -32602 Invalid params。
#[tokio::test]
async fn test_delete_missing_session_id_returns_error() {
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

    let err = handle_request(
        "session/delete",
        &json!({}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报 -32602，实际: {}",
        err.message
    );
}

// ── session/rename（标准 ACP；stdio 与 TUI 共用统一 host 注册于 requests.rs）──

/// 重命名成功：thread store 持久化标题 + `session/update` 通知携带
/// `SessionInfoUpdate.title` + 响应往返 `{sessionId, title}`。
#[tokio::test]
async fn test_rename_persists_title_and_pushes_session_info_update() {
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
    let mock = std::sync::Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let cwd = tmp.path().to_str().unwrap();

    // 真实创建线程（id 即 session id），与 session/new 后的持久层状态一致
    let sid = create_bound_fixture(&cfg, cwd, None).await;
    handle_request(
        "session/load",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let new_title = "重构 ACP 协议".to_string();

    let resp = handle_request(
        "session/rename",
        &json!({ "sessionId": sid, "title": new_title }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/rename 应成功");

    // 标准响应往返
    assert_eq!(resp["sessionId"], sid, "响应 sessionId: {resp}");
    assert_eq!(resp["title"], new_title, "响应 title: {resp}");

    // 持久化：load_meta 标题已更新
    let meta = cfg.session_resources.load_session_meta(&sid).await.unwrap();
    assert_eq!(meta.title.as_deref(), Some(new_title.as_str()));

    // 通知：session/update 携带 SessionInfoUpdate.title，供标题栏与外部客户端刷新
    let (method, payload) = mock
        .notifications()
        .iter()
        .find(|(method, payload)| {
            method == "session/update"
                && payload["update"]["sessionUpdate"] == "session_info_update"
        })
        .cloned()
        .expect("rename 应推送 session/update 通知");
    assert_eq!(method, "session/update");
    assert_eq!(payload["sessionId"], sid);
    assert_eq!(payload["update"]["sessionUpdate"], "session_info_update");
    assert_eq!(payload["update"]["title"], new_title);
}

/// 缺失 sessionId → -32602 Invalid params。
#[tokio::test]
async fn test_rename_missing_session_id_returns_error() {
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

    let err = handle_request(
        "session/rename",
        &json!({ "title": "无 sessionId" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报 -32602，实际: {}",
        err.message
    );
}

/// 缺失 title → -32602 Invalid params。
#[tokio::test]
async fn test_rename_missing_title_returns_error() {
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

    let err = handle_request(
        "session/rename",
        &json!({ "sessionId": "some-session" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(
        err.message.contains("missing title"),
        "缺失 title 应报 -32602，实际: {}",
        err.message
    );
}

// ── AvailableCommandsUpdate + 注册表回调重发（Slice 6 / DD-5 / Phase 6 A4）──
