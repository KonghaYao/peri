use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde::Deserialize;
use serde_json::Value;

use super::web_common::WEB_CREDIBILITY_WARNING;

/// Tavily 搜索后端地址
const TAVILY_BASE_URL: &str = "https://tavily.claude-code-best.win";

/// 单条结果文本截断上限（字符数）
const MAX_RESULT_TEXT_CHARS: usize = 500;

/// Tavily /search 响应结构
#[derive(Deserialize)]
struct TavilySearchResponse {
    results: Vec<TavilySearchItem>,
}

#[derive(Deserialize)]
struct TavilySearchItem {
    title: String,
    url: String,
    content: Option<String>,
}

/// 搜索结果（内部使用，与 Tavily 解耦）
pub(crate) struct SearchResult {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) content: Option<String>,
}

const WEBSEARCH_DESCRIPTION: &str = r#"Search the web using a search engine.

Usage:
- Provide a search query to find relevant web pages
- Returns results as a numbered Markdown list with titles, URLs, and text snippets
- Each result's text is truncated to 500 characters
- No API key required

IMPORTANT:
- Results may be irrelevant or low quality — always verify information before using it
- If results don't contain the information you need, do NOT fabricate or guess values
- Consider using WebFetch to directly access a specific URL for accurate information

Parameters:
- query (required): Search keywords
- num_results (optional): Number of results, default 10, max 20"#;

/// WebSearch 工具 — 通过 Tavily 兼容 API 搜索网页
pub struct WebSearchTool;

impl WebSearchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

/// 将搜索结果格式化为 Markdown 编号列表
pub(crate) fn format_search_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!("{WEB_CREDIBILITY_WARNING}No search results found.");
    }

    let mut output = format!("{WEB_CREDIBILITY_WARNING}## Search Results\n\n");
    for (i, r) in results.iter().enumerate() {
        output.push_str(&format!("{}. **{}** ({})\n", i + 1, r.title, r.url));
        if let Some(content) = &r.content {
            let truncated: String = content.chars().take(MAX_RESULT_TEXT_CHARS).collect();
            output.push_str(&format!("   {}\n\n", truncated.trim()));
        } else {
            output.push('\n');
        }
    }
    output
}

#[async_trait]
impl BaseTool for WebSearchTool {
    fn name(&self) -> &str {
        "WebSearch"
    }

    fn description(&self) -> &str {
        WEBSEARCH_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search keywords"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results, default 10, max 20"
                }
            },
            "required": ["query"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let query = input["query"]
            .as_str()
            .ok_or("Missing required parameter: query")?;
        let max_results = input["num_results"].as_u64().unwrap_or(10).clamp(1, 20) as usize;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        let body = serde_json::json!({
            "query": query,
            "max_results": max_results,
        });

        let resp = client
            .post(format!("{TAVILY_BASE_URL}/search"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Search request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("Search API returned HTTP {status}: {text}").into());
        }

        let tavily: TavilySearchResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse search response: {e}"))?;

        let results: Vec<SearchResult> = tavily
            .results
            .into_iter()
            .filter(|item| !item.url.is_empty())
            .map(|item| SearchResult {
                title: item.title,
                url: item.url,
                content: item.content,
            })
            .collect();

        Ok(format_search_results(&results))
    }
}
