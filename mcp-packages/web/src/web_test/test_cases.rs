use super::{web_search, SearchResult, Value, WebFetchTool, WebSearchTool};
use peri_agent::tools::BaseTool;

// --- WebFetchTool tests ---

#[test]
pub(super) fn test_tool_name_is_web_fetch() {
    assert_eq!(WebFetchTool::new().name(), "WebFetch");
}

#[test]
pub(super) fn test_tool_parameters_required_url() {
    let params = WebFetchTool::new().parameters();
    let required = params["required"].as_array().unwrap();
    assert!(required.contains(&Value::String("url".to_string())));
}

// --- WebSearchTool tests ---

#[test]
pub(super) fn test_websearch_name() {
    assert_eq!(WebSearchTool::new().name(), "WebSearch");
}

#[test]
pub(super) fn test_websearch_parameters_required() {
    let params = WebSearchTool::new().parameters();
    let required = params["required"].as_array().unwrap();
    assert!(required.contains(&Value::String("query".to_string())));
}

/// 未配置实例 env 时沿用编译期默认后端且不带凭据（默认公共服务行为不变）。
#[test]
pub(super) fn test_websearch_defaults_without_instance_env() {
    let tool = WebSearchTool::new();
    assert_eq!(tool.base_url, "https://tavily.claude-code-best.win");
    assert_eq!(tool.token, None);
}

/// 实例 env 的取值语义：URL 覆盖默认值；token 缺省或空串 = 不发送凭据。
#[test]
pub(super) fn test_websearch_instance_env_token_semantics() {
    let env = std::collections::HashMap::from([(
        "PERI_WEB_SEARCH_URL".to_string(),
        "http://127.0.0.1:1".to_string(),
    )]);
    let tool = WebSearchTool::from_instance_env(&env);
    assert_eq!(tool.base_url, "http://127.0.0.1:1");
    assert_eq!(tool.token, None, "只配 URL 时不发送凭据");

    let env =
        std::collections::HashMap::from([("PERI_WEB_SEARCH_TOKEN".to_string(), String::new())]);
    let tool = WebSearchTool::from_instance_env(&env);
    assert_eq!(
        tool.base_url, "https://tavily.claude-code-best.win",
        "只配 token 时 URL 仍走默认值"
    );
    assert_eq!(tool.token, None, "空串 token 视为未配置");

    let env = std::collections::HashMap::from([(
        "PERI_WEB_SEARCH_TOKEN".to_string(),
        "test-token-0000".to_string(),
    )]);
    let tool = WebSearchTool::from_instance_env(&env);
    assert_eq!(tool.token.as_deref(), Some("test-token-0000"));
}

// --- format_search_results ---

#[test]
pub(super) fn test_format_search_results_empty() {
    let result = web_search::format_search_results(&[]);
    assert!(result.contains("No search results found."));
    assert!(result.contains("Web content may be inaccurate"));
}

#[test]
pub(super) fn test_format_search_results_with_content() {
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
    let output = web_search::format_search_results(&results);
    assert!(output.contains("## Search Results"));
    assert!(output.contains("1. **Test Page** (https://example.com)"));
    assert!(output.contains("2. **Another Page** (https://example.org)"));
    assert!(output.contains("A sample snippet."));
}

#[test]
pub(super) fn test_format_search_results_text_truncation() {
    let long_text = "x".repeat(600);
    let results = vec![SearchResult {
        title: "Long Text".to_string(),
        url: "https://example.com".to_string(),
        content: Some(long_text),
    }];
    let output = web_search::format_search_results(&results);
    let snippet_start = output.find("   ").unwrap() + 3;
    let snippet_end = output[snippet_start..].find("\n\n").unwrap();
    let snippet = &output[snippet_start..snippet_start + snippet_end];
    assert_eq!(snippet.chars().count(), 500);
}

#[test]
pub(super) fn test_format_search_results_no_content() {
    let results = vec![SearchResult {
        title: "No Content".to_string(),
        url: "https://example.com".to_string(),
        content: None,
    }];
    let output = web_search::format_search_results(&results);
    assert!(output.contains("**No Content** (https://example.com)"));
}

// --- invoke with missing params ---

#[tokio::test]
pub(super) async fn test_websearch_missing_query() {
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
pub(super) async fn test_webfetch_missing_url() {
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

/// 浮点 num_results 必须显式报错（在发起 HTTP 请求之前），
/// 不得被 as_u64() 静默吞掉回退默认值 10
#[tokio::test]
pub(super) async fn test_websearch_fractional_num_results_rejected_before_request() {
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
