use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde_json::json;

const AGENT_RESULT_DESCRIPTION: &str = include_str!("descriptions/agent_result.md");

/// AgentResult 工具：合成 tool_use 占位符
///
/// 此工具**不**供 LLM 主动查询。后台任务完成时，系统通过
/// `prompt_with_bg_results` 把结构化结果作为新一轮 user message 提交，
/// server 端 `executor.rs` 把结果转成合成 `AgentResult` tool_use +
/// tool_result 块插入 LLM 上下文。
///
/// 此工具存在的唯一目的是让 ToolRegistry 能识别 `AgentResult` 工具名，
/// 从而在合成 tool_use 块出现时不会触发 "unknown tool" 错误。`invoke`
/// 永远返回引导文本，提示 LLM 不要再调用。
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
        Ok("Background task results are delivered to you automatically when tasks complete—do not call this tool. If you cannot see results yet, the task is still running; wait for the completion notification before acting on its output."
            .to_string())
    }
}
