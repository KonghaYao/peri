use std::time::Duration;

use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde::Deserialize;
use serde_json::Value;

use super::web_common::WEB_CREDIBILITY_WARNING;

/// Tavily 兼容搜索后端默认地址（MCP 实例 env 未提供时使用）。
const TAVILY_BASE_URL: &str = "https://tavily.claude-code-best.win";

/// MCP 实例 env 键：搜索后端根地址。
const ENV_BASE_URL: &str = "PERI_WEB_SEARCH_URL";

/// MCP 实例 env 键：可选的 Bearer 凭据（缺省或空串 = 不发送 `Authorization`）。
const ENV_TOKEN: &str = "PERI_WEB_SEARCH_TOKEN";

/// 请求总超时（生产路径的硬编码值；测试可注入短超时以复现超时分支）
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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

const WEBSEARCH_DESCRIPTION: &str = include_str!("descriptions/web_search.md");

/// WebSearch 工具 — 通过 Tavily 兼容 API 搜索网页
pub struct WebSearchTool {
    /// Tavily 兼容后端根地址（缺省 [`TAVILY_BASE_URL`]；测试可注入本地桩）。
    base_url: String,
    /// Bearer 凭据；`None` = 不发送 `Authorization`（缺省行为）。
    token: Option<String>,
    /// 请求总超时（生产恒为 [`REQUEST_TIMEOUT`]；测试可注入短超时）。
    timeout: Duration,
}

impl WebSearchTool {
    /// 默认实例构造：使用编译期默认后端、无凭据。
    pub fn new() -> Self {
        Self::from_instance_env(&std::collections::HashMap::new())
    }

    /// MCP 实例环境构造；只解释传给本 MCP 实例的环境，不读取宿主进程环境。
    ///
    /// 实例环境在连接建立时冻结，连接重建时由调用方再次提供：
    /// `PERI_WEB_SEARCH_URL` 缺省为内置公共服务，`PERI_WEB_SEARCH_TOKEN`
    /// 缺省（或空串）时不发送凭据。
    pub fn from_instance_env(env: &std::collections::HashMap<String, String>) -> Self {
        let base_url = env
            .get(ENV_BASE_URL)
            .cloned()
            .unwrap_or_else(|| TAVILY_BASE_URL.into());
        let token = env
            .get(ENV_TOKEN)
            .filter(|token| !token.is_empty())
            .cloned();
        Self {
            base_url,
            token,
            timeout: REQUEST_TIMEOUT,
        }
    }

    /// 测试构造：注入端点（本地回环桩），使搜索协议的 200 / 非 2xx 形态可在
    /// 无网络条件下覆盖；生产路径只经 [`Self::new`] / [`Self::from_instance_env`]。
    #[cfg(test)]
    pub(crate) fn with_endpoint_for_test(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            token: None,
            timeout: REQUEST_TIMEOUT,
        }
    }

    /// 测试构造：同时注入端点与请求总超时，让超时分支在毫秒级真实时钟内可复现。
    ///
    /// 不采用 `tokio::test(start_paused = true)`：虚拟时钟需要 tokio 的 `test-util`
    /// feature，本 crate 未启用（`peri-agent` / `peri-tui` 在各自 dev-dependencies 里
    /// 单独启用，本 crate 不加依赖）。生产路径只经 [`Self::new`] /
    /// [`Self::from_instance_env`]。
    #[cfg(test)]
    pub(crate) fn with_endpoint_and_timeout_for_test(
        base_url: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            token: None,
            timeout,
        }
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

    /// 网络工具分组（design v2 §2.5.1：同类工具按 namespace 组织声明段）。
    fn namespace(&self) -> Option<&str> {
        Some("web")
    }

    /// 提示词层声明模板（design v2 §2.5.3）。
    /// title 不覆盖——走 `BaseTool::tool_description` 默认路径由 name 推导。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Look up current information beyond your knowledge → `{{name}}` ({{title}}). Query the web for recent or external facts."
                .to_string(),
        )
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
                    "minimum": 1,
                    "maximum": 20,
                    "description": "Number of results, default 10, max 20"
                }
            },
            "required": ["query"]
        })
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let query = input["query"]
            .as_str()
            .ok_or("Missing required parameter: query")?;
        // 非法类型（浮点/字符串/负数）显式报错，不再静默回退默认值；
        // 合法整数越界按描述 clamp 到 [1, 20]
        let max_results =
            match peri_mcp_common::parse_optional_u64(&input["num_results"], "num_results")? {
                Some(n) => (n as usize).clamp(1, 20),
                None => 10,
            };

        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        let body = serde_json::json!({
            "query": query,
            "max_results": max_results,
        });

        let mut request = client
            .post(format!("{}/search", self.base_url.trim_end_matches('/')))
            .json(&body);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }

        let resp = request
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

// WebSearch / WebFetch 两个工具的用例（sub-plan F §6.4 第 2 步：挂载点从
// `middleware/web.rs` 迁到本文件——该文件由 I-03 在 W3 删除；16 个用例，
// 过滤器 `middleware::web_search::tests`）。
#[cfg(test)]
#[path = "web_test.rs"]
mod tests;
