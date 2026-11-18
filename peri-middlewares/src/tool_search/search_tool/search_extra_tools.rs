use super::{
    async_trait, direct_tools_description, fmt, json, Arc, BaseTool, Debug, Formatter,
    ToolSearchIndex, Value, SEARCH_EXTRA_TOOLS_NAME, TOOL_SEARCH_NAME,
};

/// 搜索延迟加载工具的元工具
///
/// LLM 通过此工具发现不在直接工具列表中的 deferred tools，
/// 获取完整 schema 后通过 ToolInvoke 调用。
pub struct SearchExtraTools {
    pub(super) index: Arc<ToolSearchIndex>,
    /// description 基于当前 session 的实际 direct tool 集合构造。
    pub(super) description: String,
}

impl Debug for SearchExtraTools {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolSearch")
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

impl SearchExtraTools {
    #[must_use]
    pub fn new(index: Arc<ToolSearchIndex>) -> Self {
        Self::with_direct_tools(index, std::iter::empty())
    }

    #[must_use]
    pub fn with_direct_tools<'a>(
        index: Arc<ToolSearchIndex>,
        direct_tool_names: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let description = format!(
            "Search for deferred tools by name or keyword. LOW PRIORITY — only use this tool when no directly available tool can accomplish the task. {} Use directly available tools directly. This tool is for discovering additional capabilities like MCP tools, cron scheduling, etc.\n\nReturns matching tools with their full JSON schemas. Returns an empty array when nothing matches.\n\nIMPORTANT: ToolInvoke is available in your tool list. After this search returns tool names, you MUST call ToolInvoke with {{\"name\": \"<returned_name>\", \"arguments\": {{...}}}} to invoke the deferred tool. This is the ONLY way to execute deferred tools — do not read source code or analyze whether the tool is callable, just use ToolInvoke directly.\n\nQuery forms:\n- \"select:mcp__peri__cron_list,mcp__peri__cron_remove\" — fetch these exact tools by name\n- \"slack send\" — keyword search, best matches returned",
            direct_tools_description(direct_tool_names)
        );
        Self { index, description }
    }
}

#[async_trait]
impl BaseTool for SearchExtraTools {
    fn name(&self) -> &str {
        TOOL_SEARCH_NAME
    }

    /// 旧名仅作为 resolver alias，不进入模型 schema。
    fn aliases(&self) -> &[&str] {
        &[SEARCH_EXTRA_TOOLS_NAME]
    }

    fn is_direct(&self) -> bool {
        true
    }

    /// Meta 工具统一分组（design v2 §2.5.1）。
    fn namespace(&self) -> Option<&str> {
        Some("meta")
    }

    /// 提示词层声明模板（design v2 §2.5.3）：说明桥接用途——Deferred 工具
    /// 对 LLM 不可直接见，必须先经此发现、再由 ToolInvoke 调用。
    /// title 不覆盖——走派生路径（"ToolSearch" → "Tool Search"）。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Discover deferred tools by name or keyword → `{{name}}` ({{title}}). \
             Deferred tools are not directly visible to you; find them here, then invoke them via ToolInvoke."
                .to_string(),
        )
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
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or("ToolSearch: missing required 'query' parameter")?;

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
