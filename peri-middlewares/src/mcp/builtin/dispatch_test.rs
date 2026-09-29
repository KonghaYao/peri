//! `dispatch` 层（实例名 → handler）的 crate 内证据（owner H-01，接线扩展 H-05）。
//!
//! 覆盖口径（主 plan §6 H-01/H-05 行的闸门用例）：工厂覆盖的实例是**精确**集合——注册表里
//! 每个已实现实例各得自己的变体（保留名表与已实现实例表在 W3-B 后逐项一致，**未接线集合为空**），
//! 表外的名字一律 `None`：由 `runtime` 按 `BuiltinSpawnError::HandlerNotWired` 收口，**不得**
//! 静默回退到任一已实现实例。
//!
//! `workspace` 是唯一的例外形状（AW3-11）：它的工厂 arm **无条件**构造——缺 session 级输入
//! 时仍返回 `Workspace` 变体（可见但退化），因此 `None` 在这一支上不代表「未接线」。
//!
//! 工厂选择与输入装配测试位于 `dispatch_factory_tests`；这里保留跨 handler 的 wire 行为。
//! 两条用例的分工：`dispatch_factory_covers_implemented_instances_only` 从注册表派生断言
//! 「未接线集合为空 + 表外名字恒 `None`」；`dispatch_covers_cron_and_lsp_variants` 用带实例
//! 输入的上下文断言各实例拿到**自己的**变体（含「注入的同一份状态对象进了 handler」）以及
//! `workspace` 在**无**输入时的可见但退化。
//!
//! V 矩阵（v4-part-3 sub-plan V §8）第 20/22/23 行的具名用例在同文件末尾：
//! - [`all_registered_instances_have_handler`]：注册表 → 工厂的一一映射（遍历注册表派生）；
//! - [`builtin_source_propagates`]：cron / lsp 的 `ConfigSource::Builtin` 经 overlay →
//!   建传输（三分类）→ status 快照 → `DiscoverMCP` 只读投影保留，transport 分类 = builtin；
//! - [`call_tool_uses_shared_result_mapping`]：真 handler 经 effective 桥的成功 / 业务 Err /
//!   未知工具三形态都走 `peri_mcp_common::invoke_tool_call` 唯一共享助手；
//! - [`call_tool_error_text_is_fixed_and_redacted`]：cron 两类 / LSP 六类业务错误经 handler
//!   路径的文本全部等于固定脱敏文本（`LspToolError::NotReady` 的可触发性与现场判定见该
//!   用例文档，属 UNVERIFIED，不写 flaky 断言）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::builtin_mcp::{
    find, is_reserved_instance_name, BUILTIN_MCP_INSTANCES, BUILTIN_RESERVED_INSTANCE_NAMES,
};
use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use peri_acp_types::tasks::{BgTaskKind, TaskManager};
use peri_agent::agent::async_tasks::TaskManager as ConcreteTaskManager;
use peri_agent::tools::{BaseTool, ToolContext};
use peri_mcp_lsp::config::{LspConfigFile, LspServerConfig};
use peri_mcp_lsp::pool::LspServerPool;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientRequest, CustomRequest,
        ErrorCode, ServerResult,
    },
    service::{Peer, RoleClient, ServiceError},
    ServerHandler,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::{builtin_server_handler, BuiltinServerHandler};
use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::context::{BuiltinInstanceContext, CronInstanceInput, LspInstanceInput};
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BuiltinServerExit,
    TickCloseOutcome, BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::builtin::{apply_builtin_overlay, BuiltinInjectionPolicy};
use crate::mcp::client::{
    serve_client_auto, ClientStatus, McpClientHandle, McpClientPool, McpServiceWrapper, OAuthStatus,
};
use crate::mcp::discover_tool::DiscoverMCPTool;
use crate::mcp::tool_bridge::{build_deferred_tool_bridges, McpToolBridge};
use crate::mcp::transport::{require_known_builtin_instance, TransportConfig, TransportKind};
use crate::mcp::ToolCallError;
use peri_mcp_common::invoke_tool_call;
use peri_mcp_cron::{CronScheduler, CronTrigger, MAX_CRON_TASKS};
use peri_mcp_lsp::LspTool;
use peri_mcp_workspace::{
    ResourceRoot, ResourceScope, WorkspaceInstanceInput, WorkspaceMcpServer,
    WorkspaceResourcesInput,
};

// ══════════════════════════════════════════════════════════════════════════════
// V 矩阵第 20 行：注册表 → 工厂映射；builtin 身份经 discover / status 传播
// ══════════════════════════════════════════════════════════════════════════════

/// wave 2 的**被验实例**（本文件三处断言的被验对象）。
///
/// 只固定实例身份；工具名、键、数量、effective name 一律读注册表（`find` / `tools`），
/// 不出现第二份清单——注册表增删工具时本文件的循环自动跟随。
const WAVE2_INSTANCES: [&str; 2] = ["cron", "lsp"];

// ══════════════════════════════════════════════════════════════════════════════
// V 矩阵第 22/23 行：IF-D14 结果映射（真 handler + effective 桥 + 固定脱敏文本）
// ══════════════════════════════════════════════════════════════════════════════

/// 非法 cron 表达式（1 段，解析必然失败）：逼出 `CronError::InvalidExpression`。
const INVALID_EXPRESSION: &str = "not-a-cron-expression";
/// 合法 cron 表达式（填充任务上限用）。
const VALID_EXPRESSION: &str = "*/5 * * * *";
/// 用户 prompt 的可辨认形状串：任何失败文本里**不得**出现。
const LEAK_PROMPT: &str = "SENTINEL-PROMPT-must-not-leak";
/// 路径形状串（§9 规则 7：路径不入模型面文本）。
const LEAK_PATH_MARKER: &str = "secret-marker";
/// 无路由扩展名（不得回传给模型）。
const LEAK_EXTENSION: &str = "zzz";

/// `serve_client_auto` 侧握手上界（同进程链路）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界（收尾共用）。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// perl 编写的极简 LSP 服务器（与 `lsp_test.rs` 同源，按本文件需要重抄一份：那个常量是
/// 对方文件的私有项，无跨 owner 复用面）：
/// - 每次 spawn 向 `$PERI_LSP_TEST_COUNT` 追加一行（本文件据此证明「服务器真的起来了」）；
/// - 对任何带 id 的请求回 `{"result":null}`（满足 initialize 握手；也用来逼出
///   `LspToolError::RequestFailed`——`incomingCalls` 要把 `null` 反序列化成调用层级数组）。
const FAKE_LSP_SCRIPT: &str = r#"open my $c, '>>', $ENV{PERI_LSP_TEST_COUNT} or exit 1;
print $c "spawned\n";
close $c;
binmode STDIN;
select STDOUT;
$| = 1;
while (1) {
    my $h = '';
    while (1) {
        my $l = <STDIN>;
        last unless defined $l;
        last if $l =~ /^\r?\n$/;
        $h .= $l;
    }
    my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
    last unless defined $len;
    my $b = '';
    read(STDIN, $b, $len) == $len or last;
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}"#;

/// 交叉面夹具：一份齐备的实例上下文（cron 真实 scheduler + lsp 真实惰性 pool）+ 观察端。
///
/// `web` / `artifact` 不需要额外输入（artifact 的解析根是 `ctx.cwd`），因此同一份上下文可
/// 喂给四个已实现实例的工厂。lsp pool 路由 `.rs` → perl fake server；`LspServerPool::new`
/// 惰性，不调用 LSP 工具时**不拉任何进程**。
struct CrossFixture {
    ctx: BuiltinInstanceContext,
    scheduler: Arc<Mutex<CronScheduler>>,
    pool: Arc<LspServerPool>,
    /// cron 触发通道接收端（保持存活，使发送端始终有对端；本文件不驱动 tick）。
    _cron_triggers: mpsc::UnboundedReceiver<CronTrigger>,
    dir: tempfile::TempDir,
}

impl CrossFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let (trigger_tx, cron_triggers) = mpsc::unbounded_channel();
        let scheduler = Arc::new(Mutex::new(CronScheduler::new(trigger_tx)));
        let cwd = dir.path().to_string_lossy().to_string();
        let mut env = HashMap::new();
        env.insert(
            "PERI_LSP_TEST_COUNT".to_string(),
            dir.path()
                .join("spawn_count.txt")
                .to_string_lossy()
                .into_owned(),
        );
        let pool = Arc::new(LspServerPool::new(
            &cwd,
            LspConfigFile {
                lsp_servers: HashMap::from([(
                    "fixture-lsp".to_string(),
                    LspServerConfig {
                        name: "fixture-lsp".to_string(),
                        command: "perl".to_string(),
                        args: vec!["-e".to_string(), FAKE_LSP_SCRIPT.to_string()],
                        env: Some(env),
                        extension_to_language: HashMap::from([(
                            ".rs".to_string(),
                            "rust".to_string(),
                        )]),
                        initialization_options: None,
                        disabled: None,
                        max_restarts: Some(3),
                        startup_timeout: None,
                        source: None,
                    },
                )]),
            },
        ));
        let ctx = BuiltinInstanceContext::new(cwd)
            .with_cron(CronInstanceInput {
                scheduler: Arc::clone(&scheduler),
                tick_enabled: false,
            })
            .with_lsp(LspInstanceInput {
                pool: Arc::clone(&pool),
            });
        Self {
            ctx,
            scheduler,
            pool,
            _cron_triggers: cron_triggers,
            dir,
        }
    }

    fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// 夹具目录里写一个真实文件，返回绝对路径（LSP 成功路径 / 无路由扩展名路径用）。
    fn write_file(&self, rel: &str, body: &str) -> String {
        let path = self.dir().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("夹具目录可创建");
        }
        std::fs::write(&path, body).expect("夹具文件可写");
        path.to_string_lossy().to_string()
    }
}

/// 一条已握手的 builtin 链路：client service + 本代关闭所有权。
struct Pair {
    service: McpServiceWrapper,
    supervisor: BuiltinInstanceSupervisor,
}

impl Pair {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    /// 夹具收尾：释放 client（server 侧读到 EOF）→ 本代监督者按冻结顺序有界收敛。
    async fn shutdown(mut self) {
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
async fn connect_via_dispatch(instance: &str, ctx: &BuiltinInstanceContext) -> Pair {
    let handler = builtin_server_handler(instance, ctx)
        .unwrap_or_else(|| panic!("{instance}: 工厂必须给出 handler（本夹具输入齐备）"));
    connect_handler(instance, handler).await
}

/// 以**调用方给出的** handler 装配真实同进程链路并由生产 `serve_client_auto` 握手。
///
/// W1 资源面证据用本入口绕过工厂（那时夹具直接把 handler 交给链路）；W4a 起生产工厂 arm
/// 已按 `ctx.workspace_resources` 装载资源面，装资源输入的用例走 [`connect_via_dispatch`]。
async fn connect_handler(instance: &str, handler: BuiltinServerHandler) -> Pair {
    let transport = spawn_builtin_transport_with_handler(instance, handler);
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(
        io,
        None,
        None,
        &McpCapabilityProfile::disabled(),
        HANDSHAKE_TIMEOUT,
    )
    .await
    .expect("builtin 握手不得超时（同进程链路）")
    .expect("builtin 握手不得失败");
    Pair {
        service,
        supervisor,
    }
}

/// `tools/call` 请求（`arguments` 必须是 JSON object；缺省 = 空对象）。
fn call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

/// 结果里的首个文本块。
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

/// handler 路径的**业务失败**文本：协议必须成功（`is_error` 承载语义），且必须有文本块。
async fn call_error_text(pair: &Pair, tool: &str, input: Value) -> String {
    let response = pair
        .peer()
        .call_tool_once(call(tool, input))
        .await
        .unwrap_or_else(|error| panic!("{tool}: 业务失败仍走协议成功（IF-D14），实际：{error:?}"));
    let result = complete(response);
    assert_eq!(
        result.is_error,
        Some(true),
        "{tool}: 业务失败必须是 error 结果"
    );
    first_text(&result).unwrap_or_else(|| panic!("{tool}: error 结果必须有文本块"))
}

/// 只为派生 IF-D14 固定文本的替身工具：`invoke` 恒失败，错误原文可辨认（不得出现在模型面）。
struct FailingStubTool {
    name: String,
}

#[async_trait]
impl BaseTool for FailingStubTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "派生 IF-D14 固定文本的替身工具（不触网、不读实例状态）"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Err(format!("STUB-DETAIL-must-not-leak: raw failure of {}", self.name).into())
    }
}

/// IF-D14 失败形态的**固定文本**：由唯一共享助手 [`invoke_tool_call`] 对**同名**替身工具
/// 派生——本文件不复制映射逻辑、不硬编码规则字面量（规则文本只由工具名决定）。
async fn shared_failure_text(tool_name: &str) -> String {
    let stub: Vec<Arc<dyn BaseTool>> = vec![Arc::new(FailingStubTool {
        name: tool_name.to_string(),
    })];
    let response = invoke_tool_call(&stub, "", &call(tool_name, json!({})))
        .await
        .expect("工具级失败必须是 Ok(Complete(error 结果))，不是 Err");
    let result = complete(response);
    assert_eq!(
        result.is_error,
        Some(true),
        "共享助手对工具级失败必须产出 error 结果"
    );
    let text = first_text(&result).expect("error 结果必须有文本块");
    assert!(
        !text.contains("STUB-DETAIL-must-not-leak"),
        "固定文本本身不得含替身工具的原文：{text}"
    );
    text
}

/// 把命中文本的泄漏标记追加进 `leaks`（计数口径：「泄露计数 == 0」）。
fn collect_leaks<'a>(
    text: &str,
    markers: impl IntoIterator<Item = &'a str>,
    leaks: &mut Vec<String>,
) {
    for marker in markers {
        if !marker.is_empty() && text.contains(marker) {
            leaks.push(marker.to_string());
        }
    }
}

/// V 矩阵第 22 行：真 handler（经 dispatch 工厂 + 真实 wire）的成功 / 业务 Err / 未知工具
/// 三形态，与唯一共享助手 `peri_mcp_common::invoke_tool_call` 的输出逐字一致；effective 名字（模型面）
/// 经生产 `McpToolBridge` 打回同一条真实链路。
///
/// 判据来源：固定文本由共享助手对**同名**替身工具派生（不硬编码规则字面量），未知工具协议
/// 错误与共享助手的 `invalid_params` 逐字比较——若 handler 另写了一份映射，任一条立刻红。
#[tokio::test]
async fn call_tool_uses_shared_result_mapping() {
    let fixture = CrossFixture::new();
    let src = fixture.write_file("main.rs", "fn main() {}\n");
    let cases = [
        Wave2CallCase {
            instance: "cron",
            success_tool: "cron_list",
            success_input: json!({}),
            fail_tool: "cron_register",
            fail_input: json!({ "expression": INVALID_EXPRESSION, "prompt": LEAK_PROMPT }),
        },
        Wave2CallCase {
            instance: "lsp",
            success_tool: "LSP",
            success_input: json!({ "operation": "documentSymbol", "file_path": src }),
            fail_tool: "LSP",
            // 缺 `file_path` ⇒ `LspToolError::MissingParam`：不依赖任何 language server 进程。
            fail_input: json!({ "operation": "documentSymbol" }),
        },
    ];

    for case in cases {
        // 注册表前置：两个工具名必须是本实例声明的原始名（名字不凭空写）。
        let declared =
            find(case.instance).unwrap_or_else(|| panic!("{}: 实例必须在注册表内", case.instance));
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
                .effective_name
        };

        // 真实链路：dispatch 工厂 → 真实 transport → 生产 client 握手。
        let pair = connect_via_dispatch(case.instance, &fixture.ctx).await;
        let peer = pair.peer();
        let expected_failure = shared_failure_text(case.fail_tool).await;

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

        // ② 业务 Err 形态：`Complete(error 结果)` + 固定脱敏文本。
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
        assert_eq!(
            failure_text, expected_failure,
            "{}: 失败文本必须等于共享助手的固定文本",
            case.instance
        );
        assert!(
            failure_text.contains(case.fail_tool),
            "固定文本只带工具名：{failure_text}"
        );

        // ③ 未知工具形态：invalid params，且与共享助手逐字同源（不 panic）。
        let unknown = "wave2-unknown-tool";
        let helper_error = invoke_tool_call(&[], "", &call(unknown, json!({})))
            .await
            .expect_err("共享助手的未知工具分支必须是协议错误");
        let wire_error = peer
            .call_tool_once(call(unknown, json!({})))
            .await
            .expect_err("线上未知工具名必须是协议错误");
        match wire_error {
            ServiceError::McpError(data) => {
                assert_eq!(
                    data.code.0,
                    ErrorCode::INVALID_PARAMS.0,
                    "{}: 未知工具必须是 -32602",
                    case.instance
                );
                assert_eq!(
                    data.message, helper_error.message,
                    "{}: 未知工具错误必须与共享助手逐字一致",
                    case.instance
                );
            }
            other => panic!(
                "{}: 期望 McpError(invalid_params)，实际 {other:?}",
                case.instance
            ),
        }

        // ④ effective 调用桥接：模型面名字（注册表冻结的 effective name）经生产
        //    `build_deferred_tool_bridges` 打回**同一条**真实链路的 handler。
        let pool = bridge_pool(case.instance, &peer).await;
        let bridges = build_deferred_tool_bridges(&pool);
        let success_bridge = bridge_of(&bridges, effective_of(case.success_tool));
        let bridged_success = success_bridge
            .invoke(
                case.success_input.clone(),
                ToolContext::new(&[], &fixture.ctx.cwd),
            )
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
        let bridged_error = failure_bridge
            .invoke(
                case.fail_input.clone(),
                ToolContext::new(&[], &fixture.ctx.cwd),
            )
            .await
            .expect_err("工具级失败经桥必须 Err（桥把 error 结果转成 CallFailed）");
        match bridged_error.downcast_ref::<ToolCallError>() {
            Some(ToolCallError::CallFailed { reason, tool, .. }) => {
                assert_eq!(
                    reason.as_str(),
                    expected_failure.as_str(),
                    "{}: 桥接失败原因必须是同一份固定脱敏文本",
                    case.instance
                );
                assert_eq!(tool, case.fail_tool);
            }
            other => panic!(
                "{}: 期望 CallFailed（reason = 固定文本），实际 {other:?}",
                case.instance
            ),
        }

        println!(
            "[W2 dispatch/source] instance={} success=1 error=1 invalid_params=1 bridged=2",
            case.instance
        );
        pair.shutdown().await;
    }
}

/// 一个 wave 2 实例的三形态用例输入（工具名必须能在注册表里查到）。
struct Wave2CallCase {
    instance: &'static str,
    success_tool: &'static str,
    success_input: Value,
    fail_tool: &'static str,
    fail_input: Value,
}

/// 把一条**已握手**的真实 builtin 链路登记成 pool 里的 Connected 句柄（形状同生产
/// `run_initialize` 的提交点：`peer` / `tools` 取自真实往返），再经生产
/// `build_deferred_tool_bridges` 取 effective name 的 `McpToolBridge`。
///
/// 句柄 `name` 必须是实例名：bridge 的 effective name 由 `(server_name, 原始工具名)` 计算
/// （`McpToolBridge::new` 用 `client.name`），因此这一步同时验证注册表冻结字面量与生产计算
/// 一致。
async fn bridge_pool(instance: &str, peer: &Peer<RoleClient>) -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_empty());
    let tools = peer
        .list_all_tools()
        .await
        .expect("真实 tools/list 必须成功");
    pool.clients.write().insert(
        instance.to_string(),
        Arc::new(McpClientHandle {
            name: instance.to_string(),
            version: None,
            cache_version: None,
            peer: Some(peer.clone()),
            tools,
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
            channel_capable: false,
        }),
    );
    pool
}

/// 按注册表冻结的 effective name 取真实桥（缺桥 ⇒ 断言失败）。
fn bridge_of<'a>(bridges: &'a [McpToolBridge], effective_name: &str) -> &'a McpToolBridge {
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

/// V 矩阵第 23 行：cron 两类 + LSP 六类业务错误**经 handler 路径**返回的文本全部等于固定
/// 脱敏文本（只含工具名），且不含原始错误 Display、表达式原文、用户 prompt、文件路径 / 扩展名。
///
/// 触发方式：每类先用**工具实现层**（同一 pool / 同一 scheduler）跑一次同一输入，取其错误
/// Display 作为「确实落到该变体」的正控与泄漏对照，再断言 handler 路径（真实 wire）的文本。
///
/// **UNVERIFIED（不写 flaky 断言）**：`LspToolError::NotReady` 需要
/// `ensure_server_for_file` / `ensure_initialized` 返回 `Ok` 而没有任何 server `Running`；
/// 而 `LspClient::start` 成功即以 `ServerState::Running` 收尾（`mcp-packages/lsp/src/client/lifecycle.rs`
/// 的 `do_start`），`is_ready()` 与 `any_server()` 用同一谓词，唯一剩余路径是「ensure 与
/// ready 复查之间服务器转 `Error`」的竞态，无法由输入稳定构造，因此本用例只断言可稳定
/// 触发的 7 类（2 + 5），`NotReady` 记 UNVERIFIED。
#[tokio::test]
async fn call_tool_error_text_is_fixed_and_redacted() {
    let fixture = CrossFixture::new();
    let src = fixture.write_file("main.rs", "fn main() {}\n");
    // 无路由扩展名路径：目录名与扩展名都是可辨认的泄漏标记。
    let no_route = fixture.write_file(
        &format!("{LEAK_PATH_MARKER}/private.{LEAK_EXTENSION}"),
        "no server routes this extension\n",
    );

    let mut matched = 0usize;
    let mut leaks: Vec<String> = Vec::new();

    // ─── cron：两类业务错误 ───────────────────────────────────────────────────
    let cron = connect_via_dispatch("cron", &fixture.ctx).await;
    let expected_cron = shared_failure_text("cron_register").await;
    // 正控 ①：非法表达式在**同一 scheduler** 的工具实现层失败（原文即泄漏对照）。
    let raw_invalid = fixture
        .scheduler
        .lock()
        .register(INVALID_EXPRESSION, LEAK_PROMPT)
        .expect_err("非法表达式必须在 CronScheduler::register 失败")
        .to_string();
    // 正控 ②：先填到上限（次数读 `MAX_CRON_TASKS`，不硬编码 20）再注册。
    while fixture.scheduler.lock().list_tasks().len() < MAX_CRON_TASKS {
        fixture
            .scheduler
            .lock()
            .register(VALID_EXPRESSION, LEAK_PROMPT)
            .expect("上限内的注册必须成功");
    }
    let raw_limit = fixture
        .scheduler
        .lock()
        .register(VALID_EXPRESSION, LEAK_PROMPT)
        .expect_err("达到上限后注册必须失败")
        .to_string();
    assert_ne!(raw_invalid, raw_limit, "两个变体的原文必须可区分");
    let limit_text = MAX_CRON_TASKS.to_string();

    for input in [
        json!({ "expression": INVALID_EXPRESSION, "prompt": LEAK_PROMPT }),
        json!({ "expression": VALID_EXPRESSION, "prompt": LEAK_PROMPT }),
    ] {
        matched += 1;
        let text = call_error_text(&cron, "cron_register", input).await;
        assert_eq!(
            text, expected_cron,
            "cron 的业务错误文本必须等于固定脱敏文本（与失败原因无关）"
        );
        collect_leaks(
            &text,
            [
                raw_invalid.as_str(),
                raw_limit.as_str(),
                limit_text.as_str(),
                INVALID_EXPRESSION,
                LEAK_PROMPT,
            ],
            &mut leaks,
        );
    }
    cron.shutdown().await;

    // ─── lsp：六类中的五类（NotReady 见用例文档：无法稳定构造） ────────────────
    let lsp = connect_via_dispatch("lsp", &fixture.ctx).await;
    let expected_lsp = shared_failure_text("LSP").await;
    let tool = LspTool::new(Arc::clone(&fixture.pool));
    let cases: [(&str, Value); 5] = [
        // MissingParam：缺 `file_path`（不进 pool）。
        ("MissingParam", json!({ "operation": "documentSymbol" })),
        // InvalidOperation：服务器就绪后落到 `_` 分支。
        (
            "InvalidOperation",
            json!({ "operation": "wave2-no-such-operation", "file_path": src, "line": 1, "character": 1 }),
        ),
        // RequestFailed：prepareCallHierarchy 回 `null` ⇒ 反序列化调用层级数组失败。
        (
            "RequestFailed",
            json!({ "operation": "incomingCalls", "file_path": src, "line": 1, "character": 1 }),
        ),
        // InvalidPosition：`line == 0`（在任何服务器访问之前返回）。
        (
            "InvalidPosition",
            json!({ "operation": "goToDefinition", "file_path": src, "line": 0, "character": 1 }),
        ),
        // NoServerForExtension：扩展名无路由（原文内嵌路径与扩展名）。
        (
            "NoServerForExtension",
            json!({ "operation": "documentSymbol", "file_path": no_route }),
        ),
    ];
    let mut raws: Vec<String> = Vec::new();
    for (variant, input) in &cases {
        // 正控：同一输入必须在工具实现层落到该变体（否则本行断言是空转）。
        let raw = tool
            .invoke(input.clone(), ToolContext::new(&[], ""))
            .await
            .unwrap_err()
            .to_string();
        assert!(!raw.is_empty(), "{variant}: 错误原文不得为空");
        // 变体身份：只允许用**本文件给出的**输入事实或可独立复算的事实证伪「其实落到别的
        // 变体」，不硬编码生产 Display 字面量。
        match *variant {
            "MissingParam" => assert!(raw.contains("file_path"), "缺参变体必须回传缺失的参数名"),
            "InvalidOperation" => assert!(
                raw.contains("wave2-no-such-operation"),
                "无效 operation 变体必须回传被拒的 operation"
            ),
            "NoServerForExtension" => assert!(
                raw.contains(LEAK_PATH_MARKER) && raw.contains(LEAK_EXTENSION),
                "无路由扩展名变体的原文必须内嵌路径与扩展名（这正是它的泄漏面）"
            ),
            "RequestFailed" => {
                // 本行的构造是「请求返回 `null` ⇒ 反序列化调用层级数组失败」：serde 的错误
                // 文本可由本文件对同一 `null` 载荷独立复算（不引用生产字面量）。
                let recomputed = serde_json::from_value::<Vec<Value>>(Value::Null)
                    .expect_err("null 不得反序列化成数组")
                    .to_string();
                assert!(
                    raw.contains(&recomputed),
                    "RequestFailed 行必须是 null 载荷反序列化失败：{raw}"
                );
            }
            "InvalidPosition" => {}
            other => panic!("未登记的变体 {other}"),
        }
        raws.push(format!("{variant}: {raw}"));

        matched += 1;
        let text = call_error_text(&lsp, "LSP", input.clone()).await;
        assert_eq!(
            text, expected_lsp,
            "{variant}: LSP 的错误文本必须等于固定脱敏文本"
        );
        collect_leaks(
            &text,
            [
                raw.as_str(),
                src.as_str(),
                no_route.as_str(),
                LEAK_PATH_MARKER,
                LEAK_EXTENSION,
                LEAK_PROMPT,
            ],
            &mut leaks,
        );
    }
    // 正控有效性：五个变体的原文必须两两不同（否则「逐类」是同一类的重复计数）。
    let mut distinct_raws = raws.clone();
    distinct_raws.sort();
    distinct_raws.dedup();
    assert_eq!(
        distinct_raws.len(),
        cases.len(),
        "五个 LSP 变体必须各自触发不同的错误原文：{raws:#?}"
    );
    // InvalidOperation / RequestFailed 两行必须在**就绪**的 language server 上打到（fake
    // server 的 spawn 观测文件非空）；配合 `call_tool_uses_shared_result_mapping` 的成功行，
    // 「就绪」这一前提有独立证据。
    let spawns = std::fs::read_to_string(fixture.dir().join("spawn_count.txt"))
        .unwrap_or_default()
        .lines()
        .count();
    assert!(
        spawns >= 1,
        "InvalidOperation / RequestFailed 必须有真实拉起的 language server"
    );
    lsp.shutdown().await;

    // 固定文本只由工具名决定：两条期望的差异必须恰是工具名槽位。
    assert_eq!(
        expected_lsp.replace("`LSP`", "`cron_register`"),
        expected_cron,
        "cron / lsp 的固定文本必须只差工具名槽位"
    );
    // 夹具前置：无路由扩展名文件与真实源文件都存在（否则 NoServerForExtension 会变体漂移）。
    assert!(Path::new(&no_route).exists() && Path::new(&src).exists());

    // 先 assert 再打印（`--nocapture` 抄录用）：matched = 2（cron）+ 5（LSP 可稳定触发类）。
    println!(
        "[W2 dispatch/source] error-variants declared=8 matched={matched} unverified=NotReady leaks={}",
        leaks.len()
    );
    assert_eq!(matched, 7, "8 类中 7 类可在 handler 路径稳定触发");
    assert_eq!(leaks.len(), 0, "业务细节不得进入模型面文本：{leaks:?}");
}

#[path = "dispatch_resource_wire_test.rs"]
mod dispatch_resource_wire_tests;

#[path = "dispatch_factory_test.rs"]
mod dispatch_factory_tests;
