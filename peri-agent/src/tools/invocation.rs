use peri_acp_types::builtin_mcp::original_tool_name_of_effective;
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

pub fn bind_target_invocation(
    raw_call: &ToolCall,
    target: Arc<dyn BaseTool>,
    normalized_input: serde_json::Value,
    wrapper_name: Option<String>,
) -> AgentResult<CanonicalToolInvocation> {
    match target
        .bind_invocation(normalized_input.clone())
        .map_err(|error| AgentError::ToolExecutionFailed {
            tool: target.name().to_string(),
            reason: error.to_string(),
        })? {
        Some(bound) => Ok(CanonicalToolInvocation {
            raw_call: raw_call.clone(),
            policy_call: ToolCall::new(raw_call.id.clone(), bound.policy_name, bound.policy_input),
            target: bound.target,
            wrapper_name,
        }),
        None => Ok(CanonicalToolInvocation {
            raw_call: raw_call.clone(),
            policy_call: ToolCall::new(
                raw_call.id.clone(),
                target.name().to_string(),
                normalized_input,
            ),
            target,
            wrapper_name,
        }),
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
        let normalized = normalize_params(raw_call.input.clone(), Some(target.as_ref()));
        bind_target_invocation(raw_call, target, normalized, None)
    }
}

/// 工具名 → 输入字段别名表（`(工具名, 别名, 规范名)`）。
///
/// v4-part-2（A4 ⑤，IF-D6 匹配型归一）：Web / Artifact 迁移为 builtin MCP 实例后，
/// `WebSearch` 在模型面的名字变成 effective name（`mcp__web__WebSearch`），而本表
/// 的行仍按**原始工具名**声明。匹配点（`normalize_params`）因此按「**原样优先，
/// 未命中再用归一后的原始名**」比较：裸名（迁移前口径）与 effective name（迁移后
/// 口径）都命中同一行。
///
/// 只为防御「模型输出 `search_term` 而没有 `query`」的静默失败：不命中时别名不生效、
/// 输入原样透传（无 normalize 面副作用）。**不得**在此表新增 effective name 行
/// （A4 禁止在消费点硬编码 `mcp__web__*`；归一表只有 `peri_acp_types::builtin_mcp`
/// 一份）。
const TOOL_PARAM_ALIASES: &[(&str, &str, &str)] = &[
    ("Write", "contents", "content"),
    ("Glob", "glob_pattern", "pattern"),
    ("Glob", "target_directory", "path"),
    ("WebSearch", "search_term", "query"),
];

fn apply_param_alias(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    declared_params: &serde_json::Map<String, serde_json::Value>,
    alias: &str,
    canonical: &str,
) {
    if declared_params.contains_key(canonical)
        && !declared_params.contains_key(alias)
        && obj.contains_key(alias)
        && !obj.contains_key(canonical)
    {
        let value = obj.remove(alias).expect("alias existence checked above");
        obj.insert(canonical.to_string(), value);
    }
}

/// 将 LLM 常见的参数别名归一化为工具 schema 使用的名称。
///
/// `path → file_path` 是文件工具的通用兼容；其余 alias 同时受目标工具名和
/// schema 约束，避免把字段名相似但语义不同的 API 强行兼容。
///
/// 工具名比较走 IF-D6 ⑤ 匹配型归一：先按 `target.name()` **原样**比较，未命中再用
/// [`original_tool_name_of_effective`] 得到的原始名比较一次——因此 builtin 一等工具的
/// effective name（如 `mcp__web__WebSearch`）与迁移前的裸名共享同一行别名；未命中
/// 归一表的名字（未知 / 外部 `mcp__*`）行为与迁移前逐位一致。
pub fn normalize_params(
    input: serde_json::Value,
    target: Option<&dyn BaseTool>,
) -> serde_json::Value {
    let mut obj = match input {
        serde_json::Value::Object(map) => map,
        _ => return input,
    };
    let Some(target) = target else {
        return serde_json::Value::Object(obj);
    };
    let parameters = target.parameters();
    let Some(declared_params) = parameters
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return serde_json::Value::Object(obj);
    };

    apply_param_alias(&mut obj, declared_params, "path", "file_path");
    let effective_name = target.name();
    let original_name = original_tool_name_of_effective(effective_name);
    for (tool_name, alias, canonical) in TOOL_PARAM_ALIASES {
        if effective_name.eq_ignore_ascii_case(tool_name)
            || original_name.is_some_and(|original| original.eq_ignore_ascii_case(tool_name))
        {
            apply_param_alias(&mut obj, declared_params, alias, canonical);
        }
    }
    serde_json::Value::Object(obj)
}

#[cfg(test)]
#[path = "invocation_test.rs"]
mod tests;
