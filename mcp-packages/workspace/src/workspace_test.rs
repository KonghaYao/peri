//! 工具调用面的失败投影证据（handler 侧接线：唯一共享助手 `invoke_tool_call`）：
//! 未知工具按协议错误收口（`-32602`，不落到任何已注册工具）；工具执行错误以
//! `is_error` 结果返回，文本为固定规则 + allowlist 恢复原因（不透传路径形状串或输入值）。
//!
//! 从 `peri-middlewares` 的 host wire 证据下沉（2026-09-30）：同一断言主体（工具语义 +
//! 共享投影）不依赖 host 装配，本包直接经 handler 的工具表验证。

use super::*;
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{json, Value};

/// `tools/call` 请求（`arguments` 必须是 JSON object；缺省 = 空对象）。
fn request(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

/// 断言响应是 `Complete` 并取出结果（工具级失败**不是**协议 `Err`）。
fn complete(response: CallToolResponse) -> CallToolResult {
    match response {
        CallToolResponse::Complete(result) => result,
        other => panic!("expected Complete result, got: {other:?}"),
    }
}

fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

fn server_in(dir: &tempfile::TempDir) -> WorkspaceMcpServer {
    WorkspaceMcpServer::new(dir.path().to_string_lossy().to_string(), None)
}

#[tokio::test]
async fn unknown_tool_is_invalid_params() {
    let dir = tempfile::tempdir().expect("临时目录夹具必须可创建");
    let server = server_in(&dir);

    let error = invoke_tool_call(
        server.tools(),
        &server.cwd,
        &request("NoSuchTool", json!({})),
    )
    .await
    .expect_err("未知工具名必须按协议错误收口");

    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert!(
        error.message.contains("unknown tool: NoSuchTool"),
        "错误文本必须含未知工具名；实际：{}",
        error.message
    );
}

#[tokio::test]
async fn tool_failures_return_sanitized_error_result() {
    let dir = tempfile::tempdir().expect("临时目录夹具必须可创建");
    let server = server_in(&dir);

    // ① 缺必填参数：固定规则文本含工具名，恢复原因点明缺参（不透传输入值）。
    let missing = complete(
        invoke_tool_call(server.tools(), &server.cwd, &request("Read", json!({})))
            .await
            .expect("工具级失败仍走协议成功（is_error 承载语义）"),
    );
    assert_eq!(missing.is_error, Some(true));
    let missing_text = first_text(&missing).expect("错误结果必须有文本块");
    assert!(
        missing_text.contains("tool `Read` failed to execute"),
        "固定规则文本必须含工具名：{missing_text}"
    );
    assert!(
        missing_text.contains("file_path") && missing_text.contains("required"),
        "缺参恢复原因必须点明参数与 required：{missing_text}"
    );

    // ② 路径不存在且含路径形状哨兵：原始错误不得进入模型面文本，只给固定的安全原因。
    const LEAK_PATH: &str = "/tmp/secret-marker/private.txt";
    let leaky = complete(
        invoke_tool_call(
            server.tools(),
            &server.cwd,
            &request("Read", json!({ "file_path": LEAK_PATH })),
        )
        .await
        .expect("工具级失败仍走协议成功"),
    );
    assert_eq!(leaky.is_error, Some(true));
    let leaky_text = first_text(&leaky).expect("错误结果必须有文本块");
    assert_ne!(leaky_text, missing_text, "不同失败必须给出不同的安全原因");
    assert!(
        leaky_text.contains("File not found"),
        "不存在路径必须给固定原因：{leaky_text}"
    );
    assert!(
        !leaky_text.contains(LEAK_PATH),
        "模型面文本不得含路径形状串：{leaky_text}"
    );

    // ③ Bash 的失败路径同样经共享投影（含命令文本的原始错误不得外泄）。
    let bash = complete(
        invoke_tool_call(server.tools(), &server.cwd, &request("Bash", json!({})))
            .await
            .expect("工具级失败仍走协议成功"),
    );
    assert_eq!(bash.is_error, Some(true));
    let bash_text = first_text(&bash).expect("错误结果必须有文本块");
    assert!(
        bash_text.contains("tool `Bash` failed to execute"),
        "固定规则文本必须含工具名：{bash_text}"
    );
}
