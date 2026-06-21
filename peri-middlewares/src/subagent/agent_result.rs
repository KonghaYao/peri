use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde_json::json;

const AGENT_RESULT_DESCRIPTION: &str = r#"Query background agent task results. Returns the output of completed background agents. If no task_id is specified, returns all available results."#;

/// AgentResult 工具：供主 Agent 查询后台任务结果
///
/// 实际的后台任务结果通过合成消息注入（tool_use + tool_result），
/// 此工具的 invoke 不执行真实查询，仅作为工具定义占位使 LLM
/// 能识别 AgentResult 类型的 tool_use 块。
pub struct AgentResultTool;

impl Default for AgentResultTool {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentResultTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BaseTool for AgentResultTool {
    fn name(&self) -> &str {
        "AgentResult"
    }

    fn description(&self) -> &str {
        AGENT_RESULT_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Optional specific task ID to query. If omitted, returns all completed results."
                }
            }
        })
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("No results yet. Background tasks will notify you on completion — do not call this tool again until notified."
            .to_string())
    }
}
