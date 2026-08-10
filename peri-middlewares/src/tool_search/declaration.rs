//! 提示词层声明收集器（design v2 §2.5.2/2.5.3）
//!
//! 遍历 LLM 可见工具集，收集非 None 的 `prompt_declaration()` 模板，
//! 渲染 4 个占位符后按 (namespace, name) 字典序拼接为声明段。

use std::sync::Arc;

use peri_acp_types::tools::ToolDescription;
use peri_agent::tools::BaseTool;

/// 收集声明段：渲染非 None 模板，按 (namespace, name) 字典序排序拼接。
///
/// - `prompt_declaration()` 为 `None` 的工具跳过（默认行为基线）
/// - 排序键：namespace（`None` 按空串）→ name；跨会话输出字节级稳定
/// - 条目间以 `\n` 分隔；空集返回 `None`（调用方保持无声明段语义）
pub fn collect_declarations(tools: &[Arc<dyn BaseTool>]) -> Option<String> {
    let mut rendered: Vec<(String, String, String)> = tools
        .iter()
        .filter_map(|tool| {
            let template = tool.prompt_declaration()?;
            let desc = tool.tool_description();
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
