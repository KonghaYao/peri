//! Builtin 运行时（`mcp::builtin::runtime`）的 crate 内验证。
//!
//! 夹具形态：server 半边是**真实** `rmcp::serve_server`，client 半边是**生产**
//! `serve_client_auto`（真实 Auto lifecycle），两侧用 `(ReadHalf, WriteHalf)` 元组交给
//! `IntoTransport`。每个用例都有界收尾，不留 orphan task。

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig,
        Tool,
    },
    service::{QuitReason, RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};

use crate::mcp::{
    apps::McpCapabilityProfile,
    builtin::{
        context::BuiltinInstanceContext,
        runtime::{
            spawn_builtin_transport_with_handler, spawn_builtin_transport_with_tap,
            BuiltinCloseOutcome, BuiltinInstanceSupervisor, BuiltinServerExit, BuiltinServerTask,
            BuiltinSpawnError, TickCloseOutcome, TickGuard, BUILTIN_CONVERGE_TIMEOUT,
            BUILTIN_DUPLEX_BUF,
        },
    },
    client::{serve_client_auto, ClientStatus, McpClientHandle, McpClientPool, SHUTDOWN_TIMEOUT},
    config::McpServerConfig,
};
use peri_acp_types::plugin::ConfigSource;

/// client 侧握手上界（夹具自身收尾用；生产 builtin 超时常量在 `transport.rs`）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

// ─── 夹具 ─────────────────────────────────────────────────────────────────────

/// server 侧观测：`tools/list` / `tools/call` 的实际到达次数。
#[derive(Default)]
struct Probe {
    list_tools: AtomicUsize,
    call_tool: AtomicUsize,
}

impl Probe {
    fn list_tools(&self) -> usize {
        self.list_tools.load(Ordering::SeqCst)
    }

    fn call_tool(&self) -> usize {
        self.call_tool.load(Ordering::SeqCst)
    }
}

/// 真实 `ServerHandler` 夹具：声明一个工具，`tools/call` 回显构造时给定的正文
/// （大 payload 用例用它证明「capacity 只影响背压，不是单帧上限」）。
#[derive(Clone)]
struct FixtureServer {
    probe: Arc<Probe>,
    tool: &'static str,
    reply: String,
}

impl ServerHandler for FixtureServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("builtin-runtime-fixture", "0.0.1"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.probe.list_tools.fetch_add(1, Ordering::SeqCst);
        let schema = serde_json::json!({ "type": "object", "properties": {} })
            .as_object()
            .expect("json! 对象字面量必为 object")
            .clone();
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            self.tool,
            "builtin runtime 夹具工具：回显固定正文",
            schema,
        )]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.probe.call_tool.fetch_add(1, Ordering::SeqCst);
        if request.name.as_ref() != self.tool {
            return Err(McpError::invalid_params(
                format!("unknown tool: {}", request.name),
                None,
            ));
        }
        Ok(CallToolResponse::Complete(CallToolResult::success(vec![
            ContentBlock::text(self.reply.clone()),
        ])))
    }
}

fn fixture(instance_tool: &'static str, reply: &str) -> (FixtureServer, Arc<Probe>) {
    let probe = Arc::new(Probe::default());
    let server = FixtureServer {
        probe: Arc::clone(&probe),
        tool: instance_tool,
        reply: reply.to_string(),
    };
    (server, probe)
}

/// 一条已握手的 builtin 链路（client service + 本代关闭所有权）。
struct Link {
    service: crate::mcp::client::McpServiceWrapper,
    supervisor: BuiltinInstanceSupervisor,
}
impl Link {
    /// 夹具收尾：关闭 client（释放 duplex 写半边）→ 本代监督者按冻结顺序有界收敛
    /// （先 tick 再 server task）。
    async fn shutdown(mut self) -> BuiltinCloseOutcome {
        let _ = self.service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        self.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await
    }

    /// server 半边是否仍在运行。监督者不导出该观测点，夹具直接读子模块可见的私有字段
    /// （与 `BuiltinServerTask { instance, handle }` 的直接构造同一可见性）。
    fn server_task_is_finished(&self) -> bool {
        self.supervisor.server_task.is_finished()
    }
}

/// 用**生产** `serve_client_auto` 握手一条经 [`spawn_builtin_transport_with_handler`]
/// 装配的链路。
async fn connect(instance: &str, server: FixtureServer) -> Link {
    let transport = spawn_builtin_transport_with_handler(instance, server);
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("builtin 握手不得超时（同进程链路）")
        .expect("builtin 握手不得失败");
    Link {
        service,
        supervisor,
    }
}

/// 生产形态的 pool：绑定执行目录 + 注入实例上下文（A33：注入早于 `initialize`）。
///
/// 生产里 `execution_cwd` 与 `BuiltinInstanceContext::cwd` 同源（host 单 cwd）；spawn 路径
/// 读的是上下文里的 `cwd`，本夹具两者设成同一值，因此与生产同形。
fn pool_with_context(cwd: &std::path::Path) -> McpClientPool {
    let pool = McpClientPool::new_pending();
    pool.bind_execution_cwd(cwd).expect("夹具绑定执行目录");
    pool.set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new(
        cwd.to_string_lossy().to_string(),
    )))
    .expect("夹具首次注入上下文必须成功");
    pool
}

fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

fn call_reply(response: &CallToolResponse) -> String {
    match response {
        CallToolResponse::Complete(result) => {
            first_text(result).unwrap_or_else(|| panic!("必须返回文本正文: {result:?}"))
        }
        other => panic!("未返回完整结果: {other:?}"),
    }
}

// ─── ① 握手 / peer_info / 线路 ───────────────────────────────────────────────

/// builtin 链路必须完成 modern 握手且 `peer_info()` 可读——`initialize.rs` 的
/// `version` 从它派生。
#[tokio::test]
async fn builtin_link_uses_modern_handshake_and_keeps_peer_info() {
    let (server, probe) = fixture("fixture_tool", "reply");
    let link = connect("web", server).await;

    let peer = link.service.peer().clone();
    let info = peer
        .peer_info()
        .unwrap_or_else(|| panic!("modern 路径下 peer_info 必须是 Some"));
    assert_eq!(
        info.protocol_version,
        ProtocolVersion::V_2026_07_28,
        "modern 路径应选 preferred 版本"
    );
    assert!(
        !link.server_task_is_finished(),
        "握手完成后 server task 必须仍在运行（尚未关闭）"
    );

    // tools/list 必须真实到达 server（handler 侧计数），不是「client 侧空清单」。
    let tools = peer.list_all_tools().await.expect("tools/list 必须成功");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name.as_ref(), "fixture_tool");
    assert_eq!(probe.list_tools(), 1, "server 侧必须收到一次 tools/list");

    let outcome = link.shutdown().await;
    assert!(
        matches!(outcome.server, BuiltinServerExit::Quit(_)),
        "正常关闭必须靠 EOF 自然收敛（Q2 证据），实际: {outcome:?}"
    );
    assert!(
        matches!(outcome.tick, TickCloseOutcome::NotSpawned),
        "非 cron 实例不挂 tick（生产装配恒 tick: None），实际: {outcome:?}"
    );
}

/// 线路级证据：modern 路径下线路**不含** `initialize`，且 `tools/list` 真的上了线路。
#[tokio::test]
async fn builtin_wire_has_no_initialize_and_tools_list_reaches_server() {
    let (server, probe) = fixture("fixture_tool", "reply");
    let (transport, wire) = spawn_builtin_transport_with_tap("web", server);
    let mut service = serve_client_auto(
        transport.io,
        &McpCapabilityProfile::disabled(),
        HANDSHAKE_TIMEOUT,
    )
    .await
    .expect("builtin 握手不得超时")
    .expect("builtin 握手不得失败");
    let mut server_task = transport.server_task;

    service
        .peer()
        .list_all_tools()
        .await
        .expect("tools/list 必须成功");

    let methods = wire.methods();
    assert!(
        !methods.iter().any(|method| method == "initialize"),
        "builtin 必须走 modern（线路不得出现 initialize），实际: {methods:?}"
    );
    assert_eq!(
        methods.first().map(String::as_str),
        Some("server/discover"),
        "握手首帧必须是 server/discover，实际: {methods:?}"
    );
    assert!(
        methods.iter().any(|method| method == "tools/list"),
        "tools/list 必须真的到达 server，实际: {methods:?}"
    );
    assert_eq!(probe.list_tools(), 1, "server 侧必须收到一次 tools/list");

    let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
    let exit = server_task.converge(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(matches!(exit, BuiltinServerExit::Quit(_)), "实际: {exit:?}");
}

// ─── ①′ 三分类接线（IF-D1）───────────────────────────────────────────────────

/// 超时与失败日志字段必须来自**同一**三分类结果：builtin 不得复用 stdio / http 超时
/// （那会走错超时，并把失败日志的 `transport` 写成事实错误）。
#[test]
fn builtin_uses_its_own_timeout_and_transport_label() {
    use crate::mcp::initialize::{connect_timeout, transport_label};
    use crate::mcp::transport::{TransportKind, BUILTIN_CONNECT_TIMEOUT};

    assert_eq!(
        BUILTIN_CONNECT_TIMEOUT,
        Duration::from_secs(5),
        "builtin 握手超时的冻结取值"
    );
    assert_eq!(
        connect_timeout(TransportKind::Builtin),
        BUILTIN_CONNECT_TIMEOUT,
        "builtin 必须走自己的超时常量"
    );
    assert_ne!(
        connect_timeout(TransportKind::Builtin),
        connect_timeout(TransportKind::Stdio),
        "builtin 不得复用 stdio 超时"
    );
    assert_ne!(
        connect_timeout(TransportKind::Builtin),
        connect_timeout(TransportKind::Http),
        "builtin 不得复用 http 超时"
    );
    assert_eq!(transport_label(TransportKind::Builtin), "builtin");
    assert_eq!(transport_label(TransportKind::Stdio), "stdio");
    assert_eq!(transport_label(TransportKind::Http), "http");
}

// ─── ② 大 payload（A16）──────────────────────────────────────────────────────

/// 大正文必须完整往返：`BUILTIN_DUPLEX_BUF` 只影响背压，不是单帧上限。
#[tokio::test]
async fn large_payload_round_trips_intact() {
    let body = format!("HEAD:{}:TAIL", "x".repeat(300 * 1024));
    assert!(
        body.len() > 4 * BUILTIN_DUPLEX_BUF,
        "用例前提：正文必须远大于 duplex 容量（{BUILTIN_DUPLEX_BUF}）"
    );
    let (server, probe) = fixture("fixture_tool", &body);
    let link = connect("web", server).await;

    let called = link
        .service
        .peer()
        .call_tool_once(CallToolRequestParams::new("fixture_tool"))
        .await
        .expect("tools/call 必须成功");
    let reply = call_reply(&called);
    assert_eq!(reply.len(), body.len(), "大正文长度不得被截断");
    assert_eq!(reply, body, "大正文必须逐字节完整");
    assert_eq!(probe.call_tool(), 1, "server 侧必须真实收到一次 tools/call");

    let outcome = link.shutdown().await;
    assert!(
        matches!(outcome.server, BuiltinServerExit::Quit(_)),
        "实际: {outcome:?}"
    );
}

// ─── ③ 有界关闭（无 orphan）──────────────────────────────────────────────────

/// client 关闭后 server task 必须靠 EOF 收敛；未收敛才 abort，且 abort 后不留 orphan。
#[tokio::test]
async fn converge_waits_then_aborts_without_orphan() {
    let (server, _probe) = fixture("fixture_tool", "reply");
    let transport = spawn_builtin_transport_with_handler("web", server);
    // 保持 io 存活：server 读不到 EOF，因此不可能在等待上界内收敛。
    let io = transport.io;
    let mut server_task = transport.server_task;

    let started = std::time::Instant::now();
    let exit = server_task.converge(BUILTIN_CONVERGE_TIMEOUT).await;
    let elapsed = started.elapsed();

    assert!(
        matches!(exit, BuiltinServerExit::AbortedAfterTimeout),
        "未收敛的 task 必须走上界 + abort 路径，实际: {exit:?}"
    );
    assert!(
        server_task.is_finished(),
        "abort 后 task 必须已 join 完成（不留 orphan）"
    );
    assert!(
        elapsed >= Duration::from_millis(900),
        "必须先有界等待再 abort，实际等待 {elapsed:?}"
    );
    drop(io);
}

/// 名称未注册：typed error，且文本只含实例名（不含路径 / env / 凭据）。
///
/// 被验对象是**两张表都查不到**的名字（`nope` / `not-a-builtin` / 空串）：`workspace` 曾是
/// 「保留但未实现」的样例，W3-B 接线后它是**已实现实例**（其解析失败改由
/// `InstanceInputMissing` / `HandlerNotWired` 表达，见同文件
/// `missing_instance_input_is_typed_error_before_dispatch`），因此不得再拿它当未注册名
/// ——否则本用例会把「已注册」误当「未注册」，把一台真实存在的行为写成断言（先例：
/// AW3-09 要求把「未接线集合非空」的前置改成确定的具名断言）。
///
/// 两种 pool 状态都断言：**未注入上下文**（`ContextMissing` 成立的前提）与已注入上下文。
/// 前者是「实例解析先于上下文要求」的证伪点——若池方法先取上下文，未注册实例会被报成
/// `ContextMissing`，本用例即失败（不得改断言迁就实现）。
#[test]
fn spawn_rejects_unregistered_instance_with_typed_error() {
    let cwd = std::env::temp_dir();
    let without_context = McpClientPool::new_pending();
    let with_context = pool_with_context(&cwd);

    for (pool_state, pool) in [
        ("上下文缺失", &without_context),
        ("上下文已注入", &with_context),
    ] {
        for unknown in ["nope", "not-a-builtin", ""] {
            let error = pool
                .spawn_builtin_transport(unknown)
                .err()
                .unwrap_or_else(|| panic!("未注册实例 {unknown:?} 不得建立传输"));
            match error {
                BuiltinSpawnError::UnknownInstance { instance } => {
                    assert_eq!(instance, unknown, "pool 状态: {pool_state}")
                }
                other => panic!("{pool_state} 下必须是 typed UnknownInstance，实际: {other:?}"),
            }
        }
    }

    for unknown in ["nope", "not-a-builtin", ""] {
        let text = BuiltinSpawnError::UnknownInstance {
            instance: unknown.to_string(),
        }
        .to_string();
        assert!(text.contains("builtin MCP 实例未注册"), "实际: {text}");
        assert!(
            text.contains(unknown) || unknown.is_empty(),
            "错误文本只保留实例名，实际: {text}"
        );
    }
}

/// 已注册实例缺该类输入（`cron` 无 scheduler）：必须在 dispatch **之前**
/// 落成 typed `InstanceInputMissing`。
///
/// 「输入缺失」与「handler 未接线」是两件需要不同修复动作的事：`builtin_server_handler`
/// 的 `Option` 签名区分不了二者，若先跑 dispatch，`cron` 会被误报成
/// `HandlerNotWired`（把宿主装配缺陷写成代码缺陷），H-05 接线后更会变成「缺输入却照样
/// 建链路」。本用例是那条顺序的可证伪点。
#[test]
fn missing_instance_input_is_typed_error_before_dispatch() {
    let cwd = std::env::temp_dir();
    let pool = pool_with_context(&cwd);

    {
        let instance = "cron";
        let error = pool
            .spawn_builtin_transport(instance)
            .err()
            .unwrap_or_else(|| panic!("缺少 {instance} 输入的上下文不得建立传输"));
        match error {
            BuiltinSpawnError::InstanceInputMissing { instance: reported } => {
                assert_eq!(reported, instance)
            }
            other => panic!(
                "{instance}: 必须是 typed InstanceInputMissing（不得是 HandlerNotWired / panic），实际: {other:?}"
            ),
        }

        let text = BuiltinSpawnError::InstanceInputMissing {
            instance: instance.to_string(),
        }
        .to_string();
        assert!(
            text.contains("builtin 实例缺少上下文输入"),
            "错误文本必须是固定规则短语，实际: {text}"
        );
        assert!(
            text.contains(instance),
            "错误文本必须含实例名，实际: {text}"
        );
        assert!(
            !text.contains('/') && !text.contains('\\'),
            "错误文本不得含路径，实际: {text}"
        );
    }
}

/// 已注册实例必须经**真实** handler 完成握手与工具发现（生产 seam 的端到端证据）。
///
/// 与上面用夹具 handler 的用例的区别：这里走的是 `McpClientPool::spawn_builtin_transport`
/// 的实例名分派（`builtin/dispatch.rs` + `builtin/web.rs`），因此断言的是「web 实例真的能
/// 列出它的两个工具」。
#[tokio::test]
async fn registered_web_instance_handshakes_with_real_handler() {
    let cwd = std::env::temp_dir();
    let pool = pool_with_context(&cwd);
    let (io, supervisor) = pool
        .spawn_builtin_transport("web")
        .expect("web 实例必须已接线")
        .into_parts();
    let mut service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("web 实例握手不得超时")
        .expect("web 实例握手不得失败");

    let tools = service
        .peer()
        .list_all_tools()
        .await
        .expect("真实 handler 的 tools/list 必须成功");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        vec!["WebSearch", "WebFetch"],
        "工具集合与顺序必须与注册表声明一致"
    );
    // 模型面名字由 `tool_bridge` 的同一份规则与模板派生（IF-D5 的冻结字面量）。
    let effective: Vec<String> = names
        .iter()
        .map(|name| crate::mcp::tool_bridge::effective_mcp_tool_name("web", name))
        .collect();
    assert_eq!(
        effective,
        vec!["mcp__web__WebSearch", "mcp__web__WebFetch"],
        "模型面名字必须与 IF-D5 冻结字面量逐字一致"
    );
    for tool in &tools {
        assert!(
            tool.input_schema
                .get("properties")
                .is_some_and(|properties| properties.is_object()),
            "builtin 工具的 input_schema 必须是含 properties 的 object（否则启动期被 schema 校验拒绝）"
        );
    }

    let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
    let outcome = supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        matches!(outcome.server, BuiltinServerExit::Quit(_)),
        "实际: {outcome:?}"
    );
}

/// 已注册实例**绝不** panic、绝不静默降级成 stdio / http：
/// handler 接线落地前是 typed `HandlerNotWired`，落地后是可用链路。
#[tokio::test]
async fn registered_instance_never_silently_falls_back() {
    let cwd = std::env::temp_dir();
    let pool = pool_with_context(&cwd);
    match pool.spawn_builtin_transport("web") {
        Ok(transport) => {
            // 已接线：链路必须真的能收敛（无 orphan），证明返回的是可用形态。
            // 用监督者的关闭路径（先 tick 再 server task）：没有 tick 的一代，
            // `close` 的 tick 侧恒是 `NotSpawned`。
            let (io, supervisor) = transport.into_parts();
            drop(io);
            let outcome = supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
            assert!(
                matches!(
                    outcome.server,
                    BuiltinServerExit::Quit(_) | BuiltinServerExit::NotStarted(_)
                ),
                "已接线链路必须可收敛，实际: {outcome:?}"
            );
        }
        Err(BuiltinSpawnError::HandlerNotWired { instance }) => {
            assert_eq!(instance, "web", "未接线错误必须只报实例名");
        }
        Err(other) => panic!("已注册实例不得以其它形态失败（不得降级）: {other:?}"),
    }
}

// ─── ④ 隔离 / task 换新 ──────────────────────────────────────────────────────

/// 每实例一条独立链路：调用只落各自 handler，状态不串（契约 5 的 crate 内部分）。
#[tokio::test]
async fn instances_are_isolated_per_link() {
    let (web, web_probe) = fixture("WebSearch", "web-reply");
    let (artifact, artifact_probe) = fixture("artifact", "artifact-reply");
    let web_link = connect("web", web).await;
    let artifact_link = connect("artifact", artifact).await;

    // 两条链路各自完成一次 tools/list：互不代劳。
    let web_tools = web_link
        .service
        .peer()
        .list_all_tools()
        .await
        .expect("web tools/list 必须成功");
    let artifact_tools = artifact_link
        .service
        .peer()
        .list_all_tools()
        .await
        .expect("artifact tools/list 必须成功");
    assert_eq!(web_tools[0].name.as_ref(), "WebSearch");
    assert_eq!(artifact_tools[0].name.as_ref(), "artifact");
    assert_eq!(web_probe.list_tools(), 1);
    assert_eq!(artifact_probe.list_tools(), 1);

    // 调用只落 web 的 handler。
    let called = web_link
        .service
        .peer()
        .call_tool_once(CallToolRequestParams::new("WebSearch"))
        .await
        .expect("web 链路调用必须成功");
    assert_eq!(call_reply(&called), "web-reply");
    assert_eq!(web_probe.call_tool(), 1, "web handler 必须收到调用");
    assert_eq!(
        artifact_probe.call_tool(),
        0,
        "artifact handler 不得收到 web 的调用（namespace 不串）"
    );

    assert!(matches!(
        web_link.shutdown().await.server,
        BuiltinServerExit::Quit(_)
    ));
    assert!(matches!(
        artifact_link.shutdown().await.server,
        BuiltinServerExit::Quit(_)
    ));
}

/// 重连语义的运行时部分：新链路必须使用**新的** task，旧 task 在新链路建立前收敛。
#[tokio::test]
async fn reconnect_replaces_task_and_old_task_converges() {
    let (first, first_probe) = fixture("fixture_tool", "first");
    let (second, second_probe) = fixture("fixture_tool", "second");
    let old_link = connect("web", first).await;

    // 生产 reconnect 的第一步：关闭旧 client service（旧 server task 随之靠 EOF 收敛），
    // 再按冻结顺序收敛旧代监督者——新代建立前旧代必须已停。
    let old_outcome = old_link.shutdown().await;
    assert!(
        matches!(old_outcome.server, BuiltinServerExit::Quit(_)),
        "旧 server task 必须在新链路建立前收敛，实际: {old_outcome:?}"
    );

    // 新链路：全新 duplex + 全新 handler 实例（不复用旧 io / 旧 handler）。
    let new_link = connect("web", second).await;
    let called = new_link
        .service
        .peer()
        .call_tool_once(CallToolRequestParams::new("fixture_tool"))
        .await
        .expect("新链路调用必须成功");
    assert_eq!(call_reply(&called), "second", "必须由新链路响应");
    assert_eq!(first_probe.call_tool(), 0, "旧 handler 不得再收到调用");
    assert_eq!(second_probe.call_tool(), 1);
    assert!(matches!(
        new_link.shutdown().await.server,
        BuiltinServerExit::Quit(_)
    ));
}

// ─── ⑤ pool 侧 task 表 / 关闭 / transport_type（IF-D11 / IF-D12）─────────────

fn connected_handle(
    name: &str,
    source: Option<peri_acp_types::plugin::ConfigSource>,
) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: None,
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: Default::default(),
        source,
        url: None,
        skills_capable: false,
    })
}

fn builtin_source(instance: &str) -> Option<ConfigSource> {
    Some(ConfigSource::Builtin {
        instance: instance.to_string(),
    })
}

/// builtin 默认层的条目形状：无 command / url，`source` 是身份的唯一来源。
fn builtin_config(instance: &str) -> McpServerConfig {
    McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        subscriptions: None,
        system_mcp: Some(true),
        system_mcp_tools: Some(Vec::new()),
        system_mcp_timeout: None,
        source: builtin_source(instance),
    }
}

/// pool 关闭必须排空 builtin task 表且不留 orphan（已握手链路：正常关闭路径）。
#[tokio::test]
async fn pool_shutdown_drains_builtin_tasks() {
    let pool = McpClientPool::new_pending();
    let (server, _probe) = fixture("fixture_tool", "reply");
    let mut link = connect("web", server).await;
    // client 半边在生产路径里由 `services` 的 close 完成：关闭后 server 读半收到 EOF。
    let _ = link.service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
    assert!(pool
        .register_builtin_task("web".to_string(), link.supervisor)
        .is_none());
    assert_eq!(pool.builtin_task_count(), 1);

    pool.shutdown().await;

    assert_eq!(pool.builtin_task_count(), 0, "pool 关闭后 task 表必须为空");
}

/// pool 关闭时仍未收敛的 task 也必须收口（有界等待 → abort），不留 orphan。
#[tokio::test]
async fn pool_shutdown_aborts_unconverged_builtin_task() {
    let pool = McpClientPool::new_pending();
    let (server, _probe) = fixture("fixture_tool", "reply");
    let transport = spawn_builtin_transport_with_handler("web", server);
    let (io, supervisor) = transport.into_parts(); // io 保持存活：server 收不到 EOF
    pool.register_builtin_task("web".to_string(), supervisor);

    pool.shutdown().await;

    assert_eq!(pool.builtin_task_count(), 0);
    drop(io);
}

/// `transport_type` 三分类（IF-D11）：builtin 身份来自 `source`，其余逐位保持既有推断。
#[test]
fn transport_type_of_reports_builtin_stdio_and_http() {
    let pool = McpClientPool::new_pending();
    {
        let mut clients = pool.clients.write();
        clients.insert(
            "web".to_string(),
            connected_handle("web", builtin_source("web")),
        );
        clients.insert("local".to_string(), connected_handle("local", None));
        let mut http = connected_handle("remote", None);
        Arc::make_mut(&mut http).url = Some("https://example.invalid/mcp".to_string());
        clients.insert("remote".to_string(), http);
    }
    // config-only 行（clients 中不存在的条目）同样按 source 判定。
    pool.configs
        .write()
        .insert("artifact".to_string(), builtin_config("artifact"));

    let infos = pool.all_server_infos();
    let of = |name: &str| {
        infos
            .iter()
            .find(|info| info.name == name)
            .unwrap_or_else(|| panic!("缺少 {name} 的 ServerConfig"))
            .transport_type
            .clone()
    };
    assert_eq!(of("web"), "builtin", "builtin 身份必须来自 source");
    assert_eq!(
        of("artifact"),
        "builtin",
        "config-only 行同样按 source 判定"
    );
    assert_eq!(of("local"), "stdio", "无 url 的既有推断逐位不变");
    assert_eq!(of("remote"), "http", "有 url 的既有推断逐位不变");

    let snapshot = pool.server_infos();
    let web = snapshot
        .iter()
        .find(|info| info.name == "web")
        .expect("server_infos 必须包含 web");
    assert_eq!(web.transport_type, "builtin");
}

/// 代监督者表按 server name 登记 / 移除；已握手链路关闭后必须走 `Quit`（自然收敛）。
#[tokio::test]
async fn builtin_task_table_closes_handshaked_task() {
    let pool = McpClientPool::new_pending();
    let (server, _probe) = fixture("fixture_tool", "reply");
    let mut link = connect("web", server).await;
    let _ = link.service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
    assert!(pool
        .register_builtin_task("web".to_string(), link.supervisor)
        .is_none());
    assert_eq!(pool.builtin_task_count(), 1);
    // 生产装配恒 `tick: None`：非 cron 实例的一代「已无运行中的 tick」。
    assert_eq!(pool.builtin_tick_is_finished("web"), Some(true));

    let outcome = pool
        .close_builtin_task("web")
        .await
        .expect("必须取出并收敛");
    assert!(
        matches!(outcome.server, BuiltinServerExit::Quit(_)),
        "client 关闭后 server task 必须自然收敛，实际: {outcome:?}"
    );
    assert!(
        matches!(outcome.tick, TickCloseOutcome::NotSpawned),
        "不挂 tick 的一代不得报 joined/aborted，实际: {outcome:?}"
    );
    assert_eq!(pool.builtin_task_count(), 0);
    assert_eq!(
        pool.builtin_tick_is_finished("web"),
        None,
        "移除后表内无该条目（不是 `false`）"
    );
    assert!(
        pool.close_builtin_task("web").await.is_none(),
        "重复关闭应为无操作"
    );
}

/// 同名登记返回被替换的旧监督者（调用方负责收敛），且表内同名只有一个条目。
///
/// 未握手的链路（client 半边直接丢弃）退出事实是 `NotStarted`——server 未进入服务即
/// 退出，不得伪装成优雅关闭。
#[tokio::test]
async fn builtin_task_table_replaces_same_name() {
    let pool = McpClientPool::new_pending();
    let (first, _first_probe) = fixture("fixture_tool", "first");
    let (second, _second_probe) = fixture("fixture_tool", "second");
    let (first_io, first_supervisor) =
        spawn_builtin_transport_with_handler("web", first).into_parts();
    let (second_io, second_supervisor) =
        spawn_builtin_transport_with_handler("web", second).into_parts();
    drop(first_io);
    drop(second_io);

    assert!(pool
        .register_builtin_task("web".to_string(), first_supervisor)
        .is_none());
    let replaced = pool
        .register_builtin_task("web".to_string(), second_supervisor)
        .expect("同名登记必须返回被替换的旧监督者");
    assert_eq!(replaced.instance(), "web");
    assert_eq!(pool.builtin_task_count(), 1, "同名只有一个条目");

    let outcome = replaced.close(BUILTIN_CONVERGE_TIMEOUT).await;
    // `close` 的三条分支都在句柄解析后才返回 ⇒ 返回即「server task 已结束」（无 orphan）；
    // 被替换的一代没有 tick，tick 侧恒 `NotSpawned`。
    assert!(
        matches!(outcome.server, BuiltinServerExit::NotStarted(_)),
        "未握手的链路不得伪装成优雅关闭，实际: {outcome:?}"
    );
    assert!(
        matches!(outcome.tick, TickCloseOutcome::NotSpawned),
        "不挂 tick 的一代不得报 joined/aborted，实际: {outcome:?}"
    );
}

/// 非 builtin server 在代监督者表里没有条目：关闭是无操作（不引入第二条关闭路径）。
#[tokio::test]
async fn closing_task_of_non_builtin_server_is_noop() {
    let pool = McpClientPool::new_pending();
    assert!(pool.close_builtin_task("some-stdio-server").await.is_none());
    assert_eq!(pool.builtin_task_count(), 0);
    assert_eq!(pool.builtin_tick_is_finished("some-stdio-server"), None);
}

// ─── ⑥ A32 代监督者：关闭顺序（tick 先于 server task）────────────────────────

/// 等 tick task 至少触发一次（有界；到期即 panic，不静默放过）。
async fn wait_until_ticked(ticks: &AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while ticks.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("tick task 必须在有界等待内触发一次");
}

/// A32 冻结顺序的**因果**证明：`BuiltinInstanceSupervisor::close` 必须先停 tick，
/// 再有界收敛 server task。本用例不比较时间戳，而是让 server task 的收敛**依赖** tick
/// task 的退出：
///
/// - 假 server task 的 task 体在 `stopped.notified().await` 之后才返回 `Quit`；
/// - `stopped` 只在 tick task 退出前触发（`TickGuard::spawn` 的 body 尾部
///   `notify_waiters()`）。
///
/// 因此 server task **只有在 tick task 已退出之后**才可能收敛：若 `close` 先收敛 server
/// task（或两侧并发），server 侧必然耗满上界 ⇒ `AbortedAfterTimeout`，下面的 `Quit`
/// 断言失败——顺序被证伪，而不是「碰巧时间戳更小」。
#[tokio::test]
async fn supervisor_close_orders_tick_before_server_task() {
    const INTERVAL: Duration = Duration::from_millis(20);
    let ticks = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&ticks);
    let guard = TickGuard::spawn(INTERVAL, move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let stopped = guard.stopped();

    // 注册握手：`notify_waiters()` **不存 permit**，若 tick task 先退出而 server task 尚未
    // 注册等待者，唤醒会丢失（那会让本用例假失败，与顺序无关）。当前线程运行时下
    // `notify_one` 与注册在同一轮 poll 内先后完成，因此 `registered` 触发后等待者必已注册。
    let registered = Arc::new(tokio::sync::Notify::new());
    let registered_task = Arc::clone(&registered);
    let handle = tokio::spawn(async move {
        registered_task.notify_one();
        stopped.notified().await;
        BuiltinServerExit::Quit(QuitReason::Closed)
    });
    registered.notified().await;

    // 关闭前事实：tick 已真实触发过（否则本用例什么都没在测）。
    wait_until_ticked(&ticks).await;
    let before = ticks.load(Ordering::SeqCst);
    assert!(before > 0, "关闭前 tick 计数必须 > 0");

    let supervisor = BuiltinInstanceSupervisor::new(
        "cron".to_string(),
        BuiltinServerTask {
            instance: "cron".to_string(),
            handle,
        },
        Some(guard),
    );
    assert!(
        !supervisor.tick_is_finished(),
        "关闭前本代 tick 必须仍在运行"
    );

    let outcome = supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;

    assert!(
        matches!(outcome.tick, TickCloseOutcome::Joined),
        "tick 必须被 cancel 后有界 join 完成，实际: {outcome:?}"
    );
    assert!(
        matches!(outcome.server, BuiltinServerExit::Quit(QuitReason::Closed)),
        "server task 只能在 tick task 退出后收敛（先收敛 server 必然超时），实际: {outcome:?}"
    );

    // 平台基线必须在 `close()` **返回之后**取样：关闭前的最后一次快照（`before`）与
    // `close` 真正 `cancel()` 之间存在窗口，20ms 周期可能在这个窗口内**合法**触发一次；
    // 拿它当「关闭后不再触发」的基线，会把合法的关闭前 tick 判成失败（Windows CI 上实际
    // 发生过）。`close()` 返回时 tick task 已 cancel 且有界 join 完成（上面的 `Joined`
    // 断言），此后不可能再有任何 tick，计数已进入平台。
    let settled = ticks.load(Ordering::SeqCst);
    assert!(
        settled >= before,
        "关闭不得让 tick 计数回退：before={before}, settled={settled}"
    );

    // 关闭后 >2×interval 计数不再增长 ⇒ 「本代已无运行中的 tick」。`close(self)` 消费了
    // 监督者，关闭后的 `tick_is_finished()` 不可观测；它的可观察等价物正是上面两条
    // （`Joined` = 有界 join 完成）加上这里的计数平台。
    tokio::time::sleep(INTERVAL * 5).await;
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        settled,
        "关闭后 tick 不得再触发（task 已 join）"
    );
}
