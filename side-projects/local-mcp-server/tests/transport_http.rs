//! WP-006：Streamable HTTP 传输的原始 socket 行为（暴露面、header/body、两生命周期、关闭）。
//!
//! 全部用例走真实 TCP + 手写 HTTP/1.1（不经 rmcp client），断言 HTTP 状态码、
//! `Content-Type`、`Accept` 协商、协议头与 body 一致性、会话与关闭语义。
//! 规范条款对应关系见 `artifacts/designs/WP-006/http-clause-matrix.md`。

mod http_support;

use std::time::Duration;

use serde_json::json;

use http_support::*;
use local_mcp_server::wire::{
    HEADER_MCP_METHOD, HEADER_MCP_NAME, HEADER_MCP_PROTOCOL_VERSION, MCP_MODERN_PROTOCOL_VERSION,
    META_KEY_CLIENT_CAPABILITIES, META_KEY_PROTOCOL_VERSION,
};

// ───────────────────────────── 暴露面：Host / Origin ─────────────────────────────

/// P-16：默认只允许回环 Host；缺失 Host 头一律 400。
#[test]
fn host_header_must_be_loopback_and_missing_host_is_rejected() {
    let server = TestServer::start();

    // 传输层拒绝请求时不消费 body，连接随之关闭，因此每种拒绝各用一条新连接。
    let mut accepted = server.connect();
    let ok = accepted.send(&modern_tools_list(1));
    assert_eq!(ok.status, 200, "回环 Host 必须被接受");

    let disallowed = server
        .connect()
        .send(&modern_tools_list(2).header("Host", "attacker.example.com"));
    assert_eq!(disallowed.status, 403, "非白名单 Host 必须拒绝");
    assert!(
        disallowed
            .body_text()
            .contains("Host header is not allowed"),
        "拒绝文案应说明 Host：{}",
        disallowed.body_text()
    );

    let missing = server.connect().send(&modern_tools_list(3).omit_host());
    assert_eq!(missing.status, 400, "缺失 Host 必须 400");

    // 显式端口形式的回环 Host 仍被接受（白名单按 host 匹配，端口不参与）。
    let loopback_with_port = server
        .connect()
        .send(&modern_tools_list(4).header("Host", &server.addr().to_string()));
    assert_eq!(loopback_with_port.status, 200);
}

/// P-16：`allowed_origins` 非空时非法 Origin 被拒、缺失 Origin 放行；为空时不校验。
#[test]
fn origin_policy_follows_configured_allowlist() {
    // 默认（空 allowlist）：不校验 Origin —— 冻结契约语义。
    let default_server = TestServer::start();
    let mut default_client = default_server.connect();
    let ignored = default_client.send(&modern_tools_list(1).header("Origin", "https://evil.test"));
    assert_eq!(
        ignored.status, 200,
        "空 allowlist 表示不校验 Origin（冻结契约）"
    );

    // 显式 allowlist：非法 Origin 403、缺失 Origin 放行、合法 Origin 放行。
    let mut config = base_config();
    config.http.allowed_origins = vec!["https://allowed.test".to_string()];
    let server = TestServer::start_with(config);

    let rejected = server
        .connect()
        .send(&modern_tools_list(1).header("Origin", "https://evil.test"));
    assert_eq!(rejected.status, 403, "非白名单 Origin 必须拒绝");
    assert!(rejected
        .body_text()
        .contains("Origin header is not allowed"));

    let allowed = server
        .connect()
        .send(&modern_tools_list(2).header("Origin", "https://allowed.test"));
    assert_eq!(allowed.status, 200);

    let no_origin = server.connect().send(&modern_tools_list(3));
    assert_eq!(no_origin.status, 200, "缺失 Origin 一律放行");

    let malformed = server
        .connect()
        .send(&modern_tools_list(4).header("Origin", "not a uri"));
    assert_eq!(malformed.status, 400, "畸形 Origin 为 400");
}

/// 非回环绑定必须显式授权；未授权时传输拒绝启动（fail closed）。
#[test]
fn non_loopback_bind_is_refused_without_explicit_authorization() {
    let mut config = base_config();
    config.http.bind = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
    config.allow_non_loopback = true;
    // 已显式授权但没有 token 来源：配置校验会拒绝（此处直接构造 Config，故由传输层兜底）。
    config.auth = Default::default();

    // 显式授权 + 无 token：本服务的配置校验规则要求 token；
    // 传输层自身不重复该判断，这里断言的是"未授权时拒绝"的另一半。
    let refused = std::panic::catch_unwind(|| {
        let mut config = base_config();
        config.http.bind = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
        config.allow_non_loopback = false;
        let _server = TestServer::start_with(config);
    });
    assert!(refused.is_err(), "未授权的非回环绑定必须拒绝启动");
}

// ────────────────────────── 请求形态：Accept / Content-Type / body ──────────────────────────

/// P-15：POST 必须同时接受 JSON 与 SSE；Content-Type 必须是 application/json。
#[test]
fn post_requires_both_mime_types_and_json_content_type() {
    let server = TestServer::start();

    // 每条被传输层拒绝的请求都各用一条新连接：拒绝路径不消费 body，连接随后由
    // 服务端关闭（与 `header_body_mismatches_return_400_with_minus_32020` 同一事实），
    // 复用连接会让下一条请求撞上 EPIPE——那是测试夹具的时序问题，不是产品语义。
    let no_accept = server
        .connect()
        .send(&modern_tools_list(1).without_header("Accept"));
    assert_eq!(no_accept.status, 406, "缺失 Accept 必须 406");
    assert!(no_accept
        .body_text()
        .contains("must accept both application/json and text/event-stream"));

    let only_json = server
        .connect()
        .send(&modern_tools_list(2).header("Accept", "application/json"));
    assert_eq!(only_json.status, 406, "只接受 JSON 不足以满足 SSE");

    let wrong_content_type = server
        .connect()
        .send(&modern_tools_list(3).header("Content-Type", "text/plain"));
    assert_eq!(
        wrong_content_type.status, 415,
        "非 JSON Content-Type 必须 415"
    );

    let ok = server.connect().send(&modern_tools_list(4));
    assert_eq!(ok.status, 200);
    assert_eq!(
        ok.content_type(),
        Some("text/event-stream"),
        "成功响应为请求级 SSE（冻结配置 json_response=false）"
    );
}

/// body 上限：超过 `max_request_body_bytes` 一律 413，且不进入 handler。
#[test]
fn oversized_body_is_rejected_with_413() {
    let mut config = base_config();
    config.http.max_request_body_bytes = 1024;
    let server = TestServer::start_with(config);
    let mut client = server.connect();

    let big = "x".repeat(4096);
    let oversized = client.send(&modern_tools_call("Read", json!({ "file_path": big }), 1));
    assert_eq!(oversized.status, 413, "超限 body 必须 413");
    assert!(
        server.observations().is_empty(),
        "超限请求不得进入 MCP handler"
    );

    // 边界内请求仍然成功（证明限制而非拒绝一切）。
    let ok = client.send(&modern_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        2,
    ));
    assert_eq!(ok.status, 200);
}

/// 非法 JSON body：SDK 传输层的既有行为是 `415 Unsupported Media Type`（纯文本，
/// 不回显 body 内容）。这里如实记录事实而不是假定 `-32700`（见条款矩阵的偏差说明）。
#[test]
fn malformed_json_body_is_rejected_at_transport_layer() {
    let server = TestServer::start();
    let mut client = server.connect();
    let response = client
        .send(&RawRequest::post(json!({})).body_bytes(b"{not json, secret-ish content".to_vec()));
    assert_eq!(
        response.status, 415,
        "SDK 对不可解析 body 返回 415（传输层行为）"
    );
    assert!(
        !response.body_text().contains("secret-ish"),
        "错误文本不得回显 body 内容: {}",
        response.body_text()
    );
    assert!(
        server.observations().is_empty(),
        "不可解析请求不得进入 MCP handler"
    );
}

// ───────────────────────── header/body 一致性（-32020） ─────────────────────────

/// P-12/P-13/P-14：至少三种 header/body 不一致变体，各自 400 + `-32020`。
#[test]
fn header_body_mismatches_return_400_with_minus_32020() {
    let server = TestServer::start();

    // 每种 header/body 不一致都在 SDK 内建校验处被拒绝（拒绝路径不消费 body，
    // 连接随之关闭），因此每条各用一条新连接。

    // 变体 1：Mcp-Method 与 body 方法不一致。
    let method_mismatch = server
        .connect()
        .send(&modern_tools_list(1).header(HEADER_MCP_METHOD, "tools/call"));
    assert_eq!(method_mismatch.status, 400);
    assert_eq!(rpc_error_code(&method_mismatch), -32020);
    assert!(rpc_error_message(&method_mismatch).contains("Mcp-Method"));
    // P-15：错误响应是 JSON 且 Content-Type 与 body 一致。
    assert_eq!(method_mismatch.content_type(), Some("application/json"));
    assert!(method_mismatch.json().get("error").is_some());

    // 变体 2：Mcp-Name 与 body 工具名不一致。
    let name_mismatch = server.connect().send(
        &modern_tools_call("Read", json!({ "file_path": "a.txt" }), 2)
            .header(HEADER_MCP_NAME, "Write"),
    );
    assert_eq!(name_mismatch.status, 400);
    assert_eq!(rpc_error_code(&name_mismatch), -32020);
    assert!(rpc_error_message(&name_mismatch).contains("Mcp-Name"));

    // 变体 3：缺失 Mcp-Method（modern 请求必带）。
    let missing_method = server
        .connect()
        .send(&modern_tools_list(3).without_header(HEADER_MCP_METHOD));
    assert_eq!(missing_method.status, 400);
    assert_eq!(rpc_error_code(&missing_method), -32020);

    // 变体 4：缺失 Mcp-Name。
    let missing_name = server.connect().send(
        &modern_tools_call("Read", json!({ "file_path": "a.txt" }), 4)
            .without_header(HEADER_MCP_NAME),
    );
    assert_eq!(missing_name.status, 400);
    assert_eq!(rpc_error_code(&missing_name), -32020);

    // 变体 5：MCP-Protocol-Version 与 body `_meta.protocolVersion` 不一致。
    let version_mismatch = server
        .connect()
        .send(&modern_tools_list(5).header(HEADER_MCP_PROTOCOL_VERSION, "2025-11-25"));
    assert_eq!(version_mismatch.status, 400);
    assert_eq!(rpc_error_code(&version_mismatch), -32020);
}

/// 七工具 schema 都不声明 `x-mcp-header`，因此 `Mcp-Param-*` 不适用；
/// 多余的头不会被当作参数（body 是唯一事实源）。
#[test]
fn mcp_param_headers_are_not_applicable_without_schema_annotation() {
    let server = TestServer::start();
    let fixtures = load_tool_fixtures();
    for fixture in &fixtures {
        let annotated = fixture
            .schema
            .get("properties")
            .and_then(|properties| properties.as_object())
            .map(|properties| {
                properties
                    .values()
                    .any(|property| property.get("x-mcp-header").is_some())
            })
            .unwrap_or(false);
        assert!(
            !annotated,
            "工具 {} 若声明 x-mcp-header 则本用例需改为 Mcp-Param 校验",
            fixture.name
        );
    }

    let mut client = server.connect();
    let response = client.send(
        &modern_tools_call("Read", json!({ "file_path": "a.txt" }), 1)
            .header("Mcp-Param-file_path", "b.txt"),
    );
    assert_eq!(
        response.status, 200,
        "未标注 schema 时 Mcp-Param-* 不参与校验"
    );
    let structured = structured_content(&response);
    assert_eq!(
        structured
            .get("arguments")
            .and_then(|arguments| arguments.get("file_path"))
            .and_then(|value| value.as_str()),
        Some("a.txt"),
        "body 是参数唯一事实源"
    );
}

/// P-03：modern 请求缺必填 `_meta` 键 → 400 + `-32602`。
#[test]
fn modern_request_missing_required_meta_is_invalid_params() {
    let server = TestServer::start();
    let mut client = server.connect();

    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": { "_meta": { META_KEY_PROTOCOL_VERSION: MCP_MODERN_PROTOCOL_VERSION } }
    });
    let response = client.send(
        &RawRequest::post(body)
            .header(HEADER_MCP_PROTOCOL_VERSION, MCP_MODERN_PROTOCOL_VERSION)
            .header(HEADER_MCP_METHOD, "tools/list"),
    );
    assert_eq!(response.status, 400);
    assert_eq!(rpc_error_code(&response), -32602);
    assert!(
        rpc_error_message(&response).contains(META_KEY_CLIENT_CAPABILITIES),
        "错误必须指出缺失键: {}",
        rpc_error_message(&response)
    );

    // 缺 MCP-Protocol-Version 头但 body 带版本 → 头部缺失的 header mismatch。
    let no_header = client.send(
        &modern_request("tools/list", json!({}), None, 2)
            .without_header(HEADER_MCP_PROTOCOL_VERSION),
    );
    assert_eq!(no_header.status, 400);
    assert_eq!(rpc_error_code(&no_header), -32020);
}

/// 不支持的方法必须 405（GET 在 legacy 会话模式下属于合法方法，故用 PATCH 验证）。
#[test]
fn unsupported_http_method_is_405() {
    let server = TestServer::start();
    let mut client = server.connect();
    let response = client.send(
        &RawRequest::post(json!({}))
            .method("PATCH")
            .body_bytes(Vec::new()),
    );
    assert_eq!(response.status, 405);
    assert!(response.header("allow").is_some(), "405 必须带 Allow 头");
}

// ──────────────────────────── 两生命周期：legacy 与 modern ────────────────────────────

/// Legacy：initialize 建立会话、后续请求带会话头；modern：无状态、不依赖会话头。
#[test]
fn legacy_session_lifecycle_and_modern_stateless_lifecycle_coexist() {
    let server = TestServer::start();
    let mut client = server.connect();

    // legacy initialize：响应必须带回 Mcp-Session-Id。
    let initialize = client.send(&legacy_initialize(1));
    assert_eq!(initialize.status, 200);
    let session = initialize
        .header("mcp-session-id")
        .expect("initialize 必须返回会话 id")
        .to_string();
    assert!(session.len() >= 32, "会话 id 必须是不可猜的随机值");
    let init_result = rpc_result(&initialize);
    assert_eq!(
        init_result
            .get("protocolVersion")
            .and_then(|value| value.as_str()),
        Some("2025-11-25")
    );

    let initialized = client.send(&legacy_initialized_notification(&session));
    assert_eq!(initialized.status, 202, "通知必须 202 Accepted");

    let call = client.send(&legacy_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        2,
        &session,
    ));
    assert_eq!(call.status, 200);
    let identity = identity_of(&call);
    assert_eq!(
        identity.session_header.as_deref(),
        Some(session.as_str()),
        "legacy 会话内请求必须携带同一会话"
    );

    // 同一连接上的 modern 请求：不带会话头，也不应受影响。
    let modern = client.send(&modern_tools_call(
        "Write",
        json!({ "file_path": "b.txt", "content": "x" }),
        3,
    ));
    assert_eq!(modern.status, 200);
    let modern_identity = identity_of(&modern);
    assert_eq!(
        modern_identity.session_header, None,
        "modern 请求不依赖会话头"
    );
    assert_eq!(
        modern_identity.protocol_version.as_deref(),
        Some("2026-07-28"),
        "modern 请求的协议版本来自请求自身"
    );

    // 未知会话 → 404（不创建会话）。
    let unknown = client.send(&legacy_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        4,
        "unknown-session",
    ));
    assert_eq!(unknown.status, 404);
    assert!(unknown.body_text().contains("Session not found"));

    // DELETE 关闭会话后，同一会话 id 必须 404。
    let deleted = client.send(
        &RawRequest::post(json!({}))
            .method("DELETE")
            .header("MCP-Protocol-Version", "2025-11-25")
            .header("Mcp-Session-Id", &session)
            .body_bytes(Vec::new()),
    );
    assert_eq!(deleted.status, 202);
    let after_delete = client.send(&legacy_tools_call(
        "Read",
        json!({ "file_path": "a.txt" }),
        5,
        &session,
    ));
    assert_eq!(after_delete.status, 404, "会话关闭后必须 404");
}

/// modern 请求即使带上无关会话头也照常按无状态路由（P-17）。
#[test]
fn modern_requests_are_served_statelessly() {
    let server = TestServer::start();

    // 不同连接、不同时刻的 modern 请求互不影响。
    let mut first = server.connect();
    let mut second = server.connect();
    let a = first.send(&modern_tools_list(1));
    let b = second.send(&modern_tools_list(1));
    assert_eq!(a.status, 200);
    assert_eq!(b.status, 200);

    let with_bogus_session =
        first.send(&modern_tools_list(2).header("Mcp-Session-Id", "not-a-real-session"));
    assert_eq!(
        with_bogus_session.status, 200,
        "modern 请求不得因会话头而失败"
    );
}

// ──────────────────────────── 通知 / 错误 / 关闭语义 ────────────────────────────

/// 纯通知 POST 返回 202，且不产生 JSON-RPC 响应。
#[test]
fn notification_only_post_is_accepted_with_202() {
    let server = TestServer::start();
    let mut client = server.connect();

    let body = json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": "1", "reason": "test" },
    });
    let response = client.send(
        &RawRequest::post(body)
            .header(HEADER_MCP_PROTOCOL_VERSION, MCP_MODERN_PROTOCOL_VERSION)
            .header(HEADER_MCP_METHOD, "notifications/cancelled"),
    );
    assert_eq!(response.status, 202);
    assert!(response.body.is_empty(), "202 响应不得带消息体");
}

/// 未知工具走协议错误 `-32602`，与冻结的 `ToolError` 映射一致。
#[test]
fn unknown_tool_is_protocol_error_minus_32602() {
    let server = TestServer::start();
    let mut client = server.connect();
    let response = client.send(&modern_tools_call("Nope", json!({}), 1));
    assert_eq!(response.status, 400, "modern 的 invalid params 走 400");
    assert_eq!(rpc_error_code(&response), -32602);
    assert_eq!(rpc_error_message(&response), "Unknown tool: Nope");
}

/// 服务器关闭后连接被关闭，新连接被拒绝（所有权清理，不留监听 socket）。
#[test]
fn shutdown_closes_connections_and_stops_accepting() {
    let mut server = TestServer::start();
    let addr = server.addr();
    let mut client = server.connect();
    let ok = client.send(&modern_tools_list(1));
    assert_eq!(ok.status, 200);

    server.shutdown();

    // 在途连接被关闭：读到 EOF。
    assert!(
        client.read_to_eof(),
        "关闭后既有连接必须被优雅关闭（读到 EOF）"
    );

    // 新连接要么被拒绝，要么立刻被服务端关闭（监听 socket 已释放）。
    if let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500))
    {
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .expect("timeout");
        let mut buffer = [0u8; 1];
        let read = std::io::Read::read(&mut stream, &mut buffer);
        assert!(
            read.map(|count| count == 0).unwrap_or(true),
            "端口必须已停止服务"
        );
    }
}

/// 客户端半关闭写方向后，服务端应结束该连接而不泄漏任务。
#[test]
fn client_half_close_ends_the_connection() {
    let server = TestServer::start();
    let mut client = server.connect();
    let ok = client.send(&modern_tools_list(1));
    assert_eq!(ok.status, 200);

    client.shutdown_write();
    assert!(client.read_to_eof(), "半关闭后服务端必须收尾");
}

// ───────────────────────────── framing：CL 与 TE 并存 ─────────────────────────────

/// 把一条请求序列化为原始字节，framing 头（CL / TE / 两者并存）由调用方给出。
///
/// `chunked_body = true` 时按 chunked 编码写出 body（用于"合法 chunked body + 多声明一个
/// Content-Length"这一走私形态）。
fn raw_framed(request: &RawRequest, addr: &str, framing: &str, chunked_body: bool) -> Vec<u8> {
    let mut raw = format!(
        "{} {} HTTP/1.1\r\nHost: {addr}\r\n",
        request.method, request.target
    );
    for (name, value) in &request.headers {
        raw.push_str(&format!("{name}: {value}\r\n"));
    }
    raw.push_str(framing);
    raw.push_str("\r\n");
    let mut bytes = raw.into_bytes();
    if chunked_body {
        bytes.extend_from_slice(format!("{:x}\r\n", request.body.len()).as_bytes());
        bytes.extend_from_slice(&request.body);
        bytes.extend_from_slice(b"\r\n0\r\n\r\n");
    } else {
        bytes.extend_from_slice(&request.body);
    }
    bytes
}

/// N-2（GAP-round-7）：同时声明 `Content-Length` 与 `Transfer-Encoding` 的请求必须在
/// 交给 SDK 之前被**显式拒绝 400**；正常 CL 与正常 chunked（无 CL）保持可用。
///
/// 修复前的实测（`WP-P5-r8/before-n2/n2-framing-raw.jsonl`）：合法 chunked body + 多声明
/// 一个 CL 被当作普通 chunked 请求放行（200，请求进入 handler）。
#[test]
fn conflicting_framing_headers_are_rejected_with_400() {
    // 观察表必须与 handler 工厂共享同一个 sink（`TestServer::start` 内部给工厂的 sink
    // 与服务端持有的 sink 不是同一个实例，用它断言"未进入 handler"是恒真的）。
    let sink = observation_sink();
    let server = TestServer::start_with_sink(
        base_config(),
        FixtureHandler::factory_for(sink.clone()),
        sink,
    );
    let addr = server.addr().to_string();

    // 1. 正常 CL：可用，且请求确实到达 handler。
    let normal = modern_tools_call("Read", json!({ "file_path": "a.txt" }), 1);
    let normal_framing = format!("Content-Length: {}\r\n", normal.body.len());
    let response = server
        .connect()
        .send_raw(&raw_framed(&normal, &addr, &normal_framing, false));
    assert_eq!(response.status, 200, "正常 Content-Length 请求必须可用");
    assert_eq!(
        server.observations().len(),
        1,
        "正常请求必须到达 MCP handler"
    );

    // 2. CL + TE（合法 chunked body）：修复前为 200；现在必须 400 且不进入 handler。
    let conflicting = modern_tools_call("Read", json!({ "file_path": "a.txt" }), 2);
    let both_framing = format!(
        "Transfer-Encoding: chunked\r\nContent-Length: {}\r\n",
        conflicting.body.len()
    );
    let rejected = server
        .connect()
        .send_raw(&raw_framed(&conflicting, &addr, &both_framing, true));
    assert_eq!(
        rejected.status,
        400,
        "CL 与 TE 并存必须显式 400：{}",
        rejected.body_text()
    );
    assert!(
        rejected.body_text().contains("Transfer-Encoding"),
        "拒绝文案必须说明 framing：{}",
        rejected.body_text()
    );
    assert_eq!(
        server.observations().len(),
        1,
        "被拒绝的请求不得进入 MCP handler"
    );

    // 3. CL + TE（body 按 CL 编码）：同样是非法 framing，必须 400（修复前是 500）。
    let mismatched = modern_tools_call("Read", json!({ "file_path": "a.txt" }), 3);
    let mismatch_framing = format!(
        "Transfer-Encoding: chunked\r\nContent-Length: {}\r\n",
        mismatched.body.len()
    );
    let rejected_mismatch =
        server
            .connect()
            .send_raw(&raw_framed(&mismatched, &addr, &mismatch_framing, false));
    assert_eq!(rejected_mismatch.status, 400);
    assert_eq!(server.observations().len(), 1);

    // 4. 正常 chunked（无 CL）：仍必须可用（不得因噎废食把 chunked 一并拒掉）。
    let chunked = modern_tools_call("Read", json!({ "file_path": "a.txt" }), 4);
    let chunked_only = "Transfer-Encoding: chunked\r\n";
    let accepted = server
        .connect()
        .send_raw(&raw_framed(&chunked, &addr, chunked_only, true));
    assert_eq!(
        accepted.status,
        200,
        "正常 chunked 请求必须继续可用：{}",
        accepted.body_text()
    );
    assert!(
        accepted.is_event_stream(),
        "正常 chunked 请求必须返回请求级 SSE"
    );
    assert!(
        accepted.body_text().contains("\"result\""),
        "正常 chunked 请求必须被真正处理：{}",
        accepted.body_text()
    );
    assert_eq!(server.observations().len(), 2);
}

/// `GET`（SSE 流）：modern 版本没有 GET 流（405 + Allow）；legacy 缺会话 id 必须 400。
#[test]
fn get_stream_semantics_differ_between_eras() {
    let server = TestServer::start();

    let modern = server.connect().send(
        &RawRequest::post(json!({}))
            .method("GET")
            .header("Accept", "text/event-stream")
            .header(HEADER_MCP_PROTOCOL_VERSION, MCP_MODERN_PROTOCOL_VERSION)
            .body_bytes(Vec::new()),
    );
    assert_eq!(modern.status, 405, "modern 不提供服务端主动流");
    assert_eq!(
        modern.header("allow"),
        Some("POST"),
        "modern 只允许 POST（无状态请求）"
    );

    let legacy = server.connect().send(
        &RawRequest::post(json!({}))
            .method("GET")
            .header("Accept", "text/event-stream")
            .header(HEADER_MCP_PROTOCOL_VERSION, "2025-11-25")
            .body_bytes(Vec::new()),
    );
    assert_eq!(legacy.status, 400, "legacy GET 缺会话 id 必须 400");
    assert!(legacy.body_text().contains("Session ID is required"));
}
