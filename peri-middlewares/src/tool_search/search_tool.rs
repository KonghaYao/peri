//! SearchExtraTools 元工具 — 搜索并发现延迟加载的工具

use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde_json::{json, Value};

use super::{
    core_tools::{core_tools_sorted_csv, SEARCH_EXTRA_TOOLS_NAME},
    tool_index::ToolSearchIndex,
};

/// 搜索延迟加载工具的元工具
///
/// LLM 通过此工具发现不在直接工具列表中的 deferred tools，
/// 获取完整 schema 后通过 ExecuteExtraTool 调用。
pub struct SearchExtraTools {
    index: Arc<ToolSearchIndex>,
    /// description 含动态生成的 Core 工具列表，构造时一次性生成（P1-1）。
    description: String,
}

impl SearchExtraTools {
    pub fn new(index: Arc<ToolSearchIndex>) -> Self {
        let description = format!(
            "Search for deferred tools by name or keyword. LOW PRIORITY — only use this tool when no core tool can accomplish the task. Core tools ({}) are always available and should be used directly. This tool is for discovering additional capabilities like MCP tools, cron scheduling, etc.\n\nReturns matching tools with their full JSON schemas.\n\nIMPORTANT: ExecuteExtraTool is always available in your tool list. After this search returns tool names, you MUST call ExecuteExtraTool with {{\"tool_name\": \"<returned_name>\", \"params\": {{...}}}} to invoke the deferred tool. This is the ONLY way to execute deferred tools — do not read source code or analyze whether the tool is callable, just use ExecuteExtraTool directly.\n\nQuery forms:\n- \"select:CronCreate,Snip\" — fetch these exact tools by name\n- \"slack send\" — keyword search, best matches returned",
            core_tools_sorted_csv()
        );
        Self { index, description }
    }
}

#[async_trait]
impl BaseTool for SearchExtraTools {
    fn name(&self) -> &str {
        SEARCH_EXTRA_TOOLS_NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Query to find deferred tools. Use \"select:<tool_name>\" for direct selection, or keywords to search."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5)"
                }
            },
            "required": ["query"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or("SearchExtraTools: missing required 'query' parameter")?;

        let max_results = input
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let results = self.index.search(query, max_results);
        let total = self.index.total_count();
        let output = json!({
            "results": results,
            "total_available": total
        });

        Ok(serde_json::to_string(&output)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_search::tool_index::ToolSearchIndex;
    include!("search_tool_test.rs");
}
