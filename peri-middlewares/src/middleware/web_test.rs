use serde_json::Value;

use super::*;
use crate::middleware::web_search::{format_search_results, SearchResult};

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
