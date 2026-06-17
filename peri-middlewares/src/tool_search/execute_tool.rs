//! ExecuteExtraTool 元工具 — 代理执行延迟加载的工具

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::tools::BaseTool;
use serde_json::{json, Value};

use super::core_tools::{
    core_tools_sorted_csv, EXECUTE_EXTRA_TOOL_NAME, EXTRA_TOOL_NAME_FIELD, EXTRA_TOOL_PARAMS_FIELD,
};

/// 代理执行延迟加载工具的元工具
///
/// LLM 通过 SearchExtraTools 发现工具后，使用此工具代理调用。
/// 输入目标工具名称和参数，从共享工具注册表中查找并执行。
pub struct ExecuteExtraTool {
    /// 共享工具注册表（由 executor 在工具收集后填充）
    shared_tools: Arc<RwLock<HashMap<String, Arc<dyn BaseTool>>>>,
    /// description 含动态生成的 Core 工具列表，构造时一次性生成（P1-1）。
    description: String,
}

impl ExecuteExtraTool {
    pub fn new(shared_tools: Arc<RwLock<HashMap<String, Arc<dyn BaseTool>>>>) -> Self {
        let description = format!(
            "ExecuteExtraTool — a first-class core tool, always loaded, always available in your tool list. Runs locally with full permissions — NOT a remote or external tool. You do NOT need to search for it.\n\nThis tool accepts a tool_name and params object, looks up the target tool in the global tool registry, and delegates execution to it. The target tool runs with the same permissions and capabilities as if it were called directly.\n\nWhen to use: After SearchExtraTools discovers a deferred tool name, call this tool with {{\"tool_name\": \"<name>\", \"params\": {{...}}}} to invoke it immediately.\nWhen NOT to use: For core tools already in your tool list ({}, etc.) — call those directly.",
            core_tools_sorted_csv()
        );
        Self {
            shared_tools,
            description,
        }
    }
}

#[async_trait]
impl BaseTool for ExecuteExtraTool {
    fn name(&self) -> &str {
        EXECUTE_EXTRA_TOOL_NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tool_name": {
                    "type": "string",
                    "description": "The exact name of the target tool to execute (e.g., \"CronCreate\", \"mcp__server__action\")"
                },
                "params": {
                    "type": "object",
                    "description": "The parameters to pass to the target tool"
                }
            },
            "required": ["tool_name", "params"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let tool_name = input
            .get(EXTRA_TOOL_NAME_FIELD)
            .and_then(|v| v.as_str())
            .ok_or(format!(
                "{}: missing required '{}' parameter",
                EXECUTE_EXTRA_TOOL_NAME, EXTRA_TOOL_NAME_FIELD
            ))?;

        let params = input
            .get(EXTRA_TOOL_PARAMS_FIELD)
            .ok_or(format!(
                "{}: missing required '{}' parameter",
                EXECUTE_EXTRA_TOOL_NAME, EXTRA_TOOL_PARAMS_FIELD
            ))?
            .clone();

        let tool = {
            let tools = self.shared_tools.read();
            tools.get(tool_name).cloned().ok_or(format!(
                "{}: tool '{}' not found or not registered as a deferred tool",
                EXECUTE_EXTRA_TOOL_NAME, tool_name
            ))?
        };

        let result = tool.invoke(params).await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("execute_tool_test.rs");
}
