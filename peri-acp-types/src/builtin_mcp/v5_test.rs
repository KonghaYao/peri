//! `builtin_mcp::v5` 声明、映射与加载期校验的契约测试。
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
    test_capability_with_key(name, legacy_instance, "TestMiddleware", tools)
}

const fn test_capability_with_key(
    name: &'static str,
    legacy_instance: &'static str,
    legacy_policy_key: &'static str,
    tools: &'static [CapabilityTool],
) -> BuiltinCapability {
    test_capability_full(
        name,
        legacy_instance,
        legacy_policy_key,
        &[],
        tools,
        NO_RESOURCES,
    )
}

const fn test_capability_full(
    name: &'static str,
    legacy_instance: &'static str,
    legacy_policy_key: &'static str,
    dependencies: &'static [&'static str],
    tools: &'static [CapabilityTool],
    resources: CapabilityResourceSurface,
) -> BuiltinCapability {
    BuiltinCapability {
        name,
        legacy_instance,
        legacy_policy_key,
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
    legacy_policy_key: &'static str,
    tools: &'static [CapabilityTool],
    resources: CapabilityResourceSurface,
) -> BuiltinCapability {
    test_capability_full(
        name,
        legacy_instance,
        legacy_policy_key,
        &[],
        tools,
        resources,
    )
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
        assert_eq!(view.legacy_policy_key, capability.legacy_policy_key);
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

#[test]
fn registry_validation_accepts_frozen_table() {
    validate_registry().expect("冻结的 v5 声明表必须通过加载期校验");
    validate_capabilities(BUILTIN_V5_CAPABILITIES).expect("同一张表显式校验也必须通过");
}

#[test]
fn validation_rejects_duplicate_capability_name() {
    const CAPABILITIES: &[BuiltinCapability] = &[
        test_capability_with_key("same", "a", "PolicyA", TOOL_A_DIRECT),
        test_capability_with_key("same", "b", "PolicyB", TOOL_B_DIRECT),
    ];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::DuplicateCapabilityName {
            name: "same".to_owned()
        })
    );
}

#[test]
fn validation_rejects_duplicate_legacy_instance() {
    const CAPABILITIES: &[BuiltinCapability] = &[
        test_capability_with_key("a", "shared", "PolicyA", TOOL_A_DIRECT),
        test_capability_with_key("b", "shared", "PolicyB", TOOL_B_DIRECT),
    ];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::DuplicateLegacyInstance {
            instance: "shared".to_owned(),
            first: "a".to_owned(),
            second: "b".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_duplicate_original_tool_name() {
    const CAPABILITIES: &[BuiltinCapability] = &[
        test_capability_with_key("a", "a", "PolicyA", TOOL_A_DEFERRED),
        test_capability_with_key("b", "b", "PolicyB", TOOL_A_DIRECT),
    ];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::DuplicateOriginalToolName {
            original_name: "ToolA".to_owned(),
            first: "a".to_owned(),
            second: "b".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_duplicate_model_tool_name() {
    // direct 工具占用 `mcp__peri__*` 原名，与 deferred 模型面冲突。
    const CAPABILITIES: &[BuiltinCapability] = &[
        test_capability_with_key("a", "a", "PolicyA", TOOL_RESERVED_DIRECT),
        test_capability_with_key("b", "b", "PolicyB", TOOL_SHARED_DEFERRED),
    ];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::DuplicateModelToolName {
            model_name: "mcp__peri__Shared".to_owned(),
            first: "a".to_owned(),
            second: "b".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_invalid_model_name() {
    const CAPABILITIES: &[BuiltinCapability] = &[test_capability("a", "a", TOOL_BAD_MODEL)];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::InvalidModelToolName {
            capability: "a".to_owned(),
            original_name: "ToolA".to_owned(),
            model_name: "Wrong".to_owned(),
            expected: "ToolA".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_reserved_tool_namespace() {
    const CAPABILITIES: &[BuiltinCapability] = &[test_capability("a", "a", TOOL_RESERVED_ONLY)];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::ReservedToolName {
            capability: "a".to_owned(),
            original_name: "mcp__peri__Solo".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_empty_capability_and_tool_name() {
    const EMPTY_TOOLS: &[CapabilityTool] = &[];
    assert_eq!(
        validate_capabilities(&[test_capability("a", "a", EMPTY_TOOLS)]),
        Err(CapabilityRegistryError::EmptyCapabilityTools {
            capability: "a".to_owned()
        })
    );
    assert_eq!(
        validate_capabilities(&[test_capability("a", "a", TOOL_EMPTY_NAME)]),
        Err(CapabilityRegistryError::EmptyToolName {
            capability: "a".to_owned()
        })
    );
    assert_eq!(
        validate_capabilities(&[test_capability("", "", TOOL_A_DIRECT)]),
        Err(CapabilityRegistryError::EmptyCapabilityName)
    );
}

#[test]
fn user_defined_peri_and_migration_names_are_reserved() {
    for name in [
        "peri",
        "workspace",
        "web",
        "artifact",
        "cron",
        "monitor",
        "workflow",
    ] {
        assert!(is_reserved_server_name(name), "{name} 必须是保留名");
        assert_eq!(
            validate_user_server_name(name),
            Err(CapabilityRegistryError::ReservedUserServerName {
                name: name.to_owned()
            }),
            "{name} 不得被用户配置接管"
        );
    }
    assert!(!is_reserved_server_name("my-server"));
    assert_eq!(validate_user_server_name("my-server"), Ok(()));
    assert_eq!(
        validate_user_server_name(""),
        Err(CapabilityRegistryError::EmptyServerName)
    );
}

#[test]
fn legacy_names_normalize_to_v5_model_names() {
    for capability in BUILTIN_V5_CAPABILITIES {
        for tool in capability.tools {
            let legacy_name = tool.legacy_name;
            let normalized =
                resolve_display_tool_name(legacy_name).expect("旧名必须可归一到展示名");
            assert_eq!(normalized.capability, capability.name);
            assert_eq!(normalized.original_name, tool.original_name);
            assert_eq!(normalized.model_name, tool.model_name);
            assert_eq!(normalized.legacy_name, legacy_name);
            assert_eq!(
                normalized.source,
                if tool.direct {
                    ToolNameSource::SharedDirectName
                } else {
                    ToolNameSource::LegacyName
                }
            );
            assert_eq!(
                normalize_legacy_tool_name(legacy_name).expect("旧名归一 API 必须命中"),
                normalized
            );

            let current = resolve_display_tool_name(tool.model_name).expect("新名必须可归一");
            assert_eq!(current.capability, capability.name);
            assert_eq!(current.original_name, tool.original_name);
            assert_eq!(current.legacy_name, legacy_name);
            assert_eq!(
                current.source,
                if tool.direct {
                    ToolNameSource::SharedDirectName
                } else {
                    ToolNameSource::ModelName
                }
            );
        }
    }
    // 旧 transcript 可能保存 direct 工具的 `mcp__<instance>__*` 拼接拼写；
    // 只做展示归一，模型面名字仍是 direct 原名。
    let namespaced_direct =
        resolve_display_tool_name("mcp__web__WebSearch").expect("旧 namespaced direct 名可归一");
    assert_eq!(namespaced_direct.capability, "web");
    assert_eq!(namespaced_direct.original_name, "WebSearch");
    assert_eq!(namespaced_direct.model_name, "WebSearch");
    assert_eq!(namespaced_direct.source, ToolNameSource::LegacyName);
    assert_eq!(
        normalize_legacy_tool_name("mcp__workspace__Read")
            .expect("旧 namespaced direct 名可归一")
            .model_name,
        "Read"
    );
}

#[test]
fn legacy_instance_and_tool_mapping_is_deterministic() {
    for capability in BUILTIN_V5_CAPABILITIES {
        assert_eq!(
            capability_of_legacy_instance(capability.legacy_instance)
                .expect("旧实例必须映射")
                .name,
            capability.name
        );
        for tool in capability.tools {
            let mapped = map_legacy_tool(capability.legacy_instance, tool.original_name)
                .expect("旧工具必须映射");
            assert_eq!(mapped.capability, capability.name);
            assert_eq!(mapped.original_name, tool.original_name);
            assert_eq!(mapped.model_name, tool.model_name);
            assert_eq!(
                mapped.source,
                if tool.direct {
                    ToolNameSource::SharedDirectName
                } else {
                    ToolNameSource::LegacyName
                }
            );
        }
    }
    assert_eq!(
        capability_of_legacy_instance("nope"),
        Err(CapabilityNameError::UnknownLegacyInstance {
            instance: "nope".to_owned()
        })
    );
    assert_eq!(
        map_legacy_tool("workspace", "nope"),
        Err(CapabilityNameError::UnknownTool {
            capability: "workspace".to_owned(),
            original_name: "nope".to_owned()
        })
    );
}

#[test]
fn execution_routing_rejects_legacy_names() {
    let routed = resolve_model_tool("mcp__peri__cron_register").expect("新 deferred 名可路由");
    assert_eq!(routed.capability, "cron");
    assert_eq!(routed.tool.original_name, "cron_register");

    let direct = resolve_model_tool("Read").expect("direct 原名可路由");
    assert_eq!(direct.capability, "workspace");
    assert_eq!(direct.tool.original_name, "Read");

    for legacy in [
        "mcp__cron__cron_register",
        "mcp__workflow__Workflow",
        "mcp__web__WebSearch",
    ] {
        assert_eq!(
            resolve_model_tool(legacy),
            Err(CapabilityNameError::LegacyNameNotExecutable {
                name: legacy.to_owned()
            }),
            "旧 deferred 名字不得作为执行别名"
        );
    }
    assert_eq!(
        normalize_legacy_tool_name("mcp__peri__cron_register"),
        Err(CapabilityNameError::NotLegacyToolName {
            name: "mcp__peri__cron_register".to_owned()
        }),
        "旧名归一 API 必须拒绝 v5-only 名字"
    );
    assert!(
        normalize_legacy_tool_name("mcp__cron__cron_register").is_ok(),
        "旧名归一 API 必须接受旧 deferred 名"
    );
    for unknown in ["", "mcp__unknown__x", "mcp__peri__unknown", "websearch"] {
        assert_eq!(
            resolve_model_tool(unknown),
            Err(CapabilityNameError::UnknownToolName {
                name: unknown.to_owned()
            }),
            "{unknown} 必须显式失败"
        );
    }
}

#[test]
fn unknown_capability_and_tool_are_explicit() {
    assert!(capability("nope").is_none());
    assert_eq!(
        capability_tool("nope", "Read"),
        Err(CapabilityNameError::UnknownCapability {
            name: "nope".to_owned()
        })
    );
    assert_eq!(
        capability_tool("workspace", "read"),
        Err(CapabilityNameError::UnknownTool {
            capability: "workspace".to_owned(),
            original_name: "read".to_owned()
        }),
        "原始名匹配区分大小写"
    );
    assert_eq!(
        resolve_display_tool_name("mcp__cron__unknown"),
        Err(CapabilityNameError::UnknownToolName {
            name: "mcp__cron__unknown".to_owned()
        })
    );
}

#[test]
fn host_injection_and_resources_are_frozen() {
    let scoped: HashSet<&str> = BUILTIN_V5_CAPABILITIES
        .iter()
        .filter(|capability| capability.host_injection.task_owner_scoped)
        .map(|capability| capability.name)
        .collect();
    assert_eq!(scoped, HashSet::from(["monitor", "workspace"]));
    for capability in BUILTIN_V5_CAPABILITIES {
        if capability.host_injection.task_owner_scoped {
            assert_eq!(
                capability.host_injection.task_owner_meta_key,
                Some(TASK_OWNER_META_KEY)
            );
            assert_eq!(
                capability.host_injection.thread_owner_meta_key,
                Some(THREAD_OWNER_META_KEY)
            );
        } else {
            assert_eq!(capability.host_injection.task_owner_meta_key, None);
            assert_eq!(capability.host_injection.thread_owner_meta_key, None);
        }
    }
    assert_eq!(
        capability("workflow")
            .unwrap()
            .host_injection
            .consumer_meta_key,
        Some(WORKFLOW_CONSUMER_META_KEY)
    );

    let workspace = capability("workspace").unwrap();
    assert_eq!(
        workspace.resources.default_subscriptions,
        &["workspace://git/ref"]
    );
    assert!(workspace.resources.uri_prefixes.contains(&"workspace://"));
    assert!(workspace.resources.uri_prefixes.contains(&"skill://"));
    assert!(capability("cron")
        .unwrap()
        .resources
        .static_resources
        .contains(&"cron://jobs"));
    assert!(capability("workflow")
        .unwrap()
        .resources
        .static_resources
        .contains(&"workflow://runs"));

    let mut prefixes = HashSet::new();
    for capability in BUILTIN_V5_CAPABILITIES {
        for prefix in capability.resources.uri_prefixes {
            assert!(
                prefixes.insert(*prefix),
                "资源 URI 前缀 {prefix} 不得被多个能力声明"
            );
        }
    }
}

#[test]
fn execution_binding_and_state_roots_are_frozen() {
    for capability in BUILTIN_V5_CAPABILITIES {
        match capability.name {
            "workspace" => {
                assert_eq!(
                    capability.execution.binding,
                    CapabilityExecutionBinding::SandboxHttpProxy
                );
                assert_eq!(
                    capability.state.state_root,
                    CapabilityStateRoot::SandboxWorkspace
                );
            }
            _ => {
                assert_eq!(
                    capability.execution.binding,
                    CapabilityExecutionBinding::InProcess
                );
            }
        }
    }
    let web = capability("web").unwrap().state;
    assert_eq!(web.credential_namespace, Some("web"));
    assert_eq!(
        web.credential_keys,
        &[
            "PERI_WEB_SEARCH_URL",
            "PERI_WEB_SEARCH_TOKEN",
            "PERI_WEB_FETCH_URL",
            "PERI_WEB_FETCH_TOKEN",
        ]
    );
    let artifact = capability("artifact").unwrap().state;
    assert_eq!(artifact.credential_namespace, Some("artifact"));
    assert_eq!(
        artifact.credential_keys,
        &["PERI_ARTIFACTS_URL", "PERI_ARTIFACTS_TOKEN"]
    );
    for name in ["workspace", "cron", "monitor", "workflow"] {
        assert_eq!(
            capability(name).unwrap().state.credential_namespace,
            None,
            "{name} 不应声明环境凭据命名空间"
        );
    }
    assert_eq!(
        capability("cron").unwrap().state.state_root,
        CapabilityStateRoot::HostScheduler
    );
    assert_eq!(
        capability("monitor").unwrap().state.state_root,
        CapabilityStateRoot::SessionHostMemory
    );
    assert_eq!(
        capability("workflow").unwrap().state.state_root,
        CapabilityStateRoot::WorkflowRuntime
    );
}

#[test]
fn host_only_and_completion_tools_stay_non_direct() {
    for (_, tool) in all_tools() {
        if matches!(
            tool.original_name,
            "cron_acknowledge"
                | "WorkflowTaskObserved"
                | "WorkflowAgentClaim"
                | "WorkflowConsumerReady"
                | "WorkflowCompletionAck"
                | "WorkflowClose"
                | "WorkflowAgentResult"
        ) {
            assert!(!tool.model_visible, "{} 必须 host-only", tool.original_name);
            assert!(!tool.direct, "{} 必须 deferred", tool.original_name);
        }
    }
}

#[test]
fn switch_mapping_is_capability_first_and_deterministic() {
    for capability in BUILTIN_V5_CAPABILITIES {
        let by_name = capability_of_switch(capability.name).expect("能力名必须可作为开关");
        let by_legacy =
            capability_of_switch(capability.legacy_policy_key).expect("旧键必须确定性映射");
        assert_eq!(by_name.name, capability.name);
        assert_eq!(by_legacy.name, capability.name, "旧键不得映射到第二个能力");
        assert_eq!(
            capability_of_legacy_policy_key(capability.legacy_policy_key)
                .expect("旧键映射 API 必须命中")
                .name,
            capability.name
        );
    }
    assert_eq!(
        capability_of_switch("WebMiddleware").map(|capability| capability.name),
        Ok("web")
    );
    assert_eq!(
        capability_of_switch("web").map(|capability| capability.name),
        Ok("web")
    );
    assert_eq!(
        capability_of_switch("nope"),
        Err(CapabilityNameError::UnknownSwitch {
            key: "nope".to_owned()
        })
    );
    assert_eq!(
        capability_of_legacy_policy_key("nope"),
        Err(CapabilityNameError::UnknownPolicyKey {
            key: "nope".to_owned()
        })
    );
    let declared: HashSet<&str> = legacy_policy_keys().collect();
    assert_eq!(declared.len(), BUILTIN_V5_CAPABILITIES.len());
}

#[test]
fn resource_ownership_is_unique() {
    assert_eq!(
        capability_of_resource_uri("workspace://git/ref").map(|capability| capability.name),
        Ok("workspace")
    );
    assert_eq!(
        capability_of_resource_uri("skill://user/x/SKILL.md").map(|capability| capability.name),
        Ok("workspace")
    );
    assert_eq!(
        capability_of_resource_uri("cron://jobs").map(|capability| capability.name),
        Ok("cron")
    );
    assert_eq!(
        capability_of_resource_uri("workflow://runs/abc").map(|capability| capability.name),
        Ok("workflow")
    );
    assert_eq!(
        capability_of_resource_uri("unknown://x"),
        Err(CapabilityNameError::UnknownResourceUri {
            uri: "unknown://x".to_owned()
        })
    );
}

#[test]
fn validation_rejects_duplicate_legacy_policy_key() {
    const CAPABILITIES: &[BuiltinCapability] = &[
        test_capability_with_key("a", "a", "SharedMiddleware", TOOL_A_DIRECT),
        test_capability_with_key("b", "b", "SharedMiddleware", TOOL_B_DIRECT),
    ];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::DuplicateLegacyPolicyKey {
            key: "SharedMiddleware".to_owned(),
            first: "a".to_owned(),
            second: "b".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_policy_key_that_shadows_capability_name() {
    const CAPABILITIES: &[BuiltinCapability] = &[
        test_capability_with_key("a", "a", "b", TOOL_A_DIRECT),
        test_capability_with_key("b", "b", "PolicyB", TOOL_B_DIRECT),
    ];
    assert_eq!(
        validate_capabilities(CAPABILITIES),
        Err(CapabilityRegistryError::DuplicateLegacyPolicyKey {
            key: "b".to_owned(),
            first: "capability name".to_owned(),
            second: "a".to_owned(),
        })
    );
}

#[test]
fn validation_rejects_invalid_legacy_names_and_reserved_peri() {
    const BAD_LEGACY: &[CapabilityTool] = &[test_tool("ToolA", "mcp__peri__ToolA", "wrong", false)];
    assert_eq!(
        validate_capabilities(&[test_capability_full(
            "a",
            "a",
            "PolicyA",
            &[],
            BAD_LEGACY,
            NO_RESOURCES,
        )]),
        Err(CapabilityRegistryError::InvalidLegacyToolName {
            capability: "a".to_owned(),
            original_name: "ToolA".to_owned(),
            legacy_name: "wrong".to_owned(),
            expected: "mcp__a__ToolA".to_owned(),
        })
    );
    assert_eq!(
        validate_capabilities(&[test_capability("peri", "peri", TOOL_A_DIRECT)]),
        Err(CapabilityRegistryError::ReservedCapabilityName {
            capability: "peri".to_owned()
        })
    );
    assert_eq!(
        validate_capabilities(&[test_capability_with_key("a", "", "PolicyA", TOOL_A_DIRECT)]),
        Err(CapabilityRegistryError::EmptyLegacyInstance {
            capability: "a".to_owned()
        })
    );
    assert_eq!(
        validate_capabilities(&[test_capability_with_key("a", "a", "", TOOL_A_DIRECT)]),
        Err(CapabilityRegistryError::EmptyLegacyPolicyKey {
            capability: "a".to_owned()
        })
    );
}

const RES_A: CapabilityResourceSurface = CapabilityResourceSurface {
    uri_prefixes: &["a://"],
    static_resources: &["a://root"],
    resource_templates: &["a://item/{id}"],
    default_subscriptions: &[],
};
const RES_B: CapabilityResourceSurface = CapabilityResourceSurface {
    uri_prefixes: &["b://"],
    static_resources: &["b://root"],
    resource_templates: &[],
    default_subscriptions: &[],
};
const RES_DUPLICATE_PREFIX: CapabilityResourceSurface = CapabilityResourceSurface {
    uri_prefixes: &["a://"],
    static_resources: &[],
    resource_templates: &[],
    default_subscriptions: &[],
};
const RES_OVERLAP: CapabilityResourceSurface = CapabilityResourceSurface {
    uri_prefixes: &["a://nested"],
    static_resources: &["a://nested/root"],
    resource_templates: &[],
    default_subscriptions: &[],
};
const RES_NOT_OWNED: CapabilityResourceSurface = CapabilityResourceSurface {
    uri_prefixes: &["a://"],
    static_resources: &["b://escape"],
    resource_templates: &[],
    default_subscriptions: &[],
};

#[test]
fn validation_rejects_resource_conflicts() {
    assert_eq!(
        validate_capabilities(&[
            test_capability_with_resources("a", "a", "PolicyA", TOOL_A_DIRECT, RES_A),
            test_capability_with_resources(
                "b",
                "b",
                "PolicyB",
                TOOL_B_DIRECT,
                RES_DUPLICATE_PREFIX
            ),
        ]),
        Err(CapabilityRegistryError::DuplicateResourceUriPrefix {
            prefix: "a://".to_owned(),
            first: "a".to_owned(),
            second: "b".to_owned(),
        })
    );
    assert_eq!(
        validate_capabilities(&[
            test_capability_with_resources("a", "a", "PolicyA", TOOL_A_DIRECT, RES_A),
            test_capability_with_resources("b", "b", "PolicyB", TOOL_B_DIRECT, RES_OVERLAP),
        ]),
        Err(CapabilityRegistryError::OverlappingResourceUriPrefix {
            first: "a://".to_owned(),
            second: "a://nested".to_owned(),
        })
    );
    assert_eq!(
        validate_capabilities(&[test_capability_with_resources(
            "a",
            "a",
            "PolicyA",
            TOOL_A_DIRECT,
            RES_NOT_OWNED
        )]),
        Err(CapabilityRegistryError::ResourceNotOwned {
            capability: "a".to_owned(),
            uri: "b://escape".to_owned(),
        })
    );
    validate_capabilities(&[
        test_capability_with_resources("a", "a", "PolicyA", TOOL_A_DIRECT, RES_A),
        test_capability_with_resources("b", "b", "PolicyB", TOOL_B_DIRECT, RES_B),
    ])
    .expect("不冲突的资源面必须通过");
}

#[test]
fn validation_rejects_invalid_dependencies() {
    assert_eq!(
        validate_capabilities(&[test_capability_full(
            "a",
            "a",
            "PolicyA",
            &["missing"],
            TOOL_A_DIRECT,
            NO_RESOURCES
        )]),
        Err(CapabilityRegistryError::InvalidDependency {
            capability: "a".to_owned(),
            dependency: "missing".to_owned()
        })
    );
    assert_eq!(
        validate_capabilities(&[test_capability_full(
            "a",
            "a",
            "PolicyA",
            &["a"],
            TOOL_A_DIRECT,
            NO_RESOURCES
        )]),
        Err(CapabilityRegistryError::InvalidDependency {
            capability: "a".to_owned(),
            dependency: "a".to_owned()
        })
    );
    assert_eq!(
        validate_capabilities(&[
            test_capability_full(
                "a",
                "a",
                "PolicyA",
                &["b", "b"],
                TOOL_A_DIRECT,
                NO_RESOURCES
            ),
            test_capability_full("b", "b", "PolicyB", &[], TOOL_B_DIRECT, NO_RESOURCES),
        ]),
        Err(CapabilityRegistryError::DuplicateDependency {
            capability: "a".to_owned(),
            dependency: "b".to_owned()
        })
    );
    assert_eq!(
        validate_capabilities(&[
            test_capability_full("a", "a", "PolicyA", &["b"], TOOL_A_DIRECT, NO_RESOURCES),
            test_capability_full("b", "b", "PolicyB", &["a"], TOOL_B_DIRECT, NO_RESOURCES),
        ]),
        Err(CapabilityRegistryError::DependencyCycle {
            capability: "a".to_owned()
        })
    );
    validate_capabilities(&[
        test_capability_full("a", "a", "PolicyA", &["b"], TOOL_A_DIRECT, NO_RESOURCES),
        test_capability_full("b", "b", "PolicyB", &[], TOOL_B_DIRECT, NO_RESOURCES),
    ])
    .expect("无环依赖必须通过");
}
