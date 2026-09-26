use std::fs;

use super::*;

/// 从落盘提示中提取临时文件路径。
/// 提示格式：`... saved to <path> — use Read tool ...`
fn extract_path_from_hint(hint: &str) -> &str {
    let prefix = "saved to ";
    let suffix = " — use Read";
    let path_start = hint.find(prefix).expect("应包含 'saved to'") + prefix.len();
    let path_end = hint[path_start..]
        .find(suffix)
        .map(|i| path_start + i)
        .unwrap_or(hint.len());
    &hint[path_start..path_end]
}

#[test]
fn test_truncate_content_超限时触发落盘() {
    // 生成 MAX_CONTENT_LINES + 1 行内容
    let lines: Vec<String> = (0..=MAX_CONTENT_LINES)
        .map(|i| format!("line {i}"))
        .collect();
    let full_content = lines.join("\n");
    let result = truncate_content(&full_content, MAX_CONTENT_LINES);
    // 截断提示存在（P2-3: 统一为英文）
    assert!(
        result.contains("Content truncated"),
        "应包含截断提示: {result}"
    );
    // 落盘提示存在
    assert!(result.contains("saved to "), "应包含落盘路径提示: {result}");
    // 验证落盘文件内容与原始完全一致
    let path = extract_path_from_hint(&result);
    let saved = fs::read_to_string(path).expect("落盘文件应存在");
    assert_eq!(saved, full_content, "落盘内容应与原始内容完全一致");
    fs::remove_file(path).ok();
}

#[test]
fn test_truncate_content_未超限时不落盘() {
    let content = "line1\nline2\nline3";
    let result = truncate_content(content, MAX_CONTENT_LINES);
    assert_eq!(result, content, "未超限时应原样返回");
    assert!(
        !result.contains("saved to "),
        "未超限时不应有落盘提示: {result}"
    );
}

#[test]
fn test_truncate_content_行数未超但字节超限_触发落盘() {
    // 单行超大内容（模拟 minified JS），行数不超限但字节远超 MAX_CONTENT_CHARS
    let single_line = "x".repeat(MAX_CONTENT_CHARS + 1000);
    let result = truncate_content(&single_line, MAX_CONTENT_LINES);
    assert!(
        result.contains("exceeds") || result.contains("Content truncated"),
        "应包含字节截断提示: {result}"
    );
    assert!(result.contains("saved to "), "应包含落盘路径提示: {result}");
    // 验证落盘文件内容与原始完全一致
    let path = extract_path_from_hint(&result);
    let saved = fs::read_to_string(path).expect("落盘文件应存在");
    assert_eq!(saved, single_line, "落盘内容应与原始内容完全一致");
    fs::remove_file(path).ok();
}

#[test]
fn test_truncate_content_多行但字节超限_触发落盘() {
    // 多行，行数不超限但总字节超限：1500 行 x 100 字节 = 150000 > MAX_CONTENT_CHARS
    let line_content = "a".repeat(99) + "\n";
    let content = line_content.repeat(1500);
    assert!(
        content.lines().count() <= MAX_CONTENT_LINES,
        "测试数据应行数不超限: actual={}",
        content.lines().count()
    );
    let result = truncate_content(&content, MAX_CONTENT_LINES);
    assert!(result.contains("saved to "), "应包含落盘路径提示: {result}");
    let path = extract_path_from_hint(&result);
    let saved = fs::read_to_string(path).expect("落盘文件应存在");
    assert_eq!(saved, content, "落盘内容应与原始内容完全一致");
    fs::remove_file(path).ok();
}

// ─── 真实 HTTP 发包（本地回环桩；无网络、无凭据） ─────────────────────────────
//
// 验收记录 §7 第 6 条的闭合点：此前本文件只覆盖 `truncate_content`（纯函数），
// `invoke` 的网络分支从未真正发出过 HTTP 请求。下面用 `with_endpoint_for_test`
// 把端点指向本地回环桩，让生产 `invoke` 真的经 reqwest 走 200 + 正文 / 非 2xx 分支。
mod real_http_round_trip {
    use peri_agent::tools::{BaseTool, ToolContext};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::middleware::web_fetch::WebFetchTool;

    /// 合法的 Tavily /extract 响应：**故意不含** `failed_results` —— 生产结构
    /// （`web_fetch.rs` 的 `TavilyExtractResponse`）上有 `#[serde(default)]`，
    /// 缺字段必须仍能解析。`url` 等未声明字段被 serde 忽略。
    const EXTRACT_OK_BODY: &str = r#"{
        "results": [
            {
                "url": "https://example.com/page",
                "raw_content": "This is the extracted content from the page."
            }
        ]
    }"#;

    /// 本地回环 HTTP 桩：接受一次连接，读到请求头后回一段固定原文响应。
    ///
    /// 返回 `(base_url, 请求头任务)`；任务的值是请求头原文，供断言请求行与「不发
    /// auth header」，**不得**打印（§9 规则 7：不打印 headers）。
    async fn spawn_response_stub(
        status_line: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("本地桩必须能绑定回环端口");
        let port = listener.local_addr().expect("本地桩地址可读").port();
        let handle = tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return String::new();
            };
            let mut received = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        received.extend_from_slice(&chunk[..read]);
                        if received.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let head = String::from_utf8_lossy(&received).into_owned();
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
            head
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    #[tokio::test]
    async fn test_webfetch_invoke_round_trips_real_http_200_body() {
        let (base_url, head) = spawn_response_stub("HTTP/1.1 200 OK", EXTRACT_OK_BODY).await;
        let tool = WebFetchTool::with_endpoint_for_test(&base_url);

        let output = tool
            .invoke(
                serde_json::json!({"url": "https://example.com/page"}),
                ToolContext::new(&[], "."),
            )
            .await
            .expect("200 + 缺 failed_results 的合法响应体必须成功（#[serde(default)]）");

        // 生产 reqwest 真的发出了 HTTP 请求：桩收到的请求行指向注入端点的 /extract
        let head = head.await.expect("桩任务不得 panic");
        assert!(
            head.starts_with("POST /extract "),
            "请求行必须打到注入端点的 /extract；实际首行：{}",
            head.lines().next().unwrap_or("<空>")
        );
        assert!(
            !head.to_ascii_lowercase().contains("authorization"),
            "web 侧工具不发任何 auth header（代码事实：invoke 不设 header）"
        );

        assert!(
            output.contains("This is the extracted content from the page."),
            "raw_content 须原样进入结果（未超截断阈值）: {output}"
        );
        assert!(
            output.contains("Web content may be inaccurate"),
            "结果须带可信度警告（WEB_CREDIBILITY_WARNING）: {output}"
        );
    }

    #[tokio::test]
    async fn test_webfetch_invoke_reports_non_2xx_status_and_body() {
        let (base_url, head) = spawn_response_stub(
            "HTTP/1.1 502 Bad Gateway",
            r#"{"detail":"extract upstream down"}"#,
        )
        .await;
        let tool = WebFetchTool::with_endpoint_for_test(&base_url);

        let err = tool
            .invoke(
                serde_json::json!({"url": "https://example.com/page"}),
                ToolContext::new(&[], "."),
            )
            .await
            .expect_err("502 必须返回 Err（不静默降级为空内容）");

        assert!(
            head.await
                .expect("桩任务不得 panic")
                .starts_with("POST /extract "),
            "非 2xx 分支同样必须真的发出请求"
        );
        let text = err.to_string();
        assert!(
            text.contains("Extract API returned HTTP 502"),
            "错误文本须含状态行: {text}"
        );
        assert!(
            text.contains("extract upstream down"),
            "非 2xx 的响应体须进入错误文本: {text}"
        );
    }
}
