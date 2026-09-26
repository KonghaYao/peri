//! `web` builtin MCP handler 的 crate 内证据。
//!
//! 覆盖口径（主 plan §3 IF-D14 / §6 I-01 行）：
//! - **映射三种形态**：未知工具名 → `invalid_params`；`Ok(text)` → success；
//!   `Err` → 模型可见的 error 结果（且原始错误文本不泄漏）。
//! - **真实链路**：server 半边是生产 handler（`rmcp::serve_server`），client 半边是
//!   生产 `serve_client_auto`（Auto lifecycle，与 `builtin_spike_test.rs` 同形）；
//!   `tools/list` 与 `tools/call` 两个方向都经真实 wire。
//!
//! 证据边界（不得升级为整体结论）：
//! - Web 两个工具的**真实后端调用**（Tavily）不在本文件的证据范围内：其 base url 是
//!   编译期常量，单测内无法指向本地桩，因此成功 / 失败形态由替身工具产生。
//!   真实网络调用记 **UNVERIFIED**（验收记录口径）。

use std::{path::Path, sync::Arc, sync::Mutex, time::Duration};

use async_trait::async_trait;
use peri_acp_types::builtin_mcp::find;
use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode},
    service::{Peer, QuitReason, RoleClient},
    ServerHandler, ServiceError,
};
use serde_json::{json, Value};

use super::{
    builtin_server_handler, invoke_tool_call, rmcp_tool_from_base, BuiltinServerHandler,
    WebMcpServer,
};
use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::client::{serve_client_auto, McpServiceWrapper};

/// duplex 双向缓冲：单帧 JSON-RPC 行是几百字节级，8 KiB 与既有夹具一致。
const DUPLEX_BUF: usize = 8 * 1024;
/// client 侧握手上界（`serve_client_auto` 内建）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界（收尾共用）。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// 替身工具的成功文本（可辨认）。
const STUB_OK_TEXT: &str = "stub-web-result";
/// 替身工具失败信息里故意埋入的「敏感形状」串：映射后**不得**出现在模型面文本里。
const LEAK_PATH: &str = "/tmp/secret-marker/private.html";
const LEAK_TOKEN: &str = "test-token-0000";

// ─── 夹具 ─────────────────────────────────────────────────────────────────────

/// 受控替身工具：记录收到的参数，返回固定文本或固定错误。
struct StubTool {
    name: &'static str,
    outcome: Result<&'static str, &'static str>,
    calls: Arc<Mutex<Vec<Value>>>,
}

impl StubTool {
    fn succeeding(name: &'static str, calls: &Arc<Mutex<Vec<Value>>>) -> Arc<dyn BaseTool> {
        Arc::new(Self {
            name,
            outcome: Ok(STUB_OK_TEXT),
            calls: Arc::clone(calls),
        })
    }

    fn failing(name: &'static str, calls: &Arc<Mutex<Vec<Value>>>) -> Arc<dyn BaseTool> {
        Arc::new(Self {
            name,
            outcome: Err("File not found: /tmp/secret-marker/private.html (token=test-token-0000)"),
            calls: Arc::clone(calls),
        })
    }
}

#[async_trait]
impl BaseTool for StubTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "crate 内替身工具（不触网）"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.calls.lock().expect("替身工具记录锁未中毒").push(input);
        match self.outcome {
            Ok(text) => Ok(text.to_string()),
            Err(message) => Err(message.into()),
        }
    }
}

/// 构造一条已握手的同进程链路：server 半边 = 生产 handler，client 半边 = 生产
/// `serve_client_auto`（Auto lifecycle）。收尾有界，不留 orphan task。
struct Pair {
    service: McpServiceWrapper,
    server_task: tokio::task::JoinHandle<Result<QuitReason, tokio::task::JoinError>>,
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
    let service = serve_client_auto(
        tokio::io::split(client_io),
        None,
        None,
        &McpCapabilityProfile::disabled(),
        HANDSHAKE_TIMEOUT,
    )
    .await
    .expect("client 侧握手超时")
    .expect("client 侧握手失败");
    Pair {
        service,
        server_task,
    }
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

fn tool_names(result: &rmcp::model::ListToolsResult) -> Vec<&str> {
    result.tools.iter().map(|tool| tool.name.as_ref()).collect()
}

// ─── 声明面 ───────────────────────────────────────────────────────────────────

#[test]
fn web_server_info_declares_tools_capability_and_instance_name() {
    let info = WebMcpServer::new().get_info();
    assert!(
        info.capabilities.tools.is_some(),
        "必须声明 tools 能力，否则 tools/list 不可达"
    );
    let implementation = &info.server_info;
    assert_eq!(implementation.name, "peri-web-mcp");
    assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn web_list_tools_matches_registry_declared_tool_set() {
    let server = WebMcpServer::new();
    let declared = find("web").expect("web 是已实现实例");
    let mut expected: Vec<&str> = declared
        .tools
        .iter()
        .map(|tool| tool.original_name)
        .collect();
    expected.sort_unstable();

    let listed = super::list_tools_of(server.tools());
    let mut actual = tool_names(&listed);
    actual.sort_unstable();
    assert_eq!(
        actual, expected,
        "handler 的工具集必须与注册表声明逐项一致（漂移即红）"
    );
}

#[test]
fn web_tool_schemas_map_to_non_empty_objects() {
    for tool in WebMcpServer::new().tools() {
        let mapped = rmcp_tool_from_base(tool.as_ref());
        assert_eq!(mapped.name.as_ref(), tool.name(), "名字必须逐字映射");
        assert_eq!(
            mapped.description.as_deref(),
            Some(tool.description()),
            "description 必须逐字映射"
        );
        let schema = mapped.input_schema.as_ref();
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{}: input_schema 根必须是 object（启动期 validate_input_schema 的同源约束）",
            tool.name()
        );
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{}: properties 必须是 object", tool.name()));
        assert!(
            !properties.is_empty(),
            "{}: properties 不得为空",
            tool.name()
        );
    }
}

// ─── IF-D14 结果映射 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn web_call_tool_unknown_name_is_invalid_params() {
    let server = WebMcpServer::new();
    let error = invoke_tool_call(server.tools(), "", &call("Nope", json!({})))
        .await
        .expect_err("未知工具名必须是协议错误（invalid_params）");
    assert_eq!(error.code.0, ErrorCode::INVALID_PARAMS.0);
    assert!(
        error.message.contains("unknown tool: Nope"),
        "错误文本应含工具名；实际：{}",
        error.message
    );
}

#[tokio::test]
async fn web_call_tool_success_maps_to_text_content() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let server = WebMcpServer::with_tools(vec![StubTool::succeeding("StubSearch", &calls)]);

    let response = invoke_tool_call(
        server.tools(),
        "",
        &call("StubSearch", json!({"query": "rust"})),
    )
    .await
    .expect("工具存在时必须返回 Ok(CallToolResponse)");

    let result = complete(response);
    assert_eq!(result.is_error, Some(false));
    assert_eq!(first_text(&result).as_deref(), Some(STUB_OK_TEXT));

    let recorded = calls.lock().expect("锁未中毒");
    assert_eq!(recorded.len(), 1, "工具必须被调用恰好一次");
    assert_eq!(recorded[0], json!({"query": "rust"}), "参数必须原样透传");
}

#[tokio::test]
async fn web_call_tool_empty_arguments_are_passed_as_empty_object() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let server = WebMcpServer::with_tools(vec![StubTool::succeeding("StubSearch", &calls)]);

    let request = CallToolRequestParams::new("StubSearch");
    invoke_tool_call(server.tools(), "", &request)
        .await
        .expect("缺省参数不是协议错误");

    let recorded = calls.lock().expect("锁未中毒");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0], json!({}), "缺省参数 = 空对象");
}

#[tokio::test]
async fn web_call_tool_error_maps_to_error_result_without_detail_leak() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let server = WebMcpServer::with_tools(vec![StubTool::failing("StubFailing", &calls)]);

    let response = invoke_tool_call(server.tools(), "", &call("StubFailing", json!({})))
        .await
        .expect("工具级失败必须是 Ok(Complete(error 结果))，不是 Err(internal_error)");

    let result = complete(response);
    assert_eq!(
        result.is_error,
        Some(true),
        "失败必须进 CallToolResult::error"
    );
    let text = first_text(&result).expect("错误结果必须有文本块");
    assert!(text.contains("StubFailing"), "错误文本应含工具名：{text}");
    assert!(
        !text.contains(LEAK_PATH),
        "错误文本不得含路径（§9 规则 7）：{text}"
    );
    assert!(
        !text.contains(LEAK_TOKEN),
        "错误文本不得含凭据形状串（§9 规则 7）：{text}"
    );
}

// ─── 实例工厂 ─────────────────────────────────────────────────────────────────

#[test]
fn web_handler_factory_covers_implemented_instances_only() {
    let cwd = Path::new(".");
    assert!(
        matches!(
            builtin_server_handler("web", cwd),
            Some(BuiltinServerHandler::Web(_))
        ),
        "web 必须有 handler"
    );
    assert!(
        matches!(
            builtin_server_handler("artifact", cwd),
            Some(BuiltinServerHandler::Artifact(_))
        ),
        "artifact 必须有 handler"
    );
    for name in ["cron", "lsp", "workspace", "not-a-builtin"] {
        assert!(
            builtin_server_handler(name, cwd).is_none(),
            "{name}: 保留但未实现的实例名不得产出 handler"
        );
    }
}

// ─── 真实链路（duplex + 生产 client） ─────────────────────────────────────────

#[tokio::test]
async fn web_handler_tools_list_and_unknown_tool_round_trip_over_wire() {
    let pair = connect(WebMcpServer::new()).await;
    let peer = pair.peer();

    let tools = peer
        .list_all_tools()
        .await
        .expect("tools/list 必须成功（覆写 discover 会让它在会话层被拒）");
    let mut actual: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    actual.sort_unstable();
    let mut expected: Vec<&str> = find("web")
        .expect("web 是已实现实例")
        .tools
        .iter()
        .map(|tool| tool.original_name)
        .collect();
    expected.sort_unstable();
    assert_eq!(actual, expected, "线路上的工具集必须与注册表一致");

    let error = peer
        .call_tool_once(call("Nope", json!({})))
        .await
        .expect_err("未知工具名必须回 -32602");
    match error {
        ServiceError::McpError(data) => {
            assert_eq!(data.code.0, ErrorCode::INVALID_PARAMS.0);
            assert!(data.message.contains("unknown tool: Nope"));
        }
        other => panic!("期望 McpError(invalid_params)，实际：{other:?}"),
    }

    pair.shutdown().await;
}

#[tokio::test]
async fn web_handler_success_and_failure_forms_round_trip_over_wire() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let server = WebMcpServer::with_tools(vec![
        StubTool::succeeding("StubSearch", &calls),
        StubTool::failing("StubFailing", &calls),
    ]);
    let pair = connect(server).await;
    let peer = pair.peer();

    let success = complete(
        peer.call_tool_once(call("StubSearch", json!({"query": "rust"})))
            .await
            .expect("成功形态必须是协议成功"),
    );
    assert_eq!(success.is_error, Some(false));
    assert_eq!(first_text(&success).as_deref(), Some(STUB_OK_TEXT));

    let failure = complete(
        peer.call_tool_once(call("StubFailing", json!({})))
            .await
            .expect("工具级失败仍走协议成功（is_error 承载语义）"),
    );
    assert_eq!(failure.is_error, Some(true));
    let text = first_text(&failure).expect("错误结果必须有文本块");
    assert!(text.contains("StubFailing"));
    assert!(!text.contains(LEAK_PATH), "线路上的错误文本不得含路径");
    assert!(
        !text.contains(LEAK_TOKEN),
        "线路上的错误文本不得含凭据形状串"
    );

    pair.shutdown().await;
}
