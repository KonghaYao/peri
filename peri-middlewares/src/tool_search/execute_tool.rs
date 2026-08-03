//! ExecuteExtraTool 元工具 — 代理执行延迟加载的工具

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::tools::{BaseTool, DirectToolInvocationResolver};
use peri_agent::{
    agent::react::ToolCall,
    error::{AgentError, AgentResult},
    tools::{CanonicalToolInvocation, ToolInvocationResolver},
};
use serde_json::{json, Value};

use super::core_tools::{core_tools_sorted_csv, parse_extra_tool_call, EXECUTE_EXTRA_TOOL_NAME};

pub struct ExecuteExtraToolResolver {
    direct: DirectToolInvocationResolver,
}

impl Default for ExecuteExtraToolResolver {
    fn default() -> Self {
        Self {
            direct: DirectToolInvocationResolver,
        }
    }
}

fn resolve_extra_tool_target(
    input: &Value,
    tools: &BTreeMap<String, Arc<dyn BaseTool>>,
) -> AgentResult<(Arc<dyn BaseTool>, Value)> {
    let (target_name, params) =
        parse_extra_tool_call(input).map_err(|reason| AgentError::ToolExecutionFailed {
            tool: EXECUTE_EXTRA_TOOL_NAME.to_string(),
            reason,
        })?;
    let target = DirectToolInvocationResolver.resolve_target(&target_name, tools)?;
    let normalized = peri_agent::tools::normalize_params(params, Some(target.as_ref()));
    Ok((target, normalized))
}

impl ToolInvocationResolver for ExecuteExtraToolResolver {
    fn resolve(
        &self,
        raw_call: &ToolCall,
        tools: &BTreeMap<String, Arc<dyn BaseTool>>,
    ) -> AgentResult<CanonicalToolInvocation> {
        let outer = self.direct.resolve(raw_call, tools)?;
        if outer.target.name() != EXECUTE_EXTRA_TOOL_NAME {
            return Ok(outer);
        }

        let (target, params) = resolve_extra_tool_target(&raw_call.input, tools)?;
        Ok(CanonicalToolInvocation {
            raw_call: raw_call.clone(),
            policy_call: ToolCall::new(
                raw_call.id.clone(),
                target.name().to_string(),
                peri_agent::tools::normalize_params(params, Some(target.as_ref())),
            ),
            target,
            wrapper_name: Some(outer.target.name().to_string()),
        })
    }
}

/// 代理执行延迟加载工具的元工具
///
/// LLM 通过 SearchExtraTools 发现工具后，使用此工具代理调用。
/// 输入目标工具名称和参数，从共享工具注册表中查找并执行。
pub struct ExecuteExtraTool {
    /// 共享工具注册表（由 executor 在工具收集后填充）
    shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    /// description 含动态生成的 Core 工具列表，构造时一次性生成（P1-1）。
    description: String,
}

impl ExecuteExtraTool {
    pub fn new(shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>) -> Self {
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

    fn is_direct(&self) -> bool {
        true
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
                    "description": "A registered target tool name, case variation, or declared alias (e.g., \"CronCreate\", \"mcp__server__action\")"
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
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let (tool, params) = {
            let tools = self.shared_tools.read();
            resolve_extra_tool_target(&input, &tools).map_err(|error| error.to_string())?
        };

        let result = tool.invoke(params, ctx).await?;
        Ok(result)
    }
}

#[cfg(test)]
#[path = "execute_tool_test.rs"]
mod tests;
