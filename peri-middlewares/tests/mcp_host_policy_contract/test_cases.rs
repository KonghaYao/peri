use super::AsyncBufReadExt as _;
use super::AsyncWriteExt as _;
use super::{
    async_trait, default_requires_approval, hook_state, json, permission, service, tool_dispatch,
    ApprovalDecision, Arc, AsyncRwTransport, AtomicBool, BTreeMap, BaseTool, BufReader,
    CancellationToken, ClientCapabilities, ClientLifecycleMode, ClientStatus,
    ExecuteExtraToolResolver, FrozenContext, Implementation, InitializeRequestParams,
    InteractionContext, InteractionResponse, McpClientHandle, McpClientPool, McpToolBridge,
    Middleware, MiddlewareChain, Mutex, OAuthStatus, Ordering, PermissionMiddleware,
    PermissionMode, Reasoning, RoleClient, RunningService, RwLock, SearchExtraTools, Session,
    SessionToolCatalog, SharedPermissionMode, SharedToolMap, StageContext, ToolCall, ToolContext,
    ToolSearchIndex, ToolSearchMiddleware, TrustedInteractionOwner, UserInteractionBroker, Value,
    TOOL_INVOKE_NAME, TOOL_SEARCH_NAME,
};

// ─── 真实 MCP wire fixture（duplex + rmcp 官方 client service）─────────────────

pub(super) const FIXTURE_SERVER: &str = "host-fixture";

pub(super) const FIXTURE_SESSION: &str = "host-policy-session";

pub(super) const REQUIRED_TOOL: &str = "write_note";

pub(super) const DEFERRED_TOOL: &str = "read_note";

/// `_meta.ui.visibility = ["app"]`：server 声明的「仅 App 可用」工具（H5）。
pub(super) const APP_ONLY_TOOL: &str = "app_only_note";

pub(super) fn effective_name(tool: &str) -> String {
    format!("mcp__{FIXTURE_SERVER}__{tool}")
}

pub(super) fn tool_declaration(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

/// 一次 `tools/call` 的 wire 事实：server 端收到的原始参数。
#[derive(Default)]
pub(super) struct WireLog {
    pub(super) calls: Mutex<Vec<Value>>,
    /// 每次收到 `tools/call` 唤醒；`Notify::notify_one` 会保存 permit，
    /// 因此「先到调用、后 await」与「先 await、后到调用」都成立（无睡眠）。
    pub(super) call_reached: tokio::sync::Notify,
    /// `Some` 时 `tools/call` 挂起，直到测试显式放行（在飞取消用例）。
    pub(super) release: Option<Arc<tokio::sync::Notify>>,
}

impl WireLog {
    pub(super) fn calls(&self) -> Vec<Value> {
        self.calls.lock().clone()
    }

    pub(super) fn called_tool_names(&self) -> Vec<String> {
        self.calls()
            .iter()
            .filter_map(|params| params["name"].as_str().map(str::to_string))
            .collect()
    }
}

/// 一个已连接的 fixture server：持有真实 client service，drop 即断开 transport。
pub(super) struct Fixture {
    pub(super) handle: Arc<McpClientHandle>,
    pub(super) log: Arc<WireLog>,
    pub(super) pool: Arc<McpClientPool>,
    pub(super) _task_manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    pub(super) service: RunningService<RoleClient, InitializeRequestParams>,
}

impl Fixture {
    pub(super) fn bridge(&self, tool: &str) -> McpToolBridge {
        let declaration = self
            .handle
            .tools
            .iter()
            .find(|candidate| candidate.name.as_ref() == tool)
            .expect("fixture must expose the requested tool on the wire");
        McpToolBridge::new(FIXTURE_SERVER, declaration, Arc::clone(&self.handle))
            .with_output_store(&self.pool, Some(FIXTURE_SESSION))
    }
}

/// 启动一个最小 MCP server（initialize / tools/list / tools/call），
/// 让真实 rmcp client service 完成握手并返回由 wire 声明的工具。
pub(super) async fn spawn_fixture(blocking_call: bool) -> Fixture {
    let pool = Arc::new(McpClientPool::new_pending());
    let task_manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager(FIXTURE_SESSION, &task_manager);
    let declarations = vec![
        tool_declaration(
            REQUIRED_TOOL,
            "写入一条笔记（写入型工具，默认需要审批）",
            json!({
                "type": "object",
                "properties": {"note": {"type": "string"}},
                "required": ["note"]
            }),
        ),
        tool_declaration(
            DEFERRED_TOOL,
            "读取一条笔记",
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}}
            }),
        ),
        json!({
            "name": APP_ONLY_TOOL,
            "description": "App 专用笔记面板（server 声明仅 app 可见）",
            "inputSchema": {
                "type": "object",
                "properties": {"id": {"type": "string"}}
            },
            "_meta": {"ui": {"visibility": ["app"]}}
        }),
    ];
    let release = blocking_call.then(|| Arc::new(tokio::sync::Notify::new()));
    let log = Arc::new(WireLog {
        calls: Mutex::new(Vec::new()),
        call_reached: tokio::sync::Notify::new(),
        release: release.clone(),
    });

    let (client_io, server_io) = tokio::io::duplex(16 * 1024);
    let server_log = Arc::clone(&log);
    tokio::spawn(serve_fixture(server_io, declarations, server_log, release));

    let (read, write) = tokio::io::split(client_io);
    let transport = AsyncRwTransport::new(read, write);
    let service = service::serve_client_with_lifecycle(
        InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::from_build_env(),
        ),
        transport,
        ClientLifecycleMode::Initialize,
    )
    .await
    .expect("fixture handshake must succeed");

    // 工具集来自真实 `tools/list` 响应：fixture 不做静态清单注入。
    let listed = service
        .peer()
        .list_tools(None)
        .await
        .expect("fixture tools/list must succeed");
    let handle = Arc::new(McpClientHandle {
        call_gate: Default::default(),
        name: FIXTURE_SERVER.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(service.peer().clone()),
        tools: listed.tools,
        resources: Vec::new(),
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
    });
    Fixture {
        handle,
        log,
        pool,
        _task_manager: task_manager,
        service,
    }
}

pub(super) async fn serve_fixture(
    io: tokio::io::DuplexStream,
    declarations: Vec<Value>,
    log: Arc<WireLog>,
    release: Option<Arc<tokio::sync::Notify>>,
) {
    let (read, mut write) = tokio::io::split(io);
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(method) = message["method"].as_str() else {
            continue;
        };
        let id = message.get("id").cloned();
        let response = match method {
            "initialize" => json!({
                "protocolVersion": message["params"]["protocolVersion"],
                "capabilities": {},
                "serverInfo": {"name": "host-policy-fixture", "version": "1"}
            }),
            "tools/list" => json!({"tools": declarations}),
            "tools/call" => {
                log.calls.lock().push(message["params"].clone());
                log.call_reached.notify_one();
                if let Some(release) = &release {
                    release.notified().await;
                }
                let tool = message["params"]["name"].as_str().unwrap_or_default();
                json!({
                    "content": [{"type": "text", "text": format!("fixture handled {tool}")}],
                    "isError": false
                })
            }
            // 通知（如 notifications/initialized）没有 id，不回包。
            _ => {
                let Some(id) = id else { continue };
                let error = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "Method not found"}
                });
                write
                    .write_all(format!("{error}\n").as_bytes())
                    .await
                    .expect("fixture must stay writable");
                continue;
            }
        };
        let Some(id) = id else { continue };
        let payload = json!({"jsonrpc": "2.0", "id": id, "result": response});
        write
            .write_all(format!("{payload}\n").as_bytes())
            .await
            .expect("fixture must stay writable");
        write.flush().await.expect("fixture flush must succeed");
    }
}

// ─── broker / chain / context fixtures ────────────────────────────────────────

/// 记录 HITL 交互内容并返回固定决策的 broker。
///
/// 只用于审批分支 fixture：它不能替代真实 UI/HITL 交互证据，这一点在文件头已声明。
pub(super) struct RecordingBroker {
    pub(super) seen: Mutex<Vec<(String, Value)>>,
    pub(super) approve: bool,
}

impl RecordingBroker {
    pub(super) fn new(approve: bool) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            approve,
        })
    }

    pub(super) fn seen(&self) -> Vec<(String, Value)> {
        self.seen.lock().clone()
    }
}

#[async_trait]
impl UserInteractionBroker for RecordingBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        ctx: InteractionContext,
    ) -> InteractionResponse {
        let InteractionContext::Approval { items } = ctx else {
            return InteractionResponse::Rejected;
        };
        let decision = if self.approve {
            ApprovalDecision::Approve { source: None }
        } else {
            ApprovalDecision::Reject {
                reason: "contract fixture rejected".to_string(),
                source: None,
            }
        };
        let mut decisions = Vec::with_capacity(items.len());
        for item in items {
            self.seen
                .lock()
                .push((item.tool_name.clone(), item.tool_input.clone()));
            decisions.push(decision.clone());
        }
        InteractionResponse::Decisions(decisions)
    }
}

/// 记录 `before_tools_batch` 可见调用的 middleware：direct 注入不得短路链上 hook 位。
pub(super) struct PolicyRecorder(pub(super) Arc<Mutex<Vec<ToolCall>>>);

#[async_trait]
impl Middleware for PolicyRecorder {
    fn name(&self) -> &str {
        "PolicyRecorder"
    }

    async fn before_tools_batch(
        &self,
        _state: &mut dyn hook_state::BeforeToolState,
        calls: &[ToolCall],
    ) -> Vec<peri_agent::error::AgentResult<ToolCall>> {
        self.0.lock().extend_from_slice(calls);
        calls.iter().cloned().map(Ok).collect()
    }
}

/// 生产构造形态的审批链：`PermissionMiddleware::with_shared_mode` +
/// 生产 `default_requires_approval`，外加链位记录器。
pub(super) fn approval_chain(
    broker: Arc<dyn UserInteractionBroker>,
    policy: Arc<Mutex<Vec<ToolCall>>>,
) -> MiddlewareChain {
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(PermissionMiddleware::with_shared_mode(
        broker,
        default_requires_approval,
        SharedPermissionMode::new(PermissionMode::Default),
        None,
    )));
    chain.add(Box::new(PolicyRecorder(policy)));
    chain
}

pub(super) fn make_context(
    tools: BTreeMap<String, Arc<dyn BaseTool>>,
    chain: MiddlewareChain,
) -> (
    StageContext,
    peri_agent::agent::events_v2::EventHandles,
    Arc<SessionToolCatalog>,
) {
    make_context_with_session(Some(FIXTURE_SESSION), tools, chain)
}

/// [`make_context`] 的会话身份可缺省形态：`session_id = None` 表示本次执行
/// **没有真实 ACP 会话身份**（如子 agent 自建 session），用于证明交互 fail-closed。
pub(super) fn make_context_with_session(
    session_id: Option<&str>,
    tools: BTreeMap<String, Arc<dyn BaseTool>>,
    chain: MiddlewareChain,
) -> (
    StageContext,
    peri_agent::agent::events_v2::EventHandles,
    Arc<SessionToolCatalog>,
) {
    let session = Session::new(
        Arc::from("/tmp/mcp-host-policy"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let (event_bus, handles) = peri_agent::agent::events_v2::EventBus::new(Default::default());
    let shared: SharedToolMap = Arc::new(RwLock::new(tools));
    let catalog = Arc::new(
        SessionToolCatalog::try_new(shared.read().clone(), None)
            .expect("fixture tool map must not contain conflicting aliases"),
    );
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_tools(shared)
        .with_tool_catalog(Arc::clone(&catalog))
        .with_tool_invocation_resolver(Arc::new(ExecuteExtraToolResolver::default()))
        .with_middleware_chain(Arc::new(chain))
        .with_event_bus(Arc::new(event_bus))
        .build();
    if let Some(session_id) = session_id {
        context
            .session
            .session_context
            .write()
            .insert("session_id".into(), session_id.into());
    }
    (context, handles, catalog)
}

pub(super) fn bridge_tools(bridges: &[McpToolBridge]) -> BTreeMap<String, Arc<dyn BaseTool>> {
    bridges
        .iter()
        .map(|bridge| {
            (
                bridge.name().to_string(),
                Arc::new(bridge.clone()) as Arc<dyn BaseTool>,
            )
        })
        .collect()
}

pub(super) fn direct_definition_names(
    snapshot: &peri_agent::session::tool_catalog::SessionToolCatalogSnapshot,
) -> Vec<String> {
    snapshot
        .direct_definitions
        .iter()
        .map(|definition| definition.name.clone())
        .collect()
}

/// 只提供 `CatalogState` 需要的两项能力的最小 state。
pub(super) struct CatalogStateFixture {
    pub(super) tools: SharedToolMap,
    pub(super) recalls: Vec<String>,
}

impl hook_state::CatalogState for CatalogStateFixture {
    fn local_tools(&self) -> Option<&SharedToolMap> {
        Some(&self.tools)
    }

    fn push_recall(&mut self, item: String) {
        self.recalls.push(item);
    }
}

/// `is_direct` 过滤边界的探针：名字与 MCP 无关，只因 `is_direct()==true` 而进入
/// direct 列表。它只用于证明 direct/deferred 的**分界是 `is_direct`**，
/// **不**构成「真实 MCP bridge 被提升为 direct」的证据（那归 D-02 / B-07）。
pub(super) struct DirectProbeTool {
    pub(super) name: &'static str,
    pub(super) invoked: Arc<AtomicBool>,
}

#[async_trait]
impl BaseTool for DirectProbeTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "direct boundary probe"
    }

    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn is_direct(&self) -> bool {
        true
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.invoked.store(true, Ordering::SeqCst);
        Ok("probe".to_string())
    }
}

pub(super) fn assert_policy_saw(chain_seen: &Arc<Mutex<Vec<ToolCall>>>, expected_name: &str) {
    let seen = chain_seen.lock();
    assert_eq!(
        seen.len(),
        1,
        "MCP bridge 调用必须完整经过 middleware chain（before_tools_batch）"
    );
    assert_eq!(seen[0].name, expected_name);
}

// ─── 契约 6：Permission + HITL + effective name（批准分支）────────────────────

/// 能力：Permission + HITL + effective tool name。
///
/// 一次批准后的 MCP 工具调用必须：审批看到 `mcp__{server}__{tool}`；wire 上只出现
/// **一次**、且是所属 namespace 的裸工具名；调用完整经过 middleware chain；
/// render 事件属于同一 turn。
#[tokio::test]
pub(super) async fn hitl_approval_gates_mcp_bridge_by_effective_name_and_calls_server_once() {
    let fixture = spawn_fixture(false).await;
    let bridge = fixture.bridge(REQUIRED_TOOL);
    let effective = effective_name(REQUIRED_TOOL);

    assert_eq!(bridge.name(), effective);
    assert_eq!(bridge.mcp_server_name(), Some(FIXTURE_SERVER));
    // 判定输入事实：生产敏感规则对 `mcp__` 前缀生效——审批只看工具名，
    // 不接触 BaseTool，因此 `is_direct` 在结构上无法影响这条决策。
    assert!(permission::default_requires_approval(&effective));

    let broker = RecordingBroker::new(true);
    let chain_seen = Arc::new(Mutex::new(Vec::new()));
    let chain = approval_chain(broker.clone(), Arc::clone(&chain_seen));
    let tools = bridge_tools(std::slice::from_ref(&bridge));
    let (context, mut events, _catalog) = make_context(tools, chain);
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new(
            "call-approved",
            effective.clone(),
            json!({"note": "hello"}),
        )],
    );

    let outcome = tool_dispatch::dispatch_tools(
        &context,
        &reasoning,
        &context.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
    )
    .await
    .expect("approved MCP call must settle without a fatal error");

    // Permission/HITL：broker 收到的名字是 effective name，参数原样透传。
    assert_eq!(
        broker.seen(),
        vec![(effective.clone(), json!({"note": "hello"}))],
        "HITL 必须看到 effective tool name，而不是裸 MCP 工具名"
    );

    // 链未被绕过：before_tools_batch 观察到同一次调用（Hook/Workflow 等
    // 链上能力的接入点）。具体实现不在本层重测。
    assert_policy_saw(&chain_seen, &effective);

    // 结果：来自真实 wire 的响应。
    assert_eq!(outcome.results.len(), 1);
    let (call, result) = &outcome.results[0];
    assert_eq!(call.name, effective);
    assert!(!result.is_error, "approved call must succeed: {result:?}");
    assert_eq!(result.tool_name, effective);
    assert!(result.output.contains("fixture handled write_note"));

    // wire：恰好一次 tools/call，且 wire 上是所属 namespace 的裸工具名。
    assert_eq!(
        fixture.log.called_tool_names(),
        vec![REQUIRED_TOOL.to_string()],
        "批准后必须恰好触发一次真实 MCP tools/call"
    );
    assert_eq!(
        fixture.log.calls()[0]["arguments"],
        json!({"note": "hello"})
    );

    // render 事件：effective name + 同一 turn（session/ACP 投影归 B-07）。
    let turn_id = context.turn_id();
    let started = events.render_rx.recv().await.expect("ToolStarted expected");
    match started {
        peri_agent::agent::events_v2::RenderEvent::ToolStarted {
            name,
            input,
            turn_id: event_turn,
            ..
        } => {
            assert_eq!(name, effective);
            assert_eq!(input, json!({"note": "hello"}));
            assert_eq!(event_turn, turn_id);
        }
        event => panic!("expected ToolStarted, got {event:?}"),
    }
    let ended = events.render_rx.recv().await.expect("ToolEnded expected");
    match ended {
        peri_agent::agent::events_v2::RenderEvent::ToolEnded {
            name,
            is_error,
            turn_id: event_turn,
            ..
        } => {
            assert_eq!(name, effective);
            assert!(!is_error);
            assert_eq!(event_turn, turn_id);
        }
        event => panic!("expected ToolEnded, got {event:?}"),
    }
}

// ─── 契约 6：Permission + HITL（拒绝分支）────────────────────────────────────

/// 能力：Permission + HITL 拒绝时不得触发真实 MCP 调用。
#[tokio::test]
pub(super) async fn hitl_rejection_never_reaches_the_mcp_server() {
    let fixture = spawn_fixture(false).await;
    let bridge = fixture.bridge(REQUIRED_TOOL);
    let effective = effective_name(REQUIRED_TOOL);

    let broker = RecordingBroker::new(false);
    let chain_seen = Arc::new(Mutex::new(Vec::new()));
    let chain = approval_chain(broker.clone(), Arc::clone(&chain_seen));
    let tools = bridge_tools(std::slice::from_ref(&bridge));
    let (context, _events, _catalog) = make_context(tools, chain);
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new(
            "call-rejected",
            effective.clone(),
            json!({"note": "blocked"}),
        )],
    );

    let outcome = tool_dispatch::dispatch_tools(
        &context,
        &reasoning,
        &context.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
    )
    .await
    .expect("rejected call is settled, not fatal");

    // 审批确实发生过（否则下面的「零调用」可能只是目标解析失败）。
    assert_eq!(
        broker
            .seen()
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>(),
        vec![effective.clone()]
    );

    let (_, result) = &outcome.results[0];
    assert!(result.is_error);
    assert_eq!(result.tool_name, effective);
    assert_eq!(
        result.effective_error_code,
        Some(peri_agent::tools::EffectiveToolErrorCode::UserRejected),
        "拒绝必须分类为 UserRejected，不能退化成普通失败"
    );

    assert!(
        fixture.log.calls().is_empty(),
        "被拒绝的调用不得触发任何真实 MCP tools/call，实际收到 {:?}",
        fixture.log.calls()
    );
}

/// [回归] **缺少真实会话身份**（如子 agent 自建 session）的 canonical 执行：
/// 审批归属铸造不出可信事实，因此必须显式拒绝——不静默放行、不用 agent_id 之类
/// 非会话身份冒充归属、不触发任何 broker 交互、不得到达 MCP wire。
///
/// 假身份（`session_id = agent_id`）曾让这条路径「成功地」弹窗审批，使
/// `untrusted_owner_error` 成为死分支（设计 §4.3 fail-closed）。
#[tokio::test]
pub(super) async fn missing_session_identity_is_fail_closed_and_never_reaches_the_wire() {
    let fixture = spawn_fixture(false).await;
    let bridge = fixture.bridge(REQUIRED_TOOL);
    let effective = effective_name(REQUIRED_TOOL);

    let broker = RecordingBroker::new(true);
    let chain_seen = Arc::new(Mutex::new(Vec::new()));
    // broker 恒批准：若归属被伪造出来，本用例会以 Ok 通过——断言因此有判别力。
    let chain = approval_chain(broker.clone(), Arc::clone(&chain_seen));
    let tools = bridge_tools(std::slice::from_ref(&bridge));
    let (context, _events, _catalog) = make_context_with_session(None, tools, chain);
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new(
            "call-unowned",
            effective.clone(),
            json!({"note": "no session identity"}),
        )],
    );

    let outcome = tool_dispatch::dispatch_tools(
        &context,
        &reasoning,
        &context.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
    )
    .await
    .expect("无归属的调用应被结算为失败结果，而不是致命错误");

    assert!(
        broker.seen().is_empty(),
        "无归属不得触发任何审批交互，实际: {:?}",
        broker.seen()
    );
    let (_, result) = &outcome.results[0];
    assert!(result.is_error, "无归属必须显式失败，不得静默放行");
    assert_eq!(result.tool_name, effective);
    assert!(
        result.output.contains("缺少可信归属"),
        "失败原因必须可诊断（含无归属说明），实际: {}",
        result.output
    );
    assert!(
        !result.output.contains("用户拒绝"),
        "无归属不是用户拒绝，不得伪装：{}",
        result.output
    );
    assert!(
        fixture.log.calls().is_empty(),
        "无归属的调用不得触发任何真实 MCP tools/call，实际收到 {:?}",
        fixture.log.calls()
    );
}

// ─── 契约 6：cancel ──────────────────────────────────────────────────────────

/// 能力：cancel。在飞 `tools/call` 被取消后：dispatch 以 `Interrupted` 结束，
/// 不重放、不补发第二次 wire 请求。
#[tokio::test]
pub(super) async fn in_flight_cancellation_ends_the_call_without_a_second_wire_request() {
    let fixture = spawn_fixture(true).await;
    let bridge = fixture.bridge(REQUIRED_TOOL);
    let effective = effective_name(REQUIRED_TOOL);
    let wire_log = Arc::clone(&fixture.log);

    let broker = RecordingBroker::new(true);
    let chain_seen = Arc::new(Mutex::new(Vec::new()));
    let chain = approval_chain(broker.clone(), Arc::clone(&chain_seen));
    let tools = bridge_tools(std::slice::from_ref(&bridge));
    let (context, _events, _catalog) = make_context(tools, chain);
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new(
            "call-cancelled",
            effective.clone(),
            json!({"note": "cancel me"}),
        )],
    );
    let catalog = context.runtime.tool_catalog.snapshot();
    let cancel = CancellationToken::new();

    let dispatch_cancel = cancel.clone();
    let dispatch_context = context.clone();
    let dispatch = tokio::spawn(async move {
        tool_dispatch::dispatch_tools(&dispatch_context, &reasoning, &catalog, &dispatch_cancel)
            .await
    });

    // 等到 server 真的收到 tools/call（显式信号，不用睡眠）后再取消：
    // 取消必须打在**已批准且已在飞**的调用上，不是启动前拦截。
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        wire_log.call_reached.notified(),
    )
    .await
    .expect("approved MCP call must reach the fixture before cancellation");
    cancel.cancel();

    let outcome = dispatch.await.expect("dispatch task must not panic");
    match outcome {
        Err(peri_agent::error::AgentError::Interrupted) => {}
        Err(error) => panic!("取消必须以 Interrupted 结束，实际 Err({error:?})"),
        Ok(outcome) => panic!("取消不得产出成功结果，实际 {} 项", outcome.results.len()),
    }

    assert_eq!(
        broker
            .seen()
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>(),
        vec![effective.clone()],
        "取消失效前的审批必须已完成（否则本用例退化为启动前拦截）"
    );
    assert_policy_saw(&chain_seen, &effective);
    assert_eq!(
        wire_log.called_tool_names(),
        vec![REQUIRED_TOOL.to_string()],
        "取消不得触发重放或第二次 tools/call"
    );

    // 放行 fixture 收尾，避免后台 server 任务挂在通知上。
    if let Some(release) = &fixture.log.release {
        release.notify_one();
    }
}

// ─── 契约 3：deferred MCP 工具仍走 ToolSearch 路径 ───────────────────────────

/// 能力：ToolSearch deferral。
///
/// 未被提升的 MCP bridge 必须：不进 RCRA `direct_definitions`；可被
/// `ToolSearch` 检索；只能经 `ToolInvoke` 到达 wire；链与审批照旧。
/// 探针工具钉住 direct/deferred 的分界是 `is_direct()`，不是别的过滤条件。
#[tokio::test]
pub(super) async fn deferred_mcp_bridge_is_reachable_only_through_tool_search() {
    let fixture = spawn_fixture(false).await;
    let bridge = fixture.bridge(DEFERRED_TOOL);
    let effective = effective_name(DEFERRED_TOOL);
    assert!(
        !bridge.is_direct(),
        "未要求提升的 MCP bridge 必须保持 deferred 默认行为"
    );
    // 排除「因对模型不可见而被排除」这一替代解释：本用例的排除必须只由 deferred 造成。
    assert!(
        bridge.visible_to_model(),
        "fixture 工具必须对模型可见，否则 direct 排除失去因果意义"
    );

    let mut tools = bridge_tools(std::slice::from_ref(&bridge));
    tools.insert(
        "DirectProbe".to_string(),
        Arc::new(DirectProbeTool {
            name: "DirectProbe",
            invoked: Arc::new(AtomicBool::new(false)),
        }) as Arc<dyn BaseTool>,
    );
    let shared: SharedToolMap = Arc::new(RwLock::new(tools));
    let index = Arc::new(ToolSearchIndex::new());
    let tool_search = ToolSearchMiddleware::new(Arc::clone(&index), Arc::clone(&shared));

    // 生产顺序：宿主先 merge `collect_tools` 的 meta 工具，再在 Reason 边界重绑。
    for tool in
        <ToolSearchMiddleware as Middleware>::collect_tools(&tool_search, "/tmp/mcp-host-policy")
    {
        let name = tool.name().to_string();
        shared.write().insert(name, Arc::from(tool));
    }
    let mut state = CatalogStateFixture {
        tools: Arc::clone(&shared),
        recalls: Vec::new(),
    };
    <ToolSearchMiddleware as Middleware>::before_reason_catalog(&tool_search, &mut state)
        .await
        .expect("ToolSearch rebind must succeed");

    // (a) direct 列表：探针在，deferred MCP bridge 不在。`direct_definitions` 与
    // Reason 阶段下发给 LLM 的工具集使用同一谓词 `is_direct() && visible_to_model()`
    // （`peri-agent/src/agent/stages/reason.rs:104`），因此该集合可代表模型直连视图。
    let direct = direct_definition_names(
        &SessionToolCatalog::try_new(shared.read().clone(), None)
            .expect("fixture catalog must build")
            .snapshot(),
    );
    assert!(
        direct.contains(&"DirectProbe".to_string()),
        "is_direct 工具必须进入 direct_definitions（探针前提失败）: {direct:?}"
    );
    assert!(
        !direct.contains(&effective),
        "deferred MCP 工具不得进入模型直连工具列表: {direct:?}"
    );

    // (b) deferred 索引：MCP 工具可检索；direct 探针不在 deferred 索引里。
    let messages: Vec<peri_agent::messages::BaseMessage> = Vec::new();
    let ctx = || ToolContext::new(&messages, "/tmp/mcp-host-policy");
    let search = SearchExtraTools::new(Arc::clone(&index));
    let found: Value = serde_json::from_str(
        &search
            .invoke(json!({"query": format!("select:{effective}")}), ctx())
            .await
            .expect("search must succeed"),
    )
    .expect("search output must be JSON");
    let found_names: Vec<&str> = found["results"]
        .as_array()
        .expect("results must be an array")
        .iter()
        .filter_map(|result| result["name"].as_str())
        .collect();
    assert!(
        found_names.contains(&effective.as_str()),
        "deferred MCP 工具必须能被 ToolSearch 检索: {found}"
    );
    let probe: Value = serde_json::from_str(
        &search
            .invoke(json!({"query": "select:DirectProbe"}), ctx())
            .await
            .expect("search must succeed"),
    )
    .expect("search output must be JSON");
    assert!(
        probe["results"]
            .as_array()
            .expect("results must be an array")
            .is_empty(),
        "direct 工具不得出现在 deferred 索引中: {probe}"
    );

    // (c) 经 ToolInvoke 调用：审批看到 effective name，wire 收裸工具名一次。
    let broker = RecordingBroker::new(true);
    let chain_seen = Arc::new(Mutex::new(Vec::new()));
    let (context, _events, _catalog) = make_context(
        shared.read().clone(),
        approval_chain(broker.clone(), Arc::clone(&chain_seen)),
    );
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new(
            "call-deferred",
            TOOL_INVOKE_NAME,
            json!({"name": effective, "arguments": {"id": "n1"}}),
        )],
    );
    let outcome = tool_dispatch::dispatch_tools(
        &context,
        &reasoning,
        &context.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
    )
    .await
    .expect("deferred call must settle");

    assert_eq!(
        broker
            .seen()
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>(),
        vec![effective.clone()],
        "wrapper 必须把 canonical effective name 交给审批，而不是 ToolInvoke"
    );
    assert_policy_saw(&chain_seen, &effective);
    let (_, result) = &outcome.results[0];
    assert!(!result.is_error, "deferred call must succeed: {result:?}");
    assert_eq!(result.tool_name, effective);
    assert_eq!(
        fixture.log.called_tool_names(),
        vec![DEFERRED_TOOL.to_string()],
        "deferred 调用必须落到所属 namespace 的裸 MCP 工具名上，且只有一次"
    );
}

/// 能力：契约 6 的「未绕过」补充说明——`TOOL_SEARCH_NAME` / `TOOL_INVOKE_NAME`
/// 常量必须与 `ToolSearchMiddleware` 注册的 meta 工具名一致，否则上面的断言会
/// 退化成「调用了不存在的工具」。这是一条防止 seam 漂移的护栏，不是新能力覆盖。
#[test]
pub(super) fn tool_search_meta_tool_names_match_the_middleware_registration() {
    let shared: SharedToolMap = Arc::new(RwLock::new(BTreeMap::new()));
    let middleware =
        ToolSearchMiddleware::new(Arc::new(ToolSearchIndex::new()), Arc::clone(&shared));
    let names: Vec<String> =
        <ToolSearchMiddleware as Middleware>::collect_tools(&middleware, "/tmp/mcp-host-policy")
            .into_iter()
            .map(|tool| tool.name().to_string())
            .collect();
    assert_eq!(
        names,
        vec![TOOL_SEARCH_NAME.to_string(), TOOL_INVOKE_NAME.to_string()]
    );
}
