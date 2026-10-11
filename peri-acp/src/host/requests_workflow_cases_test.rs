use super::*;
use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport};

#[tokio::test]
async fn session_new_and_load_publish_task_snapshots_and_revisioned_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config.clone(), provider, &tmp).await;
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd":tmp.path()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_owned();
    let first = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(data) = mock.notifications().iter().find_map(|(method, payload)| {
                (method == "peri/unstable_event" && payload["event"] == "bg-task-snapshot")
                    .then(|| payload["data"].clone())
            }) {
                break data;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(first["revision"], 0);
    let manager = cfg
        .session_manager
        .get_session(&sid)
        .unwrap()
        .task_manager
        .clone();
    manager
        .register(peri_acp_types::tasks::BgTaskRegistration {
            task_id: "wf-revision-1".into(),
            kind: BgTaskKind::Workflow,
            summary: "workflow revision".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    let started = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(data) = mock.notifications().iter().find_map(|(method, payload)| {
                (method == "peri/unstable_event" && payload["event"] == "bg-task-started")
                    .then(|| payload["data"].clone())
            }) {
                break data;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(started["revision"], 1);
    assert_eq!(started["task_id"], "wf-revision-1");

    // 冷宿主接管执行前，原宿主须释放同一 Session 的持久 owner（与
    // `requests_workspace_cases_test` 的接管前置同款）：同一 session 不允许被
    // 两个活宿主同时持有，先 close（delete=false，不删数据）再 load。
    // close 要求会话任务终态且无未确认的外部执行：先取消上面注册的演示任务
    // （kill 为空操作 ⇒ 无真实执行），并确认该作用域确已排空（Kill 句柄无法
    // 自证清空，生产由工具层调用点在执行真停止后确认）。
    manager.cancel("wf-revision-1").unwrap();
    manager.confirm_external_execution_stopped("wf-revision-1");
    handle_request(
        "session/close",
        &json!({"sessionId": sid}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    let second_cfg = make_server_config(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let load_mock = Arc::new(MockTransport::default());
    let load_transport: Arc<dyn crate::transport::AcpTransport> = load_mock.clone();
    let mut loaded_sessions = HashMap::new();
    handle_request(
        "session/load",
        &json!({"sessionId":sid,"cwd":tmp.path()}),
        &second_cfg,
        &mut loaded_sessions,
        &load_transport,
    )
    .await
    .unwrap();
    let loaded = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(data) = load_mock
                .notifications()
                .iter()
                .find_map(|(method, payload)| {
                    (method == "peri/unstable_event" && payload["event"] == "bg-task-snapshot")
                        .then(|| payload["data"].clone())
                })
            {
                break data;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(loaded["revision"].is_u64());
    assert!(loaded["tasks"].is_array());
}

/// [回归测试] cancel-bg-task 对 Workflow 类型任务必须真正 kill（issue 2026-08-05）。
/// 历史 bug：Workflow 注册时固定 `Kill(None)`，cancel() 只 warn 并返回 success——
/// 条目移除但 runner 继续运行。修复后 kill 闭包（生产路径转发
/// WorkflowTaskRegistry::kill）随注册存入条目，cancel() 触发闭包。
/// 本测试用探针闭包在 RPC 层锁定该行为。

#[tokio::test]
async fn test_cancel_bg_task_workflow_invokes_kill_closure() {
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
    let created = handle_request(
        "session/new",
        &json!({"cwd":tmp.path()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_owned();

    let killed = Arc::new(AtomicBool::new(false));
    let killed_clone = killed.clone();
    let registry = &cfg.session_manager.get_session(&sid).unwrap().task_manager;
    registry
        .register(peri_acp_types::tasks::BgTaskRegistration {
            task_id: "wf-run-1".to_string(),
            kind: BgTaskKind::Workflow,
            summary: "wf cancel test".to_string(),
            pid: None,
            kill: Some(Box::new(move || {
                killed_clone.store(true, Ordering::SeqCst);
            })),
        })
        .unwrap();
    assert_eq!(registry.active_count(), 1);

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": sid, "taskId": "wf-run-1" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(
        result.is_ok(),
        "取消 Workflow 任务应返回 success，实际: {:?}",
        result.err()
    );
    assert!(
        killed.load(Ordering::SeqCst),
        "cancel-bg-task 必须触发 kill 闭包（runner 真正被终止），而非仅移除条目"
    );
    assert_eq!(registry.active_count(), 0, "取消后条目应从 registry 移除");
}

/// [回归测试] cancel-bg-task 会话不存在时必须如实报错（issue 2026-08-05）。
/// 历史 bug：静默返回 success，掩盖"取消未生效"。
#[tokio::test]
async fn test_cancel_bg_task_session_not_found_returns_error() {
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

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": "no-such-session", "taskId": "wf-run-1" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "会话不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "错误消息应提及 session not found，实际: {}",
        err.message
    );
}

/// [回归测试] cancel-bg-task 任务不存在时必须如实报错（issue 2026-08-05）。
/// 与 session_not_found 区分（错误消息不同），客户端可据此判断重试策略。
#[tokio::test]
async fn test_cancel_bg_task_task_not_found_returns_error() {
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
    let sid = "cancel-bg-session".to_string();
    cfg.session_manager
        .new_session_with_id(&sid, tmp.path().to_str().unwrap())
        .await
        .unwrap();

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": sid, "taskId": "no-such-task" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "任务不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("not found"),
        "错误消息应提及 not found，实际: {}",
        err.message
    );
}

// ── workflow/* 消费面：经会话作用域池（P4，设计 §6.3）──
//
// 迁移前这几条用例用真实 middleware + registry 断言；迁移后状态源是**会话作用域
// 池**中的 builtin `workflow` 实例（`AcpServerConfig::mcp_pool`）。本文件用假池
// 锁定 ACP 侧映射：请求 → 池调用（实例/工具/参数）与 池文本 → ACP 响应/错误码。
// 真实实例链路（池 → WorkflowMcpServer → WorkflowRuntime）的验收由用户承担。

/// 假池：记录 `(instance, 工具或 resources/read, 参数)` 调用，按预置文本应答。
/// 只实现 ACP 侧消费的三个方法面，其余走 `McpPoolPort` 默认实现。
struct FakeWorkflowPool {
    calls: std::sync::Mutex<Vec<(String, String, Value)>>,
    tool_reply: std::sync::Mutex<Result<String, String>>,
    runs_reply: std::sync::Mutex<Result<String, String>>,
}

impl FakeWorkflowPool {
    fn new(tool_reply: Result<&str, &str>, runs_reply: Result<&str, &str>) -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            tool_reply: std::sync::Mutex::new(tool_reply.map(str::to_owned).map_err(str::to_owned)),
            runs_reply: std::sync::Mutex::new(runs_reply.map(str::to_owned).map_err(str::to_owned)),
        }
    }

    fn set_tool_reply(&self, reply: Result<&str, &str>) {
        *self.tool_reply.lock().unwrap() = reply.map(str::to_owned).map_err(str::to_owned);
    }

    fn calls(&self) -> Vec<(String, String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl McpPoolPort for FakeWorkflowPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn shutdown(&self) -> McpPoolShutdownReport {
        McpPoolShutdownReport::Complete {
            settled_services: 0,
            failed_services: 0,
        }
    }

    fn snapshot(&self) -> Value {
        json!({})
    }

    async fn call_instance_tool(
        &self,
        instance: &str,
        tool: &str,
        arguments: Value,
        _request_meta: Option<Value>,
    ) -> Result<String, String> {
        self.calls
            .lock()
            .unwrap()
            .push((instance.to_string(), tool.to_string(), arguments));
        self.tool_reply.lock().unwrap().clone()
    }

    async fn read_instance_resource(&self, instance: &str, uri: &str) -> Result<String, String> {
        self.calls.lock().unwrap().push((
            instance.to_string(),
            "resources/read".to_string(),
            json!(uri),
        ));
        self.runs_reply.lock().unwrap().clone()
    }
}

/// 夹具：一个已登记会话（有本机绑定、无 environment ⇒ `handle_request` 走 base cfg）
/// + 一个装进 base cfg 的假池。
struct WorkflowPoolFixture {
    cfg: AcpServerConfig,
    sessions: HashMap<String, SessionState>,
    sid: String,
    pool: Arc<FakeWorkflowPool>,
    _tmp: tempfile::TempDir,
}

async fn workflow_pool_fixture(
    tool_reply: Result<&str, &str>,
    runs_reply: Result<&str, &str>,
) -> WorkflowPoolFixture {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    let pool = Arc::new(FakeWorkflowPool::new(tool_reply, runs_reply));
    cfg.mcp_pool = Some(Arc::clone(&pool) as Arc<dyn McpPoolPort>);
    let mut sessions = HashMap::new();
    let cwd = tmp.path().to_str().unwrap();
    let sid = register_session_with_history(&mut sessions, cwd, &cfg).await;
    WorkflowPoolFixture {
        cfg,
        sessions,
        sid,
        pool,
        _tmp: tmp,
    }
}

/// 回归：kill_run 经池调用 `WorkflowKill{runId}`，池文本决定 `killed`（P4）。
///
/// 迁移前按会话 registry 断言；迁移后 ACP 侧保证：①调用落在**请求 sessionId
/// 对应会话的池**上；②参数只带 `runId`（run 级 kill 不得扩成单 agent kill）；
/// ③命中文本以 `killed.` 结尾 ⇒ `killed:true`；④池不可用如实报错。
#[tokio::test]
async fn test_kill_run_maps_pool_reply_to_killed() {
    let fixture = workflow_pool_fixture(Ok("Workflow run 'run-b' killed."), Ok("[]")).await;
    let mut sessions = fixture.sessions;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    // 命中：killed:true，且调用参数只有 runId
    let resp = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": fixture.sid, "runId": "run-b" }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["killed"], true);
    assert_eq!(
        fixture.pool.calls(),
        vec![(
            "workflow".to_string(),
            "WorkflowKill".to_string(),
            json!({ "runId": "run-b" })
        )],
        "kill_run 应只带 runId 调用池内 workflow 实例的 WorkflowKill"
    );

    // 未命中（run 不在该会话实例的 registry）→ 工具级文本，killed:false
    fixture
        .pool
        .set_tool_reply(Ok("Workflow run 'run-a' not found."));
    let resp = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": fixture.sid, "runId": "run-a" }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["killed"], false);

    // 池不可用（实例未连接等）→ -32603，文本如实透传
    fixture
        .pool
        .set_tool_reply(Err("workflow instance is not connected"));
    let err = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": fixture.sid, "runId": "run-a" }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("not connected"),
        "池错误文本应透传，实际: {}",
        err.message
    );

    // 缺失 sessionId → -32602
    let err = handle_request(
        "workflow/kill_run",
        &json!({ "runId": "run-a" }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报错，实际: {}",
        err.message
    );

    // session 不存在 → 明确错误（修复前静默返回 killed:false）
    let err = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": "no-such-session", "runId": "run-a" }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "会话不存在应报 session not found，实际: {}",
        err.message
    );
}

/// 会话无池（无 workflow 消费面）→ kill 明确报错（兼容文本 `session not found`）。
///
/// base cfg 无池（host 级装配）时 `handle_request` 的 cfg 即 base cfg；
/// 迁移前「会话存在但无 middleware」走同一文本与错误码（-32602）。
#[tokio::test]
async fn test_kill_run_without_pool_reports_session_not_found() {
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
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;

    let err = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": sid, "runId": "run-a" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "无消费面的会话应报错，实际: {}",
        err.message
    );
}

/// list_runs 经池读 `workflow://runs` 资源并解析 JSON（P4）。
///
/// 覆盖：资源正文 → `{"runs": ...}` 透传、读失败 -32603、正文非 JSON 如实报错。
#[tokio::test]
async fn test_list_runs_reads_pool_resource() {
    let fixture = workflow_pool_fixture(
        Ok("unused"),
        Ok(r#"[{"run_id":"run-a","status":"running"}]"#),
    )
    .await;
    let mut sessions = fixture.sessions;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    let resp = handle_request(
        "workflow/list_runs",
        &json!({ "sessionId": fixture.sid }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(
        resp["runs"],
        json!([{ "run_id": "run-a", "status": "running" }])
    );
    assert_eq!(
        fixture.pool.calls(),
        vec![(
            "workflow".to_string(),
            "resources/read".to_string(),
            json!("workflow://runs")
        )],
        "list_runs 应读池内 workflow 实例的 runs 资源"
    );

    // 资源读取失败 → -32603，文本透传
    *fixture.pool.runs_reply.lock().unwrap() = Err("workflow instance not connected".to_string());
    let err = handle_request(
        "workflow/list_runs",
        &json!({ "sessionId": fixture.sid }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("not connected"),
        "池错误文本应透传，实际: {}",
        err.message
    );

    // 正文不是 JSON（资源不是 run 索引）→ 如实报错，不伪造空列表
    *fixture.pool.runs_reply.lock().unwrap() = Ok("not json".to_string());
    let err = handle_request(
        "workflow/list_runs",
        &json!({ "sessionId": fixture.sid }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("not valid JSON"),
        "非 JSON 正文应报错，实际: {}",
        err.message
    );
}

/// 无池（无 workflow 消费面）时 list_runs 保持历史语义：空列表而非错误。
#[tokio::test]
async fn test_list_runs_without_pool_is_empty() {
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
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;

    let resp = handle_request(
        "workflow/list_runs",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["runs"], json!([]));

    // 缺失 sessionId 仍为协议错误（-32602）
    let err = handle_request(
        "workflow/list_runs",
        &json!({}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报错，实际: {}",
        err.message
    );
}

/// 回归：kill_agent 经池调用 `WorkflowKill{runId, agentId}`，池文本决定 `killed`（P4）。
///
/// 深层 kill 依赖 runner 内部 active_channels（外部不可注入），此处锁定协议层：
/// 参数逐字透传（含 agentId），缺失/不存在的 session 如实报错、不触达池。
#[tokio::test]
async fn test_kill_agent_maps_pool_reply_to_killed() {
    let fixture =
        workflow_pool_fixture(Ok("Agent 3 in workflow run 'run-x' killed."), Ok("[]")).await;
    let mut sessions = fixture.sessions;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    // 命中：killed:true，参数带 runId + agentId
    let resp = handle_request(
        "workflow/kill_agent",
        &json!({ "sessionId": fixture.sid, "runId": "run-x", "agentId": 3 }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["killed"], true);
    assert_eq!(
        fixture.pool.calls(),
        vec![(
            "workflow".to_string(),
            "WorkflowKill".to_string(),
            json!({ "runId": "run-x", "agentId": 3 })
        )],
        "kill_agent 应带 runId + agentId 调用池内实例"
    );

    // 无该 agent 的活跃通道 → 工具级文本，killed:false（不报错）
    fixture
        .pool
        .set_tool_reply(Ok("No active agent 3 in workflow run 'run-x'."));
    let resp = handle_request(
        "workflow/kill_agent",
        &json!({ "sessionId": fixture.sid, "runId": "run-x", "agentId": 3 }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["killed"], false);

    // 缺失 agentId → -32602，且不触达池
    let err = handle_request(
        "workflow/kill_agent",
        &json!({ "sessionId": fixture.sid, "runId": "run-x" }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing agentId"),
        "缺失 agentId 应报错，实际: {}",
        err.message
    );
    assert_eq!(
        fixture.pool.calls().len(),
        2,
        "参数缺失不得触达池（仍为前两次成功调用）"
    );

    // 缺失 sessionId → -32602
    let err = handle_request(
        "workflow/kill_agent",
        &json!({ "runId": "run-x", "agentId": 1 }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报错，实际: {}",
        err.message
    );

    // session 不存在 → 明确错误（修复前静默返回 killed:false）
    let err = handle_request(
        "workflow/kill_agent",
        &json!({ "sessionId": "no-such-session", "runId": "run-x", "agentId": 1 }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "会话不存在应报 session not found，实际: {}",
        err.message
    );
}

/// 回归：resume 以 `resumeFromRunId` 调池内 `Workflow` 工具，新 run id 取自
/// 内核启动消息（P4；设计 §6.3：内核完成 state/journal 读取与启动，ACP 不拼装）。
///
/// 覆盖：正常恢复响应形状、内核错误透传（-32603）、启动消息缺 run id 如实报错、
/// 缺失 sessionId/会话不存在。
#[tokio::test]
async fn test_resume_calls_workflow_tool_with_resume_from() {
    let started = "Workflow 'wf-resume' started.\n\
                   run_id: 0198f0aa-1111-7000-8000-000000000001\n\n\
                   The workflow is running in the background.";
    let fixture = workflow_pool_fixture(Ok(started), Ok("[]")).await;
    let mut sessions = fixture.sessions;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    // 正常恢复：ACP 只传 resumeFromRunId（内核完成 state/journal 读取与启动），
    // 新 run id 从内核启动消息读取。
    let old_run = "0198f0aa-0000-7000-8000-000000000000";
    let resp = handle_request(
        "workflow/resume",
        &json!({ "sessionId": fixture.sid, "runId": old_run }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(
        resp["runId"], "0198f0aa-1111-7000-8000-000000000001",
        "新 run id 应来自内核启动消息"
    );
    assert_eq!(resp["resumedFrom"], old_run);
    assert_eq!(
        fixture.pool.calls(),
        vec![(
            "workflow".to_string(),
            "Workflow".to_string(),
            json!({ "resumeFromRunId": old_run })
        )],
        "resume 应以 resumeFromRunId 调 Workflow，不拼装其它参数"
    );

    // 内核失败（如 state 缺失 / 未就绪）→ -32603，文本如实透传
    fixture
        .pool
        .set_tool_reply(Err("Failed to read workflow state: missing state.json"));
    let err = handle_request(
        "workflow/resume",
        &json!({ "sessionId": fixture.sid, "runId": old_run }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("Failed to read workflow state"),
        "内核错误文本应透传，实际: {}",
        err.message
    );

    // 启动消息缺 run id 行 → 如实报错，不伪造 runId
    fixture.pool.set_tool_reply(Ok("Workflow started (no id)"));
    let err = handle_request(
        "workflow/resume",
        &json!({ "sessionId": fixture.sid, "runId": old_run }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("has no run id"),
        "启动消息缺 run id 应报错，实际: {}",
        err.message
    );

    // 缺失 sessionId → -32602
    let err = handle_request(
        "workflow/resume",
        &json!({ "runId": "no-such-run" }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报错，实际: {}",
        err.message
    );

    // session 不存在 → 明确错误（修复前可能误用第一个 session 的 middleware）
    let err = handle_request(
        "workflow/resume",
        &json!({ "sessionId": "no-such-session", "runId": "no-such-run" }),
        &fixture.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "会话不存在应报 session not found，实际: {}",
        err.message
    );
}

// ── session/delete（标准 ACP，agentclientprotocol.com/protocol/v1/session-delete）──
