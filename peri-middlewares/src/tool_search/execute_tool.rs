//! ExecuteExtraTool 元工具 — 代理执行延迟加载的工具

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::tools::{bind_target_invocation, BaseTool, DirectToolInvocationResolver};
use peri_agent::{
    agent::react::ToolCall,
    error::{AgentError, AgentResult},
    tools::{CanonicalToolInvocation, ToolInvocationResolver},
};
use serde_json::{json, Value};

use super::core_tools::{direct_tools_description, parse_extra_tool_call, EXECUTE_EXTRA_TOOL_NAME};

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
    let target = DirectToolInvocationResolver.resolve_model_target(&target_name, tools)?;
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
        if outer.target.name() != EXECUTE_EXTRA_TOOL_NAME
            || outer.target.mcp_server_name().is_some()
            || outer.target.namespace() != Some("meta")
        {
            return Ok(outer);
        }

        let (target, params) = resolve_extra_tool_target(&raw_call.input, tools)?;
        let normalized = peri_agent::tools::normalize_params(params, Some(target.as_ref()));
        bind_target_invocation(
            raw_call,
            target,
            normalized,
            Some(outer.target.name().to_string()),
        )
    }
}

/// 代理执行延迟加载工具的元工具
///
/// LLM 通过 SearchExtraTools 发现工具后，使用此工具代理调用。
/// 输入目标工具名称和参数，从共享工具注册表中查找并执行。
pub struct ExecuteExtraTool {
    /// 共享工具注册表（由 executor 在工具收集后填充）
    shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    /// description 基于当前 session 的实际 direct tool 集合构造。
    description: String,
}

impl ExecuteExtraTool {
    pub fn new(shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>) -> Self {
        Self::with_direct_tools(shared_tools, std::iter::empty())
    }

    pub fn with_direct_tools<'a>(
        shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
        direct_tool_names: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let description = format!(
            "ExecuteExtraTool — a directly available meta tool in this session. 仅解析目标工具名称并委托执行，不增加权限或绕过审批。 You do NOT need to search for it.\n\nThis tool accepts a tool_name and params object, looks up the target tool in the current session's tool registry, and delegates execution to it. 目标工具可以是本地工具或远端 MCP 工具，保持与直接调用相同的权限和审批规则。\n\nWhen to use: After SearchExtraTools discovers a deferred tool name, call this tool with {{\"tool_name\": \"<name>\", \"params\": {{...}}}} to invoke it immediately.\nWhen NOT to use: For tools already in your tool list. {}",
            direct_tools_description(direct_tool_names)
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

    /// Meta 工具统一分组（design v2 §2.5.1）。
    fn namespace(&self) -> Option<&str> {
        Some("meta")
    }

    /// 提示词层声明模板（design v2 §2.5.3）：说明桥接用途——按名称调用已注册
    /// 的 Deferred 工具；Core 工具无需经此。
    /// title 不覆盖——走派生路径（"ExecuteExtraTool" → "Execute Extra Tool"）。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Invoke a registered deferred tool by name → `{{name}}` ({{title}}). \
             Use this after SearchExtraTools returns a tool name; call core tools in your tool list directly."
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
                "tool_name": {
                    "type": "string",
                    "description": "已注册的目标工具名称、大小写变体或声明的别名（例如 mcp__cron__cron_list）；以当前会话目录为准"
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
