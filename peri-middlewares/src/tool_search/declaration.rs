//! 提示词层声明收集器（design v2 §2.5.2/2.5.3）
//!
//! 遍历 LLM 可见工具集，收集非 None 的声明模板，渲染 4 个占位符后按
//! (namespace, name) 字典序拼接为声明段。
//!
//! **A9（v4-part-2）**：Web / Artifact 迁移为 builtin MCP 实例后，三个工具在模型面
//! 的名字变为 effective name（`mcp__web__WebSearch` / `mcp__web__WebFetch` /
//! `mcp__artifact__artifact`），声明段**不得因此丢失**。模板的唯一数据源是声明表
//! [`peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES`]（逐工具
//! `prompt_declaration`，逐字搬运迁移前的模板）；本收集器按工具名取模板，
//! `{{name}}` 仍由既有渲染规则填入 effective name。

use std::sync::Arc;

use peri_acp_types::tools::ToolDescription;
use peri_agent::tools::BaseTool;

/// 收集声明段：渲染非 None 模板，按 (namespace, name) 字典序排序拼接。
///
/// - 无模板的工具跳过（工具实现与 builtin 声明表都没有，默认行为基线；
///   模板来源见 [`declaration_template`]）
/// - 排序键：namespace（`None` 按空串）→ name；跨会话输出字节级稳定
/// - 条目间以 `\n` 分隔；空集返回 `None`（调用方保持无声明段语义）
pub fn collect_declarations(tools: &[Arc<dyn BaseTool>]) -> Option<String> {
    let mut rendered: Vec<(String, String, String)> = tools
        .iter()
        .filter_map(|tool| {
            let desc = tool.tool_description();
            let template = declaration_template(tool.as_ref(), &desc.name)?;
            Some((
                desc.namespace.clone().unwrap_or_default(),
                desc.name.clone(),
                render_template(&template, &desc),
            ))
        })
        .collect();
    rendered.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    if rendered.is_empty() {
        return None;
    }
    Some(
        rendered
            .into_iter()
            .map(|(_, _, text)| text)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// 取工具的声明模板（A9）。
///
/// 1. 工具自身实现 `prompt_declaration()`（既有契约）→ 直接使用；
/// 2. 否则查 builtin 声明表：builtin 一等工具以 MCP bridge 形态进入 direct 工具集，
///    桥侧当前**未**实现 `prompt_declaration()`，而迁移不允许丢掉声明段，因此按
///    effective name 取表内模板（同一张声明表，不新增反查表、不硬编码 `mcp__*` 字面量）。
///    桥侧接线落地后分支 1 命中，本回退自然不再参与。
fn declaration_template(tool: &dyn BaseTool, name: &str) -> Option<String> {
    tool.prompt_declaration()
        .or_else(|| builtin_declaration(name).map(str::to_string))
}

/// 声明表里该 effective name 的声明模板（未命中返回 `None`）。
fn builtin_declaration(effective_name: &str) -> Option<&'static str> {
    peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .find(|tool| tool.effective_name == effective_name)
        .and_then(|tool| tool.prompt_declaration)
}

/// 渲染声明模板：单遍扫描替换 `{{name}}`/`{{title}}`/`{{description}}`/`{{namespace}}`。
///
/// 纪律（design v2 §2.5.3）：**禁止链式 `str::replace`**——description 值可能含
/// 字面 `{{ }}`（JSON/泛型示例），链式替换会把占位符误替换进 description 文本。
/// 未识别占位符原样保留（宽松保留 + 测试兜底，不中断主循环）。
fn render_template(template: &str, desc: &ToolDescription) -> String {
    let mut out = String::with_capacity(template.len() + desc.description.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("}}") {
            Some(rel) => {
                let placeholder = &rest[start + 2..start + 2 + rel];
                let end = start + 2 + rel + 2;
                match placeholder {
                    "name" => out.push_str(&desc.name),
                    "title" => out.push_str(desc.title.as_deref().unwrap_or("")),
                    "description" => out.push_str(&desc.description),
                    "namespace" => out.push_str(desc.namespace.as_deref().unwrap_or("")),
                    // 未识别占位符：原样保留
                    _ => out.push_str(&rest[start..end]),
                }
                rest = &rest[end..];
            }
            // 无闭合 `}}`：剩余全部原样输出
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
#[path = "declaration_test.rs"]
mod tests;
