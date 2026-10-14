//! [高 1 回归] canonical idle 执行的**取消父** = session 生命周期令牌，不是装配哨兵。
//!
//! 缺陷形状：装配点把 `SessionEnvironment` 的 `activation`（池初始化哨兵，语义
//! 「会话已装配」，`activate()` 在会话创建期即取消、池初始化等它）当作 service 的
//! `session_cancel` 与规则链 `AssemblyContext.cancel`。
//! `CancellationToken::child_token()` 对已取消父返回**立即取消**的子节点，而审批
//! hook 走 `biased` select（取消优先于 hook 结果）⇒ 每次 App 回调 / Host invoke 在
//! 审批之前即被判中断（`Interrupted`），outcome 落 `OutcomeUnknown`、relay 再投影成
//! 上游协议错误：生产装配下 App 面与 Host invoke **全不可用**，且
//! `session_cancelled()` 恒真。
//!
//! 用例走生产链路（`session/new` → `assemble_with_frozen` → `activate()`），断言
//! 装配产物上的取消父状态；`session/close`（`shutdown()`）之后必须翻转。
//! 既有测试全部手工构造 service（自备新令牌），因此该缺陷此前无回归——本文件补的
//! 正是「装配点接对了哪根令牌」这一层。
//!
//! 反向验证（mutation）：把装配点的取消父改回 `activation.clone()` ⇒ 第二条断言失败。
//!
//! `#[serial]`：builtin 注入开关是进程级 env（与 `requests_agent_catalog_test` 同因）。

use super::*;

/// 会话 cwd：真实目录（装配期要读它）。
fn idle_lifetime_fixture() -> (tempfile::TempDir, String) {
    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("workspace");
    std::fs::create_dir(&target).unwrap();
    std::fs::create_dir_all(target.join(".peri")).unwrap();
    (tmp, target.to_string_lossy().into_owned())
}

/// 生产装配配置：bare 池 + **apps 能力打开**（canonical idle 入口真实构造）。
async fn idle_lifetime_server_config(
    tmp: &tempfile::TempDir,
    startup_cwd: String,
) -> AcpServerConfig {
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, tmp).await;
    // 生产 stdio 装配**不注入** entitlement 身份层 ⇒ `apps/*` 默认拒绝（R4 方案 b）；
    // 本用例走生产装配链路验证 apps 面，故注入「部署侧已接入身份层」的测试端口。
    cfg.session_authorization = crate::host::apps_facade::entitlement::test_support::allow_all();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd,
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::deployment_profile(true),
        capabilities: Default::default(),
    });
    cfg
}

/// `session/new` 激活后的取消父必须仍有效；`session/close` 之后必须取消。
#[tokio::test]
#[serial]
async fn session_new_keeps_idle_invocation_cancel_parent_live_until_close() {
    let (tmp, target) = idle_lifetime_fixture();
    let cfg = idle_lifetime_server_config(&tmp, target.clone()).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();

    let created = handle_request(
        "session/new",
        &json!({"cwd": target}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 必须成功");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let environment = Arc::clone(
        sessions[&id]
            .environment
            .as_ref()
            .expect("会话环境（生产装配路径）"),
    );

    assert!(
        environment.cfg.mcp_apps_relay.is_some(),
        "apps 能力打开时必须构造 canonical idle 入口，否则下面的断言落到空对象上"
    );
    assert_eq!(
        environment.session_invocation_cancelled(),
        Some(false),
        "会话创建/激活后 canonical idle 执行的取消父必须仍有效：`activate()` 取消的是\
         装配哨兵（池初始化用），不是 session 生命周期令牌；误用哨兵会让每次 idle 执行的\
         子令牌自出生即取消，审批 hook 的 biased select 直接判中断 ⇒ App 回调与 Host \
         invoke 全不可用"
    );

    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/close 必须成功");
    assert_eq!(
        environment.session_invocation_cancelled(),
        Some(true),
        "会话关闭必须取消 canonical idle 执行的取消父（在飞执行有界收尾）"
    );
}

// ── §8.1 低 5：生产装配（`session/new` → `assemble_with_frozen`）的 Apps 端到端 ──

/// 记录审批帧并按键应答的 ACP 客户端桩（本文件专用：生产装配路径上的审批可达性）。
#[derive(Default)]
struct ApprovalTransport {
    frames: std::sync::Mutex<Vec<(String, Value)>>,
}

impl ApprovalTransport {
    fn permission_frames(&self) -> Vec<Value> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "session/request_permission")
            .map(|(_, params)| params.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl crate::transport::AcpTransport for ApprovalTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.frames
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        if method == "session/request_permission" {
            return Ok(json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}}));
        }
        Ok(Value::Null)
    }

    async fn send_notification(&self, _: &str, _: Value) -> Result<(), AcpError> {
        Ok(())
    }

    async fn recv(&self) -> Option<crate::transport::types::IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _: crate::transport::types::RequestId,
        _: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

/// 进程内 MCP peer（计数 + 工具名），只为真实执行提供端点。
struct AssemblyServer {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    tools: Arc<std::sync::Mutex<Vec<String>>>,
}

impl rmcp::ServerHandler for AssemblyServer {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        rmcp::model::ServerConfig::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "peri-canvas",
            "assembly_test",
        ))
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.tools.lock().unwrap().push(request.name.to_string());
        let result: rmcp::model::CallToolResult = serde_json::from_value(json!({
            "content": [{"type": "text", "text": "assembled canvas"}]
        }))
        .expect("夹具结果必须可解析");
        Ok(rmcp::model::CallToolResponse::Complete(result))
    }
}

/// 模型侧派发器桩（只为白名单解析；本用例不在模型面执行）。
struct AssemblyDispatcher;

#[async_trait::async_trait]
impl peri_acp_types::tools::EffectiveToolDispatcher for AssemblyDispatcher {
    async fn dispatch(
        &self,
        _: peri_acp_types::tool_invocation::EffectiveToolCall,
        _: tokio_util::sync::CancellationToken,
    ) -> Result<peri_acp_types::tools::ToolOutput, peri_acp_types::tools::EffectiveToolError> {
        unreachable!("本用例不在模型面执行")
    }

    fn tools(&self) -> Vec<peri_acp_types::tools::EffectiveToolDefinition> {
        Vec::new()
    }

    fn admitted_mcp_tool_name(&self, server: &str, wire_name: &str) -> Option<String> {
        (server == ASSEMBLY_SERVER).then(|| format!("mcp__{server}__{wire_name}"))
    }
}

const ASSEMBLY_SERVER: &str = "cursor-canvas";
const ASSEMBLY_TOOL: &str = "show_canvas";

/// 装配路径的进程内 peer（计数 + 记录工具名）与清理句柄。
struct AssemblyPeer {
    client: rmcp::service::RunningService<
        rmcp::service::RoleClient,
        rmcp::model::InitializeRequestParams,
    >,
    server: tokio::task::JoinHandle<()>,
}

impl AssemblyPeer {
    async fn close(self) {
        let _ = self.client.cancel().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), self.server).await;
    }
}

/// 把进程内 peer 装进**装配产物的池**（工具面 = 单个 App 工具）。
async fn install_assembly_peer(
    pool: &Arc<peri_middlewares::mcp::McpClientPool>,
) -> (
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::Mutex<Vec<String>>>,
    AssemblyPeer,
) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tools = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server_calls = Arc::clone(&calls);
    let server_tools = Arc::clone(&tools);
    let server_task = tokio::spawn(async move {
        use rmcp::ServiceExt;
        AssemblyServer {
            calls: server_calls,
            tools: server_tools,
        }
        .serve(server_io)
        .await
        .unwrap()
        .waiting()
        .await
        .unwrap();
    });
    let client_info = rmcp::model::InitializeRequestParams::new(
        rmcp::model::ClientCapabilities::default(),
        rmcp::model::Implementation::new("peri-acp-assembly-test", "0.1"),
    );
    use rmcp::ServiceExt;
    let client = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client_info.serve(client_io),
    )
    .await
    .expect("peer 握手超时")
    .expect("peer 握手失败");
    let tool: rmcp::model::Tool = serde_json::from_value(json!({
        "name": ASSEMBLY_TOOL,
        "description": "canvas",
        "inputSchema": {"type": "object"},
        "_meta": {"ui": {"resourceUri": "ui://app", "visibility": ["app", "model"]}}
    }))
    .expect("工具声明必须可解析");
    pool.install_client_for_tests(Arc::new(peri_middlewares::mcp::client::McpClientHandle {
        name: ASSEMBLY_SERVER.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(client.peer().clone()),
        tools: vec![tool],
        resources: vec![],
        status: peri_middlewares::mcp::client::ClientStatus::Connected,
        oauth_status: peri_middlewares::mcp::client::OAuthStatus::None,
        source: None,
        url: None,
        skills_capable: false,
    }));
    (
        calls,
        tools,
        AssemblyPeer {
            client,
            server: server_task,
        },
    )
}

/// [低 5] `session/new` 真实装配的端到端：装配产物上的池 / relay / facade 能跑通
/// 「签发 record → attach 快照 → 审批 → canonical 执行」，且审批帧确实送达连接。
/// 4a 之前所有 Apps 用例都手工构造 service —— 这正是高 1（取消父接错）漏检的原因。
#[tokio::test]
#[serial]
async fn session_new_assembled_facade_runs_app_callback_end_to_end() {
    let (tmp, target) = idle_lifetime_fixture();
    let mut cfg = idle_lifetime_server_config(&tmp, target.clone()).await;
    // 审批必须真实发生（本用例要证明审批帧经装配产物的 router 送达本连接）：
    // 夹具默认 Bypass（零审批），这里改成需要审批的模式。
    cfg.permission_mode = peri_acp_types::permission::SharedPermissionMode::new(
        peri_acp_types::permission::PermissionMode::Default,
    );
    let transport = Arc::new(ApprovalTransport::default());
    let erased: Arc<dyn crate::transport::AcpTransport> =
        Arc::clone(&transport) as Arc<dyn crate::transport::AcpTransport>;
    let mut sessions = HashMap::new();

    let created = handle_request(
        "session/new",
        &json!({"cwd": target}),
        &cfg,
        &mut sessions,
        &erased,
    )
    .await
    .expect("session/new 必须成功");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let environment = Arc::clone(
        sessions[&id]
            .environment
            .as_ref()
            .expect("会话环境（生产装配路径）"),
    );
    let facade = environment
        .apps_facade()
        .expect("apps 能力打开时必须装配 facade（否则本用例落到空对象上）")
        .clone();

    // 真实池：装入进程内 peer（与装配点同一个池实例）。
    let Some(port) = environment.cfg.mcp_pool.clone() else {
        panic!("会话装配必须构造 MCP 池");
    };
    let Ok(pool) = port.downcast_arc::<peri_middlewares::mcp::McpClientPool>() else {
        panic!("池实现为 McpClientPool");
    };
    let (calls, tools, peer) = install_assembly_peer(&pool).await;

    // 会话环境已由 `session/new` 装配：本连接的上下文注册为在线 endpoint
    //（server_loop 的生产注册点见 `dispatch_request`；本用例直调 `handle_request`
    // 与 facade，故显式登记同一连接身份）。
    let mut connection = crate::host::connection::ConnectionContext::new(true);
    connection.commit_initialize(peri_acp_types::PeriCaps::default());
    environment
        .interaction_router()
        .register_connection(connection.id(), Arc::clone(&erased))
        .expect("装配产物的 router 必须可注册在线 endpoint");

    // facade 的会话身份必须来自装配点：异会话 `sessionId` 只作定位 ⇒ 显式拒绝。
    let foreign = crate::host::mcp_apps::handle_request(
        "peri/mcp/apps/list",
        &json!({"sessionId": "some-other-session"}),
        &mut connection,
        Some(&facade),
    )
    .await
    .expect_err("异会话 sessionId 必须被拒绝");
    assert_eq!(foreign.data.unwrap()["kind"], "invalid_session");

    // 模型侧实例化（真实 bridge + 真实 peer）→ 签发 record。
    let bridges =
        peri_middlewares::mcp::tool_bridge::build_tool_bridges_visible_to(&pool, Some(&id));
    let bridge = bridges
        .iter()
        .find(|bridge| bridge.name() == format!("mcp__{ASSEMBLY_SERVER}__{ASSEMBLY_TOOL}"))
        .expect("会话可见视图必须含 App 工具");
    let invocation_id = peri_acp_types::tool_invocation::InvocationId::generate();
    let context = peri_acp_types::tools::ToolContext::new(&[], ".")
        .with_session_identity(&id, "turn-assembly")
        .with_invocation(
            Arc::new(AssemblyDispatcher),
            invocation_id,
            peri_acp_types::tool_invocation::InvocationOrigin::Host {
                namespace: "test/assembly".to_string(),
                seq: 1,
            },
            tokio_util::sync::CancellationToken::new(),
        );
    bridge
        .invoke_output(json!({"value": 1}), context)
        .await
        .expect("装配路径的实例化调用必须成功");
    let calls_after_instantiate = calls.load(std::sync::atomic::Ordering::SeqCst);

    // 经真实 ACP 分发走 `apps/list` / `apps/attach`（生产 facade + ledger）。
    let listed = crate::host::mcp_apps::handle_request(
        "peri/mcp/apps/list",
        &json!({"sessionId": id}),
        &mut connection,
        Some(&facade),
    )
    .await
    .expect("apps/list 必须成功");
    let listed: peri_acp_types::mcp_apps_facade::AppsListResponse =
        serde_json::from_value(listed).expect("list 响应必须可解码");
    let app_id = listed
        .descriptors
        .iter()
        .find(|descriptor| descriptor.instantiating_tool == ASSEMBLY_TOOL)
        .expect("record 必须出现在 apps/list")
        .app_id
        .clone();

    let attached = crate::host::mcp_apps::handle_request(
        "peri/mcp/apps/attach",
        &json!({"sessionId": id, "appId": app_id, "clientViewId": "view-1"}),
        &mut connection,
        Some(&facade),
    )
    .await
    .expect("apps/attach 必须成功");
    let attached: peri_acp_types::mcp_apps_facade::AppsAttachResponse =
        serde_json::from_value(attached).expect("attach 响应必须可解码");
    assert_eq!(
        attached.snapshot.initial_input.get("value"),
        Some(&json!(1)),
        "snapshot 的初始输入来自装配路径的真实执行"
    );
    assert_eq!(
        attached.snapshot.initial_result.as_value()["content"][0]["text"],
        "assembled canvas"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        calls_after_instantiate,
        "attach 不调用业务工具"
    );

    // App 回调：真实审批（本连接应答 allow_once）→ canonical 执行恰好一次。
    let binding = attached.attachment.clone();
    let first = crate::host::mcp_apps::handle_request(
        "peri/mcp/apps/request",
        &json!({
            "sessionId": id,
            "attachmentId": binding.attachment_id.as_str(),
            "epoch": binding.connection_epoch.get(),
            "seq": 1,
            "operation": {"method": "tools/call", "name": ASSEMBLY_TOOL, "arguments": {"value": 2}},
        }),
        &mut connection,
        Some(&facade),
    )
    .await
    .expect("apps/request 必须被受理");
    let first: peri_acp_types::mcp_apps_facade::AppsRequestResponse =
        serde_json::from_value(first).expect("request 响应必须可解码");
    assert!(first.admitted, "新序号必须登记（{:?}）", first.verdict);
    let waited = crate::host::mcp_apps::handle_request(
        "peri/mcp/apps/request",
        &json!({
            "sessionId": id,
            "attachmentId": binding.attachment_id.as_str(),
            "epoch": binding.connection_epoch.get(),
            "seq": 1,
            "operation": {"method": "tools/call", "name": ASSEMBLY_TOOL, "arguments": {"value": 2}},
        }),
        &mut connection,
        Some(&facade),
    )
    .await
    .expect("同 key 必须可取终态");
    let waited: peri_acp_types::mcp_apps_facade::AppsRequestResponse =
        serde_json::from_value(waited).expect("request 响应必须可解码");
    assert_eq!(
        waited.outcome,
        Some(
            peri_acp_types::mcp_apps_facade::AppsRequestOutcome::ToolsCall(
                peri_acp_types::mcp_apps::SessionInvocationOutcome::Completed(
                    peri_acp_types::tool_outcome::RawToolResult::new(json!({
                        "content": [{"type": "text", "text": "assembled canvas"}]
                    }))
                )
            )
        ),
        "生产装配路径必须真实执行并回传完整结果"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        calls_after_instantiate + 1,
        "恰好一次新增 wire 调用"
    );
    assert_eq!(
        transport.permission_frames().len(),
        1,
        "审批帧必须经装配产物的 router 送达本连接（低 5 的核心：生产装配真的能跑通）"
    );
    assert_eq!(
        tools.lock().unwrap().last().map(String::as_str),
        Some(ASSEMBLY_TOOL)
    );

    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &erased,
    )
    .await
    .expect("session/close 必须成功");
    peer.close().await;
}

// ── §8.1 中 4：idle 规则链消费**会话级** task manager ─────────────────────────

/// 捕获真实 `AssemblyContext.task_manager` 的探针装配器：只替代「装配中间件」这一步，
/// 上下文由**生产装配函数**构造（`build_idle_invocation_rules`）。
#[derive(Default)]
struct TaskManagerProbe {
    captured: std::sync::Mutex<Option<Arc<peri_agent::agent::async_tasks::TaskManager>>>,
}

impl TaskManagerProbe {
    fn captured(&self) -> Arc<peri_agent::agent::async_tasks::TaskManager> {
        self.captured
            .lock()
            .unwrap()
            .clone()
            .expect("装配函数必须构造并交出上下文")
    }
}

impl peri_agent::session::factory::MiddlewareChainAssembler for TaskManagerProbe {
    type Context = peri_agent::session::factory::AssemblyContext;
    type Output = peri_agent::session::factory::ChainAssembly;

    fn assemble(
        &self,
        _blueprint: &[peri_agent::session::factory::ChainSlot],
        ctx: &Self::Context,
    ) -> Self::Output {
        *self.captured.lock().unwrap() = Some(Arc::clone(&ctx.task_manager));
        peri_agent::session::factory::ChainAssembly {
            chain: peri_agent::middleware::chain::MiddlewareChain::new(),
            subagent_mw: None,
        }
    }
}

/// [中 4] idle 规则链必须消费**会话级** task manager（Hook 槽位是它在链上的唯一
/// 消费者：异步 hook 的 `spawn_owned` 与 command hook 的 `begin_external_execution`
/// 都经它进入会话任务作用域，随会话 drain / 关闭一起收口）。
///
/// 断言两件事（真实装配输入：host 装配产物的 `AcpServerConfig` + 真实
/// `SessionManager::new_session_task_manager()` 的会话级管理器）：
/// 1. **身份**：生产装配函数交出的 `AssemblyContext.task_manager` 与会话级管理器是
///    同一实例（`Arc::ptr_eq`）——不是自建游离实例；
/// 2. **失败分支行为**：会话级管理器不是本部署具体类型时，交出的是**已关闭作用域**
///    的管理器（`spawn_owned` / `begin_external_execution` 显式被拒 = no-op），
///    而不是一个能继续静默接收任务的游离实例。
#[tokio::test]
#[serial]
async fn idle_rules_consume_the_session_task_manager_and_fail_closed_on_type_mismatch() {
    let (tmp, target) = idle_lifetime_fixture();
    let cfg = idle_lifetime_server_config(&tmp, target.clone()).await;
    let peri_config = cfg.peri_config.read().clone();
    let config = Arc::new(peri_config);
    let provider = LlmProvider::from_config(&config).unwrap();
    let session_manager = cfg.session_manager.new_session_task_manager();
    let router = Arc::new(
        crate::host::interaction_router::SessionInteractionRouter::new(
            "idle-d2-session",
            Arc::new(tokio::sync::Mutex::new(())),
        ),
    );

    let probe = TaskManagerProbe::default();
    let _rules = crate::host::workspace_interaction::build_idle_invocation_rules(
        &probe,
        &cfg,
        &provider,
        &config,
        "idle-d2-session",
        &target,
        Arc::clone(&router) as Arc<dyn peri_acp_types::interaction::UserInteractionBroker>,
        tokio_util::sync::CancellationToken::new(),
        &std::collections::HashSet::new(),
        false,
        &cfg.session_resources,
        &session_manager,
    );
    let concrete = {
        let manager = Arc::clone(&session_manager);
        let any: Arc<dyn std::any::Any + Send + Sync> =
            manager as Arc<dyn std::any::Any + Send + Sync>;
        any.downcast::<peri_agent::agent::async_tasks::TaskManager>()
            .expect("夹具自检：会话级管理器必须是本部署的具体类型")
    };
    assert!(
        Arc::ptr_eq(&probe.captured(), &concrete),
        "idle 规则链的 task manager 必须是**会话级**同一实例（自建游离实例会让异步 hook \
         的取消令牌不随会话 drain，即中 4 缺口）"
    );

    // 失败分支：端口不是本部署具体类型 ⇒ no-op（已关闭作用域），显式拒绝后台接管。
    let foreign: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_acp_types::tasks::NoopTaskManager);
    let probe = TaskManagerProbe::default();
    let _rules = crate::host::workspace_interaction::build_idle_invocation_rules(
        &probe,
        &cfg,
        &provider,
        &config,
        "idle-d2-session",
        &target,
        Arc::clone(&router) as Arc<dyn peri_acp_types::interaction::UserInteractionBroker>,
        tokio_util::sync::CancellationToken::new(),
        &std::collections::HashSet::new(),
        false,
        &cfg.session_resources,
        &foreign,
    );
    let fallback = probe.captured();
    assert!(
        peri_acp_types::tasks::TaskManager::spawn_owned(&*fallback, Box::pin(async {})).is_err(),
        "downcast 失败必须交出 no-op 管理器（作用域已关闭），不得回落可接收任务的游离实例"
    );
}
