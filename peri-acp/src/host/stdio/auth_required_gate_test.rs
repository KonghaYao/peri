//! 无配置降级宿主的 wire 回归：经真实 [`StdioTransport`] 帧驱动
//! [`super::auth_required_gate::run_auth_required_gate`]。
//!
//! 覆盖：initialize 的最小能力形状与认证广告（两种能力标记 + 顶层标记 +
//! 无标记 + 非认证 `terminal` 能力）、`session/*` 的 `-32000`
//! `AUTH_REQUIRED` 数据、未知方法 `-32601`、通知不回包与 EOF 干净退出。

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use crate::transport::stdio::StdioTransport;
use crate::transport::AcpTransport;

fn duplex_transport() -> (StdioTransport, DuplexStream, DuplexStream) {
    let (input_write, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, output_read) = tokio::io::duplex(64 * 1024);
    let transport = StdioTransport::from_reader_writer(transport_read, transport_write);
    (transport, input_write, output_read)
}

async fn write_line(stream: &mut DuplexStream, line: &str) {
    stream.write_all(line.as_bytes()).await.unwrap();
    stream.write_all(b"\n").await.unwrap();
}

async fn read_line(stream: &mut DuplexStream) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            assert_eq!(
                stream.read(&mut byte).await.unwrap(),
                1,
                "输出流不应提前关闭: 已读 {buf:?}"
            );
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        String::from_utf8(buf).expect("stdout 应为 UTF-8 JSON")
    })
    .await
    .expect("read_line 超时：降级宿主未在期限内产生输出")
}

async fn read_line_or_timeout(
    stream: &mut DuplexStream,
    wait: std::time::Duration,
) -> Option<String> {
    tokio::time::timeout(wait, read_line(stream)).await.ok()
}

async fn request(
    input: &mut DuplexStream,
    output: &mut DuplexStream,
    id: i64,
    method: &str,
    params: Value,
) -> Value {
    write_line(
        input,
        &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
    )
    .await;
    let line = read_line(output).await;
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["id"], id, "RequestId 往返: {value}");
    value
}

async fn initialize(
    input: &mut DuplexStream,
    output: &mut DuplexStream,
    id: i64,
    extra: Value,
) -> Value {
    let mut params = json!({ "protocolVersion": 1 });
    if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            params.insert(key.clone(), value.clone());
        }
    }
    request(input, output, id, "initialize", params).await
}

async fn spawn_gate() -> (tokio::task::JoinHandle<()>, DuplexStream, DuplexStream) {
    let (transport, input, output) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let task = tokio::spawn(super::auth_required_gate::run_auth_required_gate(transport));
    (task, input, output)
}

async fn await_exit(task: tokio::task::JoinHandle<()>, input: DuplexStream) {
    drop(input);
    tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("降级宿主应在 stdin EOF 后退出")
        .expect("降级宿主任务不应 panic");
}

#[tokio::test]
async fn test_gate_initialize_is_minimal_and_advertises_terminal_auth() {
    let (task, mut input, mut output) = spawn_gate().await;
    let v = initialize(
        &mut input,
        &mut output,
        1,
        json!({ "clientCapabilities": { "auth": { "terminal": true } } }),
    )
    .await;

    assert!(v.get("error").is_none(), "initialize 不应报错: {v}");
    let result = &v["result"];
    assert_eq!(result["protocolVersion"], 1, "protocolVersion 基线: {v}");
    assert_eq!(
        result["authMethods"].as_array().map(Vec::len),
        Some(1),
        "恰好一个 terminal 认证方法: {v}"
    );
    let method = &result["authMethods"][0];
    assert_eq!(method["type"], "terminal", "认证方法类型: {v}");
    assert_eq!(method["id"], "peri-setup", "认证方法 ID: {v}");
    assert_eq!(method["name"], "Configure Peri", "认证方法名称: {v}");
    assert!(
        method["description"]
            .as_str()
            .is_some_and(|description| !description.is_empty()),
        "认证方法必须有可操作描述: {v}"
    );
    assert_eq!(method["args"], json!(["--setup"]), "认证参数: {v}");
    assert_eq!(method["env"]["PERI_ACP_SETUP"], "1", "认证环境: {v}");

    // 最小且诚实：不宣称 loadSession / session 生命周期 / MCP / prompt 能力。
    let caps = &result["agentCapabilities"];
    assert_eq!(caps["loadSession"], false, "不宣称 loadSession: {v}");
    for key in ["list", "close", "resume", "fork", "delete"] {
        assert!(
            caps["sessionCapabilities"].get(key).is_none(),
            "不宣称 sessionCapabilities.{key}: {v}"
        );
    }
    assert_ne!(
        caps["mcpCapabilities"]["acp"], true,
        "不宣称 MCP over ACP: {v}"
    );
    assert_ne!(
        caps["promptCapabilities"]["image"], true,
        "不宣称 prompt 能力: {v}"
    );
    assert_eq!(
        caps["_meta"]["peri.replay"], false,
        "PeriCaps 默认关闭状态回显: {v}"
    );
    assert!(result.get("agentInfo").is_none(), "不广告 agentInfo: {v}");

    await_exit(task, input).await;
}

#[tokio::test]
async fn test_gate_initialize_accepts_registry_and_top_level_markers() {
    for extra in [
        json!({ "clientCapabilities": { "_meta": { "terminal-auth": true } } }),
        json!({ "_meta": { "terminal-auth": true } }),
    ] {
        let (task, mut input, mut output) = spawn_gate().await;
        let v = initialize(&mut input, &mut output, 1, extra.clone()).await;
        assert_eq!(
            v["result"]["authMethods"].as_array().map(Vec::len),
            Some(1),
            "标记 {extra} 应广告一个认证方法: {v}"
        );
        await_exit(task, input).await;
    }
}

#[tokio::test]
async fn test_gate_initialize_requires_an_auth_capability_marker() {
    for extra in [
        json!({}),
        // `clientCapabilities.terminal` 是 terminal/* 工具能力，不是认证能力。
        json!({ "clientCapabilities": { "terminal": true } }),
    ] {
        let (task, mut input, mut output) = spawn_gate().await;
        let v = initialize(&mut input, &mut output, 1, extra.clone()).await;
        assert_eq!(
            v["result"]["authMethods"],
            json!([]),
            "标记 {extra} 不得广告认证方法: {v}"
        );
        await_exit(task, input).await;
    }
}

#[tokio::test]
async fn test_gate_session_new_returns_auth_required_with_operation_hint() {
    let (task, mut input, mut output) = spawn_gate().await;
    initialize(
        &mut input,
        &mut output,
        1,
        json!({ "clientCapabilities": { "auth": { "terminal": true } } }),
    )
    .await;

    let v = request(
        &mut input,
        &mut output,
        2,
        "session/new",
        json!({ "cwd": "/tmp", "mcpServers": [] }),
    )
    .await;
    let error = &v["error"];
    assert_eq!(error["code"], -32000, "session/new 未配置错误码: {v}");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("AUTH_REQUIRED: ")),
        "错误消息必须带 AUTH_REQUIRED 前缀: {v}"
    );
    assert_eq!(
        error["data"]["reason"], "auth_required",
        "用 data.reason 区分仓库其它 -32000: {v}"
    );
    assert_eq!(
        error["data"]["setupCommand"], "peri --setup",
        "可操作的 setup 命令: {v}"
    );
    assert!(
        error["data"]["hint"]
            .as_str()
            .is_some_and(|hint| !hint.is_empty()),
        "必须提供操作提示: {v}"
    );
    assert_eq!(
        error["data"]["authMethods"][0]["id"], "peri-setup",
        "AUTH_REQUIRED 复用 initialize 广告的认证方法: {v}"
    );

    await_exit(task, input).await;
}

#[tokio::test]
async fn test_gate_session_new_before_initialize_has_no_auth_methods() {
    let (task, mut input, mut output) = spawn_gate().await;
    let v = request(&mut input, &mut output, 1, "session/load", json!({})).await;
    assert_eq!(v["error"]["code"], -32000, "session/* 降级错误码: {v}");
    assert_eq!(v["error"]["data"]["reason"], "auth_required", "{v}");
    assert_eq!(
        v["error"]["data"]["authMethods"],
        json!([]),
        "未 initialize 前不猜测认证方法: {v}"
    );
    await_exit(task, input).await;
}

#[tokio::test]
async fn test_gate_unknown_method_reports_peri_not_configured() {
    let (task, mut input, mut output) = spawn_gate().await;
    let v = request(&mut input, &mut output, 1, "foo/bar", json!({})).await;
    assert_eq!(v["error"]["code"], -32601, "未知方法错误码: {v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("not configured")),
        "错误消息必须说明 Peri 尚未配置: {v}"
    );
    await_exit(task, input).await;
}

#[tokio::test]
async fn test_gate_ignores_notifications_and_responses() {
    let (task, mut input, mut output) = spawn_gate().await;
    // 通知与 client→server 响应都不得产生回包。
    write_line(
        &mut input,
        &json!({"jsonrpc": "2.0", "method": "session/update", "params": {}}).to_string(),
    )
    .await;
    write_line(
        &mut input,
        &json!({"jsonrpc": "2.0", "id": 99, "result": {}}).to_string(),
    )
    .await;
    initialize(&mut input, &mut output, 1, json!({})).await;
    assert!(
        read_line_or_timeout(&mut output, std::time::Duration::from_millis(100))
            .await
            .is_none(),
        "通知/响应不得产生额外回包"
    );
    await_exit(task, input).await;
}

#[tokio::test]
async fn test_gate_eof_exits_cleanly() {
    let (task, input, _output) = spawn_gate().await;
    drop(input);
    tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("stdin EOF 后降级宿主必须退出")
        .expect("降级宿主任务不应 panic");
}
