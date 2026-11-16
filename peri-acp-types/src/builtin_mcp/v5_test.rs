//! `builtin_mcp::v5` 声明、映射与校验的契约测试。
//!
//! 边界：只断言声明数据与纯查表结果；默认注入、router 与代理行为不在本文件
//! 覆盖范围（它们仍走 v4 路径）。

use std::collections::HashSet;

use crate::tools::ContextRetention;

use super::*;

fn all_tools() -> Vec<(&'static BuiltinCapability, &'static CapabilityTool)> {
    BUILTIN_V5_CAPABILITIES
        .iter()
        .flat_map(|capability| capability.tools.iter().map(move |tool| (capability, tool)))
        .collect()
}

const fn test_tool(
    original_name: &'static str,
    model_name: &'static str,
    legacy_name: &'static str,
    direct: bool,
) -> CapabilityTool {
    CapabilityTool {
        original_name,
        model_name,
        legacy_name,
        direct,
        model_visible: true,
        advertised: true,
        prompt_declaration: None,
        context_retention: ContextRetention::Recomputable,
        completes_on_response: true,
    }
}

const fn test_capability(
    name: &'static str,
    legacy_instance: &'static str,
    tools: &'static [CapabilityTool],
) -> BuiltinCapability {
    test_capability_with_key(name, legacy_instance, tools)
}

const fn test_capability_with_key(
    name: &'static str,
    legacy_instance: &'static str,
    tools: &'static [CapabilityTool],
) -> BuiltinCapability {
    test_capability_full(name, legacy_instance, &[], tools, NO_RESOURCES)
}

const fn test_capability_full(
    name: &'static str,
    legacy_instance: &'static str,
    dependencies: &'static [&'static str],
    tools: &'static [CapabilityTool],
    resources: CapabilityResourceSurface,
) -> BuiltinCapability {
    BuiltinCapability {
        name,
        legacy_instance,
        dependencies,
        tools,
        resources,
        host_injection: NO_HOST_INJECTION,
        execution: in_process("test://capability"),
        state: NO_STATE,
    }
}

const fn test_capability_with_resources(
    name: &'static str,
    legacy_instance: &'static str,
    tools: &'static [CapabilityTool],
    resources: CapabilityResourceSurface,
) -> BuiltinCapability {
    test_capability_full(name, legacy_instance, &[], tools, resources)
}

const TOOL_A_DIRECT: &[CapabilityTool] = &[test_tool("ToolA", "ToolA", "ToolA", true)];
const TOOL_B_DIRECT: &[CapabilityTool] = &[test_tool("ToolB", "ToolB", "ToolB", true)];
const TOOL_A_DEFERRED: &[CapabilityTool] = &[test_tool(
    "ToolA",
    "mcp__peri__ToolA",
    "mcp__a__ToolA",
    false,
)];
const TOOL_BAD_MODEL: &[CapabilityTool] = &[test_tool("ToolA", "Wrong", "ToolA", true)];
const TOOL_EMPTY_NAME: &[CapabilityTool] = &[test_tool("", "", "", true)];
const TOOL_RESERVED_ONLY: &[CapabilityTool] = &[test_tool(
    "mcp__peri__Solo",
    "mcp__peri__Solo",
    "mcp__peri__Solo",
    true,
)];
const TOOL_RESERVED_DIRECT: &[CapabilityTool] = &[test_tool(
    "mcp__peri__Shared",
    "mcp__peri__Shared",
    "mcp__peri__Shared",
    true,
)];
const TOOL_SHARED_DEFERRED: &[CapabilityTool] = &[test_tool(
    "Shared",
    "mcp__peri__Shared",
    "mcp__b__Shared",
    false,
)];

#[test]
fn capability_table_is_frozen_and_matches_v5_names() {
    assert_eq!(BUILTIN_V5_SERVER_NAME, "peri");
    assert_eq!(BUILTIN_V5_DEFERRED_TOOL_PREFIX, "mcp__peri__");
    assert_eq!(BUILTIN_V5_RESERVED_SERVER_NAMES, &[BUILTIN_V5_SERVER_NAME]);

    let names: Vec<&str> = BUILTIN_V5_CAPABILITIES
        .iter()
        .map(|capability| capability.name)
        .collect();
    assert_eq!(
        names.iter().copied().collect::<HashSet<_>>().len(),
        names.len(),
        "能力名必须唯一"
    );
    assert_eq!(
        names,
        vec![
            "workspace",
            "web",
            "artifact",
            "cron",
            "monitor",
            "workflow"
        ]
    );
}

/// 迁移期 v4 视图必须只是能力表的投影：不允许另存工具、开关键或任务归属。
#[test]
fn v4_compat_view_is_derived_from_capability_table() {
    let instances = &*crate::builtin_mcp::BUILTIN_MCP_INSTANCES;
    assert_eq!(instances.len(), BUILTIN_V5_CAPABILITIES.len());
    for (view, capability) in instances.iter().zip(BUILTIN_V5_CAPABILITIES) {
        assert_eq!(view.name, capability.name);
        assert_eq!(view.instance, capability.legacy_instance);
        assert_eq!(
            view.task_owner_scoped, capability.host_injection.task_owner_scoped,
            "能力 {} 的 task owner 作用域漂移",
            capability.name
        );
        assert_eq!(view.tools, capability.tools, "工具表不得复制");
    }
    let reserved: Vec<&str> = crate::builtin_mcp::BUILTIN_RESERVED_INSTANCE_NAMES
        .iter()
        .copied()
        .collect();
    let derived: Vec<&str> = BUILTIN_V5_CAPABILITIES
        .iter()
        .map(|capability| capability.legacy_instance)
        .collect();
    assert_eq!(reserved, derived, "保留实例名必须由能力表派生");
}

#[test]
fn model_names_follow_single_exit_naming() {
    for (capability, tool) in all_tools() {
        if tool.direct {
            assert_eq!(
                tool.model_name, tool.original_name,
                "direct 工具必须保持原名: {}",
                tool.original_name
            );
            assert!(
                !tool.model_name.starts_with(BUILTIN_V5_DEFERRED_TOOL_PREFIX),
                "direct 工具不得占用 deferred 命名空间: {}",
                tool.original_name
            );
        } else {
            assert_eq!(
                tool.model_name,
                format!("{BUILTIN_V5_DEFERRED_TOOL_PREFIX}{}", tool.original_name),
                "deferred 工具必须使用单出口前缀: {}",
                tool.original_name
            );
            assert_ne!(
                tool.model_name, tool.original_name,
                "deferred 工具不得使用裸原名: {}",
                tool.original_name
            );
        }
        assert_eq!(
            capability_tool(capability.name, tool.original_name)
                .expect("执行身份必须可查")
                .model_name,
            tool.model_name
        );
    }
    // 逐字哨兵：表驱动断言在同一次改错下仍可能自洽通过。
    assert_eq!(
        capability_tool("cron", "cron_register")
            .expect("cron_register")
            .model_name,
        "mcp__peri__cron_register"
    );
    assert_eq!(
        capability_tool("workspace", "Read")
            .expect("Read")
            .model_name,
        "Read"
    );
}

#[path = "v5_test/validation_test.rs"]
mod validation_tests;
