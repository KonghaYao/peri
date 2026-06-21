use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde::Deserialize;
use serde_json::Value;

use super::web_common::WEB_CREDIBILITY_WARNING;
use crate::tools::output_persist::persist_truncated_output;
use crate::tools::output_truncate::truncate_bytes;

/// Tavily 抓取后端地址
const TAVILY_BASE_URL: &str = "https://tavily.claude-code-best.win";

/// 内容截断行数上限
const MAX_CONTENT_LINES: usize = 2000;

/// 内容字节数上限（兜底：行数未超限但总字节过大时触发）
const MAX_CONTENT_CHARS: usize = 100_000;

/// Tavily /extract 响应结构
#[derive(Deserialize)]
struct TavilyExtractResponse {
    results: Vec<TavilyExtractItem>,
    #[serde(default)]
    failed_results: Vec<TavilyExtractFailure>,
}

#[derive(Deserialize)]
struct TavilyExtractItem {
    #[allow(dead_code)]
    url: String,
    raw_content: Option<String>,
}

#[derive(Deserialize)]
struct TavilyExtractFailure {
    #[allow(dead_code)]
    url: String,
    error: Option<String>,
}

/// WebFetch 工具 — 通过 Tavily 兼容 API 抓取 URL 内容
pub struct WebFetchTool;

const WEB_FETCH_DESCRIPTION: &str = r#"Fetches a web page by URL and returns its content as text.

Usage:
- Only http:// and https:// URLs are allowed
- Content is returned as clean text extracted from the page
- Results are truncated at 2000 lines; full content saved to a temp file when truncated
- An optional 'prompt' parameter provides guidance for how to use the fetched content

Security:
- Maximum response size: 10MB
- Request timeout: 30 seconds"#;

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

/// 按行数 + 字节数两层截断内容
/// 1. 先按行数截断（行数超限时触发落盘）
/// 2. 再按字节数兜底（行数未超限但字节超限时也触发落盘）
fn truncate_content(content: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() > max_lines {
        // 行数超限：head 截断 + 落盘 + 字节兜底检查
        let truncated: String = lines[..max_lines].join("\n");
        let persist_hint = persist_truncated_output(content);
        let mut result = format!(
            "{truncated}\n[Content truncated: original had {} lines]{persist_hint}",
            lines.len()
        );
        // 行截断后仍可能字节超限（截断后的头部 + 提示信息）
        if result.len() > MAX_CONTENT_CHARS {
            let byte_truncated = truncate_bytes(&result, MAX_CONTENT_CHARS);
            result = format!(
                "{byte_truncated}\n[Output truncated: exceeds {} byte limit]{persist_hint}",
                MAX_CONTENT_CHARS
            );
        }
        return result;
    }
    // 行数未超限，检查字节数
    if content.len() > MAX_CONTENT_CHARS {
        let persist_hint = persist_truncated_output(content);
        let byte_truncated = truncate_bytes(content, MAX_CONTENT_CHARS);
        return format!(
            "{byte_truncated}\n[Content truncated: exceeds {} byte limit]{persist_hint}",
            MAX_CONTENT_CHARS
        );
    }
    content.to_string()
}

#[async_trait]
impl BaseTool for WebFetchTool {
    fn name(&self) -> &str {
        "WebFetch"
    }

    fn description(&self) -> &str {
        WEB_FETCH_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The full URL to fetch (http/https)"
                },
                "prompt": {
                    "type": "string",
                    "description": "Optional. Guidance prompt for how to use the fetched content, prepended to the result for the LLM"
                }
            },
            "required": ["url"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let url = input["url"].as_str().ok_or("Missing url parameter")?;
        let prompt = input["prompt"].as_str();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        let body = serde_json::json!({
            "urls": [url]
        });

        let resp = client
            .post(format!("{TAVILY_BASE_URL}/extract"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Extract request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("Extract API returned HTTP {status}: {text}").into());
        }

        let tavily: TavilyExtractResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse extract response: {e}"))?;

        // 检查 failed_results
        let errors: Vec<String> = tavily
            .failed_results
            .iter()
            .filter_map(|f| f.error.clone())
            .collect();
        if !errors.is_empty() {
            return Err(format!("Extract failed: {}", errors.join("; ")).into());
        }

        // 从 results 中提取内容
        let raw_content = tavily
            .results
            .first()
            .and_then(|r| r.raw_content.as_deref())
            .unwrap_or("");

        if raw_content.is_empty() {
            return Ok(format!(
                "{WEB_CREDIBILITY_WARNING}No content extracted from the URL."
            ));
        }

        let truncated = truncate_content(raw_content, MAX_CONTENT_LINES);

        let result = match prompt {
            Some(p) => format!("{WEB_CREDIBILITY_WARNING}Prompt: {p}\n\n{truncated}"),
            None => format!("{WEB_CREDIBILITY_WARNING}{truncated}"),
        };

        Ok(result)
    }
}

#[cfg(test)]
#[path = "web_fetch_test.rs"]
mod web_fetch_test;
