use super::WorkflowInstanceInput;
use super::{
    builtin_mcp, client, invocation_fixture, json, mpsc, runtime, tool_bridge, Arc,
    BuiltinInstanceContext, BuiltinInstanceSupervisor, BuiltinServerExit, BuiltinServerHandler,
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientStatus, ConcreteTaskManager,
    CronInstanceInput, CronScheduler, CronTrigger, Duration, McpCapabilityProfile, McpClientHandle,
    McpClientPool, McpServiceWrapper, McpToolBridge, Mutex, OAuthStatus, Peer, RoleClient,
    TaskManager, TickCloseOutcome, ToolCallError, Value, BUILTIN_CONVERGE_TIMEOUT,
};
use peri_agent::tools::BaseTool as _;

// ══════════════════════════════════════════════════════════════════════════════
// V 矩阵第 20 行：注册表 → 工厂映射；builtin 身份经 discover / status 传播
// ══════════════════════════════════════════════════════════════════════════════

/// wave 2 的**被验实例**（本文件三处断言的被验对象）。
///
/// 只固定实例身份；工具名、键、数量、effective name 一律读注册表（`find` / `tools`），
/// 不出现第二份清单——注册表增删工具时本文件的循环自动跟随。
pub(super) const WAVE2_INSTANCES: [&str; 1] = ["cron"];

// ══════════════════════════════════════════════════════════════════════════════
// V 矩阵第 22/23 行：IF-D14 结果映射（真 handler + effective 桥 + 固定脱敏文本）
// ══════════════════════════════════════════════════════════════════════════════

/// 非法 cron 表达式（1 段，解析必然失败）：逼出 `CronError::InvalidExpression`。
pub(super) const INVALID_EXPRESSION: &str = "not-a-cron-expression";

/// 用户 prompt 的可辨认形状串：任何失败文本里**不得**出现。
pub(super) const LEAK_PROMPT: &str = "SENTINEL-PROMPT-must-not-leak";

/// `serve_client_auto` 侧握手上界（同进程链路）。
pub(super) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// client 侧关闭上界（收尾共用）。
pub(super) const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// 交叉面夹具：一份齐备的实例上下文（cron 真实 scheduler + 未绑定的 workflow 槽位）+
/// 观察端。
///
/// `web` / `artifact` 不需要额外输入（artifact 的解析根是 `ctx.cwd`）；`workflow` 的输入
/// 是会话装配期的空槽（此处不绑定运行时——本夹具只覆盖分派身份，不驱动编排），因此同一份
/// 上下文可喂给五个已实现实例的工厂。
pub(super) struct CrossFixture {
    pub(super) ctx: BuiltinInstanceContext,
    /// cron 触发通道接收端（保持存活，使发送端始终有对端；本文件不驱动 tick）。
    pub(super) _cron_triggers: mpsc::UnboundedReceiver<CronTrigger>,
    /// 夹具临时目录（仅靠 `Drop` 持有生命周期）。
    pub(super) _dir: tempfile::TempDir,
}

impl CrossFixture {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let (trigger_tx, cron_triggers) = mpsc::unbounded_channel();
        let scheduler = Arc::new(Mutex::new(CronScheduler::new(trigger_tx)));
        let cwd = dir.path().to_string_lossy().to_string();
        let ctx = BuiltinInstanceContext::new(cwd).with_cron(CronInstanceInput {
            scheduler: Arc::clone(&scheduler),
            tick_enabled: false,
        });
        // workflow 的 session 级槽位：分派身份需要它存在（运行时由会话旧链在
        // session/new 绑定；本夹具不驱动编排，槽位保持未绑定）。
        let ctx = ctx.with_workflow(WorkflowInstanceInput::slot());
        Self {
            ctx,
            _cron_triggers: cron_triggers,
            _dir: dir,
        }
    }
}

/// 一条已握手的 builtin 链路：client service + 本代关闭所有权。
pub(super) struct Pair {
    pub(super) service: McpServiceWrapper,
    pub(super) supervisor: BuiltinInstanceSupervisor,
}

impl Pair {
    pub(super) fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    /// 夹具收尾：释放 client（server 侧读到 EOF）→ 本代监督者按冻结顺序有界收敛。
    pub(super) async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        let outcome = self.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
        assert!(
            matches!(outcome.tick, TickCloseOutcome::NotSpawned),
            "夹具装配恒不挂 tick（A32：tick 归 pool 的唯一 spawn 点）"
        );
        assert!(
            matches!(outcome.server, BuiltinServerExit::Quit(_)),
            "server task 必须在有界等待内正常收敛，实际：{:?}",
            outcome.server
        );
    }
}

/// 经 **dispatch 工厂**取 handler（`None` = 未接线 / 输入缺失 ⇒ 直接断言失败），装配真实
/// 同进程链路并由生产 `serve_client_auto` 握手——实例名与注册表一致。
pub(super) async fn connect_via_dispatch(instance: &str, ctx: &BuiltinInstanceContext) -> Pair {
    let handler = super::super::builtin_server_handler(instance, ctx)
        .unwrap_or_else(|| panic!("{instance}: 工厂必须给出 handler（本夹具输入齐备）"));
    connect_handler(instance, handler).await
}

/// 以**调用方给出的** handler 装配真实同进程链路并由生产 `serve_client_auto` 握手。
///
/// W1 资源面证据用本入口绕过工厂（那时夹具直接把 handler 交给链路）；W4a 起生产工厂 arm
/// 已按 `ctx.workspace_resources` 装载资源面，装资源输入的用例走 [`connect_via_dispatch`]。
pub(super) async fn connect_handler(instance: &str, handler: BuiltinServerHandler) -> Pair {
    let transport = runtime::spawn_builtin_transport_with_handler(instance, handler);
    let (io, supervisor) = transport.into_parts();
    let service =
        client::serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
            .await
            .expect("builtin 握手不得超时（同进程链路）")
            .expect("builtin 握手不得失败");
    Pair {
        service,
        supervisor,
    }
}

/// `tools/call` 请求（`arguments` 必须是 JSON object；缺省 = 空对象）。
pub(super) fn call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

/// 结果里的首个文本块。
pub(super) fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

/// 断言响应是 `Complete` 并取出结果（IF-D14 的失败形态**不是** `Err`）。
pub(super) fn complete(response: CallToolResponse) -> CallToolResult {
    match response {
        CallToolResponse::Complete(result) => result,
        other => panic!("期望 Complete 结果，实际：{other:?}"),
    }
}

/// V 矩阵第 22 行（**宿主面**，原具名用例 `call_tool_uses_shared_result_mapping`）：cron
/// 实例经 dispatch 工厂 + 真实 wire 的**烟测**（成功 / 业务失败两形态的协议外观），
/// 以及 effective 名字（模型面）经生产 `McpToolBridge` 打回同一条真实链路的**桥接一致性**
/// （成功文本 == 线路文本；失败转 `ApplicationFailed`，其封装的 reason == 线路失败文本）。
///
/// 映射**规则本身**（固定脱敏文本、未知工具 → `invalid_params`、业务细节不入模型面）由各
/// package 的 `server_test.rs` 逐实例覆盖，本文件不重复断言规则字面量：宿主已无映射实现
/// （dispatch 的 `call_tool` 把请求原样转给 package handler），规则细节在这里没有第二个
/// 可失败面。
#[tokio::test]
pub(super) async fn call_tool_smoke_and_effective_bridge_round_trip() {
    let fixture = CrossFixture::new();
    let cases = [Wave2CallCase {
        instance: "cron",
        success_tool: "cron_list",
        success_input: json!({}),
        fail_tool: "cron_register",
        fail_input: json!({ "expression": INVALID_EXPRESSION, "prompt": LEAK_PROMPT }),
    }];

    for case in cases {
        // 注册表前置：两个工具名必须是本实例声明的原始名（名字不凭空写）。
        let declared = builtin_mcp::find(case.instance)
            .unwrap_or_else(|| panic!("{}: 实例必须在注册表内", case.instance));
        for name in [case.success_tool, case.fail_tool] {
            assert!(
                declared.tools.iter().any(|tool| tool.original_name == name),
                "{}/{name}: 必须是注册表声明的原始工具名",
                case.instance
            );
        }
        let effective_of = |name: &str| {
            declared
                .tools
                .iter()
                .find(|tool| tool.original_name == name)
                .unwrap_or_else(|| panic!("{}: 注册表必须声明 {name}", case.instance))
                .legacy_name
        };

        // 真实链路：dispatch 工厂 → 真实 transport → 生产 client 握手。
        let pair = connect_via_dispatch(case.instance, &fixture.ctx).await;
        let peer = pair.peer();

        // ① 成功形态：`Complete(success 结果)`，文本非空。
        let success = complete(
            peer.call_tool_once(call(case.success_tool, case.success_input.clone()))
                .await
                .expect("成功路径必须是协议成功"),
        );
        assert_eq!(
            success.is_error,
            Some(false),
            "{}: 成功形态必须是 success 结果",
            case.instance
        );
        let success_text = first_text(&success).expect("成功结果必须有文本块");
        assert!(
            !success_text.is_empty(),
            "{}: 成功文本不得为空",
            case.instance
        );

        // ② 业务 Err 形态 smoke：`Complete(error 结果)` + 非空文本（规则细节见用例文档）。
        let failure = complete(
            peer.call_tool_once(call(case.fail_tool, case.fail_input.clone()))
                .await
                .expect("业务失败仍走协议成功（is_error 承载语义）"),
        );
        assert_eq!(
            failure.is_error,
            Some(true),
            "{}: 业务失败必须是 error 结果，不是 Err",
            case.instance
        );
        let failure_text = first_text(&failure).expect("error 结果必须有文本块");
        assert!(
            !failure_text.is_empty(),
            "{}: 失败文本不得为空",
            case.instance
        );

        // ③ effective 调用桥接：模型面名字（注册表冻结的 effective name）经生产
        //    `build_deferred_tool_bridges` 打回**同一条**真实链路的 handler。
        let pool = bridge_pool(case.instance, &peer).await;
        let bridges = tool_bridge::build_deferred_tool_bridges(&pool);
        let invocation_fixture = invocation_fixture::InvocationFixture::new_calls(&[
            (effective_of(case.success_tool), case.success_input.clone()),
            (effective_of(case.fail_tool), case.fail_input.clone()),
        ])
        .await;
        let success_bridge = bridge_of(&bridges, effective_of(case.success_tool));
        let bridged_success = success_bridge
            .invoke(case.success_input.clone(), invocation_fixture.context(0))
            .await
            .unwrap_or_else(|error| {
                panic!("{}: effective 桥的成功路径不得失败：{error}", case.instance)
            });
        assert_eq!(
            bridged_success, success_text,
            "{}: effective 桥的成功文本必须与 handler 路径逐字一致",
            case.instance
        );

        let failure_bridge = bridge_of(&bridges, effective_of(case.fail_tool));
        let bridged_failure = failure_bridge
            .invoke_output(case.fail_input.clone(), invocation_fixture.context(1))
            .await
            .expect("工具级失败是线路 error 结果：必须是正常返回值，不是协议错误");
        assert_eq!(
            bridged_failure.is_error(),
            Some(true),
            "{}: 线路 error 结果必须作为 is_error 事实浮出",
            case.instance
        );
        assert!(
            bridged_failure
                .raw()
                .is_some_and(|raw| raw.is_error() && raw.content()[0]["text"] == failure_text),
            "{}: raw 结果必须保真承载线路 error 文本",
            case.instance
        );
        assert_eq!(
            bridged_failure.text,
            ToolCallError::CallFailed {
                server: case.instance.into(),
                tool: case.fail_tool.into(),
                reason: failure_text.into(),
            }
            .to_string(),
            "{}: 桥接失败原因必须与线路失败文本逐字一致",
            case.instance
        );

        println!(
            "[W2 dispatch/source] instance={} success=1 error=1 bridged=2",
            case.instance
        );
        pair.shutdown().await;
    }
}

/// 一个 wave 2 实例的烟测用例输入（成功 / 业务失败两形态，各配桥接对照；工具名必须能在注册表里查到）。
pub(super) struct Wave2CallCase {
    pub(super) instance: &'static str,
    pub(super) success_tool: &'static str,
    pub(super) success_input: Value,
    pub(super) fail_tool: &'static str,
    pub(super) fail_input: Value,
}

/// 把一条**已握手**的真实 builtin 链路登记成 pool 里的 Connected 句柄（形状同生产
/// `run_initialize` 的提交点：`peer` / `tools` 取自真实往返），再经生产
/// `build_deferred_tool_bridges` 取 effective name 的 `McpToolBridge`。
///
/// 句柄 `name` 必须是实例名：bridge 的 effective name 由 `(server_name, 原始工具名)` 计算
/// （`McpToolBridge::new` 用 `client.name`），因此这一步同时验证注册表冻结字面量与生产计算
/// 一致。
pub(super) async fn bridge_pool(instance: &str, peer: &Peer<RoleClient>) -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_empty());
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
    pool.bind_session_task_manager(invocation_fixture::FIXTURE_SESSION_ID, &manager);
    let tools = peer
        .list_all_tools()
        .await
        .expect("真实 tools/list 必须成功");
    pool.clients.write().insert(
        instance.to_string(),
        Arc::new(McpClientHandle {
            call_gate: Default::default(),
            name: instance.to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: Some(peer.clone()),
            tools,
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
    pool
}

/// 按注册表冻结的 effective name 取真实桥（缺桥 ⇒ 断言失败）。
pub(super) fn bridge_of<'a>(
    bridges: &'a [McpToolBridge],
    effective_name: &str,
) -> &'a McpToolBridge {
    bridges
        .iter()
        .find(|bridge| bridge.name() == effective_name)
        .unwrap_or_else(|| {
            panic!(
                "{effective_name}: 真实桥必须存在（tools/list 快照 + 注册表声明）；实际：{:?}",
                bridges
                    .iter()
                    .map(|bridge| bridge.name())
                    .collect::<Vec<_>>()
            )
        })
}
