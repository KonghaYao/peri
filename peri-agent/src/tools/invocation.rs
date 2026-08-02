use std::{collections::BTreeMap, sync::Arc};

use crate::{
    agent::react::ToolCall,
    error::{AgentError, AgentResult},
    tools::BaseTool,
};

/// 已在单个 dispatch batch 的不可变工具表中解析完成的调用事实。
///
/// `raw_call` 始终保留 LLM 原始输入；`policy_call` 是 middleware、HITL 和
/// hook 使用的 canonical target 投影。执行必须使用 `target`，不得重新按名称查表。
#[derive(Clone)]
pub struct CanonicalToolInvocation {
    pub raw_call: ToolCall,
    pub policy_call: ToolCall,
    pub target: Arc<dyn BaseTool>,
    pub wrapper_name: Option<String>,
}

impl CanonicalToolInvocation {
    pub fn with_policy_input(&self, input: serde_json::Value) -> Self {
        let mut invocation = self.clone();
        invocation.policy_call.input = input;
        invocation
    }
}

/// P0-1 的调用解析边界。每个 dispatch 只从其工具表 snapshot 解析一次。
pub trait ToolInvocationResolver: Send + Sync {
    fn resolve(
        &self,
        raw_call: &ToolCall,
        tools: &BTreeMap<String, Arc<dyn BaseTool>>,
    ) -> AgentResult<CanonicalToolInvocation>;
}

/// 默认解析器：精确 key、canonical 名称、大小写折叠 key 和 alias 必须唯一。
#[derive(Default)]
pub struct DirectToolInvocationResolver;

impl DirectToolInvocationResolver {
    pub fn resolve_target(
        &self,
        name: &str,
        tools: &BTreeMap<String, Arc<dyn BaseTool>>,
    ) -> AgentResult<Arc<dyn BaseTool>> {
        let mut candidates: Vec<Arc<dyn BaseTool>> = Vec::new();
        for (key, tool) in tools {
            if (key == name
                || tool.name() == name
                || key.eq_ignore_ascii_case(name)
                || tool.name().eq_ignore_ascii_case(name)
                || tool
                    .aliases()
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(name)))
                && !candidates
                    .iter()
                    .any(|candidate| Arc::ptr_eq(candidate, tool))
            {
                candidates.push(Arc::clone(tool));
            }
        }

        match candidates.len() {
            0 => Err(AgentError::ToolNotFound(name.to_string())),
            1 => Ok(candidates.pop().expect("one candidate")),
            _ => Err(AgentError::ToolExecutionFailed {
                tool: name.to_string(),
                reason: "ambiguous tool invocation".to_string(),
            }),
        }
    }
}

impl ToolInvocationResolver for DirectToolInvocationResolver {
    fn resolve(
        &self,
        raw_call: &ToolCall,
        tools: &BTreeMap<String, Arc<dyn BaseTool>>,
    ) -> AgentResult<CanonicalToolInvocation> {
        let target = self.resolve_target(&raw_call.name, tools)?;
        Ok(CanonicalToolInvocation {
            raw_call: raw_call.clone(),
            policy_call: ToolCall::new(
                raw_call.id.clone(),
                target.name().to_string(),
                normalize_params(raw_call.input.clone(), Some(target.as_ref())),
            ),
            target,
            wrapper_name: None,
        })
    }
}

/// 将 LLM 常见的参数别名归一化为工具 schema 使用的名称。
///
/// 仅当目标工具 schema 声明了 `file_path` 参数时才将 `path` 重命名为
/// `file_path`（Read/Write/Edit 等）。Grep/Glob 的 schema 参数名就是 `path`，
/// 无条件重命名会使其 `path` 参数丢失、静默回退全仓库搜索。
pub fn normalize_params(
    input: serde_json::Value,
    target: Option<&dyn BaseTool>,
) -> serde_json::Value {
    let mut obj = match input {
        serde_json::Value::Object(map) => map,
        _ => return input,
    };
    let accepts_file_path = target
        .map(|t| {
            t.parameters()
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|props| props.contains_key("file_path"))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if accepts_file_path && obj.contains_key("path") && !obj.contains_key("file_path") {
        if let Some(value) = obj.remove("path") {
            obj.insert("file_path".to_string(), value);
        }
    }
    serde_json::Value::Object(obj)
}
