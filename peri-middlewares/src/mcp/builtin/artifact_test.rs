//! `artifact` builtin MCP handler 的 crate 内证据。
//!
//! 覆盖口径（主 plan §3 IF-D14 / §6 I-01 行）：
//! - **映射三种形态**：未知工具名 → `invalid_params`；`Ok(text)` → success；
//!   `Err` → 模型可见的 error 结果（且路径 / 凭据形状串不泄漏）。
//! - **真实上传协议**：`ArtifactTool` + `ArtifactClient` 打到本地回环桩（注入的
//!   base url 与假 token），无网络、无真实凭据。
//! - **真实链路**：server 半边是生产 handler（`rmcp::serve_server`），client 半边是
//!   生产 `serve_client_auto`（Auto lifecycle）——handler 的 `call_tool` 与
//!   `list_tools` 由下面的 wire 用例覆盖（直接用例走 `invoke_tool_call` 同一路径，
//!   只是不经协议编解码）。
//!
//! 证据边界：`cwd` 只是相对路径解析根，**不是**安全沙箱（沿用既有口径）；本文件不
//! 断言能力根 / 凭据隔离在 builtin 形态下的成立（记 UNVERIFIED）。

use std::{sync::Arc, time::Duration};

use peri_acp_types::builtin_mcp::find;
use peri_agent::tools::BaseTool;
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode},
    service::{Peer, QuitReason, RoleClient},
    ServerHandler, ServiceError,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{invoke_tool_call, ArtifactMcpServer};
use crate::artifact::{client::ArtifactClient, ArtifactTool};
use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::client::{serve_client_auto, McpServiceWrapper};

/// duplex 双向缓冲（与既有夹具一致）。
const DUPLEX_BUF: usize = 8 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// 假 token：不是凭据，只用于证明「注入的 token 确实被带上」。
const FAKE_TOKEN: &str = "test-token-0000";
/// 桩返回的上传结果（假 URL）。
const STUB_UPLOAD_BODY: &str =
    r#"{"url":"https://example.test/artifact-1","expiresAt":"2026-10-03T00:00:00Z"}"#;
/// 不会被触碰的端口（缺失文件在发起请求前就失败）。
const UNREACHED_BASE_URL: &str = "http://127.0.0.1:9";

// ─── 夹具 ─────────────────────────────────────────────────────────────────────

/// 本地回环 HTTP 桩：接受一次连接并返回固定 JSON。
///
/// 返回 `(base_url, 请求头原文句柄)`；句柄的值供断言路径 / `Authorization` / TTL，
/// **不得**打印（§9 规则 7：不打印 headers）。
async fn spawn_upload_stub(
    body: &'static str,
) -> (String, tokio::task::JoinHandle<Option<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("本地桩必须能绑定回环端口");
    let port = listener.local_addr().expect("本地桩地址可读").port();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.ok()?;
        let mut received = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.ok()?;
            if read == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..read]);
            if received.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&received).into_owned();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.ok()?;
        socket.flush().await.ok()?;
        Some(head)
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

/// 指向本地桩（或不会被触碰的端口）的 `ArtifactTool`。
fn artifact_tool(cwd: &str, base_url: &str) -> Arc<dyn BaseTool> {
    Arc::new(ArtifactTool::with_client_for_test(
        cwd.to_string(),
        ArtifactClient::new(base_url.to_string(), FAKE_TOKEN.to_string()),
    ))
}

/// 带一个 `report.html` 的临时工作目录。
fn cwd_with_report() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("临时目录");
    std::fs::write(dir.path().join("report.html"), "<html>report</html>").expect("写夹具文件");
    dir
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

struct Pair {
    service: McpServiceWrapper,
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

fn call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

fn complete(response: CallToolResponse) -> CallToolResult {
    match response {
        CallToolResponse::Complete(result) => result,
        other => panic!("期望 Complete 结果，实际：{other:?}"),
    }
}

// ─── 声明面 ───────────────────────────────────────────────────────────────────

#[test]
fn artifact_server_info_declares_tools_capability_and_instance_name() {
    let dir = cwd_with_report();
    let server = ArtifactMcpServer::new(dir.path());
    let info = server.get_info();
    assert!(
        info.capabilities.tools.is_some(),
        "必须声明 tools 能力，否则 tools/list 不可达"
    );
    let implementation = &info.server_info;
    assert_eq!(implementation.name, "peri-artifact-mcp");
    assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn artifact_list_tools_matches_registry_declared_tool_set() {
    let dir = cwd_with_report();
    let server = ArtifactMcpServer::new(dir.path());
    let expected: Vec<&str> = find("artifact")
        .expect("artifact 是已实现实例")
        .tools
        .iter()
        .map(|tool| tool.original_name)
        .collect();

    let listed = super::super::web::list_tools_of(server.tools());
    let actual: Vec<&str> = listed.tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        actual, expected,
        "handler 的工具集必须与注册表声明逐项一致（漂移即红）"
    );

    let schema = listed.tools[0].input_schema.as_ref();
    assert_eq!(
        schema.get("type").and_then(Value::as_str),
        Some("object"),
        "input_schema 根必须是 object（启动期 validate_input_schema 的同源约束）"
    );
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("artifact 必须有 required 列表");
    assert!(
        required.iter().any(|item| item == "file_path"),
        "启动期按 required 校验入参；required 必须含 file_path"
    );
}

// ─── IF-D14 结果映射（与 handler 的 call_tool 同一路径） ──────────────────────

#[tokio::test]
async fn artifact_call_tool_unknown_name_is_invalid_params() {
    let dir = cwd_with_report();
    let server = ArtifactMcpServer::new(dir.path());
    let error = invoke_tool_call(server.tools(), &server.cwd, &call("Nope", json!({})))
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
async fn artifact_call_tool_missing_file_maps_to_error_result_without_path_leak() {
    let dir = cwd_with_report();
    let cwd = dir.path().to_string_lossy().into_owned();
    let server = ArtifactMcpServer::with_tools(&cwd, vec![artifact_tool(&cwd, UNREACHED_BASE_URL)]);

    let response = invoke_tool_call(
        server.tools(),
        &server.cwd,
        &call("artifact", json!({ "file_path": "missing.html" })),
    )
    .await
    .expect("工具级失败必须是 Ok(Complete(error 结果))，不是 Err(internal_error)");

    let result = complete(response);
    assert_eq!(result.is_error, Some(true));
    let text = first_text(&result).expect("错误结果必须有文本块");
    assert!(text.contains("artifact"), "错误文本应含工具名：{text}");
    assert!(
        !text.contains(&cwd),
        "错误文本不得含路径（§9 规则 7）：{text}"
    );
    assert!(
        !text.contains(FAKE_TOKEN),
        "错误文本不得含凭据（§9 规则 7）：{text}"
    );
}

#[tokio::test]
async fn artifact_call_tool_upload_success_maps_to_text_content() {
    let dir = cwd_with_report();
    let cwd = dir.path().to_string_lossy().into_owned();
    let (base_url, stub) = spawn_upload_stub(STUB_UPLOAD_BODY).await;
    let server = ArtifactMcpServer::with_tools(&cwd, vec![artifact_tool(&cwd, &base_url)]);

    let response = invoke_tool_call(
        server.tools(),
        &server.cwd,
        &call(
            "artifact",
            json!({ "file_path": "report.html", "ttl": "30d" }),
        ),
    )
    .await
    .expect("成功形态必须是 Ok(CallToolResponse)");

    let result = complete(response);
    assert_eq!(result.is_error, Some(false));
    let text = first_text(&result).expect("成功结果必须有文本块");
    assert!(
        text.contains("https://example.test/artifact-1"),
        "上传结果必须回传 URL：{text}"
    );

    let head = stub
        .await
        .expect("桩任务不得 panic")
        .expect("桩必须收到一次请求");
    let lower = head.to_ascii_lowercase();
    assert!(
        head.starts_with("POST /upload "),
        "上传路径必须是 /upload：{lower}"
    );
    assert!(
        lower.contains(&format!(
            "authorization: bearer {}",
            FAKE_TOKEN.to_ascii_lowercase()
        )),
        "注入的 token 必须按 Bearer 头带上（只断言假 token，不打印）"
    );
    assert!(lower.contains("x-ttl: 30d"), "ttl 参数必须透传到 X-TTL");
}

// ─── 真实链路（duplex + 生产 client） ─────────────────────────────────────────

#[tokio::test]
async fn artifact_handler_tools_list_and_both_result_forms_round_trip_over_wire() {
    let dir = cwd_with_report();
    let cwd = dir.path().to_string_lossy().into_owned();
    let (base_url, stub) = spawn_upload_stub(STUB_UPLOAD_BODY).await;
    let server = ArtifactMcpServer::with_tools(&cwd, vec![artifact_tool(&cwd, &base_url)]);
    let pair = connect(server).await;
    let peer = pair.peer();

    let tools = peer
        .list_all_tools()
        .await
        .expect("tools/list 必须成功（覆写 discover 会让它在会话层被拒）");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names, vec!["artifact"]);

    let success = complete(
        peer.call_tool_once(call("artifact", json!({ "file_path": "report.html" })))
            .await
            .expect("成功形态必须是协议成功"),
    );
    assert_eq!(success.is_error, Some(false));
    assert!(
        first_text(&success)
            .as_deref()
            .is_some_and(|text| text.contains("https://example.test/artifact-1")),
        "线路上的成功结果必须带回 URL"
    );

    let failure = complete(
        peer.call_tool_once(call("artifact", json!({ "file_path": "missing.html" })))
            .await
            .expect("工具级失败仍走协议成功（is_error 承载语义）"),
    );
    assert_eq!(failure.is_error, Some(true));
    let text = first_text(&failure).expect("错误结果必须有文本块");
    assert!(!text.contains(&cwd), "线路上的错误文本不得含路径");

    assert!(
        stub.await.expect("桩任务不得 panic").is_some(),
        "成功形态必须真的打过一次上传"
    );
    pair.shutdown().await;
}

/// 未知工具名在**线路**上也必须是协议错误（`invalid_params`），与直接调用一致。
#[tokio::test]
async fn artifact_handler_unknown_tool_round_trip_over_wire_is_invalid_params() {
    let dir = cwd_with_report();
    let cwd = dir.path().to_string_lossy().into_owned();
    let server = ArtifactMcpServer::with_tools(&cwd, vec![artifact_tool(&cwd, UNREACHED_BASE_URL)]);
    let pair = connect(server).await;

    let error = pair
        .peer()
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
