//! D-04：宿主策略与生命周期契约测试（验收契约 6，兼契约 3 的 deferred 半边）。
//!
//! ## 证据范围（诚实分级）
//!
//! 本文件是 `peri-middlewares` 的**外部集成测试**，只能使用 crate 的 `pub` API。
//! MCP 侧使用真实的 rmcp client service（`serve_client_with_lifecycle`）连到
//! `tokio::io::duplex` 上的 JSON-RPC fixture：server 端记录真实收到的 `tools/call`，
//! 因此下面所有「未调用 / 恰好调用一次」断言都是 wire 事实，不是 mock 计数。
//!
//! 逐能力断言（每条一个测试，不用一条测试覆盖多类能力）：
//!
//! | 能力 | 断言 | 承担者 |
//! | --- | --- | --- |
//! | Permission + HITL | broker 收到 effective name；拒绝 → 0 次 wire 调用；批准 → 恰好 1 次 | 本文件的 `hitl_*` 测试 |
//! | effective tool name | 策略/审批看到 `mcp__{server}__{tool}`，wire 上仍是属于该 namespace 的裸工具名 | 同上 |
//! | cancel | 在飞 `tools/call` 被取消后不再重试，dispatch 返回 `Interrupted` | `in_flight_cancellation_*` |
//! | ToolSearch deferral | 未提升的 MCP bridge 不进 `direct_definitions`（与 Reason 阶段下发给 LLM 的工具集用**同一** `is_direct() && visible_to_model()` 谓词），只能经 SearchExtraTools/ExecuteExtraTool 到达 | `deferred_*` |
//! | session / event / host assembly | **BLOCKED**（见下） | B-07 |
//!
//! ## BLOCKED（不在本层假装覆盖）
//!
//! 1. **被提升为 direct 的真实 MCP bridge**（`McpToolBridge::with_direct` 是
//!    `pub(crate)`，`prepare_system_tools` / `system_mcp_tools` 解析同理）。本层只能
//!    证明**判定输入**：Permission 的决策只看 `(name, input)`，从不接触 `BaseTool`，
//!    因此 `is_direct` 在结构上无法影响审批；而 `is_direct` → `direct_definitions`
//!    的过滤边界由 `deferred_*` 的探针工具单独钉住。真实 direct 提升后的端到端证据
//!    归 **D-02**（crate 内 `mcp_v4_seam_test.rs`）与 **B-07**（`peri-acp` host seam）。
//! 2. **`run_initialize` 驱动的配置 → 提升接线**在本层不可用：它是唯一会发布
//!    `SystemMcpManifest::Loaded` + system 依赖的 public 入口，但内部按
//!    `dirs_next::home_dir()/.peri/settings.json` 解析全局配置，没有可注入 seam
//!    （`load_merged_config_full_with_paths` 不对外）。在进程内跑它等于读开发机上
//!    真实的 MCP 配置并连真实 server（含凭据），因此本文件**不**调用它。该接线证据
//!    同样归 **B-07** 的隔离 HOME host 场景；本文件记录为 BLOCKED。
//! 3. **session 身份 / ACP 事件投影 / 首个 LLM 请求的 tools 入参 / 生产链装配
//!    （`ProductionChainAssembler` 槽位）**：需要 `peri-acp` host 装配层。
//!    本层只断言 `StageContext` 的 render 事件属于同一 turn，不代替 host seam。
//!    归 **B-07**（`peri-acp/src/host/mcp_v4_startup_test.rs`）。
//! 4. **Hook / SubAgent / Workflow / Goal / PTC 的具体实现**未在本层重测；本层只证明
//!    工具调用仍然完整经过 middleware chain（`before_tools_batch` 对 MCP bridge 可见），
//!    即 direct 注入没有短路链上既有 hook 位。
//!
//! fixture 全部定义在本文件内（不建立 workspace 级共享 test helper），且不含任何真实
//! 凭据：server 不校验 header / token，测试也不读取用户 HOME 下的 MCP 配置。

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use peri_agent::{
    agent::{
        react::{Reasoning, ToolCall},
        stages::{tool_dispatch::dispatch_tools, SharedToolMap, StageContext},
    },
    interaction::{
        ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
    },
    middleware::{capabilities as hook_state, r#trait::Middleware, MiddlewareChain},
    session::{tool_catalog::SessionToolCatalog, FrozenContext, Session},
    tools::{BaseTool, ToolContext},
};
use peri_middlewares::{
    mcp::{ClientStatus, McpClientHandle, McpToolBridge, OAuthStatus},
    permission::{
        default_requires_approval, PermissionMiddleware, PermissionMode, SharedPermissionMode,
    },
    tool_search::{
        SearchExtraTools, ToolSearchIndex, ToolSearchMiddleware, EXECUTE_EXTRA_TOOL_NAME,
        SEARCH_EXTRA_TOOLS_NAME,
    },
    ExecuteExtraToolResolver,
};
use rmcp::{
    model::{ClientCapabilities, Implementation, InitializeRequestParams},
    service::{serve_client_with_lifecycle, ClientLifecycleMode, RoleClient, RunningService},
    transport::async_rw::AsyncRwTransport,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

// ─── 真实 MCP wire fixture（duplex + rmcp 官方 client service）─────────────────

const FIXTURE_SERVER: &str = "host-fixture";
const REQUIRED_TOOL: &str = "write_note";
const DEFERRED_TOOL: &str = "read_note";

fn effective_name(tool: &str) -> String {
    format!("mcp__{FIXTURE_SERVER}__{tool}")
}

fn tool_declaration(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

/// 一次 `tools/call` 的 wire 事实：server 端收到的原始参数。
#[derive(Default)]
struct WireLog {
    calls: Mutex<Vec<Value>>,
    /// 每次收到 `tools/call` 唤醒；`Notify::notify_one` 会保存 permit，
    /// 因此「先到调用、后 await」与「先 await、后到调用」都成立（无睡眠）。
    call_reached: tokio::sync::Notify,
    /// `Some` 时 `tools/call` 挂起，直到测试显式放行（在飞取消用例）。
    release: Option<Arc<tokio::sync::Notify>>,
}

impl WireLog {
    fn calls(&self) -> Vec<Value> {
        self.calls.lock().clone()
    }

    fn called_tool_names(&self) -> Vec<String> {
        self.calls()
            .iter()
            .filter_map(|params| params["name"].as_str().map(str::to_string))
            .collect()
    }
}

/// 一个已连接的 fixture server：持有真实 client service，drop 即断开 transport。
struct Fixture {
    handle: Arc<McpClientHandle>,
    log: Arc<WireLog>,
    _service: RunningService<RoleClient, InitializeRequestParams>,
}

impl Fixture {
    fn bridge(&self, tool: &str) -> McpToolBridge {
        let declaration = self
            .handle
            .tools
            .iter()
            .find(|candidate| candidate.name.as_ref() == tool)
            .expect("fixture must expose the requested tool on the wire");
        McpToolBridge::new(FIXTURE_SERVER, declaration, Arc::clone(&self.handle))
    }
}

/// 启动一个最小 MCP server（initialize / tools/list / tools/call），
/// 让真实 rmcp client service 完成握手并返回由 wire 声明的工具。
async fn spawn_fixture(blocking_call: bool) -> Fixture {
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
    let service = serve_client_with_lifecycle(
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
        name: FIXTURE_SERVER.to_string(),
        version: None,
        cache_version: None,
        peer: Some(service.peer().clone()),
        tools: listed.tools,
        resources: Vec::new(),
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        channel_capable: false,
        skills_capable: false,
    });
    Fixture {
        handle,
        log,
        _service: service,
    }
}

async fn serve_fixture(
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
struct RecordingBroker {
    seen: Mutex<Vec<(String, Value)>>,
    approve: bool,
}

impl RecordingBroker {
    fn new(approve: bool) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            approve,
        })
    }

    fn seen(&self) -> Vec<(String, Value)> {
        self.seen.lock().clone()
    }
}

#[async_trait]
impl UserInteractionBroker for RecordingBroker {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
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
struct PolicyRecorder(Arc<Mutex<Vec<ToolCall>>>);

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
fn approval_chain(
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

fn make_context(
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
    (context, handles, catalog)
}

fn bridge_tools(bridges: &[McpToolBridge]) -> BTreeMap<String, Arc<dyn BaseTool>> {
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

fn direct_definition_names(
    snapshot: &peri_agent::session::tool_catalog::SessionToolCatalogSnapshot,
) -> Vec<String> {
    snapshot
        .direct_definitions
        .iter()
        .map(|definition| definition.name.clone())
        .collect()
}

/// 只提供 `CatalogState` 需要的两项能力的最小 state。
struct CatalogStateFixture {
    tools: SharedToolMap,
    recalls: Vec<String>,
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
struct DirectProbeTool {
    name: &'static str,
    invoked: Arc<AtomicBool>,
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

fn assert_policy_saw(chain_seen: &Arc<Mutex<Vec<ToolCall>>>, expected_name: &str) {
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
async fn hitl_approval_gates_mcp_bridge_by_effective_name_and_calls_server_once() {
    let fixture = spawn_fixture(false).await;
    let bridge = fixture.bridge(REQUIRED_TOOL);
    let effective = effective_name(REQUIRED_TOOL);

    assert_eq!(bridge.name(), effective);
    assert_eq!(bridge.mcp_server_name(), Some(FIXTURE_SERVER));
    // 判定输入事实：生产敏感规则对 `mcp__` 前缀生效——审批只看工具名，
    // 不接触 BaseTool，因此 `is_direct` 在结构上无法影响这条决策。
    assert!(default_requires_approval(&effective));

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

    let outcome = dispatch_tools(
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

    // 链未被绕过：before_tools_batch 观察到同一次调用（Hook/Workflow/PTC 等
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
async fn hitl_rejection_never_reaches_the_mcp_server() {
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

    let outcome = dispatch_tools(
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

// ─── 契约 6：cancel ──────────────────────────────────────────────────────────

/// 能力：cancel。在飞 `tools/call` 被取消后：dispatch 以 `Interrupted` 结束，
/// 不重放、不补发第二次 wire 请求。
#[tokio::test]
async fn in_flight_cancellation_ends_the_call_without_a_second_wire_request() {
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
        dispatch_tools(&dispatch_context, &reasoning, &catalog, &dispatch_cancel).await
    });

    // 等到 server 真的收到 tools/call（显式信号，不用睡眠）后再取消：
    // 取消必须打在**已批准且已在飞**的调用上，不是启动前拦截。
    wire_log.call_reached.notified().await;
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
/// `SearchExtraTools` 检索；只能经 `ExecuteExtraTool` 到达 wire；链与审批照旧。
/// 探针工具钉住 direct/deferred 的分界是 `is_direct()`，不是别的过滤条件。
#[tokio::test]
async fn deferred_mcp_bridge_is_reachable_only_through_tool_search() {
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

    // (c) 经 ExecuteExtraTool 调用：审批看到 effective name，wire 收裸工具名一次。
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
            EXECUTE_EXTRA_TOOL_NAME,
            json!({"tool_name": effective, "params": {"id": "n1"}}),
        )],
    );
    let outcome = dispatch_tools(
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
        "wrapper 必须把 canonical effective name 交给审批，而不是 ExecuteExtraTool"
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

/// 能力：契约 6 的「未绕过」补充说明——`SEARCH_EXTRA_TOOLS_NAME` 常量必须与
/// `ToolSearchMiddleware` 注册的 meta 工具名一致，否则上面的断言会退化成
/// 「调用了不存在的工具」。这是一条防止 seam 漂移的护栏，不是新能力覆盖。
#[test]
fn tool_search_meta_tool_names_match_the_middleware_registration() {
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
        vec![
            SEARCH_EXTRA_TOOLS_NAME.to_string(),
            EXECUTE_EXTRA_TOOL_NAME.to_string()
        ]
    );
}
