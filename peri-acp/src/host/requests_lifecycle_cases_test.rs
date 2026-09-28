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
        .find(|(m, _)| m == "session/update")
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

// ── A11/A22（H-04）：session/delete 不得关闭共享 host pool ────────────────────

/// 记录 shutdown 调用与终态的可观察 pool 替身（A30：`ready_for` / `did_change` /
/// `did_save` **必须显式实现**，无默认实现，故此处不得留空）。
///
/// `handle_shutdown` 是一个**可观察的终态位**：host shutdown 关闭它之后
/// `ready_for` 一律返回 false —— 让「delete 有没有关掉共享 pool」在替身上可证伪，
/// 而不是只看调用计数。
struct MockLspPool {
    shutdown_calls: Arc<std::sync::atomic::AtomicU32>,
    ready: std::sync::atomic::AtomicBool,
    sync_calls: Arc<std::sync::atomic::AtomicU32>,
}

impl MockLspPool {
    fn new(shutdown_calls: Arc<std::sync::atomic::AtomicU32>) -> Self {
        Self {
            shutdown_calls,
            ready: std::sync::atomic::AtomicBool::new(true),
            sync_calls: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }
}

#[async_trait::async_trait]
impl peri_acp_types::ports::LspPoolPort for MockLspPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn shutdown(&self) {
        self.shutdown_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    fn ready_for(&self, _path: &std::path::Path) -> bool {
        self.ready.load(std::sync::atomic::Ordering::SeqCst)
    }
    async fn did_change(
        &self,
        _path: &std::path::Path,
        _text: &str,
    ) -> Result<(), peri_acp_types::ports::LspSyncError> {
        self.sync_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn did_save(
        &self,
        _path: &std::path::Path,
    ) -> Result<(), peri_acp_types::ports::LspSyncError> {
        self.sync_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// 删除活跃会话时**不得** shutdown 共享 host pool（H-04 改写，原断言为 M2 的
/// 「必须 shutdown」）。
///
/// 裁决依据：主 plan A11/A22——pool 作用域 per-session ⇒ per-host，`session/delete`
/// 不再关 pool（关闭只发生在 host shutdown）；同一 `Arc` 被多个 session 共享，
/// delete 关掉它会掐死其它活跃 session 的 language server。
/// 断言的可证伪性：替身 shutdown 调用计数为 0 **且** pool 仍 ready（终态位未变），
/// 任一条被违反即红。
#[tokio::test]
async fn test_delete_active_session_does_not_shutdown_shared_host_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp.path().to_str().unwrap();

    // 真实创建会话（id 即 session id），与 delete 分支的会话树删除对应
    let sid = create_bound_fixture(&cfg, cwd, None).await;

    let shutdown_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let pool = Arc::new(MockLspPool::new(Arc::clone(&shutdown_calls)));
    cfg.lsp_pool = Some(Arc::clone(&pool) as Arc<dyn peri_acp_types::ports::LspPoolPort>);

    // 构造带 host pool 投影的活跃会话（其余字段与 register_session_with_workflow 一致）
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockWorkflowExecutor);
    let (notification_tx, _) = tokio::sync::broadcast::channel::<WorkflowTaskResult>(32);
    let mw = Arc::new(WorkflowMiddleware::new(
        executor,
        cwd,
        notification_tx,
        None,
    ));
    sessions.insert(
        sid.clone(),
        SessionState {
            session_id: sid.clone(),
            thread_id: sid.clone(),
            cwd: cwd.to_string(),
            execution_owner: Some(acquire_bound_owner(&cfg, &sid).await),
            environment: None,
            closing: false,
            history: Vec::new(),
            history_payloads: Vec::new(),
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: Some(Arc::clone(&mw) as Arc<dyn WorkflowMiddlewarePort>),
            // session 只投影 host pool 的同一 `Arc`（A11）
            lsp_pool: cfg.lsp_pool.clone(),
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
            lease: crate::host::lease::WriterLease::acquired("default"),
        },
    );

    let resp = handle_request(
        "session/delete",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/delete 应成功");
    assert_eq!(resp, serde_json::json!({}));
    assert!(
        !sessions.contains_key(&sid),
        "删除后活跃会话应从 sessions 表移除"
    );
    assert_eq!(
        shutdown_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "session/delete 不得 shutdown 共享 host pool（A11/A22：关闭只发生在 host shutdown）"
    );
    assert!(
        peri_acp_types::ports::LspPoolPort::ready_for(
            pool.as_ref(),
            std::path::Path::new("/tmp/whatever.rs")
        ),
        "delete 后共享 pool 必须仍可用（终态位未变），否则其它 session 的 LSP 能力被连带关闭"
    );
}

// ── AvailableCommandsUpdate + 注册表回调重发（Slice 6 / DD-5 / Phase 6 A4）──
