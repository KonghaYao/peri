use peri_agent::tools::BaseTool;
use serde_json::Value;

use crate::middleware::web_fetch::WebFetchTool;
use crate::middleware::web_search::{format_search_results, SearchResult, WebSearchTool};

// --- WebFetchTool tests ---

#[test]
fn test_tool_name_is_web_fetch() {
    assert_eq!(WebFetchTool::new().name(), "WebFetch");
}

#[test]
fn test_tool_parameters_required_url() {
    let params = WebFetchTool::new().parameters();
    let required = params["required"].as_array().unwrap();
    assert!(required.contains(&Value::String("url".to_string())));
}

// --- WebSearchTool tests ---

#[test]
fn test_websearch_name() {
    assert_eq!(WebSearchTool::new().name(), "WebSearch");
}

#[test]
fn test_websearch_parameters_required() {
    let params = WebSearchTool::new().parameters();
    let required = params["required"].as_array().unwrap();
    assert!(required.contains(&Value::String("query".to_string())));
}

// --- format_search_results ---

#[test]
fn test_format_search_results_empty() {
    let result = format_search_results(&[]);
    assert!(result.contains("No search results found."));
    assert!(result.contains("Web content may be inaccurate"));
}

#[test]
fn test_format_search_results_with_content() {
    let results = vec![
        SearchResult {
            title: "Test Page".to_string(),
            url: "https://example.com".to_string(),
            content: Some("A sample snippet.".to_string()),
        },
        SearchResult {
            title: "Another Page".to_string(),
            url: "https://example.org".to_string(),
            content: Some("Another snippet here.".to_string()),
        },
    ];
    let output = format_search_results(&results);
    assert!(output.contains("## Search Results"));
    assert!(output.contains("1. **Test Page** (https://example.com)"));
    assert!(output.contains("2. **Another Page** (https://example.org)"));
    assert!(output.contains("A sample snippet."));
}

#[test]
fn test_format_search_results_text_truncation() {
    let long_text = "x".repeat(600);
    let results = vec![SearchResult {
        title: "Long Text".to_string(),
        url: "https://example.com".to_string(),
        content: Some(long_text),
    }];
    let output = format_search_results(&results);
    let snippet_start = output.find("   ").unwrap() + 3;
    let snippet_end = output[snippet_start..].find("\n\n").unwrap();
    let snippet = &output[snippet_start..snippet_start + snippet_end];
    assert_eq!(snippet.chars().count(), 500);
}

#[test]
fn test_format_search_results_no_content() {
    let results = vec![SearchResult {
        title: "No Content".to_string(),
        url: "https://example.com".to_string(),
        content: None,
    }];
    let output = format_search_results(&results);
    assert!(output.contains("**No Content** (https://example.com)"));
}

// --- invoke with missing params ---

#[tokio::test]
async fn test_websearch_missing_query() {
    let tool = WebSearchTool::new();
    let result = tool
        .invoke(
            serde_json::json!({}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err = result.unwrap_err();
    assert!(
        err.to_string()
            .contains("Missing required parameter: query"),
        "实际: {err}"
    );
}

#[tokio::test]
async fn test_webfetch_missing_url() {
    let tool = WebFetchTool::new();
    let result = tool
        .invoke(
            serde_json::json!({}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Missing url parameter"),
        "实际: {err}"
    );
}

// --- Tavily 响应反序列化测试 ---

mod tavily_search_deserialize {
    /// 模拟 Tavily /search 标准响应
    const SAMPLE_SEARCH_RESPONSE: &str = r#"{
        "query": "rust programming",
        "results": [
            {
                "title": "Rust Programming Language",
                "url": "https://www.rust-lang.org/",
                "content": "A language empowering everyone to build reliable and efficient software.",
                "score": 0.95
            },
            {
                "title": "Learn Rust",
                "url": "https://doc.rust-lang.org/book/",
                "content": null,
                "score": 0.82
            }
        ]
    }"#;

    #[test]
    fn test_deserialize_search_response() {
        let resp: serde_json::Value = serde_json::from_str(SAMPLE_SEARCH_RESPONSE).unwrap();
        let results = resp["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0]["title"].as_str().unwrap(),
            "Rust Programming Language"
        );
        assert_eq!(
            results[0]["url"].as_str().unwrap(),
            "https://www.rust-lang.org/"
        );
        assert!(results[0]["content"].as_str().is_some());
        // score 字段被忽略（不在结构体中）
        assert_eq!(results[1]["content"].as_str(), None);
    }

    #[test]
    fn test_deserialize_search_empty_results() {
        let json = r#"{"query": "xxx", "results": []}"#;
        let resp: serde_json::Value = serde_json::from_str(json).unwrap();
        let results = resp["results"].as_array().unwrap();
        assert!(results.is_empty());
    }
}

mod tavily_extract_deserialize {
    /// 模拟 Tavily /extract 标准响应
    const SAMPLE_EXTRACT_RESPONSE: &str = r#"{
        "results": [
            {
                "url": "https://example.com/page",
                "raw_content": "This is the extracted content from the page."
            }
        ],
        "failed_results": []
    }"#;

    const SAMPLE_EXTRACT_WITH_FAILURES: &str = r#"{
        "results": [],
        "failed_results": [
            {
                "url": "https://example.com/bad",
                "error": "404 Not Found"
            }
        ]
    }"#;

    const SAMPLE_EXTRACT_NO_FAILED_FIELD: &str = r#"{
        "results": [
            {
                "url": "https://example.com/page",
                "raw_content": "Content here."
            }
        ]
    }"#;

    #[test]
    fn test_deserialize_extract_response() {
        let resp: serde_json::Value = serde_json::from_str(SAMPLE_EXTRACT_RESPONSE).unwrap();
        let results = resp["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0]["raw_content"].as_str().unwrap(),
            "This is the extracted content from the page."
        );
        assert!(resp["failed_results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_deserialize_extract_with_failures() {
        let resp: serde_json::Value = serde_json::from_str(SAMPLE_EXTRACT_WITH_FAILURES).unwrap();
        let failed = resp["failed_results"].as_array().unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["error"].as_str().unwrap(), "404 Not Found");
        assert!(resp["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_deserialize_extract_missing_failed_field() {
        // failed_results 字段缺失时应默认为空数组（#[serde(default)]）
        let resp: serde_json::Value = serde_json::from_str(SAMPLE_EXTRACT_NO_FAILED_FIELD).unwrap();
        assert!(
            resp.get("failed_results").is_none()
                || resp["failed_results"].as_array().unwrap().is_empty()
        );
    }
}

/// 浮点 num_results 必须显式报错（在发起 HTTP 请求之前），
/// 不得被 as_u64() 静默吞掉回退默认值 10
#[tokio::test]
async fn test_websearch_fractional_num_results_rejected_before_request() {
    let tool = WebSearchTool::new();
    let result = tool
        .invoke(
            serde_json::json!({"query": "test", "num_results": 12.5}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("non-negative integer"),
        "浮点 num_results 应报错而非静默回退: {err_msg}"
    );
}

// ─── 真实 HTTP 发包（本地回环桩；无网络、无凭据） ─────────────────────────────
//
// 验收记录 §7 第 6 条的闭合点：此前本文件的「Tavily 响应」用例只做
// `serde_json::from_str::<Value>`（对生产反序列化路径 0 覆盖），`invoke` 的网络分支
// 从未真正发出过 HTTP 请求。下面用 `with_endpoint_for_test` 把端点指向本地回环桩，
// 让生产 `invoke` 真的经 reqwest 走「200 + 正文 / 非 2xx / 客户端硬编码超时」三条分支。
mod real_http_round_trip {
    use std::time::Duration;

    use peri_agent::tools::{BaseTool, ToolContext};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::middleware::web_search::{WebSearchTool, REQUEST_TIMEOUT};

    /// 合法的 Tavily /search 响应：字段对齐 `web_search.rs` 的私有
    /// `TavilySearchResponse`（`results[]` 的 `title` / `url` 必填、`content` 可空；
    /// `query` / `score` 等未声明字段被 serde 忽略）。
    const SEARCH_OK_BODY: &str = r#"{
        "query": "rust programming",
        "results": [
            {
                "title": "Rust Programming Language",
                "url": "https://www.rust-lang.org/",
                "content": "A language empowering everyone to build reliable and efficient software.",
                "score": 0.95
            },
            {
                "title": "Learn Rust",
                "url": "https://doc.rust-lang.org/book/",
                "content": null,
                "score": 0.82
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

    /// 绑定回环端口、接受连接后**永不响应**的桩：让客户端停在等待响应上，只能由自己的
    /// 请求总超时终结（生产路径 = `REQUEST_TIMEOUT`）。
    ///
    /// 返回的 `oneshot` 在 TCP 连接被接受后触发：证明请求真的到达了本地桩，而不是
    /// 在连接阶段就失败。
    async fn spawn_silent_stub() -> (String, tokio::sync::oneshot::Receiver<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("本地桩必须能绑定回环端口");
        let port = listener.local_addr().expect("本地桩地址可读").port();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let Ok((_socket, _)) = listener.accept().await else {
                return;
            };
            let _ = accepted_tx.send(());
            // 持有连接、不读不写（`_socket` 活在本 future 的帧里）。
            std::future::pending::<()>().await;
        });
        (format!("http://127.0.0.1:{port}"), accepted_rx)
    }

    #[tokio::test]
    async fn test_websearch_invoke_round_trips_real_http_200_body() {
        let (base_url, head) = spawn_response_stub("HTTP/1.1 200 OK", SEARCH_OK_BODY).await;
        let tool = WebSearchTool::with_endpoint_for_test(&base_url);

        let output = tool
            .invoke(
                serde_json::json!({"query": "rust programming", "num_results": 2}),
                ToolContext::new(&[], "."),
            )
            .await
            .expect("200 + 合法响应体必须成功");

        // 生产 reqwest 真的发出了 HTTP 请求：桩收到的请求行指向注入端点的 /search
        let head = head.await.expect("桩任务不得 panic");
        assert!(
            head.starts_with("POST /search "),
            "请求行必须打到注入端点的 /search；实际首行：{}",
            head.lines().next().unwrap_or("<空>")
        );
        assert!(
            !head.to_ascii_lowercase().contains("authorization"),
            "web 侧工具不发任何 auth header（代码事实：invoke 不设 header）"
        );

        // 响应体经生产 `TavilySearchResponse` 反序列化 → `format_search_results` 文本
        assert!(output.contains("## Search Results"), "实际: {output}");
        assert!(
            output.contains("1. **Rust Programming Language** (https://www.rust-lang.org/)"),
            "实际: {output}"
        );
        assert!(
            output.contains(
                "A language empowering everyone to build reliable and efficient software."
            ),
            "实际: {output}"
        );
        assert!(
            output.contains("2. **Learn Rust** (https://doc.rust-lang.org/book/)"),
            "content: null 的第二条仍须出标题行；实际: {output}"
        );
    }

    #[tokio::test]
    async fn test_websearch_invoke_reports_non_2xx_status_and_body() {
        let (base_url, head) = spawn_response_stub(
            "HTTP/1.1 500 Internal Server Error",
            r#"{"detail":"upstream boom"}"#,
        )
        .await;
        let tool = WebSearchTool::with_endpoint_for_test(&base_url);

        let err = tool
            .invoke(
                serde_json::json!({"query": "rust"}),
                ToolContext::new(&[], "."),
            )
            .await
            .expect_err("500 必须返回 Err（不静默降级为空结果）");

        assert!(
            head.await
                .expect("桩任务不得 panic")
                .starts_with("POST /search "),
            "非 2xx 分支同样必须真的发出请求"
        );
        let text = err.to_string();
        assert!(
            text.contains("Search API returned HTTP 500"),
            "错误文本须含状态行（`resp.status()` 的 Display 含 reason phrase）：{text}"
        );
        assert!(
            text.contains("upstream boom"),
            "非 2xx 的响应体须进入错误文本：{text}"
        );
    }

    /// 请求总超时分支：生产路径的硬编码值是 `REQUEST_TIMEOUT`，本用例注入 200ms 在
    /// **真实时钟**下复现同一分支（`tokio::test(start_paused = true)` 需要 tokio 的
    /// `test-util` feature，本 crate 未启用且不改 `Cargo.toml`，故不采用虚拟时钟）。
    ///
    /// 代码事实（`web_search.rs` 的 `invoke` + reqwest 0.13 的 `error.rs`）：总超时命中后
    /// `send()` 返回 `Err(reqwest::Error)`，其 **Display 不吃 source 链**（`TimedOut` 的
    /// "operation timed out" 只挂在 source 上），所以映射出的文本是
    /// `Search request failed: error sending request for url (<url>)`
    /// ——与连接类请求错误同形，文本本身**不能**区分超时。超时的判别证据是：耗时恰好落在
    /// 注入的总超时上（连接失败会立即返回），且桩确实接受过连接。
    #[tokio::test]
    async fn test_websearch_invoke_request_timeout_terminates_request() {
        // 生产路径的超时值仍是 30s（本用例只替换注入值，不动生产常量）
        assert_eq!(
            REQUEST_TIMEOUT,
            Duration::from_secs(30),
            "生产请求总超时必须保持 30s"
        );

        let (base_url, accepted) = spawn_silent_stub().await;
        let injected_timeout = Duration::from_millis(200);
        let tool = WebSearchTool::with_endpoint_and_timeout_for_test(&base_url, injected_timeout);

        let started = std::time::Instant::now();
        let err = tool
            .invoke(
                serde_json::json!({"query": "rust"}),
                ToolContext::new(&[], "."),
            )
            .await
            .expect_err("桩永不响应 ⇒ 必须由客户端请求总超时终结为 Err");
        let elapsed = started.elapsed();

        assert!(
            elapsed >= injected_timeout,
            "耗时须至少达到注入的总超时（连接失败是立即返回的）；实际: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "超时不应落到更长的兜底值；实际: {elapsed:?}"
        );
        tokio::time::timeout(Duration::from_secs(2), accepted)
            .await
            .expect("客户端必须真的连上本地桩（TCP 连接在超时前被接受）")
            .expect("桩的接受通知不得丢失");

        let text = err.to_string();
        assert!(
            text.starts_with("Search request failed: "),
            "超时走 `send()` 的 map_err 分支：{text}"
        );
        assert!(
            text.contains(&format!("{base_url}/search")),
            "错误文本须指向注入端点的 /search：{text}"
        );
    }
}
