use super::*;
use crate::mcp::client::ClientStatus;
use rmcp::handler::server::ServerHandler;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn make_tool(name: &str, description: Option<&str>) -> Tool {
    let json = serde_json::json!({
        "name": name,
        "description": description.unwrap_or(""),
        "inputSchema": {
            "type": "object",
            "properties": { "path": { "type": "string" } }
        }
    });
    serde_json::from_value(json).unwrap()
}

fn make_disconnected_handle(name: &str) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Failed("connection lost".to_string()),
        oauth_status: Default::default(),
        source: None,
        url: None,
        skills_capable: false,
    })
}

#[test]
fn test_new_creates_correct_full_name() {
    let tool = make_tool("read_file", Some("Read a file"));
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    assert_eq!(bridge.name(), "mcp__fs__read_file");
}

#[test]
fn test_new_sanitizes_dots_in_names() {
    let tool = make_tool("web.reader", Some("Fetch URL"));
    let handle = make_disconnected_handle("plugin.ctx");
    let bridge = McpToolBridge::new("plugin.ctx", &tool, handle);
    // full_name 净化了非法字符
    assert_eq!(bridge.name(), "mcp__plugin_ctx__web_reader");
    // 但内部 tool_name 保留原始值用于 MCP 协议调用
    assert_eq!(bridge.tool_name, "web.reader");
    assert_eq!(bridge.server_name, "plugin.ctx");
}

#[test]
fn test_new_sanitizes_colons_in_names() {
    let tool = make_tool("query-docs", Some("Query docs"));
    let handle = make_disconnected_handle("context7");
    let bridge = McpToolBridge::new("plugin:context7:context7", &tool, handle);
    assert_eq!(bridge.name(), "mcp__plugin_context7_context7__query-docs");
    assert_eq!(bridge.tool_name, "query-docs");
}

#[test]
fn test_new_creates_correct_description() {
    let tool = make_tool("read_file", Some("Read a file"));
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    assert_eq!(bridge.description(), "[MCP:fs] Read a file");
}

#[test]
fn test_new_preserves_input_schema() {
    let tool = make_tool("read_file", None);
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    let params = bridge.parameters();
    assert!(params.get("properties").is_some());
}

#[test]
fn test_new_empty_description() {
    let tool = make_tool("read_file", None);
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    assert_eq!(bridge.description(), "[MCP:fs] ");
}

#[tokio::test]
async fn test_invoke_not_connected() {
    let tool = make_tool("read_file", None);
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    let result = bridge
        .invoke(
            serde_json::json!({}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("未连接"));
}

#[test]
fn test_format_content_text_only() {
    let contents = vec![rmcp::model::ContentBlock::text("hello")];
    assert_eq!(format_contents(&contents), "hello");
}

#[test]
fn test_format_content_mixed() {
    let contents = vec![
        rmcp::model::ContentBlock::text("line1"),
        rmcp::model::ContentBlock::text("line2"),
    ];
    assert_eq!(format_contents(&contents), "line1\nline2");
}

struct CatalogDispatcher(Vec<peri_acp_types::tools::EffectiveToolDefinition>);

#[async_trait::async_trait]
impl peri_acp_types::tools::EffectiveToolDispatcher for CatalogDispatcher {
    async fn dispatch(
        &self,
        _call: peri_acp_types::tools::EffectiveToolCall,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String, peri_acp_types::tools::EffectiveToolError> {
        Ok(String::new())
    }

    fn tools(&self) -> Vec<peri_acp_types::tools::EffectiveToolDefinition> {
        self.0.clone()
    }

    fn admitted_mcp_tool_name(&self, server_name: &str, wire_tool_name: &str) -> Option<String> {
        let name = effective_mcp_tool_name(server_name, wire_tool_name);
        self.0
            .iter()
            .find(|tool| tool.name == name)
            .map(|tool| tool.name.clone())
    }
}

#[test]
fn app_allowed_tools_intersects_resource_visibility_and_canonical_catalog() {
    let tool = |name: &str, resource: &str, visibility: &str| {
        serde_json::from_value::<Tool>(serde_json::json!({
            "name": name,
            "description": name,
            "inputSchema": {"type": "object"},
            "_meta": {"ui": {"resourceUri": resource, "visibility": [visibility]}}
        }))
        .unwrap()
    };
    let tools = vec![
        tool("app_only", "ui://app", "app"),
        tool("other_resource", "ui://other", "app"),
        tool("model_only", "ui://app", "model"),
    ];
    let dispatcher = CatalogDispatcher(vec![
        peri_acp_types::tools::EffectiveToolDefinition {
            name: "mcp__server__app_only".into(),
            description: String::new(),
            parameters: serde_json::json!({}),
        },
        peri_acp_types::tools::EffectiveToolDefinition {
            name: "mcp__server__other_resource".into(),
            description: String::new(),
            parameters: serde_json::json!({}),
        },
    ]);

    assert_eq!(
        app_allowed_tools("server", "ui://app", &tools, &dispatcher),
        std::collections::HashMap::from([("app_only".into(), "mcp__server__app_only".into())])
    );
}

struct BoundDispatcher;

#[async_trait::async_trait]
impl peri_acp_types::tools::EffectiveToolDispatcher for BoundDispatcher {
    async fn dispatch(
        &self,
        _call: peri_acp_types::tools::EffectiveToolCall,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String, peri_acp_types::tools::EffectiveToolError> {
        Ok(String::new())
    }

    fn tools(&self) -> Vec<peri_acp_types::tools::EffectiveToolDefinition> {
        vec![]
    }

    fn admitted_mcp_tool_name(&self, server: &str, wire_name: &str) -> Option<String> {
        (server == "alpha" && wire_name == "Read").then(|| "Read".to_string())
    }
}

#[test]
fn app_allowed_tools_uses_admitted_binding_when_raw_names_collide() {
    let tool: Tool = serde_json::from_value(serde_json::json!({
        "name": "Read",
        "inputSchema": {"type": "object"},
        "_meta": {"ui": {"resourceUri": "ui://app", "visibility": ["app"]}}
    }))
    .unwrap();
    assert_eq!(
        app_allowed_tools(
            "alpha",
            "ui://app",
            std::slice::from_ref(&tool),
            &BoundDispatcher
        ),
        std::collections::HashMap::from([("Read".into(), "Read".into())])
    );
    assert!(app_allowed_tools("zulu", "ui://app", &[tool], &BoundDispatcher).is_empty());
}

#[test]
fn test_build_tool_bridges_filters_app_only_tool_from_model_catalog() {
    let pool = Arc::new(McpClientPool::new_empty());
    let tool: Tool = serde_json::from_value(serde_json::json!({
        "name": "app_only",
        "description": "App only",
        "inputSchema": {"type": "object"},
        "_meta": {"ui": {"visibility": ["app"]}}
    }))
    .unwrap();
    pool.clients.write().insert(
        "apps".to_string(),
        Arc::new(McpClientHandle {
            name: "apps".to_string(),
            version: None,
            cache_version: None,
            peer: None,
            tools: vec![tool],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: Default::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
    let bridges = build_tool_bridges(&pool);
    assert_eq!(bridges.len(), 1);
    assert!(!bridges[0].visible_to_model());
}

#[test]
fn test_build_tool_bridges_empty_pool() {
    let pool = Arc::new(McpClientPool::new_empty());
    let bridges = build_tool_bridges(&pool);
    assert!(bridges.is_empty());
}

// ── IF-D13：builtin 直连性声明（E-03）────────────────────────────────────────

fn connected_handle(name: &str, tools: Vec<Tool>) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools,
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: Default::default(),
        source: (name != "external").then(|| crate::mcp::config::ConfigSource::Builtin {
            instance: name.to_string(),
        }),
        url: None,
        skills_capable: false,
    })
}

/// 类型化构造必须对**声明的** builtin 工具应用 direct；未类型化版本逐位保持 deferred。
///
/// 三个断言面：
/// 1. builtin 声明 direct 的工具 → 原名与 `is_direct()`；
/// 2. 外部 server 用同名工具（保留名接管的等价输入）**不**获得 direct——判定只按
///    「已实现 builtin 实例的声明表」，不按工具名反查；
/// 3. public `build_tool_bridges` 在两种输入下都全 deferred，保持前缀名。
#[test]
fn typed_bridges_apply_declared_direct_only_for_builtin_instances() {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.clients.write().insert(
        "web".to_string(),
        connected_handle(
            "web",
            vec![make_tool("WebSearch", None), make_tool("WebFetch", None)],
        ),
    );
    pool.clients.write().insert(
        "artifact".to_string(),
        connected_handle("artifact", vec![make_tool("artifact", None)]),
    );
    // 外部 server 使用与 builtin 相同的工具名：不得被当作 builtin 一等工具放行。
    pool.clients.write().insert(
        "external".to_string(),
        connected_handle(
            "external",
            vec![make_tool("WebSearch", None), make_tool("artifact", None)],
        ),
    );

    let typed = build_typed_tool_bridges(&pool);
    let boxed = build_tool_bridges(&pool);
    assert_eq!(boxed.len(), typed.len());

    let direct_of = |name: &str| {
        typed
            .iter()
            .find(|bridge| bridge.name() == name)
            .unwrap_or_else(|| panic!("缺少 bridge: {name}"))
            .is_direct()
    };
    // 1. 声明的 builtin direct（名字与 IF-D5 冻结字面量逐字一致）。
    assert!(direct_of("WebSearch"));
    assert!(direct_of("WebFetch"));
    assert!(direct_of("artifact"));
    // 2. 外部 server 的同名工具保持 deferred。
    assert!(!direct_of("mcp__external__WebSearch"));
    assert!(!direct_of("mcp__external__artifact"));
    // 3. 未类型化版本全 deferred（行为逐位不变）。
    assert!(boxed.iter().all(|bridge| !bridge.is_direct()));
    assert!(typed.iter().any(|bridge| bridge.is_direct()));
}

/// typed 构造应用声明 direct 原名，public 构造保持 deferred 前缀名。
#[test]
fn typed_and_deferred_builders_differ_only_in_direct_flag() {
    let pool = Arc::new(McpClientPool::new_empty());
    let handle = connected_handle("web", vec![make_tool("WebSearch", None)]);
    pool.clients
        .write()
        .insert("web".to_string(), Arc::clone(&handle));

    let mut direct = build_typed_tool_bridges(&pool);
    let mut deferred = build_deferred_tool_bridges(&pool);
    assert_eq!(direct.len(), 1);
    assert_eq!(deferred.len(), 1);
    assert!(direct[0].is_direct());
    assert!(!deferred[0].is_direct());

    assert_eq!(direct[0].name(), "WebSearch");
    assert_eq!(deferred[0].name(), "mcp__web__WebSearch");
    assert_eq!(
        direct[0].original_tool_name(),
        deferred[0].original_tool_name()
    );
    assert_eq!(direct[0].mcp_server_name(), deferred[0].mcp_server_name());
    assert_eq!(direct[0].parameters(), deferred[0].parameters());
    assert_eq!(direct[0].visible_to_model(), deferred[0].visible_to_model());
    // generation 与 binding leases 传递不得因提取 deferred 版本而丢失。
    for bridge in direct.iter_mut().chain(deferred.iter_mut()) {
        assert_eq!(bridge.server_generation, pool.handle_generation(&handle));
        assert!(bridge.binding_leases.is_some());
    }
}

// ── R29：桥层外层超时面（`builtin_tool_call_surfaces_timeout_error_after_bridge_deadline`）──
//
// 证据边界（诚实声明）：本用例驱动的是**真实链路**——生产 `spawn_builtin_transport_with_handler`
// （真实 `tokio::io::duplex` + 真实 `rmcp::serve_server` + 真实 server task）→ 生产
// `serve_client_auto`（真实 Auto lifecycle 握手）→ 生产 `McpToolBridge::invoke`
// （`:265` 的 `tokio::time::timeout(TOOL_CALL_TIMEOUT, peer.call_tool(..))`）。
// **只有** handler 的工具体内核是替身：它停在闸门上（延迟夹具），用来制造「调用超过
// 桥 deadline」的唯一条件。虚拟时钟只停在本用例的 runtime 上（`tokio::time::pause()`），
// 因此不需要真等 120 秒，也不改生产常量。

/// 延迟夹具的 server 半边：与生产 builtin handler 同形（不覆写 `discover`、只声明
/// tools 能力），唯一差别是 `call_tool` 在闸门上等（闸门由用例显式放行）。
#[derive(Clone)]
struct DelayedCallHandler {
    release: Arc<tokio::sync::Notify>,
    released: Arc<AtomicBool>,
    finished: Arc<tokio::sync::Notify>,
    entered_count: Arc<AtomicUsize>,
    completed: Arc<AtomicBool>,
    dropped_count: Arc<AtomicUsize>,
}

/// 在飞 handler future 的 `Drop` 哨兵：区分「被取消（drop 且未完成）」与「正常完成」。
struct InFlightSentinel {
    dropped_count: Arc<AtomicUsize>,
}

impl Drop for InFlightSentinel {
    fn drop(&mut self) {
        self.dropped_count.fetch_add(1, Ordering::SeqCst);
    }
}

impl DelayedCallHandler {
    fn new() -> Self {
        Self {
            release: Arc::new(tokio::sync::Notify::new()),
            released: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(tokio::sync::Notify::new()),
            entered_count: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicBool::new(false)),
            dropped_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 放行**所有**后续调用（粘滞）：第二次 `invoke` 不再停在闸门上。
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.release.notify_waiters();
    }

    fn entered_count(&self) -> usize {
        self.entered_count.load(Ordering::SeqCst)
    }

    fn dropped_count(&self) -> usize {
        self.dropped_count.load(Ordering::SeqCst)
    }

    fn completed(&self) -> bool {
        self.completed.load(Ordering::SeqCst)
    }
}

impl ServerHandler for DelayedCallHandler {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        rmcp::model::ServerConfig::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "peri-r29-delayed",
            "tool_bridge_test",
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListToolsResult::with_all_items(vec![
            delayed_tool(),
        ]))
    }

    async fn call_tool(
        &self,
        _request: rmcp::model::CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        self.entered_count.fetch_add(1, Ordering::SeqCst);
        let _sentinel = InFlightSentinel {
            dropped_count: Arc::clone(&self.dropped_count),
        };
        while !self.released.load(Ordering::SeqCst) {
            let notified = self.release.notified();
            if self.released.load(Ordering::SeqCst) {
                break;
            }
            notified.await;
        }
        self.completed.store(true, Ordering::SeqCst);
        self.finished.notify_waiters();
        Ok(rmcp::model::CallToolResponse::Complete(
            rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
                "r29-delayed-ok",
            )]),
        ))
    }
}

/// 延迟夹具声明的**原始**工具名 `cron_list`（effective name 由 bridge 派生为 `mcp__cron__cron_list`）。
fn delayed_tool() -> Tool {
    serde_json::from_value(serde_json::json!({
        "name": "cron_list",
        "description": "R29 延迟夹具工具（server 侧停在闸门上）",
        "inputSchema": { "type": "object", "properties": {} }
    }))
    .expect("夹具工具必须能被 rmcp Tool 接收")
}

/// R29 / 子计划 L §`builtin_tool_call_surfaces_timeout_error_after_bridge_deadline`：
/// 延迟夹具不返回 ⇒ 桥层在 `TOOL_CALL_TIMEOUT` 到期后返回 **typed 超时错误**
/// （文案逐字含 server / tool / 秒数，不含路径与参数）。
///
/// 三个观测面都不靠「只断言返回 Err」：
/// 1. **deadline 面**：虚拟耗时 ≥ `TOOL_CALL_TIMEOUT`（120s）且 typed 变体
///    `ToolCallError::Timeout { timeout_secs: 120 }`；Display 与冻结模板逐字相等，
///    且**不含**输入里的路径 / 凭据形态标记（脱敏不是巧合）；
/// 2. **在飞面**：到期返回的那一刻，server 侧已进入本次 `tools/call`（`entered_count == 1`）
///    且**尚未完成**、`Drop` 哨兵未触发 ⇒ 桥的 deadline 到期只取消了**客户端等待**，
///    没有取消 server 侧执行（子计划 V 硬约束：「不得把 client timeout 当作 server
///    自动取消成功」）；
/// 3. **收敛面**：放行延迟夹具 ⇒ 被弃置的那次调用在 server 侧正常跑完且**无重放**
///    （`entered_count` 仍为 1）；同一条链路（同一 client service / 同一 bridge）随后
///    仍能完成一次完整往返；收尾 `close_with_timeout` + `converge` 后 client service
///    与 builtin server task 都收敛（`Quit`），server 侧无残留执行。
#[tokio::test]
async fn builtin_tool_call_surfaces_timeout_error_after_bridge_deadline() {
    // 真实 builtin 链路（生产装配函数）：真实 duplex + 真实 `serve_server` + 真实 task 归属。
    let handler = DelayedCallHandler::new();
    let transport =
        crate::mcp::builtin::runtime::spawn_builtin_transport_with_handler("cron", handler.clone());
    let crate::mcp::builtin::runtime::BuiltinTransport {
        io,
        mut server_task,
        ..
    } = transport;
    let mut service = crate::mcp::client::serve_client_auto(
        io,
        &crate::mcp::apps::McpCapabilityProfile::disabled(),
        std::time::Duration::from_secs(5),
    )
    .await
    .expect("夹具握手不得超时（同进程链路）")
    .expect("夹具握手不得失败");
    let peer = service.peer().clone();

    let pool = Arc::new(McpClientPool::new_empty());
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager("deadline-session", &manager);
    let tool = delayed_tool();
    let bridge = McpToolBridge::new(
        "cron",
        &tool,
        Arc::new(McpClientHandle {
            name: "cron".to_string(),
            version: None,
            cache_version: None,
            peer: Some(peer),
            tools: vec![tool.clone()],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: Default::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    )
    .with_output_store(&pool, Some("deadline-session"));
    assert_eq!(bridge.name(), "mcp__cron__cron_list");

    // 输入带「路径形态」与「凭据形态」标记：超时文案必须逐字只含 server / tool / 秒数。
    const PATH_MARKER: &str = "/tmp/peri-r29-delayed-fixture.w2stall";
    const CREDENTIAL_MARKER: &str = "fixture-placeholder-unused-not-a-credential";
    let input = serde_json::json!({
        "prompt": PATH_MARKER,
        "token": CREDENTIAL_MARKER,
    });

    // 虚拟时钟（只本用例）：不真等 120s，也不改生产常量。
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let outcome = bridge
        .invoke(
            input.clone(),
            peri_agent::tools::ToolContext::new(&[], ".")
                .with_session_identity("deadline-session", "deadline-turn"),
        )
        .await;
    let elapsed = started.elapsed();
    tokio::time::resume();

    // ① deadline 面：typed 变体 + 逐字文案 + 脱敏。
    // `REGISTERED_TIMEOUT_SECS` 是 §7.3 / R29 **登记在案的冻结值**（字面量）：不引用常量，
    // 否则断言会与实现同义反复（改常量即跟着改断言，抓不住「值改动 = 语义变更」）。
    const REGISTERED_TIMEOUT_SECS: u64 = 120;
    assert_eq!(
        TOOL_CALL_TIMEOUT,
        std::time::Duration::from_secs(REGISTERED_TIMEOUT_SECS),
        "R29 登记的外层上界必须是 {REGISTERED_TIMEOUT_SECS}s（值改动属语义变更，必须重新登记）"
    );
    let error = outcome.expect_err("延迟夹具不返回 ⇒ 调用必须报错，不得静默成功");
    let text = error.to_string();
    let typed = error
        .downcast_ref::<ToolCallError>()
        .unwrap_or_else(|| panic!("必须浮出 typed ToolCallError，实际: {error:?}"));
    assert!(
        matches!(
            typed,
            ToolCallError::Timeout {
                server,
                tool,
                timeout_secs,
            } if server == "cron" && tool == "cron_list" && *timeout_secs == REGISTERED_TIMEOUT_SECS
        ),
        "必须是 Timeout{{server: cron, tool: cron_list, timeout_secs: {REGISTERED_TIMEOUT_SECS}}}，实际: {typed:?}"
    );
    assert_eq!(
        text,
        format!("MCP 服务器 \"cron\" 工具 \"cron_list\" 调用超时 ({REGISTERED_TIMEOUT_SECS}s)"),
        "超时文案必须与冻结模板逐字相等"
    );
    assert!(
        !text.contains(PATH_MARKER) && !text.contains(CREDENTIAL_MARKER),
        "超时文案不得回显入参（路径 / 凭据形态标记）: {text}"
    );
    let deadline = std::time::Duration::from_secs(REGISTERED_TIMEOUT_SECS);
    assert!(
        elapsed >= deadline && elapsed < deadline + std::time::Duration::from_secs(1),
        "虚拟耗时必须落在 deadline 上（{elapsed:?} 应为 {REGISTERED_TIMEOUT_SECS}s）"
    );

    // ② 在飞面：到期时 server 侧已进入、未完成、未被取消。
    assert_eq!(
        handler.entered_count(),
        1,
        "本次 tools/call 必须真的到达 server 侧 handler（否则「超时」可能是空跑）"
    );
    assert!(
        !handler.completed(),
        "到期返回时必须仍在飞（延迟夹具未放行、未完成）"
    );
    assert_eq!(
        handler.dropped_count(),
        0,
        "桥的 deadline 到期不得取消 server 侧执行（Drop 哨兵为 0 才是「client timeout ≠ server 取消」）"
    );

    // ③ 收敛面：放行被弃置的那次调用（无重放）→ 同一链路仍可服务 → 有界关闭。
    handler.release();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        handler.finished.notified(),
    )
    .await
    .expect("放行后被弃置的调用必须在有界等待内跑完（server 侧不得留在飞）");
    assert!(handler.completed(), "放行后替身必须正常完成");
    assert_eq!(
        handler.entered_count(),
        1,
        "被弃置的调用收敛不得引出重放（tools/call 仍应恰一次）"
    );

    let after = bridge
        .invoke(
            serde_json::json!({ "expression": "*/5 * * * *" }),
            peri_agent::tools::ToolContext::new(&[], ".")
                .with_session_identity("deadline-session", "deadline-turn"),
        )
        .await
        .expect("deadline 到期后同一条链路（client service / bridge）必须仍可服务");
    assert_eq!(after, "r29-delayed-ok");
    assert_eq!(handler.entered_count(), 2);

    let client_exit = service
        .close_with_timeout(crate::mcp::builtin::runtime::BUILTIN_CONVERGE_TIMEOUT)
        .await
        .expect("client service 关闭不得 join 失败");
    let server_exit = server_task
        .converge(crate::mcp::builtin::runtime::BUILTIN_CONVERGE_TIMEOUT)
        .await;
    assert!(
        matches!(
            server_exit,
            crate::mcp::builtin::runtime::BuiltinServerExit::Quit(_)
        ),
        "builtin server task 必须在有界收敛内靠 EOF 自然退出（不得 abort）: {server_exit:?}"
    );
    println!(
        "[R29 bridge] elapsed={elapsed:?} timeout=true timeout_secs={REGISTERED_TIMEOUT_SECS} text={text:?} entered={} in_flight_at_deadline=true server_side_dropped_at_deadline=0 replay=0 after_deadline_call=ok client_exit={client_exit:?} server_task={server_exit:?}",
        handler.entered_count()
    );
}
#[test]
fn completed_mcp_error_response_retains_known_application_failure() {
    let result = rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(
        "FileNotFound: missing-file.txt",
    )]);
    let error = super::completed_application_error(&result).unwrap();
    assert_eq!(
        error.code,
        peri_acp_types::tools::EffectiveToolErrorCode::ApplicationFailed,
    );
    assert_eq!(error.message, "FileNotFound: missing-file.txt");
}

#[test]
fn completed_mcp_success_does_not_infer_failure_from_content() {
    let mut result = rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
        "FileNotFound -32603 transport disconnected",
    )]);
    assert!(super::completed_application_error(&result).is_none());
    result.is_error = None;
    assert!(super::completed_application_error(&result).is_none());
}
