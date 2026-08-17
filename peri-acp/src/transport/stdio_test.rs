//! Tests for stdio transport —— 信封单测 + 集成测试（批 0 基线）。
//!
//! 集成测试经 `StdioTransport::from_reader_writer`（可测性重构，`new()`/
//! `Default` 行为不变）注入 `tokio::io::duplex` 读写端驱动：
//! stdin pump 逐行解析、stdout 写入形态、send_request id 配对（含乱序）、
//! 并发交错、EOF 关闭语义、非法 JSON 跳过、域外 id 拒绝行为、失败语义。

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

// ── 信封/序列化单测（既有基线保留）───────────────────────────────────────────

#[test]
fn test_envelope_roundtrip_response() {
    let json = r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok"}}"#;
    let envelope: JsonRpcEnvelope = serde_json::from_str(json).unwrap();
    assert_eq!(envelope.jsonrpc, "2.0");
    assert_eq!(envelope.id, Some(Value::Number(1.into())));
    assert!(envelope.result.is_some());
    assert!(envelope.error.is_none());
    let back = serde_json::to_string(&envelope).unwrap();
    assert!(back.contains("\"result\""));
}

#[test]
fn test_envelope_roundtrip_request() {
    let json = r#"{"jsonrpc":"2.0","id":42,"method":"session/prompt","params":{"msg":"hi"}}"#;
    let envelope: JsonRpcEnvelope = serde_json::from_str(json).unwrap();
    assert_eq!(envelope.method.as_deref(), Some("session/prompt"));
}

#[test]
fn test_envelope_roundtrip_notification() {
    let json = r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"session_id":"s1"}}"#;
    let envelope: JsonRpcEnvelope = serde_json::from_str(json).unwrap();
    assert!(envelope.id.is_none());
    assert_eq!(envelope.method.as_deref(), Some("session/cancel"));
}

#[test]
fn test_request_id_conversion() {
    let v = Value::Number(42.into());
    let id = value_to_request_id(&v);
    assert_eq!(id, RequestId::Number(42));
    let back = request_id_to_value(&id);
    assert_eq!(back, v);
}

// ── 辅助：duplex 驱动的 transport ──────────────────────────────────────────
//
// 返回 (transport, input, output)：
// - `input`：测试写入 stdin 报文的一端（drop 即模拟 EOF/断线）
// - `output`：测试读取 transport 写往 stdout 的一端（drop 即模拟 stdout 断裂）
fn duplex_transport() -> (StdioTransport, DuplexStream, DuplexStream) {
    let (input_write, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, output_read) = tokio::io::duplex(64 * 1024);
    let transport = StdioTransport::from_reader_writer(transport_read, transport_write);
    (transport, input_write, output_read)
}

/// 往 stdin（input 端）写入一行 JSON-RPC 报文。
async fn write_line(stream: &mut DuplexStream, line: &str) {
    stream.write_all(line.as_bytes()).await.unwrap();
    stream.write_all(b"\n").await.unwrap();
}

/// 从 stdout（output 端）读取一行报文（按 `\n` 分帧，与 pump 对称）。
async fn read_line(stream: &mut DuplexStream) -> String {
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
}

/// recv() + 超时防挂起。
async fn recv(transport: &StdioTransport) -> Option<IncomingMessage> {
    tokio::time::timeout(Duration::from_secs(5), transport.recv())
        .await
        .expect("recv 超时")
}

// ── stdin pump：报文类型解析 + EOF ─────────────────────────────────────────

/// stdin 逐行解析 → IncomingMessage 三态（Request/Notification/Response），
/// 未匹配的 Response 走转发通道；EOF 后 pump 退出、`recv()` 返回 None。
#[tokio::test]
async fn test_pump_parses_three_message_kinds_and_eof_closes() {
    let (transport, mut input, _output) = duplex_transport();
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":7,"method":"m/req","params":{"a":1}}"#,
    )
    .await;
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","method":"m/notif","params":{"b":2}}"#,
    )
    .await;
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":999,"result":{"ok":true}}"#,
    )
    .await;
    drop(input); // EOF → pump 退出

    match recv(&transport).await {
        Some(IncomingMessage::Request { id, method, params }) => {
            assert_eq!(id, RequestId::Number(7));
            assert_eq!(method, "m/req");
            assert_eq!(params, json!({"a": 1}));
        }
        other => panic!("期望第一个为 Request，实际 {other:?}"),
    }
    match recv(&transport).await {
        Some(IncomingMessage::Notification { method, params }) => {
            assert_eq!(method, "m/notif");
            assert_eq!(params, json!({"b": 2}));
        }
        other => panic!("期望第二个为 Notification，实际 {other:?}"),
    }
    match recv(&transport).await {
        Some(IncomingMessage::Response { id, result }) => {
            assert_eq!(id, RequestId::Number(999), "未匹配 id → 转发而非路由吞掉");
            assert_eq!(result.expect("应为成功响应"), json!({"ok": true}));
        }
        other => panic!("期望第三个为 Response，实际 {other:?}"),
    }
    assert!(
        recv(&transport).await.is_none(),
        "EOF 后 pump 退出（sender drop）→ recv() 返回 None"
    );
}

/// 空行被跳过、非法 JSON 行仅 error 日志后继续——不中断后续报文解析。
#[tokio::test]
async fn test_invalid_json_line_skipped_keeps_parsing() {
    let (transport, mut input, _output) = duplex_transport();
    write_line(&mut input, "").await;
    write_line(&mut input, "this is not json {").await;
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":1,"method":"m/req","params":{}}"#,
    )
    .await;
    drop(input);

    match recv(&transport).await {
        Some(IncomingMessage::Request { id, method, .. }) => {
            assert_eq!(id, RequestId::Number(1));
            assert_eq!(method, "m/req");
        }
        other => panic!("非法行应被跳过、后续合法报文正常解析，实际 {other:?}"),
    }
    assert!(recv(&transport).await.is_none());
}

// ── stdout 写入形态 ─────────────────────────────────────────────────────────

/// `send_request`：写出 `{jsonrpc, id, method, params}`（无 result/error），
/// 响应经 pump→router 按 id 配对回落到 await 处。
#[tokio::test]
async fn test_send_request_wire_shape_and_id_pairing() {
    let (transport, mut input, mut output) = duplex_transport();
    let transport = Arc::new(transport);
    let task = tokio::spawn({
        let t = Arc::clone(&transport);
        async move { t.send_request("m/echo", json!({"hello": "world"})).await }
    });

    let req: Value = serde_json::from_str(&read_line(&mut output).await).unwrap();
    assert_eq!(req["jsonrpc"], "2.0");
    assert_eq!(req["id"], 1, "独立 router 首 id = 1");
    assert_eq!(req["method"], "m/echo");
    assert_eq!(req["params"], json!({"hello": "world"}));
    assert!(
        req.get("result").is_none() && req.get("error").is_none(),
        "send_request 不应携带 result/error 字段: {req}"
    );

    // 回写同名 id 的响应 → send_request 恢复
    let resp = format!(r#"{{"jsonrpc":"2.0","id":{},"result":"pong"}}"#, req["id"]);
    write_line(&mut input, &resp).await;
    match tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
    {
        Ok(Ok(v)) => assert_eq!(v, json!("pong")),
        Ok(Err(e)) => panic!("send_request 应成功: {e}"),
        Err(e) => panic!("task panic: {e}"),
    }
}

/// `send_response`（Ok/Err 两形态）与 `send_notification` 的 stdout 写入形态。
#[tokio::test]
async fn test_send_response_and_notification_wire_shapes() {
    let (transport, _input, mut output) = duplex_transport();

    transport
        .send_response(RequestId::Number(7), Ok(json!({"status": "ok"})))
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&read_line(&mut output).await).unwrap();
    assert_eq!(v["id"], 7, "send_response 按请求 id 回写");
    assert_eq!(v["result"], json!({"status": "ok"}));
    assert!(v.get("method").is_none(), "响应不应含 method: {v}");
    assert!(v.get("error").is_none(), "成功响应不应含 error: {v}");

    transport
        .send_response(RequestId::Number(7), Err(AcpError::new(-32000, "boom")))
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&read_line(&mut output).await).unwrap();
    assert_eq!(v["error"]["code"], -32000);
    assert_eq!(v["error"]["message"], "boom");
    assert!(v.get("result").is_none(), "错误响应不应含 result: {v}");

    transport
        .send_notification("m/n", json!({"x": 1}))
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&read_line(&mut output).await).unwrap();
    assert_eq!(v["method"], "m/n");
    assert_eq!(v["params"], json!({"x": 1}));
    assert!(v.get("id").is_none(), "通知不应含 id: {v}");
}

// ── 并发 / 乱序 id 配对 ─────────────────────────────────────────────────────

/// 并发多报文交错 + 乱序 id 匹配（RequestRouter 语义）：
/// 5 个并发 send_request，响应按与请求相反的 id 顺序回写，各自恢复。
#[tokio::test]
async fn test_concurrent_requests_out_of_order_id_matching() {
    let (transport, mut input, mut output) = duplex_transport();
    let transport = Arc::new(transport);

    let n = 5;
    let mut tasks = Vec::new();
    for i in 0..n {
        let t = Arc::clone(&transport);
        tasks.push(tokio::spawn(async move {
            (i, t.send_request("m/echo", json!({"i": i})).await)
        }));
    }

    // 先读全 5 条请求行（保证全部已注册 pending），取 id 与原始 params
    let mut ids = Vec::new();
    let mut param_i = Vec::new();
    for _ in 0..n {
        let msg: Value = serde_json::from_str(&read_line(&mut output).await).unwrap();
        ids.push(msg["id"].as_i64().expect("send_request id 应为数字"));
        param_i.push(msg["params"]["i"].as_i64().expect("params.i 应为数字"));
    }

    // 乱序回写响应（反转 id 顺序）
    for idx in (0..n).rev() {
        let resp = json!({"echo": param_i[idx]});
        write_line(
            &mut input,
            &format!(r#"{{"jsonrpc":"2.0","id":{},"result":{}}}"#, ids[idx], resp),
        )
        .await;
    }

    // 每个 future 应拿到与自身 id 配对的响应（Router 按 id 匹配，不依赖到达顺序）
    for task in tasks {
        let (i, res) = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("send_request 超时")
            .expect("task panic");
        let v = res.unwrap_or_else(|e| panic!("请求 {i} 应成功: {e}"));
        assert_eq!(v, json!({"echo": i}), "id 配对应精确，请求 {i}");
    }
}

// ── EOF / 失败语义 ──────────────────────────────────────────────────────────

/// EOF 后 pump 退出、`recv()` 返回关闭；但已发出的 pending send_request
/// 不被自动失败（当前语义：关闭只在 recv() 侧可见，见 stdio.rs pump 退出
/// 依赖 channel 关闭的间接语义）——基线记录，供统一后对照。
#[tokio::test]
async fn test_pending_request_waits_after_eof() {
    let (input_write, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, mut output_read) = tokio::io::duplex(64 * 1024);
    let transport = StdioTransport::from_reader_writer(transport_read, transport_write);
    let req_task = tokio::spawn(async move { transport.send_request("m/echo", json!({})).await });

    // 请求行已写出后再模拟断线
    let _line = read_line(&mut output_read).await;
    drop(input_write); // EOF

    // recv() 关闭（pump 退出），但 pending 请求仍挂起等待——不自动失败
    let pending_after_eof = tokio::time::timeout(Duration::from_millis(150), req_task).await;
    assert!(
        pending_after_eof.is_err(),
        "EOF 不应自动失败 pending send_request（当前语义仅是 recv() 感知关闭）"
    );
}

/// `send_request` 无内置超时：对端不响应时挂起等待（而不是超时失败）。
#[tokio::test]
async fn test_send_request_has_no_builtin_timeout() {
    let (transport, _input, _output) = duplex_transport();
    let result = tokio::time::timeout(
        Duration::from_millis(150),
        transport.send_request("m/mute", json!({})),
    )
    .await;
    assert!(
        result.is_err(),
        "send_request 应挂起而非超时失败（基线：无内置超时语义）"
    );
}

/// 失败语义：stdout 断裂（对端读端已 drop）→ 写失败立即返回
/// `-32603 "Write failed: ..."`。
#[tokio::test]
async fn test_send_request_write_failure_on_broken_stdout() {
    let (_input_write, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, output_read) = tokio::io::duplex(64 * 1024);
    drop(output_read); // 模拟 stdout 已断开
    let transport = StdioTransport::from_reader_writer(transport_read, transport_write);

    let err = transport
        .send_request("m/echo", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32603);
    assert!(
        err.message.contains("broken pipe"),
        "stdout 断裂应报写/冲刷失败（broken pipe）: {err}"
    );
}

// ── id 域约束：`is_domain_id` 校验 + pump 拒绝域外 id（决策点 7 收口）──
//
// agent-client-protocol-schema 的 `RequestId` = `Null | Number(i64) |
// Str(String)`（`rpc.rs:42`，JSON-RPC 2.0 §5：id 仅 String/Number/Null，
// Number 不应含小数）。`transport::types::RequestId` = `String |
// Number(i64)`（无 Null）——域已覆盖 schema 可表示子集。批 0-3 对**域外**
// 值（小数、u64 溢出 i64、null、bool 等）静默压 `0`（`as_i64().unwrap_or(0)`），
// 非保真、且与合法 id 0 存在碰撞风险（router 配对/宿主 `send_response(0, ...)`
// 将响错对象）。批 4 收口（docs/design/acp-host-unify.md §10 决策点 7）：
// pump 对入站域外 id **拒绝该行**（warn + 丢弃，不中断 pump，与非法 JSON 行
// 处理语义一致）；id 为 null 的 Request 按 JSON-RPC 2.0 §2.2 视为通知。
// `send_request` 侧 id 由内部 `RequestId` 生成恒合法，行为不改。

#[test]
fn test_request_id_domain_validation() {
    // 域内：整数 Number(i64) 与 String 保真
    assert!(is_domain_id(&json!(0i64)));
    assert!(is_domain_id(&json!(42i64)));
    assert!(is_domain_id(&json!(-1i64)));
    assert!(is_domain_id(&json!("req-1")));
    // 域外：小数 / u64 溢出 i64 / null / bool（协议违规）
    assert!(!is_domain_id(&json!(1.5)), "小数 id 域外");
    assert!(
        !is_domain_id(&json!(18446744073709551615u64)),
        "u64 溢出 i64 域外"
    );
    assert!(
        !is_domain_id(&Value::Null),
        "null id 域外（pump 按通知单独处理）"
    );
    assert!(!is_domain_id(&Value::Bool(true)), "bool id 域外");
}

#[test]
fn test_request_id_string_conversion_fidelity() {
    // String id 保真往返（Number 往返由既有 test_request_id_conversion 覆盖）
    assert_eq!(
        value_to_request_id(&json!("req-1")),
        RequestId::String("req-1".into())
    );
    assert_eq!(
        request_id_to_value(&RequestId::String("req-1".into())),
        json!("req-1")
    );
}

/// 经 pump 的实际观察（决策点 7 收口）：String id 行保真；域外 id（小数 /
/// bool）行被拒绝（不产生 IncomingMessage、pump 不中断）；id 为 null 的
/// Request 按 JSON-RPC 2.0 §2.2 视为通知。
#[tokio::test]
async fn test_pump_rejects_out_of_domain_ids_and_null_id_becomes_notification() {
    let (transport, mut input, _output) = duplex_transport();
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":"req-auto","method":"m/req","params":{}}"#,
    )
    .await;
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":1.5,"method":"m/req","params":{}}"#,
    )
    .await;
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":true,"result":"orphan"}"#,
    )
    .await;
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":null,"method":"m/notif","params":{"c":3}}"#,
    )
    .await;
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":7,"method":"m/req2","params":{}}"#,
    )
    .await;
    drop(input);

    match recv(&transport).await {
        Some(IncomingMessage::Request { id, .. }) => assert_eq!(
            id,
            RequestId::String("req-auto".into()),
            "String id 应保真穿过 pump"
        ),
        other => panic!("期望 String id Request，实际 {other:?}"),
    }
    match recv(&transport).await {
        Some(IncomingMessage::Notification { method, params }) => {
            assert_eq!(method, "m/notif");
            assert_eq!(params, json!({"c": 3}));
        }
        other => panic!("null id Request 应视为通知（非压 0 请求），实际 {other:?}"),
    }
    match recv(&transport).await {
        Some(IncomingMessage::Request { id, method, .. }) => {
            assert_eq!(id, RequestId::Number(7), "后续合法请求正常解析");
            assert_eq!(method, "m/req2");
        }
        other => panic!("域外 id 行应被拒绝、pump 不中断，实际 {other:?}"),
    }
    assert!(recv(&transport).await.is_none(), "EOF 后 pump 退出");
}

// ── legacy type:cancel（批 3 §7 #10 移植）──────────────────────────────────

/// pump 对 `{"type":"cancel"}` 行（非 JSON-RPC）拦截：注入的 hook 收到原始行，
/// 该行不产生任何 IncomingMessage；随后的合法报文仍正常解析（pump 不中断）。
#[tokio::test]
async fn test_type_cancel_line_invokes_hook_and_produces_no_message() {
    let calls: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let hook_calls = Arc::clone(&calls);
    let hook = Arc::new(move |line: &str| {
        hook_calls.lock().unwrap().push(line.to_string());
    });
    let (transport, mut input, _output) = duplex_transport_with_hook(Some(hook));

    write_line(&mut input, r#"{"type":"cancel"}"#).await;
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":1,"method":"m/req","params":{}}"#,
    )
    .await;
    drop(input);

    // type:cancel 行不产生 IncomingMessage：首条应为后续合法请求
    match recv(&transport).await {
        Some(IncomingMessage::Request { id, method, .. }) => {
            assert_eq!(id, RequestId::Number(1));
            assert_eq!(method, "m/req");
        }
        other => panic!("type:cancel 不应占用消息流，期望后续请求，实际 {other:?}"),
    }
    // hook 收到原始行（pump 传原始行、精确 trim 匹配，与迁移前
    // cancel_debug_hook 一致）
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded, vec![r#"{"type":"cancel"}"#.to_string()]);
}

/// 同 `duplex_transport` 但允许注入 type:cancel hook。
fn duplex_transport_with_hook(
    hook: Option<CancelHook>,
) -> (StdioTransport, DuplexStream, DuplexStream) {
    let (input_write, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, output_read) = tokio::io::duplex(64 * 1024);
    let transport =
        StdioTransport::from_reader_writer_with_cancel_hook(transport_read, transport_write, hook);
    (transport, input_write, output_read)
}

/// 未注入 hook 时 `{"type":"cancel"}` 行静默跳过（不误报 invalid JSON）、
/// 不产生 IncomingMessage，后续合法报文解析不中断（与批 0
/// `test_invalid_json_line_skipped_keeps_parsing` 语义保持一致）。
#[tokio::test]
async fn test_type_cancel_without_hook_skipped_quietly() {
    let (transport, mut input, _output) = duplex_transport();
    write_line(&mut input, r#"{"type":"cancel"}"#).await;
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":2,"method":"m/req","params":{}}"#,
    )
    .await;
    drop(input);

    match recv(&transport).await {
        Some(IncomingMessage::Request { id, method, .. }) => {
            assert_eq!(id, RequestId::Number(2));
            assert_eq!(method, "m/req");
        }
        other => panic!("type:cancel（无 hook）应被静默跳过，期望后续请求，实际 {other:?}"),
    }
    assert!(recv(&transport).await.is_none());
}

/// `with_cancel_hook` 事后注入同样生效（构造后设置，共享槽位被 pump 读取）。
#[tokio::test]
async fn test_with_cancel_hook_after_construction_is_effective() {
    let calls: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let hook_calls = Arc::clone(&calls);
    let (mut input_write, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, _output_read) = tokio::io::duplex(64 * 1024);
    let transport = StdioTransport::from_reader_writer(transport_read, transport_write);
    let transport = transport.with_cancel_hook(Some(Arc::new(move |line: &str| {
        hook_calls.lock().unwrap().push(line.to_string());
    })));

    write_line(&mut input_write, r#"{"type":"cancel"}"#).await;
    write_line(
        &mut input_write,
        r#"{"jsonrpc":"2.0","id":3,"method":"m/req","params":{}}"#,
    )
    .await;
    drop(input_write);

    match recv(&transport).await {
        Some(IncomingMessage::Request { id, .. }) => assert_eq!(id, RequestId::Number(3)),
        other => panic!("期望后续请求，实际 {other:?}"),
    }
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded, vec![r#"{"type":"cancel"}"#.to_string()]);
}

/// 空白环绕的 type:cancel 行（trim 匹配）同样触发 hook。
#[tokio::test]
async fn test_type_cancel_line_trim_matching() {
    let calls: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let hook_calls = Arc::clone(&calls);
    let hook = Arc::new(move |line: &str| {
        hook_calls.lock().unwrap().push(line.to_string());
    });
    let (transport, mut input, _output) = duplex_transport_with_hook(Some(hook));

    write_line(&mut input, "  {\"type\":\"cancel\"}  ").await;
    write_line(
        &mut input,
        r#"{"jsonrpc":"2.0","id":4,"method":"m/req","params":{}}"#,
    )
    .await;
    drop(input);

    match recv(&transport).await {
        Some(IncomingMessage::Request { id, .. }) => assert_eq!(id, RequestId::Number(4)),
        other => panic!("期望后续请求，实际 {other:?}"),
    }
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec!["  {\"type\":\"cancel\"}  ".to_string()],
        "hook 收到原始行（trim 仅用于匹配）"
    );
}
