//! `cron` builtin MCP handler 的 crate 内证据（owner C-02）。
//!
//! 覆盖口径（主 plan §3 IF-D14 / §6 C-02 行 / A32）：
//! - **声明面**：`list_tools` 的三条 `rmcp::model::Tool` 与既有 `CronRegisterTool` /
//!   `CronListTool` / `CronRemoveTool` 的 `BaseTool::definition()` 逐字相等（期望值由
//!   既有工具生成，本文件不硬编码第三份 schema）。
//! - **路由**：三条工具打到**同一** scheduler（经 `list_tasks` / `get_task` 与工具
//!   返回文本观察）；未知工具名 → `invalid_params`。
//! - **IF-D14 映射**：失败 → `Ok(Complete(error 结果))`，文本等于 `invoke_tool_call`
//!   的诊断文本（保留实际错误）；成功 → 工具返回文本。
//! - **A32（handler 不驱动 tick）**：可证伪的正/反控见
//!   `cron_server_maps_three_tools_without_tick`。
//! - **A32（代监督者持有 tick，handler 不持有）**：宿主 tick 驱动由
//!   `peri-middlewares` 管理；runtime 的关闭、重连和 join 状态证据留在宿主侧测试。
//! - **真实链路**：server 半边是生产 handler（`rmcp::serve_server`），client 半边是
//!   rmcp 原生 Auto lifecycle client；`tools/list` 与 `tools/call` 两个方向都经真实 wire——覆写 `discover` 会让
//!   同一连接上的 `list_all_tools` 被会话层拒绝，这正是本文件保留 wire 用例的理由。

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode},
    service::{Peer, QuitReason, RunningService},
    ClientLifecycleMode, RoleClient, ServerHandler,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::CronMcpServer;
use crate::{CronListTool, CronRegisterTool, CronRemoveTool, CronScheduler, CronTrigger};
use peri_mcp_common::{invoke_tool_call, list_tools_of};

/// 门禁用例的注册参数（`cron_register` 的必填两项）。
const REGISTER_EXPRESSION: &str = "*/5 * * * *";
/// 注册的 prompt：可辨认，用于断言它**不**出现在任何失败文本里。
const REGISTER_PROMPT: &str = "cron-gate-test-prompt";

/// 非法 cron 表达式（1 段，必然解析失败）：用于逼出 `CronError::InvalidExpression`。
const INVALID_EXPRESSION: &str = "not-a-cron-expression";
/// 用户 prompt 的可辨认形状串：失败文本里**不得**出现。
const LEAK_PROMPT: &str = "SENTINEL-PROMPT-must-not-leak";
/// 路径形状串：失败文本里**不得**出现（§9 规则 7）。
const LEAK_PATH: &str = "/tmp/secret-marker/private.html";

/// `assemble.rs` 的 `drive_cron_tick` 用 `interval(1s)` 驱动 `CronScheduler::tick`。
const TICK_PERIOD: Duration = Duration::from_secs(1);
/// A32 反控的观测窗口：> 2× tick 周期（handler 里若藏了 tick 驱动，2 个周期内必然到达）。
const NO_TICK_OBSERVATION: Duration = Duration::from_millis(2_500);

/// duplex 双向缓冲（与既有夹具一致）。
const DUPLEX_BUF: usize = 8 * 1024;
/// client 侧握手上界。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界（收尾共用）。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

// ─── 夹具 ─────────────────────────────────────────────────────────────────────

/// scheduler 夹具的观测端。
///
/// 两个 receiver 都必须在用例存活期内**保持存活**：
/// - `_primary` 是 `CronScheduler::new` 的主通道（TUI 轮询路径）；它若被 drop，`tick()`
///   会走「primary channel closed」的 warn 分支，测到的就不是生产形态；
/// - `extra` 来自 `subscribe()`，与生产 tick 驱动（`CronSchedulerPortHandle::subscribe`）
///   是同一条路径——A32 的正/反控只看这条通道。
struct Triggers {
    _primary: mpsc::UnboundedReceiver<CronTrigger>,
    extra: mpsc::UnboundedReceiver<CronTrigger>,
}

/// 空注册表的 scheduler 夹具。
fn scheduler_fixture() -> (Arc<Mutex<CronScheduler>>, Triggers) {
    let (primary_tx, primary_rx) = mpsc::unbounded_channel();
    let scheduler = Arc::new(Mutex::new(CronScheduler::new(primary_tx)));
    let extra = {
        let mut guard = scheduler.lock();
        guard.subscribe()
    };
    (
        scheduler,
        Triggers {
            _primary: primary_rx,
            extra,
        },
    )
}

/// `CronListTool` 只打印 id 的前 8 字符（`task.id.get(..8)`）——断言按同一口径取前缀。
fn id_prefix(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// `handler.call_tool` 的同一路径（`invoke_tool_call(server.tools(), "", request)`）。
async fn route(server: &CronMcpServer, name: &str, arguments: Value) -> CallToolResponse {
    invoke_tool_call(server.tools(), "", &call(name, arguments))
        .await
        .expect("工具存在时必须返回 Ok(CallToolResponse)")
}

/// 失败形态的替身工具：名字取 `cron_register`，因此 `invoke_tool_call` 为它生成的固定
/// 文本与真实工具的**逐字相同**（规则文本只由工具名决定）；错误原文里埋入「敏感形状」串，
/// 用于证明映射后原文不泄漏。
struct FailingStubTool;

#[async_trait]
impl BaseTool for FailingStubTool {
    fn name(&self) -> &str {
        "cron_register"
    }

    fn description(&self) -> &str {
        "crate 内替身工具（只为生成 IF-D14 的固定文本，不触网、不读 scheduler）"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Err(format!("File not found: {LEAK_PATH} (token=test-token-0000)").into())
    }
}

async fn connect<S: rmcp::ServerHandler>(server: S) -> Pair {
    let (client_io, server_io) = tokio::io::duplex(DUPLEX_BUF);
    let (read, write) = tokio::io::split(server_io);
    let server_task = tokio::spawn(async move {
        let running = rmcp::serve_server(server, (read, write))
            .await
            .expect("builtin server 装配失败");
        running.waiting().await
    });
    let service = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        rmcp::serve_client_with_lifecycle(
            (),
            tokio::io::split(client_io),
            ClientLifecycleMode::Auto {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
                legacy_version: None,
            },
        ),
    )
    .await
    .expect("client 侧握手超时")
    .expect("client 侧握手失败");
    Pair {
        service,
        server_task,
    }
}

struct Pair {
    service: RunningService<RoleClient, ()>,
    server_task: tokio::task::JoinHandle<Result<QuitReason, tokio::task::JoinError>>,
}

impl Pair {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        if tokio::time::timeout(CLOSE_TIMEOUT, &mut self.server_task)
            .await
            .is_err()
        {
            self.server_task.abort();
            let _ = self.server_task.await;
        }
    }
}

/// `tools/call` 请求（`arguments` 必须是 JSON object；缺省 = 空对象）。
fn call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

/// 结果里的首个文本块（相对错误文本不打印任何夹具常量）。
fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

/// 断言响应是 `Complete` 并取出结果（IF-D14 的失败形态**不是** `Err`）。
fn complete(response: CallToolResponse) -> CallToolResult {
    match response {
        CallToolResponse::Complete(result) => result,
        other => panic!("期望 Complete 结果，实际：{other:?}"),
    }
}

// ─── 声明面 ───────────────────────────────────────────────────────────────────

#[test]
fn cron_server_info_declares_tools_capability_and_instance_name() {
    let (scheduler, _triggers) = scheduler_fixture();
    let info = CronMcpServer::new(scheduler).get_info();
    assert!(
        info.capabilities.tools.is_some(),
        "必须声明 tools 能力，否则 tools/list 不可达"
    );
    assert_eq!(info.server_info.name, "peri-cron-mcp");
    assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn list_tools_uses_existing_cron_definitions() {
    let (scheduler, _triggers) = scheduler_fixture();
    let server = CronMcpServer::new(Arc::clone(&scheduler));

    // 期望值由**既有工具**的 `definition()` 生成（本文件不硬编码第三份 schema）。
    let expected: Vec<Arc<dyn BaseTool>> = vec![
        Arc::new(CronRegisterTool::new(Arc::clone(&scheduler))),
        Arc::new(CronListTool::new(Arc::clone(&scheduler))),
        Arc::new(CronRemoveTool::new(Arc::clone(&scheduler))),
    ];

    let listed = list_tools_of(server.tools());
    assert_eq!(
        listed.tools.len(),
        expected.len(),
        "handler 必须恰好声明三条工具"
    );
    for (mapped, tool) in listed.tools.iter().zip(expected.iter()) {
        let definition = tool.definition();
        assert_eq!(
            mapped.name.as_ref(),
            definition.name.as_str(),
            "name 必须逐字映射"
        );
        assert_eq!(
            mapped.description.as_deref(),
            Some(definition.description.as_str()),
            "{}: description 必须逐字映射",
            definition.name
        );
        assert_eq!(
            Value::Object(mapped.input_schema.as_ref().clone()),
            definition.parameters,
            "{}: input_schema 必须逐字等于既有工具的 parameters()",
            definition.name
        );
    }
}

// ─── 门禁：三条工具 + 同一 scheduler + 不驱动 tick（A32） ──────────────────────

#[tokio::test]
async fn cron_server_maps_three_tools_without_tick() {
    let (scheduler, mut triggers) = scheduler_fixture();
    let pair = connect(CronMcpServer::new(Arc::clone(&scheduler))).await;
    let peer = pair.peer();

    // ① 声明面：恰好三条，名字与顺序与注册表声明逐字一致。
    let listed = peer
        .list_all_tools()
        .await
        .expect("tools/list 必须成功（覆写 discover 会让它在会话层被拒）");
    let names: Vec<&str> = listed.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        vec!["cron_register", "cron_list", "cron_remove"],
        "handler 必须恰好声明三条工具，且顺序与注册表声明一致"
    );

    // ② 三条工具打到**同一** scheduler（写 → 读都由 scheduler 侧的观察点确认）。
    let registered = complete(
        peer.call_tool_once(call(
            "cron_register",
            json!({ "expression": REGISTER_EXPRESSION, "prompt": REGISTER_PROMPT }),
        ))
        .await
        .expect("注册成功必须是协议成功"),
    );
    assert_eq!(registered.is_error, Some(false));
    let register_text = first_text(&registered).expect("成功结果必须有文本块");
    let task_id = {
        let tasks: Vec<String> = scheduler
            .lock()
            .list_tasks()
            .iter()
            .map(|task| task.id.clone())
            .collect();
        assert_eq!(
            tasks.len(),
            1,
            "cron_register 必须写进 handler 持有的那份 scheduler"
        );
        tasks[0].clone()
    };
    assert!(
        register_text.contains(&task_id),
        "cron_register 的返回文本必须回传同一 scheduler 上的 id：{register_text}"
    );

    let list_text = first_text(&complete(
        peer.call_tool_once(call("cron_list", json!({})))
            .await
            .expect("cron_list 必须成功"),
    ))
    .expect("成功结果必须有文本块");
    assert!(
        list_text.contains(id_prefix(&task_id)),
        "cron_list 必须看到 cron_register 写下的任务：{list_text}"
    );

    // ③ 正控：显式 `tick()` 必须能从观测通道收到一条 `CronTrigger`——证明「已到期」前提
    //    与观测通道都有效，否则 ④ 反控什么都没测。
    assert!(
        scheduler.lock().force_next_fire_to_past(&task_id),
        "正控前置：任务必须存在"
    );
    scheduler.lock().tick();
    let fired = triggers
        .extra
        .try_recv()
        .expect("正控：显式 tick 必须产出一条 CronTrigger（否则反控为空转）");
    assert_eq!(fired.task_id, task_id);
    assert_eq!(fired.prompt, REGISTER_PROMPT);
    assert!(
        triggers.extra.try_recv().is_err(),
        "正控后观测通道必须已排空（反控的「无 trigger」才有意义）"
    );

    // ④ 反控（A32，可证伪）：任务**再次**置为已到期，但不调用 `tick()`，等 > 2× tick
    //    周期 → 不得再出现任何 trigger。反控的观测通道有效性由 ③ 正控保证：同一 receiver、
    //    同一 CronTrigger 类型、同一「next_fire 已过期且任务 enabled」前置，唯一变量是
    //    「有没有人调 tick」。若 `CronMcpServer::new` 偷偷 spawn 了 tick 驱动，这里必然收到。
    assert!(
        NO_TICK_OBSERVATION > 2 * TICK_PERIOD,
        "反控窗口必须 > 2× tick 周期，否则本用例的结论不成立"
    );
    assert!(
        scheduler.lock().force_next_fire_to_past(&task_id),
        "反控前置：任务必须仍然存在（此时尚未 remove）"
    );
    tokio::time::sleep(NO_TICK_OBSERVATION).await;
    assert!(
        triggers.extra.try_recv().is_err(),
        "handler 内不得有 tick 驱动：到期任务在无显式 tick 时不得自行触发"
    );

    // ⑤ `cron_remove` 用其 id 删除 → `cron_list` 不再包含它（删除同样落在同一 scheduler 上）。
    let removed = complete(
        peer.call_tool_once(call("cron_remove", json!({ "id": task_id.clone() })))
            .await
            .expect("删除已存在的任务必须成功"),
    );
    assert_eq!(removed.is_error, Some(false));
    assert!(
        scheduler.lock().get_task(&task_id).is_none(),
        "cron_remove 必须删掉 handler 持有的那份 scheduler 上的任务"
    );
    let after_remove = first_text(&complete(
        peer.call_tool_once(call("cron_list", json!({})))
            .await
            .expect("cron_list 必须成功"),
    ))
    .expect("成功结果必须有文本块");
    assert!(
        !after_remove.contains(id_prefix(&task_id)),
        "删除后 cron_list 不得再包含该任务：{after_remove}"
    );

    pair.shutdown().await;
}

// ─── 路由 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn call_tool_routes_each_name_to_shared_base_tool() {
    let (scheduler, _triggers) = scheduler_fixture();
    let server = CronMcpServer::new(Arc::clone(&scheduler));

    // register → 写进 handler 持有的那份 scheduler
    let registered = complete(
        route(
            &server,
            "cron_register",
            json!({ "expression": REGISTER_EXPRESSION, "prompt": REGISTER_PROMPT }),
        )
        .await,
    );
    assert_eq!(registered.is_error, Some(false));
    let register_text = first_text(&registered).expect("成功结果必须有文本块");
    let task_id = {
        let tasks: Vec<String> = scheduler
            .lock()
            .list_tasks()
            .iter()
            .map(|task| task.id.clone())
            .collect();
        assert_eq!(tasks.len(), 1, "cron_register 必须写进同一 scheduler");
        tasks[0].clone()
    };
    assert!(
        register_text.contains(&task_id),
        "返回文本里的 id 必须就是 scheduler 上的 id：{register_text}"
    );
    let written = scheduler
        .lock()
        .get_task(&task_id)
        .map(|task| (task.expression.clone(), task.prompt.clone()));
    assert_eq!(
        written,
        Some((REGISTER_EXPRESSION.to_string(), REGISTER_PROMPT.to_string())),
        "写入的字段必须与调用参数一致"
    );

    // list → 读同一 scheduler
    let listed = first_text(&complete(route(&server, "cron_list", json!({})).await))
        .expect("成功结果必须有文本块");
    assert!(
        listed.contains(id_prefix(&task_id)),
        "cron_list 必须读到同一 scheduler 上的任务：{listed}"
    );

    // remove → 删同一 scheduler
    let removed = complete(route(&server, "cron_remove", json!({ "id": task_id.clone() })).await);
    assert_eq!(removed.is_error, Some(false));
    assert!(
        scheduler.lock().get_task(&task_id).is_none(),
        "cron_remove 必须删掉同一 scheduler 上的任务"
    );

    // 未知工具名 → 协议错误（IF-D14 第一条分支）
    let error = invoke_tool_call(server.tools(), "", &call("cron_pause", json!({})))
        .await
        .expect_err("未知工具名必须是协议错误（invalid_params）");
    assert_eq!(error.code.0, ErrorCode::INVALID_PARAMS.0);
    assert!(
        error.message.contains("unknown tool: cron_pause"),
        "错误文本应含工具名；实际：{}",
        error.message
    );
}

// ─── IF-D14 结果映射 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn call_tool_uses_if_d14_error_mapping() {
    let (scheduler, _triggers) = scheduler_fixture();
    let server = CronMcpServer::new(Arc::clone(&scheduler));

    let stub: Vec<Arc<dyn BaseTool>> = vec![Arc::new(FailingStubTool)];
    let reference = complete(
        invoke_tool_call(&stub, "", &call("cron_register", json!({})))
            .await
            .expect("工具级失败必须是 Ok(Complete(error 结果))，不是 Err"),
    );
    assert_eq!(reference.is_error, Some(true));
    let expected = first_text(&reference).expect("错误结果必须有文本块");
    assert!(
        expected.contains(LEAK_PATH),
        "错误文本必须保留替身工具的实际诊断：{expected}"
    );

    let missing = complete(
        invoke_tool_call(
            server.tools(),
            "",
            &call("cron_register", json!({ "prompt": LEAK_PROMPT })),
        )
        .await
        .expect("业务失败仍走协议成功（is_error 承载语义）"),
    );
    assert_eq!(missing.is_error, Some(true));
    let missing_text = first_text(&missing).expect("错误结果必须有文本块");
    assert!(missing_text.contains("missing expression field"));
    assert!(
        missing_text.contains("cron_register"),
        "规则文本只带工具名：{missing_text}"
    );
    assert!(
        !missing_text.contains(LEAK_PROMPT),
        "不得泄漏用户 prompt：{missing_text}"
    );

    let invalid = complete(
        invoke_tool_call(
            server.tools(),
            "",
            &call(
                "cron_register",
                json!({ "expression": INVALID_EXPRESSION, "prompt": LEAK_PROMPT }),
            ),
        )
        .await
        .expect("业务失败仍走协议成功"),
    );
    assert_eq!(invalid.is_error, Some(true));
    let invalid_text = first_text(&invalid).expect("错误结果必须有文本块");
    assert!(
        invalid_text.contains("cron 表达式无效"),
        "必须保留 CronError 原文：{invalid_text}"
    );
    assert!(
        invalid_text.contains("Invalid pattern: Pattern must have between 5 and 7 fields"),
        "必须保留解析器实际诊断：{invalid_text}"
    );
    assert!(
        !invalid_text.contains(LEAK_PROMPT),
        "不得泄漏用户 prompt：{invalid_text}"
    );

    let not_found = complete(
        invoke_tool_call(
            server.tools(),
            "",
            &call("cron_remove", json!({ "id": LEAK_PATH })),
        )
        .await
        .expect("业务失败仍走协议成功"),
    );
    assert_eq!(not_found.is_error, Some(true));
    let not_found_text = first_text(&not_found).expect("错误结果必须有文本块");
    assert!(
        not_found_text.contains(LEAK_PATH),
        "必须保留路径形状的 id：{not_found_text}"
    );
    assert!(
        not_found_text.contains("not found"),
        "必须保留工具原文：{not_found_text}"
    );

    // ④ 成功形态：content 文本 = 工具自身的返回文本（同一 scheduler、同一实现）
    let tool_text = CronListTool::new(Arc::clone(&scheduler))
        .invoke(json!({}), ToolContext::new(&[], ""))
        .await
        .expect("空注册表的 cron_list 必须成功");
    let success = complete(route(&server, "cron_list", json!({})).await);
    assert_eq!(success.is_error, Some(false));
    assert_eq!(
        first_text(&success).as_deref(),
        Some(tool_text.as_str()),
        "成功文本必须逐字等于工具返回文本"
    );
}

// ─── Handler 不驱动 tick，工具面仍可用 ─────────────────────────────────────────

/// handler 不创建 tick 驱动；工具读写仍共用 scheduler。
#[tokio::test]
async fn tick_disabled_has_no_driver_but_tools_work() {
    let (scheduler, mut triggers) = scheduler_fixture();
    // 不 spawn 任何 TickGuard：这一代没有 tick 驱动。
    let server = CronMcpServer::new(Arc::clone(&scheduler));

    // ① 声明面照常：三条工具。
    let listed = list_tools_of(server.tools());
    assert_eq!(
        listed.tools.len(),
        3,
        "tick 关闭不影响 tools/list 的三条声明"
    );

    // ② `cron_register` 走真实映射路径（register → list → remove 至少一遍）。
    let registered = complete(
        route(
            &server,
            "cron_register",
            json!({ "expression": REGISTER_EXPRESSION, "prompt": REGISTER_PROMPT }),
        )
        .await,
    );
    assert_eq!(registered.is_error, Some(false));
    let task_id = scheduler
        .lock()
        .list_tasks()
        .first()
        .map(|task| task.id.clone())
        .expect("cron_register 必须写进同一 scheduler");

    // ③ 反控：到期任务在 > 2× tick 周期内不得自行触发（这一代没有任何驱动）。
    assert!(
        scheduler.lock().force_next_fire_to_past(&task_id),
        "反控前置：任务必须存在"
    );
    assert!(
        NO_TICK_OBSERVATION > 2 * TICK_PERIOD,
        "反控窗口必须 > 2× tick 周期，否则本用例的结论不成立"
    );
    tokio::time::sleep(NO_TICK_OBSERVATION).await;
    assert!(
        triggers.extra.try_recv().is_err(),
        "tick_enabled=false ⇒ 不得有任何驱动：到期任务不得自行触发"
    );

    // ④ 正控（证明 ③ 的窗口观测有效，否则 ③ 空转）：显式 tick 必须能收到 trigger。
    scheduler.lock().tick();
    let fired = triggers
        .extra
        .try_recv()
        .expect("正控：显式 tick 必须产出一条 CronTrigger（否则 ③ 为空转）");
    assert_eq!(fired.task_id, task_id);
    assert_eq!(fired.prompt, REGISTER_PROMPT);
    assert!(
        triggers.extra.try_recv().is_err(),
        "正控后观测通道必须已排空（③ 的「无 trigger」才有意义）"
    );

    // ⑤ 读 / 删仍落在同一 scheduler。
    let list_text = first_text(&complete(route(&server, "cron_list", json!({})).await))
        .expect("成功结果必须有文本块");
    assert!(
        list_text.contains(id_prefix(&task_id)),
        "cron_list 必须看到 cron_register 写下的任务：{list_text}"
    );
    let removed = complete(route(&server, "cron_remove", json!({ "id": task_id.clone() })).await);
    assert_eq!(removed.is_error, Some(false));
    assert!(
        scheduler.lock().get_task(&task_id).is_none(),
        "cron_remove 必须删掉同一 scheduler 上的任务"
    );
}
