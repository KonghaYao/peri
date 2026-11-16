//! v5 名字归一与执行路由。
//!
//! 执行身份是 `(capability, original_name)`；本模块只负责入口处的一次名字解析。
//! 旧 `mcp__<legacy_instance>__*` 只返回展示结果，永远不是执行别名。

use super::{
    capability_of_legacy_instance, BuiltinCapability, CapabilityNameError, CapabilityTool,
    BUILTIN_V5_CAPABILITIES,
};

/// 执行路由结果：模型面名字解析出的唯一 `(capability, tool)`。
///
/// 这是 router 的入口类型；旧 `mcp__<instance>__*` 名字不会构造出本类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutedTool {
    /// 能力名（执行身份的一半）。
    pub capability: &'static str,
    /// 工具声明（原始名是执行身份的另一半）。
    pub tool: &'static CapabilityTool,
}

/// 展示 / 匹配归一结果。**不是执行身份**：执行路由必须使用
/// [`crate::builtin_mcp::v5::capability_tool`] 或 [`resolve_model_tool`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayToolName {
    /// 能力名。
    pub capability: &'static str,
    /// 原始工具名。
    pub original_name: &'static str,
    /// v5 模型面名字（展示用）。
    pub model_name: &'static str,
    /// 迁移期旧模型面名字（只做展示 / 匹配归一）。
    pub legacy_name: &'static str,
    /// 命中的名字来源。
    pub source: ToolNameSource,
}

/// 展示归一命中的名字来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolNameSource {
    /// 只命中 v5 模型面名字（deferred `mcp__peri__*`）。
    ModelName,
    /// 只命中迁移期旧名字（旧 deferred `mcp__<instance>__*`）。
    LegacyName,
    /// direct 工具的新旧名字相同。
    SharedDirectName,
}

/// 模型面名字 → 执行路由身份。**只接受 v5 模型面名字**。
///
/// 旧 deferred 名字（`mcp__<instance>__*`）返回
/// [`CapabilityNameError::LegacyNameNotExecutable`]，显式拒绝执行别名。
pub fn resolve_model_tool(model_name: &str) -> Result<RoutedTool, CapabilityNameError> {
    let mut matched = None;
    for capability in BUILTIN_V5_CAPABILITIES {
        for tool in capability.tools {
            if tool.model_name != model_name {
                continue;
            }
            if matched.is_some() {
                return Err(CapabilityNameError::AmbiguousToolName {
                    name: model_name.to_owned(),
                });
            }
            matched = Some(RoutedTool {
                capability: capability.name,
                tool,
            });
        }
    }
    if let Some(routed) = matched {
        return Ok(routed);
    }
    if !legacy_tool_matches(model_name).is_empty() {
        return Err(CapabilityNameError::LegacyNameNotExecutable {
            name: model_name.to_owned(),
        });
    }
    Err(CapabilityNameError::UnknownToolName {
        name: model_name.to_owned(),
    })
}

/// 展示 / 匹配归一：接受 v5 模型面名字或迁移期旧名。
///
/// 结果类型 [`DisplayToolName`] 不得用于执行路由；未知名字返回显式错误。
pub fn resolve_display_tool_name(name: &str) -> Result<DisplayToolName, CapabilityNameError> {
    let mut model_matches = Vec::new();
    for capability in BUILTIN_V5_CAPABILITIES {
        for tool in capability.tools {
            if tool.model_name == name {
                model_matches.push((capability, tool));
            }
        }
    }
    if model_matches.len() > 1 {
        return Err(CapabilityNameError::AmbiguousToolName {
            name: name.to_owned(),
        });
    }
    if let Some((capability, tool)) = model_matches.first().copied() {
        return Ok(DisplayToolName {
            capability: capability.name,
            original_name: tool.original_name,
            model_name: tool.model_name,
            legacy_name: tool.legacy_name,
            source: if tool.direct {
                ToolNameSource::SharedDirectName
            } else {
                ToolNameSource::ModelName
            },
        });
    }

    let legacy_matches = legacy_tool_matches(name);
    if legacy_matches.len() > 1 {
        return Err(CapabilityNameError::AmbiguousToolName {
            name: name.to_owned(),
        });
    }
    if let Some((capability, tool)) = legacy_matches.first().copied() {
        return Ok(DisplayToolName {
            capability: capability.name,
            original_name: tool.original_name,
            model_name: tool.model_name,
            legacy_name: tool.legacy_name,
            // direct 原名的新旧拼写相同才会走到 SharedDirect；历史
            // `mcp__<instance>__*` 拼接（含 direct 工具）一律是 Legacy。
            source: if name == tool.model_name {
                ToolNameSource::SharedDirectName
            } else {
                ToolNameSource::LegacyName
            },
        });
    }

    Err(CapabilityNameError::UnknownToolName {
        name: name.to_owned(),
    })
}

/// 迁移期旧名字 → 展示归一结果；v5-only 名字显式返回
/// [`CapabilityNameError::NotLegacyToolName`]。
pub fn normalize_legacy_tool_name(name: &str) -> Result<DisplayToolName, CapabilityNameError> {
    let resolved = resolve_display_tool_name(name)?;
    if resolved.source == ToolNameSource::ModelName {
        return Err(CapabilityNameError::NotLegacyToolName {
            name: name.to_owned(),
        });
    }
    Ok(resolved)
}

/// 旧 `(instance, original_name)` → 新能力 + 新模型面名字；映射失败显式报错。
pub fn map_legacy_tool(
    instance: &str,
    original_name: &str,
) -> Result<DisplayToolName, CapabilityNameError> {
    let capability = capability_of_legacy_instance(instance)?;
    let tool = capability
        .tools
        .iter()
        .find(|tool| tool.original_name == original_name)
        .ok_or_else(|| CapabilityNameError::UnknownTool {
            capability: capability.name.to_owned(),
            original_name: original_name.to_owned(),
        })?;
    Ok(DisplayToolName {
        capability: capability.name,
        original_name: tool.original_name,
        model_name: tool.model_name,
        legacy_name: tool.legacy_name,
        source: if tool.direct {
            ToolNameSource::SharedDirectName
        } else {
            ToolNameSource::LegacyName
        },
    })
}

fn legacy_tool_matches(name: &str) -> Vec<(&'static BuiltinCapability, &'static CapabilityTool)> {
    BUILTIN_V5_CAPABILITIES
        .iter()
        .flat_map(|capability| {
            capability
                .tools
                .iter()
                .filter(move |tool| {
                    // 与旧归一入口同口径：既接受冻结的旧名，也接受历史
                    // `mcp__<instance>__<original>` 拼接（direct 工具亦可有这种旧
                    // 拼写）。只做精确归一，不折叠大小写。
                    let namespaced = name
                        .strip_prefix("mcp__")
                        .and_then(|rest| rest.strip_prefix(capability.legacy_instance))
                        .and_then(|rest| rest.strip_prefix("__"));
                    tool.legacy_name == name || namespaced == Some(tool.original_name)
                })
                .map(move |tool| (capability, tool))
        })
        .collect()
}
