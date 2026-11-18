//! ToolInvoke 元工具 — 代理执行延迟加载的工具

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::agent::react::ToolCall;
use peri_agent::error::AgentError;
use peri_agent::error::AgentResult;
use peri_agent::tools;
use peri_agent::tools::BaseTool;
use peri_agent::tools::CanonicalToolInvocation;
use peri_agent::tools::DirectToolInvocationResolver;
use peri_agent::tools::ToolInvocationResolver;
use serde_json::{json, Value};

use super::core_tools;
use super::core_tools::direct_tools_description;
use super::core_tools::{EXECUTE_EXTRA_TOOL_NAME, TOOL_INVOKE_NAME};

pub struct ExecuteExtraToolResolver {
    direct: DirectToolInvocationResolver,
}

impl Debug for ExecuteExtraToolResolver {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecuteExtraToolResolver")
            .finish_non_exhaustive()
    }
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
    outer_name: &str,
) -> AgentResult<(Arc<dyn BaseTool>, Value)> {
    let call =
        core_tools::parse_proxy_call(input).map_err(|reason| AgentError::ToolExecutionFailed {
            tool: outer_name.to_string(),
            reason,
        })?;
    let target = DirectToolInvocationResolver.resolve_model_target(&call.target_name, tools)?;
    if target.mcp_server_name().is_none()
        && target.namespace() == Some("meta")
        && peri_acp_types::tool_proxy::is_invoke_proxy_name(target.name())
    {
        return Err(AgentError::ToolExecutionFailed {
            tool: outer_name.to_string(),
            reason: "ToolInvoke cannot target the host proxy tool itself".to_string(),
        });
    }
    let normalized = peri_agent::tools::normalize_params(call.arguments, Some(target.as_ref()));
    Ok((target, normalized))
}

fn is_host_invoke_proxy(tool: &dyn BaseTool) -> bool {
    peri_acp_types::tool_proxy::is_host_invoke_proxy(tool)
}

impl ToolInvocationResolver for ExecuteExtraToolResolver {
    fn resolve(
        &self,
        raw_call: &ToolCall,
        tools: &BTreeMap<String, Arc<dyn BaseTool>>,
    ) -> AgentResult<CanonicalToolInvocation> {
        let outer = self.direct.resolve(raw_call, tools)?;
        if !is_host_invoke_proxy(outer.target.as_ref()) {
            return Ok(outer);
        }

        let (target, normalized) =
            resolve_extra_tool_target(&raw_call.input, tools, &raw_call.name)?;
        tools::bind_target_invocation(
            raw_call,
            target,
            normalized,
            Some(outer.target.name().to_string()),
        )
    }
}

/// 代理执行延迟加载工具的元工具
///
/// LLM 通过 ToolSearch 发现工具后，使用此工具代理调用。
/// 输入目标工具名称和参数，从共享工具注册表中查找并执行。
pub struct ExecuteExtraTool {
    /// 共享工具注册表（由 executor 在工具收集后填充）
    shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    /// description 基于当前 session 的实际 direct tool 集合构造。
    description: String,
}

impl Debug for ExecuteExtraTool {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolInvoke")
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

impl ExecuteExtraTool {
    #[must_use]
    pub fn new(shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>) -> Self {
        Self::with_direct_tools(shared_tools, std::iter::empty())
    }

    #[must_use]
    pub fn with_direct_tools<'a>(
        shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
        direct_tool_names: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let description = format!(
            "ToolInvoke — a directly available meta tool in this session. It resolves a target tool name and delegates execution; it does not add permissions or bypass approval. You do NOT need to search for it.\n\nThis tool accepts a name and an arguments object, looks up the target tool in the current session's tool registry, and delegates execution to it. The target can be a local tool or a remote MCP tool; the same permission and approval rules apply as a direct call.\n\nWhen to use: After ToolSearch discovers a deferred tool name, call this tool with {{\"name\": \"<name>\", \"arguments\": {{...}}}} to invoke it immediately.\nWhen NOT to use: For tools already in your tool list. {}",
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
        TOOL_INVOKE_NAME
    }

    /// 旧名仅作为 resolver alias，不进入模型 schema。
    fn aliases(&self) -> &[&str] {
        &[EXECUTE_EXTRA_TOOL_NAME]
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
    /// title 不覆盖——走派生路径（"ToolInvoke" → "Tool Invoke"）。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Invoke a registered deferred tool by name → `{{name}}` ({{title}}). \
             Use this after ToolSearch returns a tool name; call core tools in your tool list directly."
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
                "name": {
                    "type": "string",
                    "description": "Exact name, case variant, or declared alias of a registered tool in the current session catalog (for example mcp__peri__cron_list)"
                },
                "arguments": {
                    "type": "object",
                    "description": "Arguments passed to the target tool; use {} when the tool takes no arguments"
                }
            },
            "required": ["name", "arguments"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let (tool, params) = {
            let tools = self.shared_tools.read();
            resolve_extra_tool_target(&input, &tools, self.name())
                .map_err(|error| error.to_string())?
        };

        let result = tool.invoke(params, ctx).await?;
        Ok(result)
    }
}

#[cfg(test)]
#[path = "execute_tool_test.rs"]
mod tests;
