use super::*;
use crate::mcp::builtin::runtime::BUILTIN_DUPLEX_BUF;

// B. 线路观测（per-instance wire，A13 ④）：现代握手 / 审批两态 / 大 payload / 不串
// ══════════════════════════════════════════════════════════════════════════════════

/// 线路级证据（§10 R2）：`web` 实例的链路必须走 modern（`server/discover` 起手、
/// **线路全程无 `initialize`**），且 `tools/list` 真的上了线路、清单等于注册表声明。
#[tokio::test]
async fn web_instance_wire_uses_modern_discover_and_lists_tools() {
    let web = find("web").expect("web 已实现");
    let stubs: Vec<Arc<dyn BaseTool>> = web
        .tools
        .iter()
        .map(|declaration| {
            let stub = Arc::new(StubTool::fixed(declaration.original_name, "wire-fixture"));
            stub as Arc<dyn BaseTool>
        })
        .collect();
    let link = TappedLink::connect(web, "runtime-fixture-web", stubs).await;

    let methods = link.wire_methods();
    assert_eq!(
        methods.first().map(String::as_str),
        Some("server/discover"),
        "builtin 握手首帧必须是 server/discover（modern），实际: {methods:?}"
    );
    assert!(
        !methods.iter().any(|method| method == "initialize"),
        "modern 路径下线路不得出现 initialize 帧，实际: {methods:?}"
    );
    assert!(
        methods.iter().any(|method| method == "tools/list"),
        "tools/list 必须真的到达 server，实际: {methods:?}"
    );

    let served: Vec<String> = link
        .pool
        .get_client("web")
        .expect("web 必须已连接")
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    let declared: Vec<String> = web
        .tools
        .iter()
        .map(|declaration| declaration.original_name.to_string())
        .collect();
    assert_eq!(
        served, declared,
        "live tools/list 必须按声明顺序返回注册表工具"
    );

    link.shutdown().await;
}

/// 契约 6 缺口闭合（approve）：被提升为 direct 的 builtin 工具走**完整审批链**，批准后
/// 内置 server 的工具执行计数恰好 +1、线路上恰好一条 `tools/call`、结果内容可辨认。
///
/// 审批 broker 收到的名字是**模型面 effective name**；线路上的工具名是**原始名**
/// （归一不得泄漏到 wire，§3 IF-D15 / §9 规则 11）。
#[tokio::test]
async fn direct_builtin_tool_call_passes_approval_then_touches_wire_once() {
    let web = find("web").expect("web 已实现");
    let declaration = &web.tools[0];
    assert!(declaration.direct, "本用例要求该工具声明为 direct");
    let stub = Arc::new(StubTool::fixed(
        declaration.original_name,
        "builtin-runtime-ok",
    ));
    let stub_tool: Arc<dyn BaseTool> = Arc::clone(&stub) as Arc<dyn BaseTool>;
    let link = TappedLink::connect(web, "runtime-fixture-web", vec![stub_tool]).await;

    let call = tool_call(
        declaration.effective_name,
        json!({ "query": "builtin runtime approval" }),
    );
    let broker = Arc::new(RecordingBroker::approving());
    let middleware = PermissionMiddleware::new(
        Arc::clone(&broker) as Arc<dyn UserInteractionBroker>,
        default_requires_approval,
    );

    let results = run_approval_chain(&middleware, &call).await;
    assert_eq!(broker.requests(), 1, "恰好一次审批请求");
    assert_eq!(
        broker.seen(),
        vec![(call.name.clone(), call.input.clone())],
        "审批 broker 必须收到 effective name 与模型给出的入参"
    );
    let approved = match results.into_iter().next().expect("一个审批结果") {
        Ok(call) => call,
        Err(error) => panic!("批准后必须放行，实际: {error}"),
    };
    assert_eq!(approved.name, call.name, "批准不得改写模型侧工具名");

    let bridge = link.bridge(declaration.effective_name);
    assert!(
        bridge.is_direct(),
        "注册表声明为 direct 的 builtin 工具必须在类型化构造点生效（IF-D13）"
    );
    let invocation_fixture = invocation_fixture::InvocationFixture::new(
        declaration.effective_name,
        std::slice::from_ref(&approved.input),
    )
    .await;

    let text = bridge
        .invoke(approved.input.clone(), invocation_fixture.context(0))
        .await
        .expect("批准后的调用必须成功");
    assert_eq!(text, "builtin-runtime-ok", "工具结果内容必须可辨认");
    assert_eq!(
        stub.call_count(),
        1,
        "内置 server 的工具执行计数必须恰好 +1"
    );
    assert_eq!(
        link.served_calls(),
        vec![declaration.original_name.to_string()],
        "到达 handler 的工具名必须是原始名"
    );
    assert_eq!(
        link.wire_call_tool_count(),
        1,
        "线路上恰好一条 tools/call，实际: {:?}",
        link.wire_methods()
    );
    assert_eq!(
        stub.seen_inputs(),
        vec![call.input.clone()],
        "server 侧必须看到模型给出的入参（参数真的过了 wire）"
    );

    link.shutdown().await;
}

/// 契约 6 缺口闭合（reject）：拒绝后内置 server 的工具执行计数为 0、线路 `tools/call`
/// 为 0（链路本身是活的，handshake 帧在），结果为拒绝语义，且**不重试**（只审批一次）。
#[tokio::test]
async fn rejected_direct_tool_call_never_reaches_the_wire() {
    let web = find("web").expect("web 已实现");
    let declaration = &web.tools[0];
    let stub = Arc::new(StubTool::fixed(declaration.original_name, "must-not-run"));
    let stub_tool: Arc<dyn BaseTool> = Arc::clone(&stub) as Arc<dyn BaseTool>;
    let link = TappedLink::connect(web, "runtime-fixture-web", vec![stub_tool]).await;

    let call = tool_call(
        declaration.effective_name,
        json!({ "query": "builtin runtime rejection" }),
    );
    let broker = Arc::new(RecordingBroker::rejecting());
    let middleware = PermissionMiddleware::new(
        Arc::clone(&broker) as Arc<dyn UserInteractionBroker>,
        default_requires_approval,
    );

    let results = run_approval_chain(&middleware, &call).await;
    assert_eq!(broker.requests(), 1, "拒绝路径不得触发第二次审批");
    assert_eq!(broker.seen().len(), 1, "拒绝路径只记录一次审批项");
    let rejection = match results.into_iter().next().expect("一个审批结果") {
        Err(error) => error,
        Ok(call) => panic!("拒绝不得放行工具调用: {call:?}"),
    };
    match rejection {
        AgentError::ToolRejected { tool, .. } => {
            assert_eq!(tool, call.name, "拒绝语义必须指名被拒的 effective name")
        }
        other => panic!("拒绝必须映射为 ToolRejected，实际: {other:?}"),
    }

    assert_eq!(stub.call_count(), 0, "被拒的调用不得执行工具");
    assert!(link.served_calls().is_empty(), "不得有请求到达 handler");
    assert_eq!(link.wire_call_tool_count(), 0, "线路不得出现 tools/call");
    assert!(
        link.wire_methods()
            .iter()
            .any(|method| method == "tools/list"),
        "0 条 tools/call 不是因为链路没起来: {:?}",
        link.wire_methods()
    );

    link.shutdown().await;
}

/// 归一的反证（IF-D15 / §9 规则 11）：effective name **不是** wire 上的工具名——
/// 直接把它发到真实 handler 上会被按未知工具拒绝（生产 handler 同样只认原始名）。
#[tokio::test]
async fn old_namespaced_name_is_not_a_wire_alias_on_real_handler() {
    let web = find("web").expect("web 已实现");
    let declaration = &web.tools[0];
    let namespaced =
        crate::mcp::tool_bridge::effective_mcp_tool_name(web.name, declaration.original_name);
    let stub = Arc::new(StubTool::fixed(declaration.original_name, "unused"));
    let stub_tool: Arc<dyn BaseTool> = Arc::clone(&stub) as Arc<dyn BaseTool>;
    let link = TappedLink::connect(web, "runtime-fixture-web", vec![stub_tool]).await;

    let error = link
        .peer()
        .call_tool(CallToolRequestParams::new(namespaced.clone()))
        .await
        .expect_err("effective name 不得是 wire 名");
    assert!(
        link.served_calls().contains(&namespaced),
        "handler 必须真的收到该请求（拒绝发生在路由层）"
    );
    match error {
        rmcp::ServiceError::McpError(data) => {
            assert_eq!(
                data.code.0,
                ErrorCode::INVALID_PARAMS.0,
                "未知工具名必须回 invalid_params"
            );
            assert!(
                data.message.contains("unknown tool"),
                "错误文本必须点明未知工具，实际: {}",
                data.message
            );
        }
        other => panic!("期望 McpError(invalid_params)，实际: {other:?}"),
    }
    assert_eq!(stub.call_count(), 0, "路由失败不得执行工具");

    link.shutdown().await;
}

/// 大 payload（A16）：`BUILTIN_DUPLEX_BUF` 只影响背压，**不是单帧上限**——本用例走
/// **大请求**方向（~200 KiB 参数完整到达 server，经生产 `McpToolBridge`）。
///
/// 大**结果**方向（~300 KiB 正文逐字节返回）的生产装配版本在
/// `mcp::builtin::runtime::tests::large_payload_round_trips_intact`（真实
/// `spawn_builtin_transport_with_handler` 链路），本文件不重复该方向。
#[tokio::test]
async fn large_payload_crosses_builtin_instance_intact() {
    let web = find("web").expect("web 已实现");
    let declaration = &web.tools[0];
    let stub = Arc::new(StubTool::new(
        declaration.original_name,
        Arc::new(
            |input: &Value| match input.get("payload").and_then(Value::as_str) {
                Some(payload) => format!("received-bytes:{}", payload.len()),
                None => "no-payload".to_string(),
            },
        ),
    ));
    let stub_tool: Arc<dyn BaseTool> = Arc::clone(&stub) as Arc<dyn BaseTool>;
    let link = TappedLink::connect(web, "runtime-fixture-web", vec![stub_tool]).await;
    let bridge = link.bridge(declaration.effective_name);

    let payload = "y".repeat(LARGE_INPUT_BYTES);
    assert!(
        payload.len() > 4 * BUILTIN_DUPLEX_BUF,
        "用例前提：参数必须远大于 duplex 容量（{BUILTIN_DUPLEX_BUF}）"
    );
    let input = json!({ "payload": payload.clone() });
    let invocation_fixture = invocation_fixture::InvocationFixture::new(
        declaration.effective_name,
        std::slice::from_ref(&input),
    )
    .await;
    let echo = bridge
        .invoke(input, invocation_fixture.context(0))
        .await
        .expect("大请求帧必须完整到达 server");
    assert_eq!(
        echo,
        format!("received-bytes:{}", payload.len()),
        "server 侧必须收到完整的大参数"
    );
    assert_eq!(link.wire_call_tool_count(), 1);
    assert_eq!(stub.call_count(), 1);

    link.shutdown().await;
}

/// A13 ④/⑤：两个实例的 wire 互不串（发给 `web` 的请求只出现在 `web` 的线路上），
/// namespace 路由正确——某实例的 effective name bridge 只落该实例的 handler
/// （由 `mcp_server_name()` 与各自的 server 侧观测共同锁定）。
#[tokio::test]
async fn per_instance_wire_does_not_cross_between_instances() {
    let web = find("web").expect("web 已实现");
    let artifact = find("artifact").expect("artifact 已实现");
    let web_stub = Arc::new(StubTool::fixed(web.tools[0].original_name, "web-reply"));
    let artifact_stub = Arc::new(StubTool::fixed(
        artifact.tools[0].original_name,
        "artifact-reply",
    ));
    let web_tool: Arc<dyn BaseTool> = Arc::clone(&web_stub) as Arc<dyn BaseTool>;
    let artifact_tool: Arc<dyn BaseTool> = Arc::clone(&artifact_stub) as Arc<dyn BaseTool>;
    let web_link = TappedLink::connect(web, "runtime-fixture-web", vec![web_tool]).await;
    let artifact_link =
        TappedLink::connect(artifact, "runtime-fixture-artifact", vec![artifact_tool]).await;

    let web_bridge = web_link.bridge(web.tools[0].effective_name);
    let artifact_bridge = artifact_link.bridge(artifact.tools[0].effective_name);
    assert_eq!(web_bridge.mcp_server_name(), Some("web"));
    assert_eq!(artifact_bridge.mcp_server_name(), Some("artifact"));
    let invocation_fixture = invocation_fixture::InvocationFixture::new_calls(&[
        (web.tools[0].effective_name, json!({})),
        (artifact.tools[0].effective_name, json!({})),
    ])
    .await;

    let text = web_bridge
        .invoke(json!({}), invocation_fixture.context(0))
        .await
        .expect("web 调用必须成功");
    assert_eq!(text, "web-reply");
    assert_eq!(web_stub.call_count(), 1);
    assert_eq!(web_link.wire_call_tool_count(), 1);
    assert_eq!(
        artifact_link.wire_call_tool_count(),
        0,
        "发往 web 的请求不得出现在 artifact 的 wire 上: {:?}",
        artifact_link.wire_methods()
    );
    assert_eq!(artifact_stub.call_count(), 0);

    let text = artifact_bridge
        .invoke(json!({}), invocation_fixture.context(1))
        .await
        .expect("artifact 调用必须成功");
    assert_eq!(text, "artifact-reply");
    assert_eq!(artifact_stub.call_count(), 1);
    assert_eq!(artifact_link.wire_call_tool_count(), 1);
    assert_eq!(
        web_link.wire_call_tool_count(),
        1,
        "发往 artifact 的请求不得出现在 web 的 wire 上: {:?}",
        web_link.wire_methods()
    );
    assert_eq!(web_stub.call_count(), 1);

    web_link.shutdown().await;
    artifact_link.shutdown().await;
}

/// A13 ④/⑤ 的**同 pool 容器**面：两条 builtin 实例的链路（client 半边都移交进
/// `pool.services`）建在**同一个** `McpClientPool` 里时，pool 的生命周期动作
/// （`reconnect` / `shutdown`）**只**作用于被点名的实例。
///
/// 与既有 [`per_instance_wire_does_not_cross_between_instances`] 的差别：那条用例的两条
/// 链路各自 new 一个 pool，且两条 client 半边都留在夹具手里，因此**不覆盖**「同一 pool
/// 容器内，重连 / 关闭的作用域是否只限于被点名实例」。本用例经
/// [`TappedLink::connect_into`] 参数化 pool，并由 [`TappedLink::hand_service_to_pool`] 把
/// 两条 client 半边移交生产表，闭合该缺口。
///
/// 断言口径（逐条）：
/// 1. web 调用一次 ⇒ web 线路 `tools/call == 1`、artifact 线路 `tools/call == 0`；
/// 2. 取 artifact 的 method 快照后调用 artifact ⇒ web 仍 `1`、artifact `1`；
/// 3. **method 序列逐字对照**（不是只比长度）：web 动作前后各取快照，artifact 的 log
///    必须**逐字不变**、web 的 log 必须**只追加一帧 `tools/call`**（前缀逐字相等）；
///    artifact 侧反向同理；
/// 4. **移交前提**：两条 client 半边都在 `pool.services` 里；未移交时 `reconnect` 的
///    「关旧 service」对夹具链路是**空操作**，第 5 条②的「逐字不变」在任何实现下都成立
///    （没有失败模式）；
/// 5. **重连隔离**（`pool.reconnect("web", None)`，先 `bind_execution_cwd`）：
///    ① 被点名的 web：其旧 client 半边由 pool 关闭 ⇒ **夹具自持**的 web server task 靠
///    EOF **自然**收敛 `Quit`——收敛的触发者是 pool 的动作，不是夹具自己的收尾；
///    ② 未被点名的 artifact：句柄 `Arc` 同一、server task **未**结束、method 序列**逐字
///    等于**重连前快照、server 侧计数不变，且 bridge 仍能完成一次完整 `tools/call`
///    （把无关实例一并关掉的实现会让这一步红）；
///    ③ 重连新建的 web 链路走**生产** handler（对端名不再是夹具替身）且登记进 pool task 表；
/// 6. 收尾：pool 关闭时两条 server task 均靠 EOF 自然收敛 `Quit`（web 那条由重连触发、
///    artifact 那条由 `pool.shutdown()` 关闭其 client 半边触发），
///    `builtin_task_count() == 0`，不留 orphan。
///
/// 证据边界（诚实标注）：
/// - 第 1–3 条的「不串」在本用例里**不是路由隔离断言**：工具调用不经 pool 转发（每个
///   bridge 自带 peer），pool 容器只承载「表」与生命周期。它们排除的是 bridge↔实例
///   **绑定写错**与观测面串台，**增量可证伪力有限**；本用例的实质内容在第 4–6 条
///   （pool 生命周期动作的作用域）。
/// - server 半边是 [`FixtureBuiltinHandler`] **替身**（原因见文件头「证据边界 B」）。
/// - 「生产 handler 在真 loader 下同 pool 的 wire 序列隔离」**无证据面**：生产 transport
///   没有 per-instance tap，[`StartupFixture`] 系用例只观察握手 / 目录 / 真实往返，不看
///   线路帧序列。本记录**不宣称**该命题（acceptance §12.5）。
#[tokio::test]
async fn same_pool_instances_never_cross_wires_and_reconnect_touches_one_link() {
    let web = find("web").expect("web 已实现");
    let artifact = find("artifact").expect("artifact 已实现");
    let web_stub = Arc::new(StubTool::fixed(web.tools[0].original_name, "web-reply"));
    let artifact_stub = Arc::new(StubTool::fixed(
        artifact.tools[0].original_name,
        "artifact-reply",
    ));
    let web_tool: Arc<dyn BaseTool> = Arc::clone(&web_stub) as Arc<dyn BaseTool>;
    let artifact_tool: Arc<dyn BaseTool> = Arc::clone(&artifact_stub) as Arc<dyn BaseTool>;

    // **同一个** pool 容器里两条链路：本用例与 `per_instance_wire_*` 的唯一结构差别。
    let pool = Arc::new(McpClientPool::new_empty());
    // A33：`pool.reconnect` 走生产 spawn 点，必须先有注入的上下文。本用例只重连 web
    // （web 不读 cwd；artifact 侧始终是夹具替身），因此 cwd 取临时目录即可。
    pool.set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new(
        std::env::temp_dir().to_string_lossy().to_string(),
    )))
    .expect("夹具首次注入上下文必须成功");
    let mut web_link = TappedLink::connect_into(
        Arc::clone(&pool),
        web,
        "runtime-fixture-web",
        vec![web_tool],
    )
    .await;
    let mut artifact_link = TappedLink::connect_into(
        Arc::clone(&pool),
        artifact,
        "runtime-fixture-artifact",
        vec![artifact_tool],
    )
    .await;

    // 移交前提（本用例的实质所在）：两条 client 半边都进 pool 的生产表，此后
    // `reconnect` 的「关旧 service」不再是被点名实例上的空操作，而 artifact 侧的
    // 「逐字不变 + 仍可往返」也才有了失败模式（把无关实例一并关掉的实现会红）。
    web_link.hand_service_to_pool();
    artifact_link.hand_service_to_pool();
    assert_eq!(
        pool.services.lock().len(),
        2,
        "两条夹具链路的 client 半边都必须登记进 pool.services"
    );

    let web_bridge = web_link.bridge(web.tools[0].effective_name);
    let artifact_bridge = artifact_link.bridge(artifact.tools[0].effective_name);
    assert_eq!(web_bridge.mcp_server_name(), Some("web"));
    assert_eq!(artifact_bridge.mcp_server_name(), Some("artifact"));
    let invocation_fixture = invocation_fixture::InvocationFixture::new_calls(&[
        (web.tools[0].effective_name, json!({})),
        (artifact.tools[0].effective_name, json!({})),
        (
            artifact.tools[0].effective_name,
            json!({ "after": "reconnect" }),
        ),
    ])
    .await;

    // 两条链路各自走完握手 + live `tools/list`；此后各自的 log 只应因**自己**的动作增长。
    let web_log_0 = web_link.wire_methods();
    let artifact_log_0 = artifact_link.wire_methods();
    assert!(
        !web_log_0.is_empty() && !artifact_log_0.is_empty(),
        "用例前提：两条链路都已产生握手 / tools/list 帧: web={web_log_0:?} artifact={artifact_log_0:?}"
    );
    assert_eq!(
        web_link.wire_call_tool_count(),
        0,
        "用例前提：web 链路尚无 tools/call"
    );
    assert_eq!(
        artifact_link.wire_call_tool_count(),
        0,
        "用例前提：artifact 链路尚无 tools/call"
    );
    assert_eq!(web_link.served_calls(), Vec::<String>::new());
    assert_eq!(artifact_link.served_calls(), Vec::<String>::new());

    // ── 动作 1：只碰 web ────────────────────────────────────────────────────────
    let text = web_bridge
        .invoke(json!({}), invocation_fixture.context(0))
        .await
        .expect("web 调用必须成功");
    assert_eq!(text, "web-reply");
    assert_eq!(web_stub.call_count(), 1);
    assert_eq!(
        artifact_stub.call_count(),
        0,
        "web 的动作不得执行 artifact 的工具"
    );
    assert_eq!(web_link.wire_call_tool_count(), 1);
    assert_eq!(
        web_link.served_calls(),
        vec![web.tools[0].original_name.to_string()],
        "web 的 server 侧只应见过一次自己的原始名 tools/call"
    );
    assert_eq!(
        artifact_link.wire_call_tool_count(),
        0,
        "发往 web 的请求不得出现在 artifact 的 wire 上: {:?}",
        artifact_link.wire_methods()
    );

    // method 序列逐字对照（不是只比长度）。
    let web_log_1 = web_link.wire_methods();
    let artifact_log_1 = artifact_link.wire_methods();
    assert_eq!(
        artifact_log_1, artifact_log_0,
        "web 的动作不得向 artifact 的 log 追加任何帧"
    );
    assert_eq!(
        artifact_link.served_calls(),
        Vec::<String>::new(),
        "web 的动作不得让 artifact 的 server 侧看到 tools/call"
    );
    assert_eq!(
        web_log_1.len(),
        web_log_0.len() + 1,
        "web 的 log 只应追加一帧: {web_log_1:?}"
    );
    assert_eq!(
        &web_log_1[..web_log_0.len()],
        web_log_0.as_slice(),
        "web 的 log 只允许追加，不得重排 / 重写既有帧"
    );
    assert_eq!(
        web_log_1.last().map(String::as_str),
        Some("tools/call"),
        "web 追加的帧必须是 tools/call: {web_log_1:?}"
    );

    // ── 动作 2：artifact 侧对称（快照 A = `artifact_log_1`）─────────────────────
    let text = artifact_bridge
        .invoke(json!({}), invocation_fixture.context(1))
        .await
        .expect("artifact 调用必须成功");
    assert_eq!(text, "artifact-reply");
    assert_eq!(artifact_stub.call_count(), 1);
    assert_eq!(
        web_stub.call_count(),
        1,
        "artifact 的动作不得执行 web 的工具"
    );
    assert_eq!(artifact_link.wire_call_tool_count(), 1);
    assert_eq!(
        artifact_link.served_calls(),
        vec![artifact.tools[0].original_name.to_string()]
    );
    assert_eq!(
        web_link.wire_call_tool_count(),
        1,
        "发往 artifact 的请求不得出现在 web 的 wire 上"
    );

    let web_log_2 = web_link.wire_methods();
    let artifact_log_2 = artifact_link.wire_methods();
    assert_eq!(
        web_log_2, web_log_1,
        "artifact 的动作不得向 web 的 log 追加任何帧"
    );
    assert_eq!(
        artifact_log_2.len(),
        artifact_log_1.len() + 1,
        "artifact 的 log 只应追加一帧: {artifact_log_2:?}"
    );
    assert_eq!(
        &artifact_log_2[..artifact_log_1.len()],
        artifact_log_1.as_slice(),
        "artifact 的 log 只允许追加，不得重排 / 重写既有帧"
    );
    assert_eq!(
        artifact_log_2.last().map(String::as_str),
        Some("tools/call"),
        "artifact 追加的帧必须是 tools/call: {artifact_log_2:?}"
    );

    // ── 动作 3：reconnect 只动被点名的 web ─────────────────────────────────────
    // builtin 重连分支要求 pool 级 `execution_cwd` 已绑定，否则直接
    // `ConnectionFailed{ "MCP execution directory is not initialized" }`
    // （`mcp/reconnect.rs:98-104`）。两条 client 半边已移交 `pool.services`（见上文
    // 「移交前提」），因此重连会真的关闭被点名实例的旧 service；未被点名实例的 service
    // 留在表里，仍是其后端（下面既断言它仍在表内，也用它完成一次真实往返）。
    let cwd = tempfile::tempdir().expect("tempdir");
    pool.bind_execution_cwd(cwd.path())
        .expect("reconnect 前提：pool 级 execution_cwd 绑定必须成功");
    assert_eq!(
        pool.builtin_task_count(),
        0,
        "用例前提：两条夹具链路不登记 pool task 表（server 半边归属在夹具手里）"
    );
    assert!(
        !web_link.server_task.is_finished() && !artifact_link.server_task.is_finished(),
        "用例前提：重连前两条夹具 server task 都在运行"
    );

    let web_handle_before = pool.get_client("web").expect("web 句柄必须存在");
    let artifact_handle_before = pool.get_client("artifact").expect("artifact 句柄必须存在");
    let artifact_log_before_reconnect = artifact_link.wire_methods();
    let artifact_served_before_reconnect = artifact_link.served_calls();
    let peer_name_of = |handle: &Arc<McpClientHandle>| -> Option<String> {
        handle
            .peer
            .as_ref()
            .and_then(|peer| peer.peer_info())
            .and_then(|info| {
                info.server_info
                    .as_ref()
                    .map(|server| server.name.to_string())
            })
    };
    // 判别基线：重连**前**的 web 对端确实是夹具替身。没有这一条，「重连后不再是替身」
    // 的断言就可能在任何对端名上碰巧成立（假绿）。
    assert_eq!(
        peer_name_of(&web_handle_before).as_deref(),
        Some("runtime-fixture-web"),
        "用例前提：重连前的 web 对端是夹具替身"
    );

    pool.reconnect("web", None)
        .await
        .expect("builtin 实例必须能重连");

    // 反证「重连真的发生了」：否则下面「artifact 逐字不变」可能只是整体 no-op 的假绿。
    let web_handle_after = pool.get_client("web").expect("重连后必须留下新句柄");
    assert!(
        !Arc::ptr_eq(&web_handle_before, &web_handle_after),
        "重连必须换新句柄（旧代证据不得继续有效）"
    );
    assert!(
        matches!(web_handle_after.status, ClientStatus::Connected),
        "重连后 web 必须重新 Connected，实际: {:?}",
        web_handle_after.status
    );
    assert!(
        Arc::ptr_eq(
            &artifact_handle_before,
            &pool.get_client("artifact").expect("artifact 必须仍在")
        ),
        "重连 web 不得触碰 artifact 的句柄"
    );
    assert_eq!(
        pool.builtin_task_count(),
        1,
        "重连新建的 web 链路必须登记进 pool task 表（与夹具链路不同）"
    );
    // 重连走**生产** handler（`spawn_builtin_transport`），不是夹具替身：这是「重连真的
    // 重建了链路」的旁证，也说明此后只有 artifact 侧（仍是替身）被断言。
    let web_peer_name = peer_name_of(&web_handle_after);
    assert!(
        web_peer_name
            .as_deref()
            .is_some_and(|name| name != "runtime-fixture-web"),
        "重连后的 web 必须已重新握手到生产 handler（不是夹具替身），实际: {web_peer_name:?}"
    );

    // 被点名的 web：pool 关闭了它登记的**夹具** client 半边（`reconnect.rs:57-60` 的
    // remove + `close_with_timeout`）⇒ 夹具自持的 server task 靠 EOF **自然**收敛。
    // 收敛的触发者是 pool 的动作（本用例在此之前只调用了 `reconnect`），因此这一条是
    // 「重连真的关掉了旧链路」的因果证据，而不是夹具自己的收尾。
    let web_exit = web_link.converge_task().await;
    assert!(
        matches!(web_exit, BuiltinServerExit::Quit(_)),
        "重连必须关掉被点名实例的旧 client 半边（其 server task 随之自然收敛），实际: {web_exit:?}"
    );

    // artifact 侧：service 仍在 pool 表内 + server task 未结束 + method 序列**逐字等于**
    // 重连前快照 + 仍可完成一次完整往返。（移交之前，「逐字不变」在任何实现下都成立；
    // 移交之后，若重连把无关实例一并关掉，下面的往返会失败。）
    assert!(
        pool.services.lock().contains_key("artifact"),
        "重连 web 不得把 artifact 的 client 半边移出 pool.services"
    );
    assert!(
        !artifact_link.server_task.is_finished(),
        "重连 web 不得让 artifact 的 server task 结束"
    );
    assert_eq!(
        artifact_link.wire_methods(),
        artifact_log_before_reconnect,
        "重连 web 不得向 artifact 的 log 追加任何帧"
    );
    assert_eq!(
        artifact_link.served_calls(),
        artifact_served_before_reconnect,
        "重连 web 不得让 artifact 的 server 侧多一次 tools/call"
    );

    let text = artifact_bridge
        .invoke(
            json!({ "after": "reconnect" }),
            invocation_fixture.context(2),
        )
        .await
        .expect("重连 web 后 artifact 必须仍可调用");
    assert_eq!(text, "artifact-reply");
    assert_eq!(artifact_stub.call_count(), 2);
    assert_eq!(artifact_link.wire_call_tool_count(), 2);
    assert_eq!(
        &artifact_link.wire_methods()[..artifact_log_before_reconnect.len()],
        artifact_log_before_reconnect.as_slice(),
        "重连后 artifact 的 log 仍只允许追加"
    );

    // ── 收尾：两条夹具链路各自 `Quit` 收敛 + pool 归属 task 排空 ────────────────
    // `TappedLink::shutdown` 内部断言 `BuiltinServerExit::Quit`（不得 `AbortedAfterTimeout`）
    // 与 `task.is_finished()`。
    // web 夹具链路的 server task 已在上文断言收敛（触发者 = reconnect）；artifact 夹具
    // 链路的 client 半边仍在 pool 表里，由 `pool.shutdown()` 关闭，其 server task 随之收敛。
    pool.begin_shutdown();
    // 重连新建的 web 链路**登记在 pool 上**（`reconnect.rs` 的 `register_builtin_task`），
    // 与两条夹具链路归属不同：它必须由 pool 侧收敛，且同样落在 `Quit`。（它的 client
    // 半边也在 `services` 里，由下面的 `pool.shutdown()` 关闭；此处先按 key 取出监督者
    // 并断言其收敛事实。）
    let outcome = pool
        .close_builtin_task("web")
        .await
        .expect("重连注册的 builtin 监督者必须还在 pool task 表里");
    assert!(
        matches!(outcome.server, BuiltinServerExit::Quit(_)),
        "pool 关闭时重连链路也必须靠 EOF 自然收敛，实际: {outcome:?}"
    );
    let report = pool.shutdown().await;
    assert!(report.is_complete(), "pool 关闭必须收敛: {report:?}");
    let artifact_exit = artifact_link.converge_task().await;
    assert!(
        matches!(artifact_exit, BuiltinServerExit::Quit(_)),
        "pool 关闭必须关掉 artifact 的 client 半边（其 server task 随之自然收敛），实际: {artifact_exit:?}"
    );
    assert_eq!(
        pool.builtin_task_count(),
        0,
        "pool 关闭后不得残留 builtin server task（含重连新建的那条）"
    );
}
