use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde_json::json;

const AGENT_RESULT_DESCRIPTION: &str = include_str!("descriptions/agent_result.md");

/// AgentResult 工具：保留历史 tool_use 身份与客户端显示映射。
///
/// 当前后台结果由 AsyncRouter 作为 Defer system reminder 自动投递，
/// 不再合成工具调用。此工具不对模型公开，也不查询任务状态；
/// `invoke` 仅返回引导文本。
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

    fn visible_to_model(&self) -> bool {
        false
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
                    "description": "Ignored—results are auto-injected, not queried by task_id."
                }
            }
        })
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("Background task results are delivered automatically when tasks complete. This tool does not query task state and must not be called."
            .to_string())
    }
}
